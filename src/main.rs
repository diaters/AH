use std::{
    io::{self, BufRead},
    sync::Arc,
    thread,
    time::Duration,
};

use anyhow::{Context, Result};
use crossbeam_channel::{unbounded, Receiver, Sender};
use harness::{
    ExternalInput, HarnessConfig, OutputMessage, ShutdownState, app_is_idle, build_harness_app,
    create_executor_from_config,
};
use tokio::runtime::Runtime;
use tracing::error;
use tracing_subscriber::{fmt, EnvFilter};

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
                    if !content.is_empty() {
                        let _ = sender.send(ExternalInput::Text(content));
                    }
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

/// 启动输出线程并将结果写回 stdout。
fn spawn_output_thread(receiver: Receiver<OutputMessage>) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        while let Ok(message) = receiver.recv() {
            println!("{}", message.content);
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
