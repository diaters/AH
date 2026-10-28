//! `/stop` 中断：停下当前回合、保留上下文。
//!
//! - 递增 `TurnGeneration`：在飞 LLM 请求的结果在 ingest 处作废；
//! - 插入 `StopRequested`：`cancel_monitor_system` 据此取消在飞异步工具调用；
//! - 清理待确认 / ask_user / wait_tasks / 子任务批次等挂起状态，撤回前端审批卡片；
//! - 级联取消未完成的子任务；任务回到 `Waiting(User)`，收件箱积压保留。

use tracing::debug;

use crate::contracts::{Clock, FrontendRegistry};
use crate::domain::{
    AskUserPending, EngineEvent, EntryMetadata, EntryRole, EventTarget, FailureReason,
    InflightLlmRequests, ShortTermMemory, StopRequested, StopTaskMessage, SubTaskBatchState,
    SystemOutputMessage, Task, TaskId, TaskStatus, ToolExecutionRequestMessage, TurnGeneration,
    WaitingForSessionInfo, WaitingForTasksInfo, WaitingReason,
};
use crate::ecs::EntityIndex;
use crate::prelude::*;

/// 写入 STM 的中断标记，让下一轮 LLM 知道上下文断点。
pub(crate) const STOP_MARKER: &str = "[用户中断了上一轮执行，未完成的工具调用已取消]";

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub(crate) fn stop_task_system(
    mut commands: Commands,
    clock: Res<Clock>,
    index: Res<EntityIndex>,
    registry: Option<Res<FrontendRegistry>>,
    mut inflight: Option<ResMut<InflightLlmRequests>>,
    messages: Query<(Entity, &StopTaskMessage)>,
    mut tasks: Query<(
        Entity,
        &mut Task,
        Option<&mut ShortTermMemory>,
        Option<&mut TurnGeneration>,
    )>,
    tool_requests: Query<(Entity, &ToolExecutionRequestMessage)>,
    batch_states: Query<(Entity, &SubTaskBatchState)>,
) {
    for (message_entity, message) in &messages {
        commands.entity(message_entity).despawn();

        let Some(task_entity) = index.get_task(&message.task_id) else {
            continue;
        };
        let Ok((_, task, _, _)) = tasks.get(task_entity) else {
            continue;
        };
        if task.status.is_terminal() {
            continue;
        }
        // 空闲任务（等待用户输入且无待确认请求）没有可中断的回合
        if task.status == TaskStatus::Waiting(WaitingReason::User)
            && task.pending_confirmation_id.is_none()
        {
            commands.spawn(SystemOutputMessage {
                task_id: message.task_id,
                content: "当前没有正在执行的回合。".to_string(),
            });
            continue;
        }
        let approval_target = task
            .routing_policy
            .approval_channel()
            .cloned()
            .map(|channel| EventTarget::Directed(vec![channel]));

        // 级联目标：未终态的直接子任务及其批次
        let children: Vec<(TaskId, Option<uuid::Uuid>)> = tasks
            .iter()
            .filter(|(_, t, _, _)| {
                t.parent_task_id == Some(message.task_id) && !t.status.is_terminal()
            })
            .map(|(_, t, _, _)| (t.id, t.batch_id))
            .collect();

        let Ok((_, mut task, short_term, generation)) = tasks.get_mut(task_entity) else {
            continue;
        };

        // 中止在飞的 LLM 请求（provider 侧 HTTP 随 future drop 取消）；
        // 代次递增仍保留，作为中止前已回送结果的兜底作废手段。
        let aborted_requests = inflight
            .as_mut()
            .map(|inflight| inflight.abort_task(message.task_id))
            .unwrap_or(0);

        let next_generation = match generation {
            Some(mut generation) => {
                generation.0 += 1;
                generation.0
            }
            None => {
                commands.entity(task_entity).insert(TurnGeneration(1));
                1
            }
        };

        // 撤回本任务排队中的工具请求（含待确认请求）
        for (request_entity, request) in &tool_requests {
            if request.request.task_id != message.task_id || request.request.work_item_id.is_some()
            {
                continue;
            }
            if let (Some(request_id), Some(target)) =
                (request.pending_confirmation_id, approval_target.clone())
                && let Some(registry) = registry.as_ref()
            {
                let event = EngineEvent::ApprovalResult {
                    target,
                    request_id,
                    decision: "cancelled".to_string(),
                };
                for frontend in &registry.frontends {
                    frontend.push_event(event.clone());
                }
            }
            commands.entity(request_entity).despawn();
        }

        task.pending_confirmation_id = None;
        commands
            .entity(task_entity)
            .insert(StopRequested)
            .remove::<(AskUserPending, WaitingForTasksInfo, WaitingForSessionInfo)>();

        if let Some(mut stm) = short_term {
            stm.add_entry(EntryRole::User, STOP_MARKER, EntryMetadata::default());
        }
        task.mark_waiting(WaitingReason::User, clock.0);

        // 子任务批次：移除批次状态，避免被取消的子任务完成时把父任务重新唤醒
        let child_batches: Vec<uuid::Uuid> = children.iter().filter_map(|(_, b)| *b).collect();
        for (batch_entity, batch) in &batch_states {
            if child_batches.contains(&batch.batch_id) {
                commands.entity(batch_entity).despawn();
            }
        }
        for (child_id, _) in &children {
            if let Some(mut child) = index
                .get_task(child_id)
                .and_then(|e| tasks.get_mut(e).ok())
                .map(|(_, t, _, _)| t)
            {
                child.mark_failed_reason(
                    FailureReason::UserCancelled,
                    "parent task stopped by user",
                    clock.0,
                );
            }
        }

        commands.spawn(SystemOutputMessage {
            task_id: message.task_id,
            content: "已中断当前执行，上下文已保留。".to_string(),
        });
        debug!(
            event = "TaskStopped",
            task_id = %message.task_id,
            turn_generation = next_generation,
            aborted_requests = aborted_requests,
            cancelled_children = children.len(),
            "task stopped by user; turn generation advanced"
        );
    }
}

/// 移除 `StopRequested` 标记（排在 `cancel_monitor_system` 之后，取消只需触发一次）。
pub(crate) fn clear_stop_requested_system(
    mut commands: Commands,
    stopped: Query<Entity, With<StopRequested>>,
) {
    for entity in &stopped {
        commands.entity(entity).remove::<StopRequested>();
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::domain::{
        AgentExecutionRequest, AgentId, AgentRequestKind, ChannelId, FrontendKind, TaskStatus,
    };

    fn channel() -> ChannelId {
        ChannelId {
            frontend: FrontendKind::Tui,
            user_id: "local".to_string(),
            thread_id: None,
        }
    }

    fn setup(status: TaskStatus) -> (App, Entity, TaskId) {
        let mut app = App::new();
        app.insert_resource(Clock::default());
        app.insert_resource(EntityIndex::default());
        app.add_systems(Update, stop_task_system);
        let mut task = Task::from_user_input("goal", 3, channel());
        match status {
            TaskStatus::Waiting(reason) => task.mark_waiting(reason, chrono::Utc::now()),
            TaskStatus::Running => task.mark_running(chrono::Utc::now()),
            other => panic!("unsupported {other:?}"),
        }
        let task_id = task.id;
        let entity = app
            .world_mut()
            .spawn((task, ShortTermMemory::default()))
            .id();
        app.world_mut()
            .resource_mut::<EntityIndex>()
            .tasks
            .insert(task_id, entity);
        (app, entity, task_id)
    }

    fn tool_request(task_id: TaskId, pending: Option<uuid::Uuid>) -> ToolExecutionRequestMessage {
        ToolExecutionRequestMessage {
            request: AgentExecutionRequest {
                task_id,
                agent_id: AgentId::nil(),
                request_kind: AgentRequestKind::LlmCompletion,
                prompt: String::new(),
                system_prompt: None,
                tools: vec![],
                conversation: None,
                work_item_id: None,
                model_override: None,
                turn_generation: None,
                vision: false,
                trusted_images: Vec::new(),
            },
            tool_name: "shell_exec".to_string(),
            tool_input: serde_json::json!({}),
            pending_confirmation_id: pending,
            tool_call_id: Some("call".to_string()),
            pending_confirmation_options: None,
            work_item_entity: None,
            confirmed_once: false,
        }
    }

    #[test]
    fn stop_advances_generation_and_clears_pending_state() {
        let (mut app, entity, task_id) = setup(TaskStatus::Running);
        let request_id = uuid::Uuid::new_v4();
        app.world_mut().entity_mut(entity).insert(AskUserPending {
            tool_call_id: "c".to_string(),
            agent_id: AgentId::nil(),
        });
        app.world_mut()
            .spawn(tool_request(task_id, Some(request_id)));
        app.world_mut().spawn(StopTaskMessage { task_id });
        app.update();

        let world = app.world();
        let task = world.get::<Task>(entity).unwrap();
        assert_eq!(task.status, TaskStatus::Waiting(WaitingReason::User));
        assert!(task.pending_confirmation_id.is_none());
        assert_eq!(
            world.get::<TurnGeneration>(entity),
            Some(&TurnGeneration(1))
        );
        assert!(world.get::<StopRequested>(entity).is_some());
        assert!(world.get::<AskUserPending>(entity).is_none());
        let stm = world.get::<ShortTermMemory>(entity).unwrap();
        assert_eq!(stm.entries.last().unwrap().content, STOP_MARKER);
        let remaining = app
            .world_mut()
            .query::<&ToolExecutionRequestMessage>()
            .iter(app.world())
            .count();
        assert_eq!(remaining, 0, "queued tool requests are withdrawn");
    }

    #[test]
    fn stop_cancels_children_and_drops_their_batch_state() {
        let (mut app, entity, task_id) = setup(TaskStatus::Waiting(WaitingReason::SubTaskBatch {
            batch_id: uuid::Uuid::nil(),
        }));
        let batch_id = uuid::Uuid::new_v4();
        let mut child = Task::from_user_input("child", 3, channel());
        child.parent_task_id = Some(task_id);
        child.batch_id = Some(batch_id);
        child.mark_running(chrono::Utc::now());
        let child_id = child.id;
        let child_entity = app.world_mut().spawn(child).id();
        app.world_mut()
            .resource_mut::<EntityIndex>()
            .tasks
            .insert(child_id, child_entity);
        app.world_mut().spawn(SubTaskBatchState {
            batch_id,
            parent_tool_call_id: "call".to_string(),
            tasks: HashMap::new(),
            completed_count: 0,
            total_count: 1,
        });
        app.world_mut().spawn(StopTaskMessage { task_id });
        app.update();

        assert!(matches!(
            app.world().get::<Task>(child_entity).unwrap().status,
            TaskStatus::Failed(FailureReason::UserCancelled)
        ));
        let batches = app
            .world_mut()
            .query::<&SubTaskBatchState>()
            .iter(app.world())
            .count();
        assert_eq!(batches, 0);
        assert_eq!(
            app.world().get::<Task>(entity).unwrap().status,
            TaskStatus::Waiting(WaitingReason::User)
        );
    }

    #[test]
    fn stop_on_idle_task_is_a_noop_notice() {
        let (mut app, entity, task_id) = setup(TaskStatus::Waiting(WaitingReason::User));
        app.world_mut().spawn(StopTaskMessage { task_id });
        app.update();

        assert!(app.world().get::<TurnGeneration>(entity).is_none());
        assert!(
            app.world()
                .get::<ShortTermMemory>(entity)
                .unwrap()
                .entries
                .is_empty()
        );
        let notices = app
            .world_mut()
            .query::<&SystemOutputMessage>()
            .iter(app.world())
            .count();
        assert_eq!(notices, 1);
    }
}
