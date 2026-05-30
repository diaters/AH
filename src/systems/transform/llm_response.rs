//! LLM 响应处理 System
//!
//! 处理 LLM 响应和 Tool 调用循环。

use bevy::prelude::*;
use tracing::{debug, trace, warn};

use crate::{
    app::{Clock, HarnessSettings},
    domain::{
        AgentExecutionOutput, AgentExecutionRequest, AgentExecutionRequestMessage,
        AgentExecutionResultMessage, AgentRequestKind, ConversationMessage, EntryMetadata,
        EntryRole, FailureReason, OutputContent, ShortTermMemory, Task, TaskStatus,
        ToolCallingState, ToolDefinition, ToolExecutionRequestMessage, ToolExecutionResultMessage,
        UserOutputMessage, WaitingReason,
    },
};

/// 从子任务输出中提取 <<<RESULT>>>...<<</RESULT>>> 标记对内容。
/// 提取最后一对标记。如果未找到，返回 None。
fn extract_result_summary(text: &str) -> Option<String> {
    let end_tag = "<<</RESULT>>>";
    let start_tag = "<<<RESULT>>>";

    // 从后向前找最后一个 end_tag
    let end_pos = text.rfind(end_tag)?;
    // 在 end_tag 之前找对应的 start_tag
    let before_end = &text[..end_pos];
    let start_pos = before_end.rfind(start_tag)?;

    let content_start = start_pos + start_tag.len();
    let content = text[content_start..end_pos].trim();
    if content.is_empty() {
        None
    } else {
        Some(content.to_string())
    }
}

fn task_status_failure_reason(task: &Task) -> Option<FailureReason> {
    match &task.status {
        TaskStatus::Failed(reason) => Some(reason.clone()),
        _ => None,
    }
}

/// LLM 响应处理 System
///
/// 处理 LLM 的响应，更新任务状态，处理 Tool 调用。
#[allow(clippy::type_complexity)]
pub fn llm_response_system(
    clock: Res<Clock>,
    settings: Res<HarnessSettings>,
    mut commands: Commands,
    mut tasks: Query<(&mut Task, Option<&mut ShortTermMemory>)>,
    results: Query<(Entity, &AgentExecutionResultMessage)>,
    calling_states: Query<(Entity, &ToolCallingState)>,
) {
    // Pre-collect ToolCallingState info to avoid mutable borrow conflicts
    struct CallingStateInfo {
        entity: Entity,
        task_id: crate::domain::TaskId,
        iteration: u32,
        max_iterations: u32,
        conversation: Vec<ConversationMessage>,
        tools: Vec<ToolDefinition>,
    }
    let state_info: Vec<CallingStateInfo> = calling_states
        .iter()
        .map(|(e, s)| CallingStateInfo {
            entity: e,
            task_id: s.task_id,
            iteration: s.iteration,
            max_iterations: s.max_iterations,
            conversation: s.conversation.clone(),
            tools: s.tools.clone(),
        })
        .collect();

    for (entity, result_message) in &results {
        if result_message.result.request_kind != AgentRequestKind::LlmCompletion {
            continue;
        }

        let result = &result_message.result;

        for (mut task, short_term) in &mut tasks {
            if task.id != result.task_id {
                continue;
            }

            debug!(
                event = "LlmResponseReceived",
                task_id = %task.id,
                agent_id = %result.agent_id,
                request_kind = ?result.request_kind,
                success = result.result.is_ok(),
                response_content = ?result.result.as_ref().ok(),
                multi_turn = task.multi_turn,
                "llm response received"
            );

            match &result.result {
                Ok(AgentExecutionOutput {
                    content: OutputContent::Text(content),
                    ..
                }) => {
                    // Despawn any ToolCallingState for this task (loop completed with text)
                    if let Some(info) = state_info.iter().find(|i| i.task_id == task.id) {
                        debug!(
                            event = "ToolCallingStateCleaned",
                            task_id = %task.id,
                            "tool calling completed with text response, cleaning up state"
                        );
                        commands.entity(info.entity).despawn();
                    }

                    let stm_len = short_term.as_ref().map(|s| s.entries.len()).unwrap_or(0);
                    let stm_tokens_before =
                        short_term.as_ref().map(|s| s.estimated_tokens).unwrap_or(0);

                    if let Some(mut stm) = short_term {
                        stm.add_entry(EntryRole::Assistant, content, EntryMetadata::default());
                    }
                    let stm_tokens_after =
                        stm_tokens_before + crate::domain::estimate_tokens(content);

                    if task.multi_turn {
                        let old_status = task.status.clone();
                        task.status = TaskStatus::Waiting(WaitingReason::User);
                        task.input_summary = content.clone();
                        task.updated_at = clock.0;
                        debug!(
                            event = "TaskStatusTransition",
                            task_id = %task.id,
                            from_status = ?old_status,
                            to_status = ?task.status,
                            reason = "multi_turn_response",
                            response_len = content.len(),
                            response_content = %content,
                            stm_entries = stm_len + 1,
                            stm_tokens_before = stm_tokens_before,
                            stm_tokens_after = stm_tokens_after,
                            "multi_turn: task now waiting for user"
                        );
                        commands.spawn(UserOutputMessage {
                            content: content.clone(),
                        });
                    } else {
                        // 子任务：从输出中提取 <<<RESULT>>> 标记对作为 result_summary
                        let result_summary = if task.parent_task_id.is_some() {
                            match extract_result_summary(content) {
                                Some(summary) => summary,
                                None => {
                                    warn!(
                                        event = "ResultMarkerNotFound",
                                        task_id = %task.id,
                                        "sub-task output missing <<<RESULT>>> marker, using full output as fallback"
                                    );
                                    content.clone()
                                }
                            }
                        } else {
                            content.clone()
                        };
                        task.mark_done(result_summary, clock.0);
                        commands.spawn(UserOutputMessage {
                            content: content.clone(),
                        });
                    }
                }
                Ok(AgentExecutionOutput {
                    content: OutputContent::ToolCalls(calls),
                    reasoning_content,
                    ..
                }) => {
                    // Check for existing ToolCallingState (follow-up iteration)
                    let existing = state_info.iter().find(|i| i.task_id == task.id);

                    if let Some(info) = existing {
                        let new_iteration = info.iteration + 1;
                        if new_iteration > info.max_iterations {
                            warn!(
                                event = "ToolCallingLimitExceeded",
                                task_id = %task.id,
                                iteration = new_iteration,
                                max_iterations = info.max_iterations,
                                "tool calling exceeded max iterations"
                            );
                            task.last_error = Some(format!(
                                "tool calling exceeded max iterations ({})",
                                info.max_iterations
                            ));
                            task.status = TaskStatus::Failed(FailureReason::AgentError);
                            task.updated_at = clock.0;
                            commands.entity(info.entity).despawn();
                            break;
                        }

                        // Despawn old state and create updated one
                        let mut new_conversation = info.conversation.clone();
                        new_conversation.push(ConversationMessage::Assistant {
                            content: None,
                            tool_calls: calls.clone(),
                            reasoning_content: reasoning_content.clone(),
                        });

                        let pending_ids: Vec<String> = calls.iter().map(|c| c.id.clone()).collect();

                        debug!(
                            event = "ToolCallingStateUpdated",
                            task_id = %task.id,
                            iteration = new_iteration,
                            pending_count = calls.len(),
                            tools = ?calls.iter().map(|c| &c.name).collect::<Vec<_>>(),
                            "tool calling state updated for follow-up iteration"
                        );

                        commands.entity(info.entity).despawn();
                        commands.spawn(ToolCallingState {
                            task_id: task.id,
                            agent_id: result.agent_id,
                            pending_tool_call_ids: pending_ids,
                            iteration: new_iteration,
                            max_iterations: info.max_iterations,
                            conversation: new_conversation,
                            tools: info.tools.clone(),
                        });
                    } else {
                        // First iteration: create new ToolCallingState
                        let mut conversation = Vec::new();
                        if let Some(sp) = &result.system_prompt {
                            conversation.push(ConversationMessage::System {
                                content: sp.clone(),
                            });
                        }
                        conversation.push(ConversationMessage::User {
                            content: result.prompt.clone(),
                        });
                        conversation.push(ConversationMessage::Assistant {
                            content: None,
                            tool_calls: calls.clone(),
                            reasoning_content: reasoning_content.clone(),
                        });

                        let pending_ids: Vec<String> = calls.iter().map(|c| c.id.clone()).collect();
                        let max_iterations = settings.0.max_tool_iterations;

                        debug!(
                            event = "ToolCallingStateCreated",
                            task_id = %task.id,
                            agent_id = %result.agent_id,
                            iteration = 1,
                            pending_count = pending_ids.len(),
                            tools = ?calls.iter().map(|c| &c.name).collect::<Vec<_>>(),
                            max_iterations = max_iterations,
                            "created tool calling state"
                        );

                        commands.spawn(ToolCallingState {
                            task_id: task.id,
                            agent_id: result.agent_id,
                            pending_tool_call_ids: pending_ids,
                            iteration: 1,
                            max_iterations,
                            conversation,
                            tools: result.tools.clone(),
                        });
                    }

                    // Spawn ToolExecutionRequestMessage for each call
                    for call in calls {
                        let tool_input: serde_json::Value = serde_json::from_str(&call.arguments)
                            .unwrap_or(serde_json::Value::Null);
                        commands.spawn(ToolExecutionRequestMessage {
                            request: AgentExecutionRequest {
                                task_id: task.id,
                                agent_id: result.agent_id,
                                request_kind: AgentRequestKind::ToolExecution {
                                    tool_name: call.name.clone(),
                                },
                                prompt: String::new(),
                                system_prompt: None,
                                tools: vec![],
                                conversation: None,
                            },
                            tool_name: call.name.clone(),
                            tool_input,
                            pending_confirmation_id: None,
                            tool_call_id: Some(call.id.clone()),
                            pending_confirmation_options: None,
                        });
                    }

                    // Set task to Waiting(ToolExecution)
                    let old_status = task.status.clone();
                    task.status = TaskStatus::Waiting(WaitingReason::ToolExecution);
                    task.updated_at = clock.0;
                    debug!(
                        event = "TaskStatusTransition",
                        task_id = %task.id,
                        from_status = ?old_status,
                        to_status = ?task.status,
                        reason = "tool_calls_received",
                        tool_count = calls.len(),
                        "task waiting for tool execution"
                    );
                }
                Err(error) if error.is_retryable() && task.retry_count < task.max_retries => {
                    // Clean up ToolCallingState before retry
                    if let Some(info) = state_info.iter().find(|i| i.task_id == task.id) {
                        commands.entity(info.entity).despawn();
                    }
                    let stm_entries = short_term.as_ref().map(|s| s.entries.len()).unwrap_or(0);
                    let stm_tokens = short_term.as_ref().map(|s| s.estimated_tokens).unwrap_or(0);
                    let stm_recent: Option<Vec<_>> = short_term.as_ref().map(|s| {
                        s.entries
                            .iter()
                            .rev()
                            .take(3)
                            .map(|e| (e.role, e.content.clone()))
                            .collect()
                    });
                    debug!(
                        event = "TaskRetryScheduled",
                        task_id = %task.id,
                        retry_count = task.retry_count,
                        max_retries = task.max_retries,
                        error = %error.message(),
                        error_type = std::any::type_name_of_val(error),
                        stm_entries = stm_entries,
                        stm_tokens = stm_tokens,
                        stm_recent = ?stm_recent,
                        "scheduling retry for task"
                    );
                    task.schedule_retry(error, clock.0);
                }
                Err(error) => {
                    // Clean up ToolCallingState before marking task failed
                    if let Some(info) = state_info.iter().find(|i| i.task_id == task.id) {
                        commands.entity(info.entity).despawn();
                    }
                    let stm_entries = short_term.as_ref().map(|s| s.entries.len()).unwrap_or(0);
                    let stm_tokens = short_term.as_ref().map(|s| s.estimated_tokens).unwrap_or(0);
                    let stm_recent: Option<Vec<_>> = short_term.as_ref().map(|s| {
                        s.entries
                            .iter()
                            .rev()
                            .take(3)
                            .map(|e| (e.role, e.content.clone()))
                            .collect()
                    });
                    debug!(
                        event = "TaskFailed",
                        task_id = %task.id,
                        error = %error.message(),
                        error_type = std::any::type_name_of_val(error),
                        retry_count = task.retry_count,
                        max_retries = task.max_retries,
                        stm_entries = stm_entries,
                        stm_tokens = stm_tokens,
                        stm_recent = ?stm_recent,
                        "task failed with non-retryable error"
                    );
                    task.mark_failed(error, clock.0);
                    commands.spawn(UserOutputMessage {
                        content: format!(
                            "任务执行失败（{:?}）：{}",
                            task_status_failure_reason(&task).unwrap_or(FailureReason::Unknown),
                            error.message()
                        ),
                    });
                }
            }

            break;
        }

        commands.entity(entity).despawn();
    }
}

/// Tool 调用循环协调器
///
/// 收集 Tool 执行结果，构建对话历史，生成后续 LLM 请求。
pub fn tool_calling_orchestrator_system(
    clock: Res<Clock>,
    mut commands: Commands,
    mut calling_states: Query<(Entity, &mut ToolCallingState)>,
    tool_results: Query<(Entity, &ToolExecutionResultMessage)>,
    mut tasks: Query<&mut Task>,
) {
    for (state_entity, mut state) in &mut calling_states {
        if state.pending_tool_call_ids.is_empty() {
            continue;
        }

        // 仅在任务处于 Waiting(ToolExecution) 状态时继续，否则跳过
        // （如 tool 需要用户确认时，任务状态会变为 Waiting(User)）
        let task_is_waiting = tasks.iter().any(|t| {
            t.id == state.task_id
                && matches!(t.status, TaskStatus::Waiting(WaitingReason::ToolExecution))
        });
        if !task_is_waiting {
            continue;
        }

        // Collect matching tool results
        let mut collected: Vec<(Entity, String, String)> = Vec::new();
        let mut remaining_ids: Vec<String> = state.pending_tool_call_ids.clone();

        for (result_entity, result) in &tool_results {
            if let Some(ref call_id) = result.tool_call_id
                && remaining_ids.contains(call_id)
            {
                let content = match &result.tool_output {
                    Ok(val) => serde_json::to_string(val).unwrap_or_else(|_| val.to_string()),
                    Err(e) => format!("error: {}", e),
                };
                collected.push((result_entity, call_id.clone(), content));
                remaining_ids.retain(|id| id != call_id);
            }
        }

        // Not all results ready yet
        if !remaining_ids.is_empty() {
            trace!(
                event = "ToolCallingPending",
                task_id = %state.task_id,
                iteration = state.iteration,
                pending_count = remaining_ids.len(),
                total_count = state.pending_tool_call_ids.len(),
                "waiting for remaining tool results"
            );
            continue;
        }

        // All results collected — add to conversation
        debug!(
            event = "ToolCallingResultsCollected",
            task_id = %state.task_id,
            iteration = state.iteration,
            result_count = collected.len(),
            "all tool results collected, building follow-up request"
        );

        for (_, call_id, content) in &collected {
            state.conversation.push(ConversationMessage::Tool {
                tool_call_id: call_id.clone(),
                content: content.clone(),
            });
        }

        // Clear pending IDs (all collected)
        state.pending_tool_call_ids.clear();

        // Despawn consumed result entities
        for (entity, _, _) in &collected {
            commands.entity(*entity).despawn();
        }

        // Check iteration limit
        if state.iteration >= state.max_iterations {
            warn!(
                event = "ToolCallingLimitExceeded",
                task_id = %state.task_id,
                iteration = state.iteration,
                max_iterations = state.max_iterations,
                "tool calling reached max iterations"
            );
            if let Some(mut task) = tasks.iter_mut().find(|t| t.id == state.task_id) {
                task.last_error = Some(format!(
                    "tool calling reached max iterations ({})",
                    state.max_iterations
                ));
                task.status = TaskStatus::Failed(FailureReason::AgentError);
                task.updated_at = clock.0;
            }
            commands.entity(state_entity).despawn();
            continue;
        }

        // Spawn follow-up LLM request with conversation
        let request = AgentExecutionRequest {
            task_id: state.task_id,
            agent_id: state.agent_id,
            request_kind: AgentRequestKind::LlmCompletion,
            prompt: String::new(),
            system_prompt: None,
            tools: state.tools.clone(),
            conversation: Some(state.conversation.clone()),
        };

        debug!(
            event = "ToolCallingFollowUp",
            task_id = %state.task_id,
            agent_id = %state.agent_id,
            iteration = state.iteration,
            conversation_messages = state.conversation.len(),
            tools_count = state.tools.len(),
            "spawning follow-up LLM request with tool results"
        );

        commands.spawn(AgentExecutionRequestMessage { request });

        // Set task back to Waiting(Agent)
        if let Some(mut task) = tasks.iter_mut().find(|t| t.id == state.task_id)
            && matches!(
                task.status,
                TaskStatus::Waiting(WaitingReason::ToolExecution)
            )
        {
            let old_status = task.status.clone();
            task.status = TaskStatus::Waiting(WaitingReason::Agent);
            task.updated_at = clock.0;
            debug!(
                event = "TaskStatusTransition",
                task_id = %task.id,
                from_status = ?old_status,
                to_status = ?task.status,
                reason = "tool_results_collected",
                "task waiting for follow-up LLM response"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_result_summary_basic() {
        let text = "一些分析\n\n<<<RESULT>>>\n69.7亿只\n<<</RESULT>>>";
        assert_eq!(extract_result_summary(text), Some("69.7亿只".to_string()));
    }

    #[test]
    fn test_extract_result_summary_no_marker() {
        let text = "没有标记对的普通输出";
        assert_eq!(extract_result_summary(text), None);
    }

    #[test]
    fn test_extract_result_summary_multiple_takes_last() {
        let text = "<<<RESULT>>>\n中间结果\n<<</RESULT>>>\n继续分析\n\n<<<RESULT>>>\n最终结果\n<<</RESULT>>>";
        assert_eq!(extract_result_summary(text), Some("最终结果".to_string()));
    }

    #[test]
    fn test_extract_result_summary_empty_content() {
        let text = "<<<RESULT>>>\n<<</RESULT>>>";
        assert_eq!(extract_result_summary(text), None);
    }

    #[test]
    fn test_extract_result_summary_multiline() {
        let text = "详细计算过程...\n\n<<<RESULT>>>\n一对小猫10年内可繁衍约69.7亿只\n公式: 2×3^20\n<<</RESULT>>>";
        let result = extract_result_summary(text).unwrap();
        assert!(result.contains("69.7亿只"));
        assert!(result.contains("2×3^20"));
    }
}
