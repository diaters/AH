//! chat_with_agent 多轮对话阻塞与结果回填系统

use crate::prelude::*;
use tracing::{debug, warn};

use crate::{
    contracts::Clock,
    domain::{
        AgentExecutionOutput, AgentExecutionResult, AgentRequestKind, ChatRoundReadyMessage,
        ChatRoundStartedMessage, ChatSession, OutputContent, Task, TaskStatus,
        TaskTerminatedMessage, ToolExecutionResultMessage, ToolReturnedHookPending, WaitingReason,
    },
    ecs::EntityIndex,
};

/// 消费 ChatRoundStartedMessage，将父任务阻塞到 Waiting(SubTaskBatch { batch_id })。
pub fn chat_round_block_system(
    mut commands: Commands,
    clock: Res<Clock>,
    index: Res<EntityIndex>,
    mut tasks: Query<&mut Task>,
    started: Query<(Entity, &ChatRoundStartedMessage)>,
) {
    for (entity, msg) in &started {
        if let Some(mut parent) = index
            .get_task(&msg.parent_task_id)
            .and_then(|e| tasks.get_mut(e).ok())
        {
            parent.status = TaskStatus::Waiting(WaitingReason::SubTaskBatch {
                batch_id: msg.batch_id,
            });
            parent.updated_at = clock.0;
            debug!(
                event = "ChatRoundBlocked",
                parent_task_id = %msg.parent_task_id,
                child_task_id = %msg.child_task_id,
                batch_id = %msg.batch_id,
                "parent task blocked waiting for chat round"
            );
        } else {
            warn!(
                event = "ChatRoundParentNotFound",
                parent_task_id = %msg.parent_task_id,
                "parent task not found for chat round block"
            );
        }
        commands.entity(entity).despawn();
    }
}

/// 消费 ChatRoundReadyMessage，生成 ToolExecutionResultMessage 回填父任务。
/// 父任务状态恢复由 tool_calling_orchestrator_system 统一处理。
pub fn chat_round_completion_system(
    mut commands: Commands,
    index: Res<EntityIndex>,
    tasks: Query<&Task>,
    ready: Query<(Entity, &ChatRoundReadyMessage)>,
) {
    for (entity, msg) in &ready {
        if index
            .get_task(&msg.parent_task_id)
            .and_then(|e| tasks.get(e).ok())
            .is_some()
        {
            debug!(
                event = "ChatRoundCompleted",
                parent_task_id = %msg.parent_task_id,
                child_task_id = %msg.child_task_id,
                batch_id = %msg.batch_id,
                "chat round completed, spawning tool result for orchestrator"
            );
        } else {
            warn!(
                event = "ChatRoundParentNotFound",
                parent_task_id = %msg.parent_task_id,
                "parent task not found for chat round completion"
            );
        }

        let execution_result = AgentExecutionResult {
            task_id: msg.parent_task_id,
            agent_id: msg.parent_agent_id,
            request_kind: AgentRequestKind::LlmCompletion,
            result: Ok(AgentExecutionOutput {
                content: OutputContent::Text(msg.response.clone()),
                reasoning_content: None,
            }),
            prompt: String::new(),
            system_prompt: None,
            tools: vec![],
            reasoning_content: None,
            work_item_id: None,
            conversation: None,
        };

        commands.spawn((
            ToolExecutionResultMessage {
                result: execution_result,
                tool_name: "chat_with_agent".to_string(),
                tool_output: Ok(serde_json::json!({
                    "handle": msg.child_task_id.to_string(),
                    "response": msg.response,
                    "agent": msg.child_agent_name
                })),
                tool_call_id: Some(msg.parent_tool_call_id.clone()),
                processed: false,
                original_tool_output: None,
            },
            ToolReturnedHookPending,
        ));
        commands.entity(entity).despawn();
    }
}

/// 父任务终止时清理所有关联的 chat_with_agent 子任务。
pub fn chat_session_cleanup_system(
    mut commands: Commands,
    terminated: Query<(Entity, &TaskTerminatedMessage)>,
    chat_children: Query<(Entity, &Task, &ChatSession)>,
) {
    for (msg_entity, msg) in &terminated {
        for (child_entity, child_task, _) in &chat_children {
            if child_task.parent_task_id == Some(msg.task_id) {
                if !child_task.status.is_terminal() {
                    warn!(
                        event = "ChatSubtaskCancelledByParentTermination",
                        child_task_id = %child_task.id,
                        parent_task_id = %msg.task_id,
                        old_status = ?child_task.status,
                        "cancelling chat subtask due to parent termination"
                    );
                }
                commands.entity(child_entity).despawn();
            }
        }
        commands.entity(msg_entity).despawn();
    }
}
