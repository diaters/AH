use std::{
    sync::Arc,
    thread,
    time::Duration,
};

use anyhow::{Context, Result};
use crossbeam_channel::unbounded;
use crossterm::event::{self, Event};
use harness::{
    EngineEvent, ExternalInput, HarnessConfig, ShutdownState, UserAction, app_is_idle,
    build_harness_app, create_executor_from_config,
};
use harness::tui::{App, TuiFrontend};
use tokio::runtime::Runtime;

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

    tracing_subscriber::registry()
        .with(file_layer)
        .init();

    guard
}

fn main() -> Result<()> {
    dotenvy::from_filename(".env.local").ok();
    let _log_guard = init_tracing();

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
    let mut app =
        build_harness_app(config, runtime, executor, input_rx, vec![Box::new(tui_frontend)]);

    // 启动 ratatui
    let mut terminal = ratatui::init();
    let mut app_state = App::new(action_tx);

    loop {
        // 1. 处理 crossterm 键盘事件
        while event::poll(Duration::ZERO)? {
            if let Event::Key(key) = event::read()? {
                app_state.handle_key_event(key);
            }
        }

        // 2. 从 channel 拉取 EngineEvent，更新 TUI 状态
        while let Ok(event) = event_rx.try_recv() {
            app_state.handle_engine_event(event);
        }

        // 3. 驱动 ECS
        app.update();
        if app.world().resource::<ShutdownState>().requested && app_is_idle(app.world_mut()) {
            break;
        }

        // 4. 退出检查
        if app_state.should_quit {
            break;
        }

        // 5. 渲染 TUI
        terminal.draw(|frame| app_state.render(frame))?;

        thread::sleep(Duration::from_millis(16));
    }

    ratatui::restore();
    Ok(())
}
