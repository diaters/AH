//! Task 9: commit_tool_effects_system 行为测试。
//!
//! 验证声明式写效果的「慢动作」apply 路径：
//! - 消费 `ToolEffectPending` 实体
//! - 经 `update_scheduler_state` 双资源入口原子落账
//! - 把最终结果（含 apply 时刻才知道的 `existed` 真相）回送通道
//! - 应用完 despawn 效果实体
//!
//! 结果落地仍单点：commit 只送通道，由 ingest 下一帧落地。

mod common;
use bevy_ecs::system::RunSystemOnce;
use common::async_tool_bridge::*;
use harness::domain::{ToolEffect, ToolEffectPending, ToolWorkerPayload};
use harness::triggers::scheduled_task::{
    DynamicScheduledTask, ScheduleSpec, ScheduledTaskInfo, ScheduledTaskRegistry, SchedulerState,
    update_scheduler_state,
};

/// 构造一个带单个 "victim" 动态任务的 world（双账本一致）。
fn world_with_one_task() -> bevy_ecs::prelude::World {
    let mut world = setup_bridge_world();
    world.insert_resource(SchedulerState::default());
    world.insert_resource(ScheduledTaskRegistry::default());
    update_scheduler_state(&mut world, |state, registry| {
        state.dynamic_tasks_mut().push(DynamicScheduledTask {
            id: uuid::Uuid::new_v4(),
            kind: "victim".into(),
            schedule: ScheduleSpec::Once(chrono::Utc::now() + chrono::Duration::hours(1)),
            created_at: chrono::Utc::now(),
        });
        registry.insert(
            "victim",
            ScheduledTaskInfo {
                content: "c".into(),
                output_channel: None,
                is_once: true,
            },
        );
    });
    world
}

#[test]
fn commit_delete_removes_from_both_ledgers_and_reports_existed() {
    let mut world = world_with_one_task();
    let e = world
        .spawn(ToolEffectPending {
            tool_call_id: "commit-1".into(),
            effect: ToolEffect::DeleteScheduledTask {
                kind: "victim".into(),
            },
        })
        .id();

    world
        .run_system_once(harness::systems::commit_tool_effects_system)
        .unwrap();

    // 双账本都删了
    assert!(
        world
            .resource::<SchedulerState>()
            .dynamic_tasks()
            .is_empty()
    );
    assert!(
        world
            .resource::<ScheduledTaskRegistry>()
            .get("victim")
            .is_none()
    );
    // 效果实体已消费
    assert!(world.get::<ToolEffectPending>(e).is_none());
    // 最终结果回送通道，existed=true
    let result = wait_for_tool_result(&mut world, 100).expect("commit result");
    assert_eq!(result.tool_call_id, "commit-1");
    match result.payload {
        ToolWorkerPayload::Completed(Ok(v)) => {
            assert_eq!(v["deleted"], "victim");
            assert_eq!(v["existed"], true);
        }
        other => panic!("unexpected {:?}", other),
    }
}

#[test]
fn commit_delete_absent_kind_reports_existed_false() {
    let mut world = world_with_one_task();
    world.spawn(ToolEffectPending {
        tool_call_id: "commit-2".into(),
        effect: ToolEffect::DeleteScheduledTask {
            kind: "ghost".into(),
        },
    });

    world
        .run_system_once(harness::systems::commit_tool_effects_system)
        .unwrap();

    // 已有任务不受影响
    assert_eq!(world.resource::<SchedulerState>().dynamic_tasks().len(), 1);
    let result = wait_for_tool_result(&mut world, 100).expect("commit result");
    match result.payload {
        ToolWorkerPayload::Completed(Ok(v)) => {
            assert_eq!(v["deleted"], "ghost");
            assert_eq!(v["existed"], false);
        }
        other => panic!("unexpected {:?}", other),
    }
}
