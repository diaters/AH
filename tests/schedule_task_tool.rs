//! schedule_task 工具端到端集成测试
//!
//! 覆盖两条链路：
//! 1. `schedule_task_commit_system` 把 `ScheduleTaskRequestMessage` 提交到
//!    `SchedulerState.dynamic_tasks` 与 `ScheduledTaskRegistry`，并保持
//!    `SchedulerStateWatcher` 通知语义。
//! 2. `trigger_task_routing_system` 在收到 `Timer { kind: "scheduled:..." }`
//!    触发后，从 `ScheduledTaskRegistry` 取出动态任务信息生成 `CreateTaskMessage`，
//!    并对一次性任务做清理。

use chrono::{Duration, Utc};
use harness::prelude::*;
use uuid::Uuid;

use harness::domain::{ChannelId, CreateTaskMessage, FrontendKind, SignalSource, TaskTrigger};
use harness::systems::tools::schedule_task_commit_system;
use harness::systems::transform::trigger_task_routing_system;
use harness::triggers::{
    ScheduleSpec, ScheduleTaskCommitPending, ScheduleTaskRequestMessage, ScheduledTaskInfo,
    ScheduledTaskRegistry, SchedulerState, SchedulerStateWatcher,
};

/// `schedule_task_commit_system` 处理一条 Once 请求后：
/// - `ScheduledTaskRegistry` 含对应 `kind`
/// - `SchedulerState.dynamic_tasks` 长度为 1
#[test]
fn schedule_task_commit_adds_task_to_registry() {
    let mut world = World::new();
    world.insert_resource(SchedulerStateWatcher::default());
    world.insert_resource(SchedulerState::default());
    world.insert_resource(ScheduledTaskRegistry::default());

    let id = Uuid::new_v4();
    let kind = format!("scheduled:{}", id);
    world.spawn((
        ScheduleTaskRequestMessage {
            id,
            kind: kind.clone(),
            content: "greet".to_string(),
            schedule: ScheduleSpec::Once(Utc::now() + Duration::minutes(5)),
            output_channel: Some(ChannelId {
                frontend: FrontendKind::Telegram,
                user_id: "chat".to_string(),
                thread_id: None,
            }),
        },
        ScheduleTaskCommitPending,
    ));

    let mut schedule = Schedule::default();
    schedule.add_systems(schedule_task_commit_system);
    schedule.run(&mut world);

    assert!(
        world
            .resource::<ScheduledTaskRegistry>()
            .get(&kind)
            .is_some(),
        "registry should contain the committed kind"
    );
    assert_eq!(
        world.resource::<SchedulerState>().dynamic_tasks().len(),
        1,
        "SchedulerState.dynamic_tasks should have one entry"
    );
}

/// `schedule_task_commit_system` 通过 `update_scheduler_state` 通知
/// `SchedulerStateWatcher`，使 timer scheduler 能感知新动态任务。
#[test]
fn schedule_task_commit_notifies_watcher() {
    use tokio::sync::watch;

    let mut world = World::new();
    world.insert_resource(SchedulerState::default());
    world.insert_resource(ScheduledTaskRegistry::default());
    let (tx, mut rx) = watch::channel(SchedulerState::default());
    world.insert_resource(SchedulerStateWatcher(Some(tx)));

    let id = Uuid::new_v4();
    let kind = format!("scheduled:{}", id);
    world.spawn((
        ScheduleTaskRequestMessage {
            id,
            kind,
            content: "watcher test".to_string(),
            schedule: ScheduleSpec::Once(Utc::now() + Duration::minutes(5)),
            output_channel: None,
        },
        ScheduleTaskCommitPending,
    ));

    let mut schedule = Schedule::default();
    schedule.add_systems(schedule_task_commit_system);
    schedule.run(&mut world);

    assert!(
        rx.has_changed().unwrap(),
        "watcher must be notified after commit"
    );
    assert_eq!(
        rx.borrow_and_update().dynamic_tasks().len(),
        1,
        "watcher must observe the new dynamic task"
    );
}

/// `trigger_task_routing_system` 在收到 `Timer { kind: "scheduled:..." }` 触发后，
/// 从 `ScheduledTaskRegistry` 取出动态任务信息生成 `CreateTaskMessage`，
/// `routing_policy.output_channel` 来自 `ScheduledTaskInfo`。
#[test]
fn scheduled_task_trigger_routes_to_create_task_message() {
    let mut app = App::new();
    app.insert_resource(harness::domain::SignalTriggerRegistry::default());
    app.insert_resource(SchedulerStateWatcher::default());
    app.insert_resource(SchedulerState::default());
    app.insert_resource(ScheduledTaskRegistry::default());
    app.add_systems(Update, trigger_task_routing_system);

    let id = Uuid::new_v4();
    let kind = format!("scheduled:{}", id);
    let channel = ChannelId {
        frontend: FrontendKind::QQ,
        user_id: "group".to_string(),
        thread_id: None,
    };
    {
        let mut registry = app.world_mut().resource_mut::<ScheduledTaskRegistry>();
        registry.insert(
            kind.clone(),
            ScheduledTaskInfo {
                content: "analyze".to_string(),
                output_channel: Some(channel.clone()),
                is_once: true,
            },
        );
    }

    app.world_mut().spawn(harness::domain::TriggerTaskMessage {
        source: SignalSource("timer".to_string()),
        trigger: TaskTrigger::Timer { kind: kind.clone() },
    });

    app.update();

    let messages: Vec<_> = app
        .world_mut()
        .query::<&CreateTaskMessage>()
        .iter(app.world())
        .collect();
    assert_eq!(messages.len(), 1, "exactly one CreateTaskMessage expected");
    assert_eq!(messages[0].content, "analyze");
    assert_eq!(
        messages[0].routing_policy.output_channel,
        Some(channel),
        "output_channel should come from ScheduledTaskInfo"
    );

    // 一次性任务触发后应从 registry 中清理
    assert!(
        app.world()
            .resource::<ScheduledTaskRegistry>()
            .get(&kind)
            .is_none(),
        "one-shot task must be removed from registry after trigger"
    );
}
