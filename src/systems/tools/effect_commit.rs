//! 通用效果提交：写效果的唯一应用点。
//!
//! 每个效果 arm 必须经 `update_scheduler_state` 双资源入口落账；
//! 应用后把最终结果（含 apply 时刻才知道的真相，如 `existed`）送回通道，
//! 由 ingest 下一帧落地——结果落地仍单点。
//!
//! 形态：exclusive system（`fn(&mut World)`），因为 `update_scheduler_state`
//! 需要 `&mut World`。仓内处理「需要整 World 写权限」的既定手法，测试侧
//! `world.run_system_once(...)` 同样适用。

use bevy_ecs::prelude::*;
use tracing::warn;

use crate::domain::{ToolAsyncResult, ToolEffect, ToolEffectPending, ToolError, ToolResultSender};
use crate::triggers::scheduled_task::update_scheduler_state;

/// exclusive system：直接拿 `&mut World` 调 `update_scheduler_state`。
///
/// 注册：与普通 system 混排，固定进 Maintenance set（commit → 下一帧 ingest 落地）。
pub fn commit_tool_effects_system(world: &mut World) {
    let mut to_apply = Vec::new();
    let mut q = world.query::<(Entity, &ToolEffectPending)>();
    for (e, p) in q.iter(world) {
        to_apply.push((e, p.tool_call_id.clone(), p.effect.clone()));
    }

    let sender = world.resource::<ToolResultSender>().0.clone();

    for (entity, call_id, effect) in to_apply {
        let output = apply_effect(world, &effect);
        let _ = sender.send(ToolAsyncResult::completed(call_id, output));
        world.entity_mut(entity).despawn();
    }
}

/// 应用单个声明式效果，返回最终喂给 LLM 的结果值。
///
/// 写效果经 `update_scheduler_state` 双资源入口原子落账；
/// `existed` 等「apply 时刻才知道的真相」在这里计算并回送。
fn apply_effect(world: &mut World, effect: &ToolEffect) -> Result<serde_json::Value, ToolError> {
    match effect {
        ToolEffect::DeleteScheduledTask { kind } => {
            let mut existed = false;
            update_scheduler_state(world, |state, registry| {
                let before = state.dynamic_tasks().len();
                state.dynamic_tasks_mut().retain(|dt| dt.kind != *kind);
                let removed_from_state = state.dynamic_tasks().len() < before;
                let removed_from_registry = registry.remove(kind).is_some();
                existed = removed_from_state || removed_from_registry;
                if removed_from_state != removed_from_registry {
                    warn!(
                        event = "LedgerDriftOnDelete",
                        kind = %kind,
                        in_state = removed_from_state,
                        in_registry = removed_from_registry,
                        "delete removed from only one ledger"
                    );
                }
            });
            Ok(serde_json::json!({ "deleted": kind, "existed": existed }))
        }
        ToolEffect::ScheduleTask { .. } => {
            // Task 14 Step B 实现双账本提交 + next_trigger 回送
            todo!("ScheduleTask commit arm - Task 14 Step B")
        }
    }
}
