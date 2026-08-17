//! `update_scheduler_state` 双资源原子入口测试（D10 不变量）。
//!
//! 验证闭包同时持有 `&mut SchedulerState` 与 `&mut ScheduledTaskRegistry`，
//! watch 在两个资源都改完后只发一次；watcher 缺失时静默不 panic。

use chrono::{Duration, Utc};
use harness::prelude::*;
use uuid::Uuid;

use harness::triggers::ScheduleSpec;
use harness::triggers::scheduled_task::{
    DynamicScheduledTask, ScheduledTaskInfo, ScheduledTaskRegistry, SchedulerState,
    update_scheduler_state,
};

/// 闭包同时对 `SchedulerState.dynamic_tasks` 与 `ScheduledTaskRegistry` 落账，
/// 两个资源在调用结束后都应反映修改。
#[test]
fn update_modifies_both_ledgers_atomically() {
    let mut world = World::new();
    world.insert_resource(SchedulerState::default());
    world.insert_resource(ScheduledTaskRegistry::default());

    update_scheduler_state(&mut world, |state, registry| {
        state.dynamic_tasks_mut().push(DynamicScheduledTask {
            id: Uuid::new_v4(),
            kind: "k1".into(),
            schedule: ScheduleSpec::Once(Utc::now() + Duration::hours(1)),
            created_at: Utc::now(),
        });
        registry.insert(
            "k1",
            ScheduledTaskInfo {
                content: "c".into(),
                output_channel: None,
                is_once: true,
            },
        );
    });

    assert_eq!(world.resource::<SchedulerState>().dynamic_tasks().len(), 1);
    assert!(
        world
            .resource::<ScheduledTaskRegistry>()
            .get("k1")
            .is_some()
    );
}

/// 回归既有行为：watcher 与两个资源都缺失时静默兜底，不 panic。
#[test]
fn update_without_watcher_does_not_panic() {
    let mut world = World::new();
    update_scheduler_state(&mut world, |state, _registry| {
        state.dynamic_tasks_mut().push(DynamicScheduledTask {
            id: Uuid::new_v4(),
            kind: "k2".into(),
            schedule: ScheduleSpec::Once(Utc::now()),
            created_at: Utc::now(),
        });
    });
    assert_eq!(world.resource::<SchedulerState>().dynamic_tasks().len(), 1);
}
