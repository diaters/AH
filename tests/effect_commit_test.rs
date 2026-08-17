//! Task 9: commit_tool_effects_system 行为测试。
//!
//! 验证声明式写效果的「慢动作」apply 路径：
//! - 消费 `ToolEffectPending` 实体
//! - 经 `update_scheduler_state` 双资源入口原子落账
//! - 把最终结果（含 apply 时刻才知道的 `existed` 真相）回送通道
//! - 应用完 despawn 效果实体
//!
//! 结果落地仍单点：commit 只送通道，由 ingest 下一帧落地。
//!
//! 大批量回归测试（101 个效果排队）验证：
//! - 50 个 hit + 51 个 miss 全部正确 apply（K3）
//! - 双账本清空且无残留（C2）
//! - 无 `LedgerDriftOnDelete` 误报（N2）
//! - 101 个结果 `existed` 字段逐一正确，`tool_call_id` 不混淆（A3）

mod common;
use bevy_ecs::system::RunSystemOnce;
use common::async_tool_bridge::*;
use harness::domain::{ToolEffect, ToolEffectPending, ToolWorkerPayload};
use harness::triggers::ScheduleSpec;
use harness::triggers::scheduled_task::{
    DynamicScheduledTask, ScheduledTaskInfo, ScheduledTaskRegistry, SchedulerState,
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

// ============ 大批量回归测试 ============

use std::fmt;
use std::sync::Arc;
use std::sync::Mutex;

use tracing::field::Field;
use tracing::field::Visit;
use tracing_subscriber::Layer;
use tracing_subscriber::Registry;
use tracing_subscriber::layer::Context;
use tracing_subscriber::layer::SubscriberExt;

/// 捕获 tracing 事件中 `event` 字段的值（与 triggers_timer_scheduler.rs 同模式）。
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
        // 仅关心 `event` 字段
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

/// 101 个效果排队：50 hit（victim-0..49）+ 51 miss（ghost-0..50）。
///
/// K3 测试结构：101 = 50 + 51，确保奇偶边界都覆盖；
/// miss 比 hit 多一个，验证「最后一条 miss 也被处理」不会因批次
/// 末尾提前退出。所有 victim 在 apply 前一次性预置进双账本。
const VICTIM_COUNT: usize = 50;
const GHOST_COUNT: usize = 51;
const TOTAL_EFFECTS: usize = VICTIM_COUNT + GHOST_COUNT; // 101

#[test]
fn commit_large_batch_applies_all_101_effects_correctly() {
    // 1. 构造 world：预置 50 个 victim-N 动态任务到双账本
    let mut world = setup_bridge_world();
    world.insert_resource(SchedulerState::default());
    world.insert_resource(ScheduledTaskRegistry::default());

    update_scheduler_state(&mut world, |state, registry| {
        for i in 0..VICTIM_COUNT {
            let kind = format!("victim-{i}");
            state.dynamic_tasks_mut().push(DynamicScheduledTask {
                id: uuid::Uuid::new_v4(),
                kind: kind.clone(),
                schedule: ScheduleSpec::Once(chrono::Utc::now() + chrono::Duration::hours(1)),
                created_at: chrono::Utc::now(),
            });
            registry.insert(
                kind,
                ScheduledTaskInfo {
                    content: format!("content-{i}"),
                    output_channel: None,
                    is_once: true,
                },
            );
        }
    });

    // 2. 捕获 tracing 事件，验证无 LedgerDriftOnDelete 误报（N2）
    let capturing = CapturingLayer::default();
    let subscriber = Registry::default().with(capturing.clone());
    let _guard = tracing::subscriber::set_default(subscriber);

    // 3. spawn 101 个 ToolEffectPending：50 hit + 51 miss，交错放置
    //    （交错可同时验证 commit 循环对 hit/miss 混合批次的处理）
    let mut spawned_entities: Vec<bevy_ecs::prelude::Entity> = Vec::with_capacity(TOTAL_EFFECTS);
    for i in 0..VICTIM_COUNT {
        let e = world
            .spawn(ToolEffectPending {
                tool_call_id: format!("hit-{i}"),
                effect: ToolEffect::DeleteScheduledTask {
                    kind: format!("victim-{i}"),
                },
            })
            .id();
        spawned_entities.push(e);
    }
    for i in 0..GHOST_COUNT {
        let e = world
            .spawn(ToolEffectPending {
                tool_call_id: format!("miss-{i}"),
                effect: ToolEffect::DeleteScheduledTask {
                    kind: format!("ghost-{i}"),
                },
            })
            .id();
        spawned_entities.push(e);
    }

    // 4. 跑 commit system 一次（apply 全部 101 个）
    world
        .run_system_once(harness::systems::commit_tool_effects_system)
        .unwrap();

    // C2: 全部效果实体已 despawn + 双账本清空且一致
    for e in &spawned_entities {
        assert!(
            world.get::<ToolEffectPending>(*e).is_none(),
            "效果实体 {:?} 未 despawn",
            e
        );
    }
    assert!(
        world
            .resource::<SchedulerState>()
            .dynamic_tasks()
            .is_empty(),
        "state.dynamic_tasks 应清空，剩余: {:?}",
        world.resource::<SchedulerState>().dynamic_tasks()
    );
    assert!(
        world
            .resource::<ScheduledTaskRegistry>()
            .iter()
            .next()
            .is_none(),
        "registry 应清空，剩余 keys: {:?}",
        world
            .resource::<ScheduledTaskRegistry>()
            .iter()
            .map(|(k, _)| k.clone())
            .collect::<Vec<_>>()
    );

    // N2: 无 LedgerDriftOnDelete 误报
    // hit 与 miss 都不会触发 drift：hit 两账本同时删；miss 两账本都没删
    let drift_events: Vec<String> = capturing
        .events
        .lock()
        .unwrap()
        .iter()
        .filter(|n| *n == "LedgerDriftOnDelete")
        .cloned()
        .collect();
    assert!(
        drift_events.is_empty(),
        "不应有 LedgerDriftOnDelete 误报，捕获到: {:?}",
        drift_events
    );

    // A3: 101 个结果 existed 字段逐一正确 + tool_call_id 不混淆
    let mut results_by_call_id: std::collections::HashMap<String, serde_json::Value> =
        std::collections::HashMap::with_capacity(TOTAL_EFFECTS);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        match wait_for_tool_result(&mut world, 0) {
            Some(r) => {
                let call_id = r.tool_call_id.clone();
                match r.payload {
                    ToolWorkerPayload::Completed(Ok(v)) => {
                        let prev = results_by_call_id.insert(call_id.clone(), v);
                        assert!(prev.is_none(), "tool_call_id {} 重复回送", call_id);
                    }
                    other => panic!("tool_call_id {} 非 Ok 值: {:?}", call_id, other),
                }
            }
            None => {
                if results_by_call_id.len() == TOTAL_EFFECTS {
                    break;
                }
                if std::time::Instant::now() >= deadline {
                    panic!(
                        "2s 内仅收到 {}/{} 条结果，已收: {:?}",
                        results_by_call_id.len(),
                        TOTAL_EFFECTS,
                        results_by_call_id.keys().collect::<Vec<_>>()
                    );
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
    }

    // 校验 hit 结果（existed=true + deleted 字段对齐 victim-i）
    for i in 0..VICTIM_COUNT {
        let call_id = format!("hit-{i}");
        let v = results_by_call_id
            .remove(&call_id)
            .unwrap_or_else(|| panic!("缺少 hit 结果 {call_id}"));
        assert_eq!(v["deleted"], format!("victim-{i}"), "deleted 字段错配");
        assert_eq!(v["existed"], true, "hit {} existed 应为 true", i);
    }
    // 校验 miss 结果（existed=false + deleted 字段对齐 ghost-i）
    for i in 0..GHOST_COUNT {
        let call_id = format!("miss-{i}");
        let v = results_by_call_id
            .remove(&call_id)
            .unwrap_or_else(|| panic!("缺少 miss 结果 {call_id}"));
        assert_eq!(v["deleted"], format!("ghost-{i}"), "deleted 字段错配");
        assert_eq!(v["existed"], false, "miss {} existed 应为 false", i);
    }
    assert!(
        results_by_call_id.is_empty(),
        "意外多出的结果: {:?}",
        results_by_call_id.keys().collect::<Vec<_>>()
    );
}
