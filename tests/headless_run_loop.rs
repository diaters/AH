//! 主循环（`run_loop`）与 headless 运行形态测试。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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

#[test]
fn waiting_user_sleeps_until_shutdown_grace_without_ecs_polling() {
    let (mut app, _input_tx) = app_with_input();
    let mut task = harness::domain::Task::from_user_input(
        "waiting",
        3,
        harness::domain::ChannelId {
            frontend: harness::domain::FrontendKind::Tui,
            user_id: "test".into(),
            thread_id: None,
        },
    );
    task.mark_waiting(harness::domain::WaitingReason::User, chrono::Utc::now());
    let id = task.id;
    let entity = app.world_mut().spawn(task).id();
    app.world_mut()
        .resource_mut::<harness::ecs::EntityIndex>()
        .tasks
        .insert(id, entity);
    let mut hooks = HeadlessLoop::new(Arc::new(AtomicUsize::new(1)), Duration::from_millis(200));
    let ticks = run_loop(&mut app, &mut hooks);
    println!("waiting_user: {ticks} ECS updates in 200ms");
    assert!(ticks <= 8, "waiting task caused {ticks} updates in 200ms");
}

struct Until(Instant);
impl LoopHooks for Until {
    fn poll(&mut self) -> bool {
        false
    }
    fn should_quit(&self) -> bool {
        Instant::now() >= self.0
    }
    fn next_deadline(&self) -> Option<Instant> {
        Some(self.0)
    }
}

struct UntilObserved(Arc<AtomicBool>, Instant);
impl LoopHooks for UntilObserved {
    fn poll(&mut self) -> bool {
        false
    }
    fn should_quit(&self) -> bool {
        self.0.load(Ordering::SeqCst) || Instant::now() >= self.1
    }
    fn next_deadline(&self) -> Option<Instant> {
        Some(self.1)
    }
}

#[test]
fn ui_notifications_do_not_run_ecs() {
    let (mut app, _input_tx) = app_with_input();
    let wake = app
        .world()
        .resource::<harness::contracts::RuntimeWake>()
        .clone();
    let worker = std::thread::spawn(move || {
        for _ in 0..10 {
            std::thread::sleep(Duration::from_millis(5));
            wake.notify_ui();
        }
    });
    let ticks = run_loop(
        &mut app,
        &mut Until(Instant::now() + Duration::from_millis(100)),
    );
    worker.join().unwrap();
    assert!(ticks <= 3, "UI notifications caused {ticks} ECS updates");
}

#[test]
fn maintenance_output_is_consumed_without_external_input() {
    use harness::domain::{SystemOutputMessage, TaskId};
    use harness::prelude::*;
    #[derive(Resource, Default)]
    struct Seen(u32, Arc<AtomicBool>);
    fn emit(mut commands: Commands, mut sent: Local<bool>) {
        if !*sent {
            *sent = true;
            // 模拟繁忙 runner 上超过原 100 ms 观察窗口的首轮处理。
            std::thread::sleep(Duration::from_millis(120));
            commands.spawn(SystemOutputMessage {
                task_id: TaskId::nil(),
                content: "late-output".into(),
            });
        }
    }
    fn observe(messages: Query<&SystemOutputMessage>, mut seen: ResMut<Seen>) {
        seen.0 += messages
            .iter()
            .filter(|message| message.content == "late-output")
            .count() as u32;
        seen.1.store(seen.0 > 0, Ordering::SeqCst);
    }
    let (mut app, _input_tx) = app_with_input();
    app.init_resource::<Seen>();
    app.add_systems(
        bevy_app::Update,
        emit.in_set(harness::systems::HarnessSet::Maintenance),
    );
    app.add_systems(
        bevy_app::Update,
        observe.in_set(harness::systems::HarnessSet::Ingress),
    );
    let observed = app.world().resource::<Seen>().1.clone();
    let ticks = run_loop(
        &mut app,
        // 完成条件是实际消费输出；deadline 只保护丢唤醒导致的挂起。
        &mut UntilObserved(observed, Instant::now() + Duration::from_secs(5)),
    );
    assert_eq!(app.world().resource::<Seen>().0, 1);
    assert!(ticks <= 5, "late output failed to settle: {ticks} updates");
}

#[test]
fn second_shutdown_signal_wakes_a_sleeping_runtime() {
    let (mut app, _input_tx) = app_with_input();
    let wake = app
        .world()
        .resource::<harness::contracts::RuntimeWake>()
        .clone();
    let signals = Arc::new(AtomicUsize::new(1));
    let worker_signals = signals.clone();
    let worker = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(30));
        worker_signals.store(2, Ordering::SeqCst);
        wake.notify();
    });
    let started = Instant::now();
    run_loop(
        &mut app,
        &mut HeadlessLoop::new(signals, Duration::from_secs(5)),
    );
    worker.join().unwrap();
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[test]
fn wait_timeout_runs_without_new_inputs() {
    use harness::domain::{AgentId, ChannelId, FrontendKind, Task, TaskId, WaitingForTasksInfo};
    let (mut app, _input_tx) = app_with_input();
    let task = Task::from_user_input(
        "wait",
        3,
        ChannelId {
            frontend: FrontendKind::Tui,
            user_id: "test".into(),
            thread_id: None,
        },
    );
    let entity = app
        .world_mut()
        .spawn((
            task,
            WaitingForTasksInfo {
                target_task_ids: vec![TaskId::new()],
                tool_call_id: "timeout-probe".into(),
                agent_id: AgentId::new(),
                timeout_at: chrono::Utc::now() + chrono::Duration::milliseconds(40),
            },
        ))
        .id();
    let ticks = run_loop(
        &mut app,
        &mut Until(Instant::now() + Duration::from_millis(150)),
    );
    assert!(app.world().get::<WaitingForTasksInfo>(entity).is_none());
    assert!(ticks <= 8, "timeout did not settle: {ticks} updates");
}

#[test]
fn delayed_brain_and_llm_results_complete_without_polling() {
    use harness::domain::{
        AgentExecutionOutput, AgentExecutionRequest, AgentRequestKind, ChannelId, EngineEvent,
        ExecutorFuture, Frontend, FrontendKind, OutputContent, UserAction,
    };
    struct Delayed;
    impl AgentExecutor for Delayed {
        fn execute(&self, request: AgentExecutionRequest) -> ExecutorFuture {
            Box::pin(async move {
                tokio::time::sleep(Duration::from_millis(20)).await;
                let text = if request.request_kind == AgentRequestKind::BrainDecision {
                    common::mock_executor::DEFAULT_BRAIN_DECISION_JSON
                } else {
                    "demand-complete"
                };
                Ok(AgentExecutionOutput {
                    content: OutputContent::Text(text.into()),
                    reasoning_content: None,
                })
            })
        }
    }
    struct OutputProbe(Arc<AtomicBool>);
    impl Frontend for OutputProbe {
        fn kind(&self) -> FrontendKind {
            FrontendKind::Tui
        }
        fn push_event(&self, event: EngineEvent) {
            if matches!(event, EngineEvent::Text { content, .. } if content == "demand-complete") {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        fn poll_actions(&self) -> Vec<UserAction> {
            vec![]
        }
        fn requires_polling(&self) -> bool {
            false
        }
    }
    let (mut app, input_tx) = app_with_input();
    app.world_mut()
        .resource_mut::<harness::systems::HarnessSettings>()
        .0
        .agents_config_path = "tests/fixtures/test_agents.toml".into();
    app.world_mut()
        .resource_mut::<harness::systems::HarnessSettings>()
        .0
        .brain = Some(harness::systems::BrainConfig {
        enabled: true,
        fallback_agent: None,
    });
    let memory_dir = tempfile::tempdir().unwrap();
    app.insert_resource(harness::infrastructure::memory::LongTermMemoryService::new(
        harness::infrastructure::memory::MemoryRepository::new(Box::new(
            harness::infrastructure::memory::JsonFileMemoryStore::new(
                memory_dir.path().join("agents"),
            ),
        )),
    ));
    let complete = Arc::new(AtomicBool::new(false));
    app.insert_resource(ExecutorRegistry::from_single_executor(
        Arc::new(Delayed),
        "default",
    ));
    app.world_mut()
        .resource_mut::<harness::contracts::FrontendRegistry>()
        .frontends
        .push(Box::new(OutputProbe(complete.clone())));
    input_tx
        .send(ExternalInput::TextWithChannel {
            sender: None,
            channel: ChannelId {
                frontend: FrontendKind::Tui,
                user_id: "test".into(),
                thread_id: None,
            },
            content: "test demand loop".into(),
        })
        .unwrap();
    let ticks = run_loop(
        &mut app,
        &mut UntilObserved(complete.clone(), Instant::now() + Duration::from_secs(5)),
    );
    assert!(
        complete.load(Ordering::SeqCst),
        "delayed execution stalled after {ticks} updates"
    );
    assert!(ticks <= 16, "delayed execution caused {ticks} updates");
}
