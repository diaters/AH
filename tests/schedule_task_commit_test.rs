//! Task 14 Step B: commit_tool_effects_system 处理 ToolEffect::ScheduleTask 的单元测试。
//!
//! 验证 schedule_task 上桥后，commit 阶段的「写效果唯一应用点」行为：
//! - 双账本一致：`SchedulerState.dynamic_tasks` + `ScheduledTaskRegistry` 同步落账
//! - watch 触发：`SchedulerStateWatcher` 收到一次通知
//! - `next_trigger` 在 apply 时刻计算（Once 直接返回；Cron 算下一次本地时区触发）
//! - 回送 `ToolAsyncResult::completed(call_id, Ok({"status": "scheduled", ...}))`
//! - `ToolEffectPending` 实体 despawn
//! - 资源缺失（watcher / registry）不 panic
//!
//! 与原 `schedule_task_commit_system` 行为对齐：原系统只做双账本落账，
//! 新 commit arm 还要回送最终结果（原 orchestrator arm 在 spawn 时即产结果）。

use std::str::FromStr;

use bevy_ecs::prelude::*;
use chrono::Utc;
use uuid::Uuid;

use harness::domain::{
    ChannelId, FrontendKind, ToolAsyncResult, ToolEffect, ToolEffectPending, ToolResultReceiver,
    ToolResultSender, ToolWorkerPayload,
};
use harness::systems::commit_tool_effects_system;
use harness::triggers::ScheduleSpec;
use harness::triggers::scheduled_task::{
    DynamicScheduledTask, ScheduledTaskInfo, ScheduledTaskRegistry, SchedulerState,
    SchedulerStateWatcher,
};

/// 构造一条 Once ScheduleTask effect。
fn once_effect(channel: Option<ChannelId>) -> (Uuid, ToolEffect) {
    let id = Uuid::new_v4();
    let kind = format!("scheduled:{}", id);
    let schedule = ScheduleSpec::Once(Utc::now() + chrono::Duration::days(1));
    (
        id,
        ToolEffect::ScheduleTask {
            id,
            kind,
            content: "send report".to_string(),
            schedule,
            output_channel: channel,
        },
    )
}

/// 构造一条 Cron ScheduleTask effect。
fn cron_effect() -> (Uuid, ToolEffect) {
    let id = Uuid::new_v4();
    let kind = format!("scheduled:{}", id);
    let schedule = ScheduleSpec::Cron(Box::new(cron::Schedule::from_str("0 0 9 * * * *").unwrap()));
    (
        id,
        ToolEffect::ScheduleTask {
            id,
            kind,
            content: "daily standup".to_string(),
            schedule,
            output_channel: None,
        },
    )
}

fn sample_channel() -> ChannelId {
    ChannelId {
        frontend: FrontendKind::Tui,
        user_id: "tester".to_string(),
        thread_id: None,
    }
}

/// 装入 scheduler 双资源 + 通道（含 receiver 防止 sender 报错）。
fn insert_scheduler_resources(world: &mut World) {
    world.insert_resource(SchedulerState::default());
    world.insert_resource(ScheduledTaskRegistry::default());
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<ToolAsyncResult>();
    world.insert_resource(ToolResultSender(tx));
    // receiver 必须装入世界，否则 sender 在 commit 调 send 时通道虽未断但仍要保留接收端
    // 以防「无 receiver 时 send 失败」——当前 commit 用 `let _ =` 吞错，但保持装齐与
    // 生产世界一致
    world.insert_resource(ToolResultReceiver(rx));
}

/// 装入带 watch sender 的资源。
fn insert_watcher(world: &mut World) -> tokio::sync::watch::Receiver<SchedulerState> {
    let (tx, rx) = tokio::sync::watch::channel(SchedulerState::default());
    world.insert_resource(SchedulerStateWatcher(Some(tx)));
    rx
}

/// 取一条 schedule_task effect，spawn 一个 ToolEffectPending 实体。
fn spawn_effect_pending(world: &mut World, tool_call_id: &str, effect: ToolEffect) -> Entity {
    world
        .spawn(ToolEffectPending {
            tool_call_id: tool_call_id.to_string(),
            effect,
        })
        .id()
}

/// Once effect 提交后：
/// - 双账本各一条，`is_once == true`
/// - watch 触发一次
/// - `ToolAsyncResult::completed` 回送 `{status: scheduled, schedule_id, kind, next_trigger}`
/// - `ToolEffectPending` 实体 despawn
#[test]
fn commit_schedule_task_once_applies_to_both_ledgers_and_sends_result() {
    let mut world = World::new();
    insert_scheduler_resources(&mut world);
    let mut watcher_rx = insert_watcher(&mut world);
    let channel = sample_channel();
    let (id, effect) = once_effect(Some(channel.clone()));
    let kind = match &effect {
        ToolEffect::ScheduleTask { kind, .. } => kind.clone(),
        _ => unreachable!(),
    };
    let pending_entity = spawn_effect_pending(&mut world, "call-once-1", effect);

    commit_tool_effects_system(&mut world);

    // 双账本一致
    let state = world.resource::<SchedulerState>();
    assert_eq!(state.dynamic_tasks().len(), 1);
    assert_eq!(state.dynamic_tasks()[0].id, id);
    assert_eq!(state.dynamic_tasks()[0].kind, kind);
    assert!(matches!(
        state.dynamic_tasks()[0].schedule,
        ScheduleSpec::Once(_)
    ));

    let registry = world.resource::<ScheduledTaskRegistry>();
    let info = registry
        .get(&kind)
        .expect("Once task must be inserted into registry");
    assert_eq!(info.content, "send report");
    assert_eq!(info.output_channel, Some(channel));
    assert!(info.is_once, "is_once must be true for Once schedule");

    // watch 触发一次
    assert!(
        watcher_rx.has_changed().unwrap(),
        "watcher must be notified after commit"
    );
    let borrowed = watcher_rx.borrow_and_update();
    assert_eq!(borrowed.dynamic_tasks().len(), 1);
    assert_eq!(borrowed.dynamic_tasks()[0].kind, kind);

    // 实体 despawn
    assert!(
        world.get_entity(pending_entity).is_err(),
        "ToolEffectPending entity must be despawned after commit"
    );

    // 通道回送 ToolAsyncResult::completed(call_id, Ok({"status": "scheduled", ...}))
    let mut rx = world.resource_mut::<ToolResultReceiver>();
    let result =
        rx.0.try_recv()
            .expect("channel should have received one ToolAsyncResult");
    assert_eq!(result.tool_call_id, "call-once-1");
    match result.payload {
        ToolWorkerPayload::Completed(Ok(v)) => {
            assert_eq!(v["status"], "scheduled");
            assert_eq!(v["schedule_id"], id.to_string());
            assert_eq!(v["kind"], kind);
            assert!(
                v["next_trigger"].is_string(),
                "next_trigger must be RFC3339 string for Once"
            );
        }
        other => panic!("expected Completed(Ok), got {:?}", other),
    }

    // 通道再无第二条
    assert!(
        rx.0.try_recv().is_err(),
        "channel should have exactly one result"
    );
}

/// Cron effect 提交后：`is_once == false`，`next_trigger` 仍存在（下一次本地时区触发）。
#[test]
fn commit_schedule_task_cron_marks_is_once_false() {
    let mut world = World::new();
    insert_scheduler_resources(&mut world);
    let _watcher_rx = insert_watcher(&mut world);
    let (id, effect) = cron_effect();
    let kind = match &effect {
        ToolEffect::ScheduleTask { kind, .. } => kind.clone(),
        _ => unreachable!(),
    };
    let _pending = spawn_effect_pending(&mut world, "call-cron-1", effect);

    commit_tool_effects_system(&mut world);

    let state = world.resource::<SchedulerState>();
    assert_eq!(state.dynamic_tasks().len(), 1);
    assert!(matches!(
        state.dynamic_tasks()[0].schedule,
        ScheduleSpec::Cron(_)
    ));

    let registry = world.resource::<ScheduledTaskRegistry>();
    let info = registry.get(&kind).expect("Cron task must be in registry");
    assert!(!info.is_once, "is_once must be false for Cron schedule");
    assert!(info.output_channel.is_none());

    // 通道回送结果，next_trigger 仍存在
    let mut rx = world.resource_mut::<ToolResultReceiver>();
    let result = rx.0.try_recv().expect("one result");
    assert_eq!(result.tool_call_id, "call-cron-1");
    match result.payload {
        ToolWorkerPayload::Completed(Ok(v)) => {
            assert_eq!(v["status"], "scheduled");
            assert_eq!(v["schedule_id"], id.to_string());
            assert!(
                v["next_trigger"].is_string(),
                "Cron must still report next_trigger"
            );
        }
        other => panic!("expected Completed(Ok), got {:?}", other),
    }
}

/// Watcher 缺失时不应 panic（与原 `schedule_task_commit_system_does_not_panic_without_watcher` 对齐）。
#[test]
fn commit_schedule_task_does_not_panic_without_watcher() {
    let mut world = World::new();
    insert_scheduler_resources(&mut world);
    // 故意不插入 SchedulerStateWatcher
    let (_, effect) = once_effect(None);
    let _pending = spawn_effect_pending(&mut world, "call-no-watcher", effect);

    commit_tool_effects_system(&mut world);

    let state = world.resource::<SchedulerState>();
    assert_eq!(state.dynamic_tasks().len(), 1);
    // registry 仍要更新
    let registry = world.resource::<ScheduledTaskRegistry>();
    assert!(
        registry
            .get(state.dynamic_tasks()[0].kind.as_str())
            .is_some(),
        "registry should still be updated without watcher"
    );
}

/// Registry 缺失时不应 panic，但 `SchedulerState.dynamic_tasks` 仍要更新
/// （与原 `schedule_task_commit_system_does_not_panic_without_registry` 对齐）。
#[test]
fn commit_schedule_task_does_not_panic_without_registry() {
    let mut world = World::new();
    // 只装 SchedulerState + 通道，不装 ScheduledTaskRegistry
    world.insert_resource(SchedulerState::default());
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<ToolAsyncResult>();
    world.insert_resource(ToolResultSender(tx));
    world.insert_resource(ToolResultReceiver(rx));
    // watcher 也不装，更纯粹

    let (_, effect) = once_effect(None);
    let _pending = spawn_effect_pending(&mut world, "call-no-registry", effect);

    commit_tool_effects_system(&mut world);

    let state = world.resource::<SchedulerState>();
    assert_eq!(
        state.dynamic_tasks().len(),
        1,
        "SchedulerState should be updated even when registry is missing"
    );
}

/// 多条 ScheduleTask effect 同帧提交：双账本顺序追加，通道回送 N 条结果。
#[test]
fn commit_schedule_task_handles_multiple_effects_same_frame() {
    let mut world = World::new();
    insert_scheduler_resources(&mut world);
    let _watcher_rx = insert_watcher(&mut world);

    let (_, e1) = once_effect(None);
    let (_, e2) = cron_effect();
    let (_, e3) = once_effect(Some(sample_channel()));

    let _p1 = spawn_effect_pending(&mut world, "call-multi-1", e1);
    let _p2 = spawn_effect_pending(&mut world, "call-multi-2", e2);
    let _p3 = spawn_effect_pending(&mut world, "call-multi-3", e3);

    commit_tool_effects_system(&mut world);

    let state = world.resource::<SchedulerState>();
    assert_eq!(state.dynamic_tasks().len(), 3);

    let registry = world.resource::<ScheduledTaskRegistry>();
    assert_eq!(registry.iter().count(), 3);

    // 通道收齐 3 条结果
    let mut rx = world.resource_mut::<ToolResultReceiver>();
    let mut call_ids = Vec::new();
    while let Ok(result) = rx.0.try_recv() {
        call_ids.push(result.tool_call_id);
    }
    call_ids.sort();
    assert_eq!(
        call_ids,
        vec!["call-multi-1", "call-multi-2", "call-multi-3"],
        "channel should receive exactly 3 results, one per effect"
    );
}

/// ScheduleTask effect 提交时 `DynamicScheduledTask.created_at` 用 apply 时刻的 `Utc::now()`，
/// 而非 worker 声明时刻（与原 schedule_task_commit_system 行为一致）。
#[test]
fn commit_schedule_task_records_created_at_at_apply_time() {
    let mut world = World::new();
    insert_scheduler_resources(&mut world);
    let before = Utc::now();
    let (_, effect) = once_effect(None);
    let _pending = spawn_effect_pending(&mut world, "call-time", effect);

    commit_tool_effects_system(&mut world);

    let after = Utc::now();
    let state = world.resource::<SchedulerState>();
    let created_at = state.dynamic_tasks()[0].created_at;
    assert!(
        created_at >= before && created_at <= after,
        "created_at ({created_at}) should fall between before ({before}) and after ({after})"
    );
}

/// 兼容性核对：`DynamicScheduledTask` / `ScheduledTaskInfo` 字段从 effect 原样迁移。
#[test]
fn commit_schedule_task_preserves_all_effect_fields() {
    let mut world = World::new();
    insert_scheduler_resources(&mut world);
    let _watcher_rx = insert_watcher(&mut world);

    let channel = sample_channel();
    let (id, effect) = once_effect(Some(channel.clone()));
    let kind = match &effect {
        ToolEffect::ScheduleTask { kind, .. } => kind.clone(),
        _ => unreachable!(),
    };
    let _pending = spawn_effect_pending(&mut world, "call-preserve", effect);

    commit_tool_effects_system(&mut world);

    let state = world.resource::<SchedulerState>();
    let dt: &DynamicScheduledTask = &state.dynamic_tasks()[0];
    assert_eq!(dt.id, id);
    assert_eq!(dt.kind, kind);

    let registry = world.resource::<ScheduledTaskRegistry>();
    let info: &ScheduledTaskInfo = registry.get(&kind).unwrap();
    assert_eq!(info.content, "send report");
    assert_eq!(info.output_channel, Some(channel));
    assert!(info.is_once);
}
