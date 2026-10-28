//! 主循环（`run_loop`）与 headless 运行形态测试。

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crossbeam_channel::unbounded;
use harness::app::{HeadlessLoop, LoopHooks, build_harness_app, run_loop};
use harness::domain::{AgentExecutor, ExternalInput};
use harness::llm::ExecutorRegistry;
use harness::systems::HarnessConfig;
use tokio::runtime::Runtime;

mod common;

fn app_with_input() -> (bevy_app::App, crossbeam_channel::Sender<ExternalInput>) {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(common::mock_executor::EchoExecutor);
    let registry = ExecutorRegistry::from_single_executor(executor, "default");
    let (input_tx, input_rx) = unbounded();
    let config = HarnessConfig {
        idle_poll_ms: 5,
        active_poll_ms: 1,
        ..HarnessConfig::default()
    };
    let app = build_harness_app(
        config,
        runtime,
        registry,
        input_rx,
        vec![],
        harness::channels::ChannelManager::empty().0,
    );
    (app, input_tx)
}

/// 空闲时收到 Shutdown 输入，主循环优雅退出。
#[test]
fn run_loop_exits_on_shutdown_when_idle() {
    let (mut app, input_tx) = app_with_input();
    input_tx.send(ExternalInput::Shutdown).unwrap();
    let mut hooks = HeadlessLoop::new(Arc::new(AtomicUsize::new(1)), Duration::from_secs(60));

    let started = Instant::now();
    run_loop(&mut app, &mut hooks);
    assert!(started.elapsed() < Duration::from_secs(5));
}

/// 第二个信号立即退出；单个信号在宽限期后退出（会话任务一直未结束时也不会挂死）。
#[test]
fn headless_loop_quits_on_second_signal_or_after_grace() {
    let signals = Arc::new(AtomicUsize::new(0));
    let mut hooks = HeadlessLoop::new(signals.clone(), Duration::from_millis(50));
    assert!(!hooks.poll());
    assert!(!hooks.should_quit());

    signals.store(1, Ordering::SeqCst);
    hooks.poll();
    assert!(
        !hooks.should_quit(),
        "first signal waits for graceful shutdown"
    );
    std::thread::sleep(Duration::from_millis(80));
    hooks.poll();
    assert!(hooks.should_quit(), "grace period elapsed");

    let signals = Arc::new(AtomicUsize::new(2));
    let mut hooks = HeadlessLoop::new(signals, Duration::from_secs(60));
    hooks.poll();
    assert!(hooks.should_quit(), "second signal quits immediately");
}
