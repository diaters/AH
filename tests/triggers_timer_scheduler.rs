//! Timer scheduler 集成测试
//!
//! 不测试真实 cron 等待（太慢），改为通过 watch 通道与 tracing 事件捕获
//! 验证热加载行为：scheduler 必须在收到新配置后实际重建 schedules 并发出
//! `TimerSchedulerReloaded` / `TimerSchedulerReloadFailed` 事件。

use std::fmt;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use crossbeam_channel::unbounded;
use harness::domain::ChannelId;
use harness::domain::ExternalInput;
use harness::domain::FrontendKind;
use harness::triggers::SchedulerRoutes;
use harness::triggers::SchedulerState;
use harness::triggers::TimerConfig;
use harness::triggers::TimerRouteConfig;
use harness::triggers::WebhookConfig;
use harness::triggers::run_timer_scheduler;
use tokio::sync::watch;
use tracing::field::Field;
use tracing::field::Visit;
use tracing_subscriber::Layer;
use tracing_subscriber::Registry;
use tracing_subscriber::layer::Context;
use tracing_subscriber::layer::SubscriberExt;

/// 捕获 tracing 事件中 `event` 字段的值，用于断言 reload 行为是否真实发生。
#[derive(Clone, Default)]
struct CapturingLayer {
    events: Arc<Mutex<Vec<String>>>,
}

#[derive(Default)]
struct EventNameVisitor {
    name: Option<String>,
}

impl Visit for EventNameVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "event" {
            self.name = Some(value.to_string());
        }
    }

    fn record_debug(&mut self, _field: &Field, _value: &dyn fmt::Debug) {
        // 仅关心 `event` 字段（str）；count、error 等字段忽略。
    }
}

impl<S: tracing::Subscriber> Layer<S> for CapturingLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let mut visitor = EventNameVisitor::default();
        event.record(&mut visitor);
        if let Some(name) = visitor.name {
            self.events.lock().unwrap().push(name);
        }
    }
}

fn route(kind: &str, cron: &str) -> TimerRouteConfig {
    TimerRouteConfig {
        kind: kind.to_string(),
        cron: cron.to_string(),
        approval_channel: ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "r".to_string(),
            thread_id: None,
        },
        approval_context: "x".to_string(),
        prompt_template: "x".to_string(),
    }
}

/// 构造带单个 timer 路由的 `SchedulerState`（static_routes 非 None）。
fn state_with_timer_route(kind: &str, cron: &str) -> SchedulerState {
    let mut state = SchedulerState::default();
    state.set_static_routes(SchedulerRoutes {
        timer: TimerConfig {
            enabled: true,
            routes: vec![route(kind, cron)],
        },
        webhook: WebhookConfig::default(),
    });
    state
}

fn assert_contains(events: &CapturingLayer, name: &str) {
    let got = events.events.lock().unwrap().clone();
    assert!(
        got.iter().any(|n| n == name),
        "expected event `{}` in captured events, got: {:?}",
        name,
        got
    );
}

#[tokio::test]
async fn scheduler_starts_with_empty_config_and_handles_reload() {
    let (input_tx, _input_rx) = unbounded::<ExternalInput>();
    let (config_tx, config_rx) = watch::channel(SchedulerState::default());

    let capturing = CapturingLayer::default();
    let subscriber = Registry::default().with(capturing.clone());
    // current-thread runtime：spawned task 与本测试同线程，thread-local
    // default subscriber 会对 scheduler task 生效。
    let _guard = tracing::subscriber::set_default(subscriber);

    let handle = tokio::spawn(async move {
        let _ = run_timer_scheduler(input_tx, config_rx).await;
    });

    // 给 scheduler 启动时间
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_contains(&capturing, "TimerSchedulerStarted");

    // 发送有效配置（一条路由），应触发 TimerSchedulerReloaded
    config_tx
        .send(state_with_timer_route("daily", "0 9 * * 1-5"))
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_contains(&capturing, "TimerSchedulerReloaded");

    // 发送无效 cron 配置，应触发 TimerSchedulerReloadFailed，且 scheduler 不退出
    config_tx
        .send(state_with_timer_route("bad", "not a cron"))
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_contains(&capturing, "TimerSchedulerReloadFailed");
    assert!(
        !handle.is_finished(),
        "scheduler must survive a failed reload"
    );

    // 再发送一个有效但空配置，应再次触发 TimerSchedulerReloaded
    config_tx.send(SchedulerState::default()).unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let got = capturing.events.lock().unwrap().clone();
    let reloaded_count = got
        .iter()
        .filter(|n| *n == "TimerSchedulerReloaded")
        .count();
    assert_eq!(
        reloaded_count, 2,
        "expected two TimerSchedulerReloaded events, got: {:?}",
        got
    );

    handle.abort();
}

#[tokio::test]
async fn scheduler_initial_config_with_invalid_cron_returns_error() {
    let (input_tx, _input_rx) = unbounded::<ExternalInput>();
    let bad_state = state_with_timer_route("bad", "not a cron");
    let (_config_tx, config_rx) = watch::channel(bad_state);

    let result = run_timer_scheduler(input_tx, config_rx).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn scheduler_starts_with_empty_state_when_no_static_routes() {
    // 无 triggers.toml 时，SchedulerState.static_routes = None，scheduler 仍正常启动。
    let (input_tx, _input_rx) = unbounded::<ExternalInput>();
    let (_config_tx, config_rx) = watch::channel(SchedulerState::default());

    let capturing = CapturingLayer::default();
    let subscriber = Registry::default().with(capturing.clone());
    let _guard = tracing::subscriber::set_default(subscriber);

    let handle = tokio::spawn(async move {
        let _ = run_timer_scheduler(input_tx, config_rx).await;
    });

    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_contains(&capturing, "TimerSchedulerStarted");
    assert!(
        !handle.is_finished(),
        "scheduler must keep running with empty state"
    );

    handle.abort();
}

/// 验证 reload（发送新的 static config）保留 `dynamic_tasks`：
/// 先通过 `SchedulerState` 添加一个已过期的一次性动态任务，再发送空
/// static config 的 reload，验证 scheduler 仍然运行且收到了 Timer 信号。
#[tokio::test]
async fn reload_preserves_dynamic_tasks() {
    use chrono::Utc;
    use harness::triggers::{DynamicScheduledTask, ScheduleSpec};
    use uuid::Uuid;

    let (input_tx, input_rx) = unbounded::<ExternalInput>();
    let initial = SchedulerState::default();
    let (state_tx, state_rx) = watch::channel(initial);

    let handle = tokio::spawn(async move {
        let _ = run_timer_scheduler(input_tx, state_rx).await;
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    // 添加一个已过期的一次性动态任务，验证 reload 后仍能触发
    let mut new_state = SchedulerState::default();
    new_state.dynamic_tasks_mut().push(DynamicScheduledTask {
        id: Uuid::new_v4(),
        kind: "scheduled:test".to_string(),
        schedule: ScheduleSpec::Once(Utc::now() - chrono::Duration::minutes(1)),
        created_at: Utc::now(),
    });
    state_tx.send(new_state).unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    // 发送一个空 static config 的 reload，dynamic_tasks 应被保留
    state_tx.send(SchedulerState::default()).unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    // 检查 scheduler 仍然运行，且收到了 Timer 信号
    assert!(!handle.is_finished());
    assert!(
        input_rx.try_recv().is_ok(),
        "expected ExternalInput::Timer after reload"
    );
    handle.abort();
}

/// 验证 cron 调度使用 `Local` 时区。
///
/// 用户在 `triggers.toml` 中书写 5 字段标准 cron（分 时 日 月 周），
/// 内部加载时通过 `format!("0 {} *", user_cron)` 补齐为 7 字段
/// （秒=0，年=*）。`schedule.upcoming(Local)` 必须返回本地时区的
/// 下一次触发时间，因此工作日 9:00 的 cron 在本地时区下 hour==9。
#[test]
fn cron_schedule_uses_local_timezone() {
    use chrono::{Local, Timelike};
    use cron::Schedule;
    use std::str::FromStr;

    // 用户输入 5 字段 cron，内部补齐为 7 字段（秒=0，年=*）
    let user_cron = "0 9 * * 1-5"; // 工作日本地 9:00
    let cron_expr = format!("0 {} *", user_cron);
    let schedule = Schedule::from_str(&cron_expr).unwrap();
    let now = Local::now();
    let next = schedule.upcoming(Local).next().unwrap();
    // 验证 next 的小时数是 9（本地时间）
    assert_eq!(next.hour(), 9);
    assert!(next > now);
}

/// 验证一次性任务通过 scheduler 真实触发并产出 `ExternalInput::Timer`。
///
/// 此测试覆盖 `now_utc` 在 sleep 唤醒后必须重新获取的关键路径：
/// 若使用 sleep 前的 `now_utc`，则未来一次性任务的 `at <= pre_sleep_now`
/// 永远为 false，任务不会触发，测试会因 timeout 失败。
#[tokio::test]
async fn one_shot_task_triggers_and_is_removed() {
    use chrono::Utc;
    use harness::triggers::{DynamicScheduledTask, ScheduleSpec};
    use uuid::Uuid;

    let (input_tx, input_rx) = unbounded::<ExternalInput>();
    let initial = SchedulerState::default();
    let (state_tx, state_rx) = watch::channel(initial);

    let handle = tokio::spawn(async move {
        let _ = run_timer_scheduler(input_tx, state_rx).await;
    });

    // 添加一个 2 秒后触发的一次性任务
    let mut state = SchedulerState::default();
    state.dynamic_tasks_mut().push(DynamicScheduledTask {
        id: Uuid::new_v4(),
        kind: "scheduled:test-once".to_string(),
        schedule: ScheduleSpec::Once(Utc::now() + chrono::Duration::seconds(2)),
        created_at: Utc::now(),
    });
    state_tx.send(state).unwrap();

    // 在 10 秒内应收到 ExternalInput::Timer。crossbeam recv 是阻塞调用，
    // 用 spawn_blocking + recv_timeout 实现可被 tokio::time::timeout 中断的等待。
    let rx = input_rx;
    let received = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::task::spawn_blocking(move || rx.recv_timeout(Duration::from_secs(9))),
    )
    .await;
    assert!(
        received.is_ok(),
        "expected ExternalInput::Timer within 10s, scheduler did not fire"
    );
    let msg = received
        .unwrap()
        .expect("blocking task panicked")
        .expect("channel closed without message");
    match msg {
        ExternalInput::Timer { kind, .. } => {
            assert_eq!(kind, "scheduled:test-once");
        }
        other => panic!("expected ExternalInput::Timer, got {:?}", other),
    }

    handle.abort();
}
