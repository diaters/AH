//! LLM 响应处理 System
//!
//! 纯路由 + Task 级通用 LLM 响应处理：
//! - WorkItem 结果按知识域分派到各领域模块（评估 → `evaluation`、摘要 →
//!   `summarization`、经验收集/画像/skill → `experience`）
//! - Task 级通用路径（文本 / 错误重试）在本模块处理
//! - 工具调用循环编排（ToolCalls 分支）见 `systems/tools/tool_calling.rs`

use crate::prelude::*;
use tracing::{debug, trace, warn};

use crate::{
    contracts::Clock,
    domain::{
        AgentExecutionOutput, AgentExecutionResultMessage, AgentRequestKind, ChatRoundReadyMessage,
        ChatSession, EntryMetadata, EntryRole, ExperienceStore, FailureReason, MemoryConfig,
        OutputContent, ProfileGenerationContext, ShortTermMemory, SkillCreationContext, Task,
        TaskStatus, ToolCallingState, UserOutputMessage, WaitingReason, WorkItem, WorkItemType,
    },
    ecs::EntityIndex,
    systems::HarnessSettings,
    systems::evaluation::handle_evaluation_work_item_result,
    systems::experience::{
        handle_experience_collection_llm_response, handle_profile_generation_llm_response,
        handle_skill_creation_llm_response, handle_skill_update_llm_response,
    },
    systems::summarization::handle_summarization_work_item_result,
    systems::tools::{CallingStateInfo, find_calling_state},
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
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub fn llm_response_system(
    clock: Res<Clock>,
    settings: Res<HarnessSettings>,
    _config: Res<MemoryConfig>,
    eval_config: Res<crate::domain::TaskEvaluationConfig>,
    mut commands: Commands,
    index: Res<EntityIndex>,
    mut tasks: Query<(
        Entity,
        &mut Task,
        Option<&mut ShortTermMemory>,
        Option<&ChatSession>,
    )>,
    results: Query<(Entity, &AgentExecutionResultMessage)>,
    calling_states: Query<(Entity, &ToolCallingState)>,
    mut work_items: Query<(Entity, &mut WorkItem)>,
    mut experience_store: ResMut<ExperienceStore>,
    profile_contexts: Query<&ProfileGenerationContext>,
    skill_creation_contexts: Query<&SkillCreationContext>,
) {
    // Pre-collect ToolCallingState info to avoid mutable borrow conflicts
    let state_info: Vec<CallingStateInfo> = calling_states
        .iter()
        .map(|(e, s)| CallingStateInfo {
            entity: e,
            task_id: s.task_id,
            iteration: s.iteration,
            max_iterations: s.max_iterations,
            conversation: s.conversation.clone(),
            tools: s.tools.clone(),
            request_kind: s.request_kind.clone(),
            work_item_id: s.work_item_id,
        })
        .collect();

    for (entity, result_message) in &results {
        let result = &result_message.result;

        // 处理 WorkItem 结果（Evaluation、Summarization 等）
        // 按 work_item_id 严格路由：声明属于 WorkItem 的响应必须由 WorkItem 分支处理；
        // 找不到对应 WorkItem（已 despawn / 异步错配）时显式丢弃，不 fall through 到
        // task 级 mark_done。这避免"无主响应误触发 Task 完成"类的循环 bug。
        if let Some(work_item_id) = result.work_item_id {
            let work_item_lookup = work_items
                .iter()
                .find(|(_, wi)| wi.id == work_item_id)
                .map(|(e, w)| (e, w.clone()));

            if let Some((work_item_entity, work_item)) = work_item_lookup {
                match work_item.work_type {
                    WorkItemType::Evaluation => {
                        handle_evaluation_work_item_result(
                            &mut commands,
                            &index,
                            &mut tasks,
                            entity,
                            work_item_entity,
                            &work_item,
                            result,
                            clock.0,
                            &eval_config,
                        );
                        continue;
                    }
                    WorkItemType::Summarization => {
                        handle_summarization_work_item_result(
                            &mut commands,
                            &index,
                            &mut tasks,
                            entity,
                            work_item_entity,
                            &work_item,
                            result,
                            clock.0,
                        );
                        continue;
                    }
                    // handler 返回 true 表示响应已被领域消费；false 表示响应形态
                    // 未被识别（如纯文本），fall through 到 `_ => {}` 走 task 级通用路径。
                    WorkItemType::ExperienceCollection
                        if handle_experience_collection_llm_response(
                            &mut commands,
                            &experience_store,
                            &mut work_items,
                            entity,
                            work_item_entity,
                            &work_item,
                            result,
                        ) =>
                    {
                        continue;
                    }
                    WorkItemType::ProfileGeneration
                        if handle_profile_generation_llm_response(
                            &mut commands,
                            &experience_store,
                            &profile_contexts,
                            entity,
                            work_item_entity,
                            &work_item,
                            result,
                        ) =>
                    {
                        continue;
                    }
                    WorkItemType::SkillUpdate
                        if handle_skill_update_llm_response(
                            &mut commands,
                            entity,
                            work_item_entity,
                            &work_item,
                            result,
                        ) =>
                    {
                        continue;
                    }
                    WorkItemType::SkillCreation
                        if handle_skill_creation_llm_response(
                            &mut commands,
                            &mut experience_store,
                            &skill_creation_contexts,
                            &mut work_items,
                            entity,
                            work_item_entity,
                            &work_item,
                            result,
                        ) =>
                    {
                        continue;
                    }
                    _ => {}
                }
            } else {
                // 无主响应：响应声明属于 WorkItem x，但 x 已不存在（despawn / 异步错配）。
                // 显式丢弃响应，不 fall through 到 task 级 mark_done。
                warn!(
                    event = "LlmResponseOrphaned",
                    task_id = %result.task_id,
                    work_item_id = %work_item_id,
                    "LLM response references non-existent work item, dropping"
                );
                commands.entity(entity).despawn();
                continue;
            }
        }

        // 非 LlmCompletion 的结果仅放行 BrainDecision+ToolCalls（让 tool calling 循环处理）
        if result.request_kind != AgentRequestKind::LlmCompletion {
            let is_brain_tool_calls = result.request_kind == AgentRequestKind::BrainDecision
                && matches!(
                    &result.result,
                    Ok(AgentExecutionOutput {
                        content: OutputContent::ToolCalls(_),
                        ..
                    })
                );
            if !is_brain_tool_calls {
                continue;
            }
        }

        for (_task_entity, mut task, short_term, chat_session) in &mut tasks {
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
                    // 按 (task_id, work_item_id) 严格匹配：仅清理当前调用循环的 State，
                    // 不影响同 Task 下其他 WorkItem 的 State。
                    if let Some(info) =
                        find_calling_state(&state_info, task.id, result.work_item_id)
                    {
                        debug!(
                            event = "ToolCallingStateCleaned",
                            task_id = %task.id,
                            work_item_id = ?result.work_item_id,
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

                    // 若当前任务是 chat_with_agent 子任务，则进入 Waiting(ChatAgent) 并触发 ChatRoundReadyMessage
                    if let Some(parent_task_id) = task.parent_task_id
                        && let Some(chat_session) = chat_session
                    {
                        let response_text = content.clone();

                        task.mark_waiting(WaitingReason::ChatAgent, clock.0);
                        task.result_summary = response_text.clone();

                        commands.spawn(ChatRoundReadyMessage {
                            child_task_id: task.id,
                            parent_task_id,
                            parent_agent_id: task.creator,
                            batch_id: chat_session.current_batch_id,
                            parent_tool_call_id: chat_session.parent_tool_call_id.clone(),
                            response: response_text,
                            child_agent_name: chat_session.child_agent_name.clone(),
                        });

                        trace!(
                            event = "ChatRoundReady",
                            child_task_id = %task.id,
                            parent_task_id = %parent_task_id,
                            batch_id = %chat_session.current_batch_id,
                            "chat subtask waiting for parent next round"
                        );

                        continue;
                    }

                    if task.multi_turn {
                        task.mark_waiting(WaitingReason::User, clock.0);
                        task.input_summary = content.clone();
                        debug!(
                            event = "MultiTurnWaitingUser",
                            task_id = %task.id,
                            response_len = content.len(),
                            response_content = %content,
                            stm_entries = stm_len + 1,
                            stm_tokens_before = stm_tokens_before,
                            stm_tokens_after = stm_tokens_after,
                            "multi-turn: task now waiting for user"
                        );
                        commands.spawn(UserOutputMessage {
                            task_id: task.id,
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
                            task_id: task.id,
                            content: content.clone(),
                        });
                    }
                }
                Ok(AgentExecutionOutput {
                    content: OutputContent::ToolCalls(calls),
                    reasoning_content,
                    ..
                }) => {
                    // 工具调用循环编排在 tools/tool_calling.rs（知识域归位）
                    crate::systems::tools::handle_tool_calls_response(
                        &mut commands,
                        &mut task,
                        &state_info,
                        result,
                        calls,
                        reasoning_content,
                        clock.0,
                        settings.0.max_tool_iterations,
                        &work_items,
                    );
                }
                Err(error) if error.is_retryable() && task.retry_count < task.max_retries => {
                    // Clean up ToolCallingState before retry.
                    // 按 (task_id, work_item_id) 严格匹配。
                    if let Some(info) =
                        find_calling_state(&state_info, task.id, result.work_item_id)
                    {
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
                    // Clean up ToolCallingState before marking task failed.
                    // 按 (task_id, work_item_id) 严格匹配。
                    if let Some(info) =
                        find_calling_state(&state_info, task.id, result.work_item_id)
                    {
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
                        task_id: task.id,
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
