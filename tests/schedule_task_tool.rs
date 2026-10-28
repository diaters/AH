//! schedule_task 工具端到端集成测试
//!
//! Task 14 上桥后，`schedule_task_commit_system` 与 `ScheduleTaskRequestMessage` /
//! `ScheduleTaskCommitPending` 已退役，写路径统一经
//! `ToolEffect::ScheduleTask` + `commit_tool_effects_system` 落账
//! （覆盖见 `tests/schedule_task_commit_test.rs`）。本文件仅保留
//! `trigger_task_routing_system` 在收到 `Timer { kind: "scheduled:..." }` 触发后
//! 从 `SchedulerState` 取出动态定时任务定义生成 `CreateTaskMessage`、
//! 并对一次性任务记账（运行结束后才删除，见 `once_schedule_completion_system`）的回归测试。

use harness::prelude::*;

use harness::domain::{
    ChannelId, CreateTaskMessage, FrontendKind, ScheduleDefinition, ScheduleExpr, SignalSource,
    TaskTrigger,
};
use harness::systems::transform::trigger_task_routing_system;
use harness::triggers::{SchedulerState, SchedulerStateWatcher};

/// `trigger_task_routing_system` 在收到 `Timer { kind: "scheduled:..." }` 触发后，
/// 从 `SchedulerState` 取出定义生成 `CreateTaskMessage`，
/// `routing_policy.output_channel` 来自定义。
#[test]
fn scheduled_task_trigger_routes_to_create_task_message() {
    let mut app = App::new();
    app.insert_resource(harness::domain::SignalTriggerRegistry::default());
    app.insert_resource(SchedulerStateWatcher::default());
    app.insert_resource(SchedulerState::default());
    app.add_systems(Update, trigger_task_routing_system);

    let channel = ChannelId {
        frontend: FrontendKind::QQ,
        user_id: "group".to_string(),
        thread_id: None,
    };
    let at = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
    let def = ScheduleDefinition::new(
        uuid::Uuid::new_v4(),
        None,
        "analyze".to_string(),
        ScheduleExpr::parse(&format!("once:{at}")).unwrap(),
        Some(channel.clone()),
        chrono::Utc::now(),
    );
    let kind = def.kind();
    app.world_mut()
        .resource_mut::<SchedulerState>()
        .dynamic_tasks_mut()
        .push(def);

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
        messages[0].routing_policy.output_channel(),
        Some(&channel),
        "output_channel should come from the definition"
    );

    // 一次性任务触发后只记账、保留到运行结束（timer 依据 last_fired_at 不再调度）
    assert!(
        app.world()
            .resource::<SchedulerState>()
            .find_by_kind(&kind)
            .is_some_and(|d| d.health.last_fired_at.is_some()),
        "one-shot task must be kept and marked fired until its run finishes"
    );
}
