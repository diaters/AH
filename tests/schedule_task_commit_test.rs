//! Task 14 Step B: commit_tool_effects_system 处理 ToolEffect::ScheduleTask 的单元测试。
//!
//! 验证 schedule_task 上桥后，commit 阶段的「写效果唯一应用点」行为：
//! - 落账为一条 `ScheduleDefinition`（`SchedulerState.dynamic_tasks` 是唯一账本）
//! - watch 触发：`SchedulerStateWatcher` 收到一次通知
//! - `next_trigger` 在 apply 时刻计算（Once 直接返回；Cron 算下一次本地时区触发）
//! - 名称冲突在 apply 时刻判定并拒绝
//! - 回送 `ToolAsyncResult::completed(call_id, Ok({"status": "scheduled", ...}))`
//! - `ToolEffectPending` 实体 despawn
//! - watcher 缺失不 panic

use bevy_ecs::prelude::*;
use chrono::Utc;
use uuid::Uuid;

use harness::domain::{
    ChannelId, FrontendKind, ScheduleExpr, ToolAsyncResult, ToolEffect, ToolEffectPending,
    ToolError, ToolResultReceiver, ToolResultSender, ToolWorkerPayload,
};
use harness::systems::commit_tool_effects_system;
use harness::triggers::scheduled_task::{SchedulerState, SchedulerStateWatcher};

fn schedule_effect(name: Option<&str>, schedule: &str, channel: Option<ChannelId>) -> ToolEffect {
    ToolEffect::ScheduleTask {
        id: Uuid::new_v4(),
        name: name.map(str::to_string),
        content: "send report".to_string(),
        schedule: ScheduleExpr::parse(schedule).unwrap(),
        output_channel: channel,
    }
}

/// 构造一条 Once ScheduleTask effect。
fn once_effect(channel: Option<ChannelId>) -> (Uuid, ToolEffect) {
    let at = (Utc::now() + chrono::Duration::days(1)).to_rfc3339();
    let effect = schedule_effect(None, &format!("once:{at}"), channel);
    (effect_id(&effect), effect)
}

/// 构造一条 Cron ScheduleTask effect。
fn cron_effect() -> (Uuid, ToolEffect) {
    let effect = schedule_effect(None, "cron:0 9 * * *", None);
    (effect_id(&effect), effect)
}

fn effect_id(effect: &ToolEffect) -> Uuid {
    match effect {
        ToolEffect::ScheduleTask { id, .. } => *id,
        _ => unreachable!(),
    }
}

fn sample_channel() -> ChannelId {
    ChannelId {
        frontend: FrontendKind::Tui,
        user_id: "tester".to_string(),
        thread_id: None,
    }
}

/// 装入 scheduler 资源 + 结果通道。
fn insert_scheduler_resources(world: &mut World) {
    world.insert_resource(SchedulerState::default());
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<ToolAsyncResult>();
    world.insert_resource(ToolResultSender(tx));
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

fn recv_result(world: &mut World) -> ToolAsyncResult {
    world
        .resource_mut::<ToolResultReceiver>()
        .0
        .try_recv()
        .expect("channel should have received one ToolAsyncResult")
}

/// Once effect 提交后：
/// - 一条定义，字段从 effect 原样迁移
/// - watch 触发一次
/// - `ToolAsyncResult::completed` 回送 `{status, schedule_id, name, kind, next_trigger}`
/// - `ToolEffectPending` 实体 despawn
#[test]
fn commit_schedule_task_once_creates_definition_and_sends_result() {
    let mut world = World::new();
    insert_scheduler_resources(&mut world);
    let mut watcher_rx = insert_watcher(&mut world);
    let channel = sample_channel();
    let (id, effect) = once_effect(Some(channel.clone()));
    let pending_entity = spawn_effect_pending(&mut world, "call-once-1", effect);

    commit_tool_effects_system(&mut world);

    let state = world.resource::<SchedulerState>();
    assert_eq!(state.dynamic_tasks().len(), 1);
    let def = &state.dynamic_tasks()[0];
    assert_eq!(def.id, id);
    assert_eq!(def.kind(), format!("scheduled:{id}"));
    assert_eq!(def.content, "send report");
    assert_eq!(def.output_channel, Some(channel));
    assert!(def.schedule.is_once());
    assert!(def.enabled);
    let kind = def.kind();
    let name = def.name.clone();

    assert!(
        watcher_rx.has_changed().unwrap(),
        "watcher must be notified after commit"
    );
    assert_eq!(watcher_rx.borrow_and_update().dynamic_tasks().len(), 1);

    assert!(
        world.get_entity(pending_entity).is_err(),
        "ToolEffectPending entity must be despawned after commit"
    );

    let result = recv_result(&mut world);
    assert_eq!(result.tool_call_id, "call-once-1");
    match result.payload {
        ToolWorkerPayload::Completed(Ok(v)) => {
            assert_eq!(v["status"], "scheduled");
            assert_eq!(v["schedule_id"], id.to_string());
            assert_eq!(v["kind"], kind);
            assert_eq!(v["name"], name);
            assert!(
                v["next_trigger"].is_string(),
                "next_trigger must be RFC3339 string for Once"
            );
        }
        other => panic!("expected Completed(Ok), got {:?}", other),
    }
    assert!(
        world
            .resource_mut::<ToolResultReceiver>()
            .0
            .try_recv()
            .is_err(),
        "channel should have exactly one result"
    );
}

/// Cron effect 提交后：定义为周期性，`next_trigger` 仍存在（下一次本地时区触发）。
#[test]
fn commit_schedule_task_cron_reports_next_trigger() {
    let mut world = World::new();
    insert_scheduler_resources(&mut world);
    let _watcher_rx = insert_watcher(&mut world);
    let (id, effect) = cron_effect();
    let _pending = spawn_effect_pending(&mut world, "call-cron-1", effect);

    commit_tool_effects_system(&mut world);

    let state = world.resource::<SchedulerState>();
    assert_eq!(state.dynamic_tasks().len(), 1);
    assert!(!state.dynamic_tasks()[0].schedule.is_once());
    assert!(state.dynamic_tasks()[0].output_channel.is_none());

    let result = recv_result(&mut world);
    match result.payload {
        ToolWorkerPayload::Completed(Ok(v)) => {
            assert_eq!(v["schedule_id"], id.to_string());
            assert!(
                v["next_trigger"].is_string(),
                "Cron must still report next_trigger"
            );
        }
        other => panic!("expected Completed(Ok), got {:?}", other),
    }
}

/// 名称缺省时取 id 前 8 位；显式名称原样保存。
#[test]
fn commit_schedule_task_assigns_default_or_explicit_name() {
    let mut world = World::new();
    insert_scheduler_resources(&mut world);
    let (id, effect) = cron_effect();
    spawn_effect_pending(&mut world, "call-default-name", effect);
    spawn_effect_pending(
        &mut world,
        "call-explicit-name",
        schedule_effect(Some("forum-watch"), "cron:*/10 * * * *", None),
    );

    commit_tool_effects_system(&mut world);

    let names: Vec<String> = world
        .resource::<SchedulerState>()
        .dynamic_tasks()
        .iter()
        .map(|d| d.name.clone())
        .collect();
    assert!(names.contains(&id.simple().to_string()[..8].to_string()));
    assert!(names.contains(&"forum-watch".to_string()));
}

/// 同名定义在 apply 时刻拒绝：账本不变，回送 InvalidInput。
#[test]
fn commit_schedule_task_rejects_duplicate_name() {
    let mut world = World::new();
    insert_scheduler_resources(&mut world);
    spawn_effect_pending(
        &mut world,
        "call-first",
        schedule_effect(Some("forum-watch"), "cron:*/10 * * * *", None),
    );
    commit_tool_effects_system(&mut world);
    let _ = recv_result(&mut world);

    spawn_effect_pending(
        &mut world,
        "call-dup",
        schedule_effect(Some("forum-watch"), "cron:0 8 * * *", None),
    );
    commit_tool_effects_system(&mut world);

    assert_eq!(world.resource::<SchedulerState>().dynamic_tasks().len(), 1);
    let result = recv_result(&mut world);
    assert_eq!(result.tool_call_id, "call-dup");
    match result.payload {
        ToolWorkerPayload::Completed(Err(ToolError::InvalidInput(msg))) => {
            assert!(msg.contains("forum-watch"), "unexpected message: {msg}");
        }
        other => panic!("expected InvalidInput, got {:?}", other),
    }
}

/// Watcher 缺失时不应 panic，定义仍要落账。
#[test]
fn commit_schedule_task_does_not_panic_without_watcher() {
    let mut world = World::new();
    insert_scheduler_resources(&mut world);
    let (_, effect) = once_effect(None);
    let _pending = spawn_effect_pending(&mut world, "call-no-watcher", effect);

    commit_tool_effects_system(&mut world);

    assert_eq!(world.resource::<SchedulerState>().dynamic_tasks().len(), 1);
}

/// 多条 ScheduleTask effect 同帧提交：顺序追加，通道回送 N 条结果。
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

    assert_eq!(world.resource::<SchedulerState>().dynamic_tasks().len(), 3);

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

/// `created_at` 用 apply 时刻的 `Utc::now()`，而非 worker 声明时刻。
#[test]
fn commit_schedule_task_records_created_at_at_apply_time() {
    let mut world = World::new();
    insert_scheduler_resources(&mut world);
    let before = Utc::now();
    let (_, effect) = once_effect(None);
    let _pending = spawn_effect_pending(&mut world, "call-time", effect);

    commit_tool_effects_system(&mut world);

    let after = Utc::now();
    let created_at = world.resource::<SchedulerState>().dynamic_tasks()[0].created_at;
    assert!(
        created_at >= before && created_at <= after,
        "created_at ({created_at}) should fall between before ({before}) and after ({after})"
    );
}
