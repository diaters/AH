//! schedule_task 工具端到端集成测试
//!
//! Task 14 上桥后，`schedule_task_commit_system` 与 `ScheduleTaskRequestMessage` /
//! `ScheduleTaskCommitPending` 已退役，写路径统一经
//! `ToolEffect::ScheduleTask` + `commit_tool_effects_system` 落账
//! （覆盖见 `tests/schedule_task_commit_test.rs`）。本文件仅保留
//! `trigger_task_routing_system` 在收到 `Timer { kind: "scheduled:..." }` 触发后
//! 从 `ScheduledTaskRegistry` 取出动态任务信息生成 `CreateTaskMessage`、
//! 并对一次性任务做清理的回归测试。

use harness::prelude::*;

use harness::domain::{ChannelId, CreateTaskMessage, FrontendKind, SignalSource, TaskTrigger};
use harness::systems::transform::trigger_task_routing_system;
use harness::triggers::{
    ScheduledTaskInfo, ScheduledTaskRegistry, SchedulerState, SchedulerStateWatcher,
};

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

    let id = uuid::Uuid::new_v4();
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
