//! 任务生命周期 System
//!
//! 处理任务终止、重试和完成。

use crate::prelude::*;
use tracing::debug;

use crate::{
    app::{Clock, MemoryConfig},
    contracts::SessionBackend,
    domain::{
        FailureReason, FinishTaskMessage, RetryReadyMessage, ShortTermMemory, SubTaskConfig,
        SummarizationRequestMessage, SummarizationTrigger, Task, TaskStatus, TaskTerminatedMessage,
        ToolCallingState, WaitingReason,
    },
    systems::NativeProcessBackend,
};

type TaskTerminationQuery<'a> = (
    &'a Task,
    Option<&'a ShortTermMemory>,
    Option<&'a SubTaskConfig>,
);

#[allow(dead_code)]
fn task_status_failure_reason(task: &Task) -> Option<FailureReason> {
    match &task.status {
        TaskStatus::Failed(reason) => Some(reason.clone()),
        _ => None,
    }
}

/// 重试就绪 System
///
/// 将到期重试的任务标记为 Ready。
pub fn retry_ready_system(
    clock: Res<Clock>,
    mut commands: Commands,
    messages: Query<(Entity, &RetryReadyMessage)>,
    mut tasks: Query<&mut Task>,
) {
    for (entity, message) in &messages {
        for mut task in &mut tasks {
            if task.id == message.task_id {
                debug!(
                    event = "RetryReady",
                    task_id = %task.id,
                    retry_count = task.retry_count,
                    max_retries = task.max_retries,
                    last_error = ?task.last_error,
                    "marking task ready for retry"
                );
                task.mark_ready_for_retry(clock.0);
                break;
            }
        }

        commands.entity(entity).despawn();
    }
}

/// 任务终止 System
///
/// 处理任务终止，清理状态并触发摘要。
pub fn task_termination_system(
    mut commands: Commands,
    config: Res<MemoryConfig>,
    tasks: Query<TaskTerminationQuery, Changed<Task>>,
    calling_states: Query<(Entity, &ToolCallingState)>,
    backend: Res<NativeProcessBackend>,
) {
    for (task, memory, sub_task_config) in &tasks {
        if task.status.is_terminal() {
            // Clean up any ToolCallingState for this task
            for (cs_entity, cs) in &calling_states {
                if cs.task_id == task.id {
                    debug!(
                        event = "ToolCallingStateTerminated",
                        task_id = %task.id,
                        iteration = cs.iteration,
                        "cleaning up tool calling state on task termination"
                    );
                    commands.entity(cs_entity).despawn();
                }
            }

            // Stop all active shell sessions owned by this task
            match backend.stop_task_sessions(task.id) {
                Ok(stopped_sessions) => {
                    if !stopped_sessions.is_empty() {
                        debug!(
                            event = "TaskShellSessionsStopped",
                            task_id = %task.id,
                            task_status = ?task.status,
                            stopped_sessions = ?stopped_sessions,
                            "stopped active shell sessions on task termination"
                        );
                    }
                }
                Err(e) => {
                    debug!(
                        event = "TaskShellSessionsStopFailed",
                        task_id = %task.id,
                        error = %e,
                        "failed to stop shell sessions on task termination"
                    );
                }
            }

            debug!(
                event = "TaskTerminated",
                task_id = %task.id,
                task_status = ?task.status,
                task_content = %task.content,
                result_summary = %task.result_summary,
                has_stm = memory.is_some(),
                "task reached terminal state"
            );
            commands.spawn(TaskTerminatedMessage { task_id: task.id });

            // 子任务完成时产出 SubTaskCompletedMessage
            if let Some(parent_id) = task.parent_task_id {
                let child_name = sub_task_config
                    .map(|c| c.child_agent_name.clone())
                    .unwrap_or_else(|| "unknown".to_string());
                debug!(
                    event = "SubTaskTerminated",
                    task_id = %task.id,
                    parent_task_id = %parent_id,
                    batch_id = ?task.batch_id,
                    child_name = %child_name,
                    success = matches!(task.status, TaskStatus::Done),
                    result_summary = %task.result_summary,
                    "child task reached terminal state, notifying parent"
                );
                commands.spawn(crate::domain::SubTaskCompletedMessage {
                    parent_task_id: parent_id,
                    batch_id: task.batch_id.unwrap_or_default(),
                    child_task_id: task.id,
                    child_task_name: child_name,
                    result_summary: task.result_summary.clone(),
                    success: matches!(task.status, TaskStatus::Done),
                });
            }

            // 任务完成时触发摘要
            if let Some(stm) = memory
                && !stm.entries.is_empty()
            {
                let content: String = stm
                    .entries
                    .iter()
                    .map(|e| format!("{:?}: {}", e.role, e.content))
                    .collect::<Vec<_>>()
                    .join("\n");

                debug!(
                    event = "SummarizationTriggered",
                    task_id = %task.id,
                    trigger = "TaskComplete",
                    stm_entries = stm.entries.len(),
                    stm_tokens = stm.estimated_tokens,
                    content_len = content.len(),
                    target_tokens = config.summary_target_tokens,
                    "triggering summarization on task completion"
                );
                commands.spawn(SummarizationRequestMessage {
                    task_id: task.id,
                    content_to_summarize: content,
                    target_tokens: config.summary_target_tokens,
                    trigger: SummarizationTrigger::TaskComplete,
                });
            }
        }
    }
}

/// 完成任务 System
///
/// 处理 /finish 命令，将任务标记为 Done。
pub fn finish_task_system(
    clock: Res<Clock>,
    mut commands: Commands,
    messages: Query<(Entity, &FinishTaskMessage)>,
    mut tasks: Query<&mut Task>,
) {
    for (entity, msg) in &messages {
        if let Some(mut task) = tasks.iter_mut().find(|t| t.id == msg.task_id) {
            debug!(
                event = "TaskFinished",
                task_id = %task.id,
                task_status = ?task.status,
                task_content = %task.content,
                "finishing task via /finish command"
            );
            task.mark_done("finished by user", clock.0);
        }
        commands.entity(entity).despawn();
    }
}

/// User Turn 结束时重置 ToolCallingState（安全网）
///
/// 核心重置已由 LLM 产出文本时的 ToolCallingState despawn 完成。
/// 本 system 处理边界场景：任务已进入 Waiting(User) 但 ToolCallingState
/// 仍残留（如外部信号直接修改了任务状态）。
pub fn tool_calling_turn_reset_system(
    mut commands: Commands,
    tasks: Query<&Task>,
    calling_states: Query<(Entity, &ToolCallingState)>,
) {
    for (state_entity, state) in &calling_states {
        if let Some(task) = tasks.iter().find(|t| t.id == state.task_id) {
            if task.status == TaskStatus::Waiting(WaitingReason::User)
                && task.pending_confirmation_id.is_none()
            {
                debug!(
                    event = "ToolCallingStateTurnReset",
                    task_id = %state.task_id,
                    "despawning residual ToolCallingState on Waiting(User)"
                );
                commands.entity(state_entity).despawn();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_task_status_failure_reason() {
        let task = Task {
            status: TaskStatus::Failed(FailureReason::AgentError),
            ..Task::from_user_input(
                "test".to_string(),
                3,
                crate::domain::ChannelId {
                    frontend: crate::domain::FrontendKind::Tui,
                    user_id: "test".to_string(),
                    thread_id: None,
                },
            )
        };
        assert_eq!(
            task_status_failure_reason(&task),
            Some(FailureReason::AgentError)
        );
    }

    #[test]
    fn test_task_status_failure_reason_not_failed() {
        let task = Task {
            status: TaskStatus::Done,
            ..Task::from_user_input(
                "test".to_string(),
                3,
                crate::domain::ChannelId {
                    frontend: crate::domain::FrontendKind::Tui,
                    user_id: "test".to_string(),
                    thread_id: None,
                },
            )
        };
        assert_eq!(task_status_failure_reason(&task), None);
    }
}
