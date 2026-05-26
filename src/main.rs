use std::{sync::Arc, thread, time::Duration};

use anyhow::{Context, Result};
use crossbeam_channel::unbounded;
use crossterm::event::{self, Event, KeyEventKind};
use harness::tui::{App, TuiFrontend};
use harness::{
    EngineEvent, ExternalInput, HarnessConfig, ShutdownState, UserAction, app_is_idle,
    build_harness_app, create_executor_from_config,
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
        ratatui::restore();
    }
}

fn main() -> Result<()> {
    dotenvy::from_filename(".env.local").ok();
    let _log_guard = init_tracing();

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

    // 为 build_harness_app 提供空的 input_rx（输入已由 FrontendRegistry 接管）
    let (_input_tx, input_rx) = unbounded::<ExternalInput>();

    // 构建 ECS app
    let mut app = build_harness_app(
        config,
        runtime,
        executor,
        input_rx,
        vec![Box::new(tui_frontend)],
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
        while let Ok(ev) = event_rx.try_recv() {
            debug!(
                event = "EngineEventReceived",
                event_kind = ?ev,
                "received engine event from channel"
            );
            app_state.handle_engine_event(ev);
        }

        // 3. 驱动 ECS
        app.update();

        let shutdown_requested = app.world().resource::<ShutdownState>().requested;
        let idle = app_is_idle(app.world_mut());

        if tick.is_multiple_of(300) {
            debug!(
                event = "TuiLoopHeartbeat",
                tick,
                shutdown_requested,
                idle,
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
                tick,
                "shutdown requested and app is idle, exiting"
            );
            break;
        }

        // 4. 退出检查
        if app_state.should_quit {
            info!(
                event = "UserQuit",
                tick,
                "user requested quit, exiting"
            );
            break;
        }

        // 5. 渲染 TUI
        if let Err(e) = terminal.draw(|frame| app_state.render(frame)) {
            warn!(
                event = "RenderError",
                error = %e,
                "TUI render failed"
            );
        }

        thread::sleep(Duration::from_millis(16));
        tick += 1;
    }

    info!(
        event = "HarnessExiting",
        total_ticks = tick,
        "AI Harness TUI exiting"
    );

    Ok(())
}
