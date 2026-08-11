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
use chrono::Utc;
use tracing::warn;

use crate::domain::{ToolAsyncResult, ToolEffect, ToolEffectPending, ToolError, ToolResultSender};
use crate::triggers::scheduled_task::{compute_next_trigger, update_scheduler_state};
use crate::triggers::{DynamicScheduledTask, ScheduleSpec, ScheduledTaskInfo};

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
/// `existed` / `next_trigger` 等「apply 时刻才知道的真相」在这里计算并回送。
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
        ToolEffect::ScheduleTask {
            id,
            kind,
            content,
            schedule,
            output_channel,
        } => {
            let is_once = matches!(schedule, ScheduleSpec::Once(_));
            let kind_owned = kind.clone();
            let content_owned = content.clone();
            let output_channel_owned = output_channel.clone();
            let id_owned = *id;
            let schedule_clone = schedule.clone();

            // 双账本单一修改入口：state + registry 同一闭包内落账，watch 一次广播。
            // created_at 用 apply 时刻的 Utc::now()——「apply 时刻才知道的真相」原则。
            update_scheduler_state(world, |state, registry| {
                state.dynamic_tasks_mut().push(DynamicScheduledTask {
                    id: id_owned,
                    kind: kind_owned.clone(),
                    schedule: schedule_clone.clone(),
                    created_at: Utc::now(),
                });
                registry.insert(
                    kind_owned,
                    ScheduledTaskInfo {
                        content: content_owned,
                        output_channel: output_channel_owned,
                        is_once,
                    },
                );
            });

            // next_trigger 在 apply 时刻计算（Once 直接返回；Cron 算下一次本地时区触发）
            let next_trigger = compute_next_trigger(schedule).map(|t| t.to_rfc3339());

            Ok(serde_json::json!({
                "status": "scheduled",
                "schedule_id": id.to_string(),
                "kind": kind,
                "next_trigger": next_trigger,
            }))
        }
        ToolEffect::WriteSkillFile {
            sandbox_dir,
            path,
            content,
        } => {
            let full_path = sandbox_dir.join(path);
            if let Some(parent) = full_path.parent()
                && let Err(e) = std::fs::create_dir_all(parent)
            {
                return Err(ToolError::ExecutionFailed(format!(
                    "failed to create directory {}: {}",
                    parent.display(),
                    e
                )));
            }
            let bytes = content.len();
            match std::fs::write(&full_path, content) {
                Ok(()) => Ok(serde_json::json!({
                    "path": path,
                    "bytes_written": bytes,
                })),
                Err(e) => Err(ToolError::ExecutionFailed(format!(
                    "failed to write file {}: {}",
                    full_path.display(),
                    e
                ))),
            }
        }
    }
}
