use std::{sync::Arc, thread, time::Duration};

use anyhow::{Context, Result};
use crossbeam_channel::unbounded;
use crossterm::event::{self, Event, KeyEventKind};
use harness::tui::{App, TuiFrontend};
use harness::{
    EngineEvent, ExternalInput, HarnessConfig, HarnessSettings, ShutdownState, UserAction,
    app_is_idle, build_harness_app, create_executor_from_config,
    channels::{Channel, ChannelManager, TelegramChannel},
};
use tokio::runtime::Runtime;
use tracing::{debug, info, warn};

fn init_tracing() -> tracing_appender::non_blocking::WorkerGuard {
    use tracing_subscriber::Layer;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let file_filter = tracing_subscriber::EnvFilter::new("debug");

    let log_dir = std::env::var("HARNESS_LOG_DIR").unwrap_or_else(|_| "logs".to_string());
    let file_appender = tracing_appender::rolling::never(
        &log_dir,
        format!(
            "harness_{}.jsonl",
            chrono::Local::now().format("%Y-%m-%d_%H-%M-%S")
        ),
    );
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    let file_layer = tracing_subscriber::fmt::layer()
        .json()
        .with_writer(non_blocking)
        .with_ansi(false)
        .with_filter(file_filter);

    tracing_subscriber::registry().with(file_layer).init();

    guard
}

/// RAII guard ensuring terminal is restored even on panic.
struct TuiGuard;

impl Drop for TuiGuard {
    fn drop(&mut self) {
        // 禁用启动时启用的终端特性，再恢复 raw mode / alternate screen
        let _ = crossterm::execute!(
            std::io::stdout(),
            crossterm::event::DisableMouseCapture,
            crossterm::event::DisableBracketedPaste,
        );
        ratatui::restore();
    }
}

fn main() -> Result<()> {
    dotenvy::from_filename(".env.local").ok();
    let _log_guard = init_tracing();

    // 安装 panic hook：确保 panic 时日志能 flush，并恢复终端
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!(
            event = "PanicCaught",
            panic_info = %info,
            "panic occurred in main thread"
        );
        // 兜底：确保 panic 时终端也能被正确恢复
        let _ = crossterm::execute!(
            std::io::stdout(),
            crossterm::event::DisableMouseCapture,
            crossterm::event::DisableBracketedPaste,
        );
        ratatui::restore();
        default_hook(info);
    }));

    info!(
        event = "HarnessStarting",
        version = env!("CARGO_PKG_VERSION"),
        "AI Harness TUI starting"
    );

    let runtime = Arc::new(Runtime::new().context("failed to create tokio runtime")?);
    let config = HarnessConfig::from_env()?;
    let executor = create_executor_from_config(&config.llm)?;

    // 创建 Frontend channel
    let (event_tx, event_rx) = unbounded::<EngineEvent>();
    let (action_tx, action_rx) = unbounded::<UserAction>();

    let tui_frontend = TuiFrontend::new(event_tx, action_rx);

    // 构建通道列表
    let mut channel_list: Vec<Arc<dyn Channel>> = vec![];
    if let Some(tg_cfg) = config.channels.telegram.clone() {
        info!(event = "TelegramChannelEnabled", "enabling Telegram channel");
        channel_list.push(Arc::new(TelegramChannel::new(tg_cfg)));
    }

    // 创建 input channel：IM 入向消息和 TUI 输入共用
    let (input_tx, input_rx) = unbounded::<ExternalInput>();

    // 启动 ChannelManager（无通道时为空操作）
    let (channel_manager, channel_handle) = ChannelManager::new(channel_list, input_tx);
    let runtime_clone = runtime.clone();

    // 构建 ECS app
    let mut app = build_harness_app(
        config,
        runtime,
        executor,
        input_rx,
        vec![Box::new(tui_frontend)],
        channel_manager,
    );

    info!(
        event = "EcsAppBuilt",
        "ECS app built, entering TUI main loop"
    );

    // 启用 crossterm 的鼠标和粘贴支持
    crossterm::execute!(
        std::io::stdout(),
        crossterm::event::EnableMouseCapture,
        crossterm::event::EnableBracketedPaste,
    )?;

    // 启动 ratatui
    let _guard = TuiGuard;
    let mut terminal = ratatui::init();
    let mut app_state = App::new(action_tx);

    let mut tick: u64 = 0;

    loop {
        // 1. 处理 crossterm 事件
        let mut key_count = 0u32;
        let mut paste_count = 0u32;
        let mut mouse_count = 0u32;

        // 用非致命方式读取事件，避免 IME 等场景下的错误导致整个程序退出
        match event::poll(Duration::ZERO) {
            Ok(true) => loop {
                let ev = match event::read() {
                    Ok(ev) => ev,
                    Err(e) => {
                        warn!(
                            event = "CrosstermReadError",
                            error = %e,
                            "failed to read terminal event, skipping"
                        );
                        break;
                    }
                };
                match ev {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        app_state.handle_key_event(key);
                        key_count += 1;
                    }
                    Event::Key(key) => {
                        // Release / Repeat 事件忽略，避免 IME 组合过程中的误触发
                        debug!(
                            event = "KeyEventIgnored",
                            kind = ?key.kind,
                            code = ?key.code,
                            "ignored non-press key event"
                        );
                    }
                    Event::Mouse(mouse) => {
                        app_state.handle_mouse_event(mouse);
                        mouse_count += 1;
                    }
                    Event::Paste(text) => {
                        app_state.handle_paste(&text);
                        paste_count += 1;
                    }
                    _ => {}
                }
                // 检查是否还有待处理事件
                match event::poll(Duration::ZERO) {
                    Ok(true) => continue,
                    Ok(false) => break,
                    Err(e) => {
                        warn!(
                            event = "CrosstermPollError",
                            error = %e,
                            "failed to poll terminal events"
                        );
                        break;
                    }
                }
            },
            Ok(false) => {}
            Err(e) => {
                warn!(
                    event = "CrosstermPollError",
                    error = %e,
                    "failed to poll terminal events"
                );
            }
        }

        if key_count > 0 || paste_count > 0 {
            debug!(
                event = "InputProcessed",
                key_count,
                paste_count,
                mode = ?app_state.mode,
                "processed input events"
            );
        }

        // 2. 从 channel 拉取 EngineEvent，更新 TUI 状态
        let mut engine_event_count = 0u32;
        while let Ok(ev) = event_rx.try_recv() {
            debug!(
                event = "EngineEventReceived",
                event_kind = ?ev,
                "received engine event from channel"
            );
            app_state.handle_engine_event(ev);
            engine_event_count += 1;
        }

        // 3. 驱动 ECS
        app.update();

        let shutdown_requested = app.world().resource::<ShutdownState>().requested;
        let idle = app_is_idle(app.world_mut());
        let had_input = key_count > 0 || paste_count > 0 || mouse_count > 0;
        let had_engine_events = engine_event_count > 0;
        let settings = app.world().resource::<HarnessSettings>();
        let sleep_ms = if idle && !had_input && !had_engine_events {
            settings.0.idle_poll_ms
        } else {
            settings.0.active_poll_ms
        };
        let should_render = !idle || had_input || had_engine_events;

        if tick.is_multiple_of(300) {
            debug!(
                event = "TuiLoopHeartbeat",
                tick,
                shutdown_requested,
                idle,
                sleep_ms,
                should_render,
                should_quit = app_state.should_quit,
                messages = app_state.messages.len(),
                agents = app_state.agents.len(),
                tasks = app_state.tasks.len(),
                pending_approvals = app_state.pending_approvals.len(),
                "TUI main loop heartbeat"
            );
        }

        if shutdown_requested && idle {
            info!(
                event = "GracefulShutdown",
                tick, "shutdown requested and app is idle, exiting"
            );
            break;
        }

        // 4. 退出检查
        if app_state.should_quit {
            info!(event = "UserQuit", tick, "user requested quit, exiting");
            break;
        }

        // 5. 渲染 TUI（空闲且无新事件时跳过无意义重绘）
        if should_render && let Err(e) = terminal.draw(|frame| app_state.render(frame)) {
            warn!(event = "RenderError", error = %e, "TUI render failed");
        }

        thread::sleep(Duration::from_millis(sleep_ms));
        tick += 1;
    }

    info!(
        event = "HarnessExiting",
        total_ticks = tick,
        "AI Harness TUI exiting"
    );

    // 优雅关闭 ChannelManager
    {
        let channel_manager = app.world().resource::<ChannelManager>();
        channel_manager.shutdown();
    }
    let _ = runtime_clone.block_on(channel_handle);

    // TuiGuard drop 时会自动禁用 mouse capture / bracketed paste 并恢复终端

    Ok(())
}
