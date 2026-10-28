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
use tracing::{debug, warn};

use crate::contracts::FrontendRegistry;
use crate::domain::compute_next_trigger;
use crate::domain::{
    EngineEvent, EventTarget, Task, TaskId, TaskTodoList, TodoItem, TodoStatus, ToolAsyncResult,
    ToolEffect, ToolEffectPending, ToolError, ToolResultSender, render_todos,
};
use crate::ecs::EntityIndex;
use crate::triggers::scheduled_task::update_scheduler_state;
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
        ToolEffect::UpdateTodos { task_id, items } => apply_update_todos(world, *task_id, items),
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

/// `update_todos` 落账：替换 `TaskTodoList` 并通知前端（纯展示数据，见 ADR-007）。
fn apply_update_todos(
    world: &mut World,
    task_id: TaskId,
    items: &[TodoItem],
) -> Result<serde_json::Value, ToolError> {
    let entity = world
        .get_resource::<EntityIndex>()
        .and_then(|index| index.get_task(&task_id))
        .ok_or_else(|| ToolError::InternalState(format!("task {task_id} not found")))?;

    let previous = world
        .get::<TaskTodoList>(entity)
        .map(|list| list.items.clone())
        .unwrap_or_default();
    let progress_changed = todo_progress_changed(&previous, items);
    world.entity_mut(entity).insert(TaskTodoList {
        items: items.to_vec(),
    });

    let target = world
        .get::<Task>(entity)
        .and_then(|task| task.routing_policy.output_channel().cloned())
        .map(|channel| EventTarget::Directed(vec![channel]));
    if let (Some(target), Some(registry)) = (target, world.get_resource::<FrontendRegistry>()) {
        let event = EngineEvent::TodoUpdated {
            target,
            task_id,
            items: items.to_vec(),
            progress_changed,
        };
        for frontend in &registry.frontends {
            frontend.push_event(event.clone());
        }
    }
    debug!(
        event = "TodoListUpdated",
        task_id = %task_id,
        todo_count = items.len(),
        progress_changed = progress_changed,
        "task todo list updated"
    );

    Ok(serde_json::json!({
        "status": "updated",
        "todos": render_todos(items),
    }))
}

/// 进行中条目变化、全部完成或清单新建/清空时视为进度变化（IM 端据此推送，避免刷屏）。
fn todo_progress_changed(previous: &[TodoItem], next: &[TodoItem]) -> bool {
    let in_progress = |items: &[TodoItem]| {
        items
            .iter()
            .find(|t| t.status == TodoStatus::InProgress)
            .map(|t| t.content.clone())
    };
    let all_done = |items: &[TodoItem]| {
        !items.is_empty() && items.iter().all(|t| t.status == TodoStatus::Completed)
    };
    previous.is_empty() != next.is_empty()
        || in_progress(previous) != in_progress(next)
        || (all_done(next) && !all_done(previous))
}

#[cfg(test)]
mod todo_tests {
    use super::*;

    fn todo(content: &str, status: TodoStatus) -> TodoItem {
        TodoItem {
            content: content.to_string(),
            status,
        }
    }

    #[test]
    fn progress_change_rules() {
        let a_doing = vec![
            todo("a", TodoStatus::InProgress),
            todo("b", TodoStatus::Pending),
        ];
        let a_doing_renamed_b = vec![
            todo("a", TodoStatus::InProgress),
            todo("c", TodoStatus::Pending),
        ];
        let b_doing = vec![
            todo("a", TodoStatus::Completed),
            todo("b", TodoStatus::InProgress),
        ];
        let all_done = vec![
            todo("a", TodoStatus::Completed),
            todo("b", TodoStatus::Completed),
        ];

        assert!(todo_progress_changed(&[], &a_doing), "new list");
        assert!(
            !todo_progress_changed(&a_doing, &a_doing_renamed_b),
            "same in-progress item"
        );
        assert!(
            todo_progress_changed(&a_doing, &b_doing),
            "in-progress moved"
        );
        assert!(todo_progress_changed(&b_doing, &all_done), "all completed");
        assert!(todo_progress_changed(&all_done, &[]), "cleared");
    }

    #[test]
    fn update_todos_effect_replaces_list_on_task() {
        let mut world = World::new();
        world.insert_resource(EntityIndex::default());
        let task = Task::from_user_input(
            "goal",
            3,
            crate::domain::ChannelId {
                frontend: crate::domain::FrontendKind::Tui,
                user_id: "local".to_string(),
                thread_id: None,
            },
        );
        let task_id = task.id;
        let entity = world.spawn(task).id();
        world
            .resource_mut::<EntityIndex>()
            .tasks
            .insert(task_id, entity);

        let output =
            apply_update_todos(&mut world, task_id, &[todo("x", TodoStatus::InProgress)]).unwrap();
        assert_eq!(output["status"], "updated");
        assert_eq!(world.get::<TaskTodoList>(entity).unwrap().items.len(), 1);

        apply_update_todos(&mut world, task_id, &[]).unwrap();
        assert!(world.get::<TaskTodoList>(entity).unwrap().items.is_empty());

        assert!(apply_update_todos(&mut world, crate::domain::TaskId::new(), &[]).is_err());
    }
}
