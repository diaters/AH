use std::{
    io::{self, BufRead},
    sync::Arc,
    thread,
    time::Duration,
};

use anyhow::{Context, Result};
use crossbeam_channel::{Sender, unbounded};
use harness::{
    ChannelId, ExternalInput, FrontendKind, HarnessConfig, ShutdownState, app_is_idle, build_harness_app, create_executor_from_config,
};
use tokio::runtime::Runtime;
use tracing::error;
use tracing_subscriber::{EnvFilter, fmt};

/// 当前等待确认的 request_id（用于关联用户响应）
static mut PENDING_CONFIRMATION: Option<uuid::Uuid> = None;

/// 初始化命令行运行所需的 tracing 日志。
///
/// 终端层：纯文本，级别受 RUST_LOG 控制（默认 INFO）。
/// 文件层：JSON Lines，级别固定 DEBUG，写入 `logs/` 目录（可通过
/// `HARNESS_LOG_DIR` 环境变量覆盖）。
fn init_tracing() -> tracing_appender::non_blocking::WorkerGuard {
    use tracing_subscriber::Layer;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let file_filter = EnvFilter::new("debug");

    let log_dir = std::env::var("HARNESS_LOG_DIR").unwrap_or_else(|_| "logs".to_string());
    let file_appender = tracing_appender::rolling::never(
        &log_dir,
        format!(
            "harness_{}.jsonl",
            chrono::Local::now().format("%Y-%m-%d_%H-%M-%S")
        ),
    );
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    let stdout_layer = fmt::layer().without_time().with_filter(env_filter);

    let file_layer = fmt::layer()
        .json()
        .with_writer(non_blocking)
        .with_ansi(false)
        .with_filter(file_filter);

    tracing_subscriber::registry()
        .with(stdout_layer)
        .with(file_layer)
        .init();

    guard
}

/// 启动阻塞 stdin 读取线程，并将输入写入 ingress channel。
fn spawn_input_thread(sender: Sender<ExternalInput>) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let stdin = io::stdin();
        let mut reader = stdin.lock();

        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => {
                    let _ = sender.send(ExternalInput::Shutdown);
                    break;
                }
                Ok(_) => {
                    let content = line.trim().to_string();
                    if content.is_empty() {
                        continue;
                    }

                    // 检查是否是确认响应
                    // SAFETY: 单线程访问
                    let pending_id = unsafe { PENDING_CONFIRMATION };

                    if let Some(request_id) = pending_id {
                        // 解析用户选择
                        let option = parse_confirmation_response(&content);
                        if let Some(opt) = option {
                            let _ = sender.send(ExternalInput::Confirmation {
                                request_id,
                                option: opt,
                            });
                            // 清除等待状态
                            unsafe { PENDING_CONFIRMATION = None };
                            continue;
                        }
                    }

                    // 普通文本输入
                    let _ = sender.send(ExternalInput::TextWithChannel {
                        channel: ChannelId { frontend: FrontendKind::Tui, user_id: "default".to_string() },
                        content,
                    });
                }
                Err(error) => {
                    error!(?error, "failed to read stdin");
                    let _ = sender.send(ExternalInput::Shutdown);
                    break;
                }
            }
        }
    })
}

/// 解析用户确认响应
fn parse_confirmation_response(input: &str) -> Option<String> {
    let trimmed = input.trim().to_lowercase();
    match trimmed.as_str() {
        "1" | "y" | "yes" | "once" => Some("allow_once".to_string()),
        "2" | "always" | "permanent" => Some("allow_always".to_string()),
        "3" | "n" | "no" | "deny" => Some("deny".to_string()),
        _ => None,
    }
}

/// 运行应用主循环，直到收到关闭请求且内部状态全部清空。
fn run_event_loop(app: &mut bevy::app::App) {
    loop {
        app.update();

        let shutdown_requested = app.world().resource::<ShutdownState>().requested;
        if shutdown_requested && app_is_idle(app.world_mut()) {
            break;
        }

        thread::sleep(Duration::from_millis(10));
    }
}

/// 组装线程、运行时与 ECS 应用，启动 MVP 主程序。
fn main() -> Result<()> {
    // 加载 .env.local 文件（如果存在）
    dotenvy::from_filename(".env.local").ok();
    let _log_guard = init_tracing();

    let runtime = Arc::new(Runtime::new().context("failed to create tokio runtime")?);
    let config = HarnessConfig::from_env()?;
    let executor = create_executor_from_config(&config.llm)?;
    let (input_tx, input_rx) = unbounded();

    let input_handle = spawn_input_thread(input_tx);
    let mut app = build_harness_app(config, runtime, executor, input_rx, vec![]);

    run_event_loop(&mut app);
    drop(app);

    input_handle
        .join()
        .map_err(|_| anyhow::anyhow!("input thread panicked"))?;

    Ok(())
}
