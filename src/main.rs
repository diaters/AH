use std::{
    io::{self, BufRead, Write},
    sync::Arc,
    thread,
    time::Duration,
};

use anyhow::{Context, Result};
use crossbeam_channel::{Receiver, Sender, unbounded};
use harness::{
    ExternalInput, HarnessConfig, OutputKind, OutputMessage, ShutdownState, app_is_idle,
    build_harness_app, create_executor_from_config,
};
use tokio::runtime::Runtime;
use tracing::error;
use tracing_subscriber::{EnvFilter, fmt};

/// 当前等待确认的 request_id（用于关联用户响应）
static mut PENDING_CONFIRMATION: Option<uuid::Uuid> = None;

/// 初始化命令行运行所需的 tracing 日志。
fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    fmt().with_env_filter(filter).without_time().init();
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
                    let _ = sender.send(ExternalInput::Text(content));
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

/// 启动输出线程并将结果写回 stdout。
fn spawn_output_thread(receiver: Receiver<OutputMessage>) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        while let Ok(message) = receiver.recv() {
            match &message.kind {
                OutputKind::Text => {
                    println!("{}", message.content);
                }
                OutputKind::ConfirmationRequest {
                    request_id,
                    title,
                    options,
                } => {
                    // 设置等待确认状态
                    // SAFETY: 单线程访问
                    unsafe { PENDING_CONFIRMATION = Some(*request_id) };

                    // 格式化输出确认请求
                    println!("\n{}", title);
                    println!("Options:");
                    for (i, opt) in options.iter().enumerate() {
                        println!("  [{}] {}", i + 1, opt.label);
                    }
                    print!("Enter choice (1/2/3): ");
                    let _ = io::stdout().flush();
                }
            }
        }
    })
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
    init_tracing();

    let runtime = Arc::new(Runtime::new().context("failed to create tokio runtime")?);
    let config = HarnessConfig::from_env()?;
    let executor = create_executor_from_config(&config.llm)?;
    let (input_tx, input_rx) = unbounded();
    let (output_tx, output_rx) = unbounded();

    let input_handle = spawn_input_thread(input_tx);
    let output_handle = spawn_output_thread(output_rx);
    let mut app = build_harness_app(config, runtime, executor, input_rx, output_tx);

    run_event_loop(&mut app);
    drop(app);

    input_handle
        .join()
        .map_err(|_| anyhow::anyhow!("input thread panicked"))?;
    output_handle
        .join()
        .map_err(|_| anyhow::anyhow!("output thread panicked"))?;

    Ok(())
}
