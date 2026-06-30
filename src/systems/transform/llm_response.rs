//! LLM 响应处理 System
//!
//! 处理 LLM 响应和 Tool 调用循环。

use bevy::prelude::*;
use tracing::{debug, trace, warn};

use crate::{
    app::{Clock, HarnessSettings, MemoryConfig},
    domain::{
        AgentExecutionOutput, AgentExecutionRequest, AgentExecutionRequestMessage,
        AgentExecutionResultMessage, AgentRequestKind, ChatRoundReadyMessage, ChatSession,
        ConversationMessage, EntryMetadata, EntryRole, ExperienceCollectionCompletedMessage,
        ExperienceStore, FailureReason, MessageDispatchedHookPending, OffTrackPolicy,
        OutputContent, ShortTermMemory, SystemOutputMessage, Task, TaskStatus,
        ToolCalledHookPending, ToolCallingState, ToolDefinition, ToolExecutionRequestMessage,
        ToolExecutionResultMessage, UserOutputMessage, WaitingReason, WorkItem,
        WorkItemLifecycleHookPending, WorkItemType,
    },
    user_plugins::hook_point::HookPoint,
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

fn has_experience_submission(store: &ExperienceStore, task_id: crate::domain::TaskId) -> bool {
    store.root_candidates_for_task(task_id).iter().any(|id| {
        store
            .candidates
            .get(id)
            .is_some_and(|c| c.producer_task_id == task_id)
    }) || store
        .inboxes
        .get(&task_id)
        .is_some_and(|inbox| !inbox.candidate_ids.is_empty())
}

/// 处理 Evaluation WorkItem 的执行结果
#[allow(clippy::too_many_arguments, clippy::drop_non_drop)]
fn handle_evaluation_work_item_result(
    commands: &mut Commands,
    tasks: &mut Query<(
        Entity,
        &mut Task,
        Option<&mut ShortTermMemory>,
        Option<&ChatSession>,
    )>,
    result_entity: Entity,
    work_item_entity: Entity,
    work_item: &WorkItem,
    result: &crate::domain::AgentExecutionResult,
    now: chrono::DateTime<chrono::Utc>,
    eval_config: &crate::domain::TaskEvaluationConfig,
) {
    use crate::domain::{EvaluationDecision, EvaluationResult};

    // 解析评估结果
    let evaluation: EvaluationResult = match &result.result {
        Ok(AgentExecutionOutput {
            content: OutputContent::Text(content),
            ..
        }) => match crate::domain::parse_evaluation_result(content) {
            Ok(e) => e,
            Err(e) => {
                warn!(
                    event = "EvaluationParseFailed",
                    task_id = %work_item.task_id,
                    work_item_id = %work_item.id,
                    error = %e,
                    content = %content,
                    "failed to parse evaluation result"
                );
                // 解析失败，恢复任务状态避免死锁
                if let Some((_, mut task, _, _)) = tasks
                    .iter_mut()
                    .find(|(_, t, _, _)| t.id == work_item.task_id)
                    && matches!(task.status, TaskStatus::Waiting(WaitingReason::Evaluator))
                {
                    let old_status = task.status.clone();
                    task.status = TaskStatus::Ready;
                    task.updated_at = now;
                    debug!(
                        event = "TaskStatusRestoredAfterEvaluationFailed",
                        task_id = %task.id,
                        from_status = ?old_status,
                        to_status = ?task.status,
                        "task restored to Ready after evaluation parse failure"
                    );
                }
                commands.entity(work_item_entity).despawn();
                commands.entity(result_entity).despawn();
                return;
            }
        },
        Ok(_) => {
            warn!(
                event = "EvaluationInvalidOutput",
                task_id = %work_item.task_id,
                work_item_id = %work_item.id,
                "evaluation returned non-text output"
            );
            // 非文本输出，恢复任务状态避免死锁
            if let Some((_, mut task, _, _)) = tasks
                .iter_mut()
                .find(|(_, t, _, _)| t.id == work_item.task_id)
                && matches!(task.status, TaskStatus::Waiting(WaitingReason::Evaluator))
            {
                let old_status = task.status.clone();
                task.status = TaskStatus::Ready;
                task.updated_at = now;
                debug!(
                    event = "TaskStatusRestoredAfterEvaluationFailed",
                    task_id = %task.id,
                    from_status = ?old_status,
                    to_status = ?task.status,
                    "task restored to Ready after evaluation invalid output"
                );
            }
            commands.entity(work_item_entity).despawn();
            commands.entity(result_entity).despawn();
            return;
        }
        Err(e) => {
            warn!(
                event = "EvaluationExecutionFailed",
                task_id = %work_item.task_id,
                work_item_id = %work_item.id,
                error = %e.message(),
                "evaluation execution failed"
            );
            // 执行失败，恢复任务状态避免死锁
            if let Some((_, mut task, _, _)) = tasks
                .iter_mut()
                .find(|(_, t, _, _)| t.id == work_item.task_id)
                && matches!(task.status, TaskStatus::Waiting(WaitingReason::Evaluator))
            {
                let old_status = task.status.clone();
                task.status = TaskStatus::Ready;
                task.updated_at = now;
                debug!(
                    event = "TaskStatusRestoredAfterEvaluationFailed",
                    task_id = %task.id,
                    from_status = ?old_status,
                    to_status = ?task.status,
                    "task restored to Ready after evaluation execution failure"
                );
            }
            commands.entity(work_item_entity).despawn();
            commands.entity(result_entity).despawn();
            return;
        }
    };

    // 更新任务状态（两阶段应用，避免借用冲突）
    if let Some((_, mut task, _, _)) = tasks
        .iter_mut()
        .find(|(_, t, _, _)| t.id == work_item.task_id)
    {
        let old_status = task.status.clone();

        // 第一阶段：推导出中间动作描述
        struct EvaluationApplyEffects {
            next_status: TaskStatus,
            last_error: Option<String>,
            stm_injection: Option<(EntryRole, String, EntryMetadata)>,
            system_message: Option<String>,
        }

        let effects = match evaluation.decision {
            EvaluationDecision::Continue => {
                debug!(
                    event = "EvaluationResultApplied",
                    task_id = %task.id,
                    work_item_id = %work_item.id,
                    decision = "Continue",
                    reasoning = %evaluation.reasoning,
                    "evaluation result: continue"
                );
                EvaluationApplyEffects {
                    next_status: TaskStatus::Ready,
                    last_error: None,
                    stm_injection: None,
                    system_message: None,
                }
            }
            EvaluationDecision::Complete => {
                debug!(
                    event = "EvaluationResultApplied",
                    task_id = %task.id,
                    work_item_id = %work_item.id,
                    decision = "Complete",
                    reasoning = %evaluation.reasoning,
                    "evaluation result: complete"
                );
                EvaluationApplyEffects {
                    next_status: TaskStatus::Done,
                    last_error: None,
                    stm_injection: None,
                    system_message: None,
                }
            }
            EvaluationDecision::Failed => {
                debug!(
                    event = "EvaluationResultApplied",
                    task_id = %task.id,
                    work_item_id = %work_item.id,
                    decision = "Failed",
                    reasoning = %evaluation.reasoning,
                    "evaluation result: failed"
                );
                EvaluationApplyEffects {
                    next_status: TaskStatus::Failed(FailureReason::AgentError),
                    last_error: None,
                    stm_injection: None,
                    system_message: None,
                }
            }
            EvaluationDecision::OffTrack => {
                debug!(
                    event = "EvaluationResultApplied",
                    task_id = %task.id,
                    work_item_id = %work_item.id,
                    decision = "OffTrack",
                    reasoning = %evaluation.reasoning,
                    suggested_action = ?evaluation.suggested_action,
                    policy = ?eval_config.offtrack_policy,
                    "evaluation result: off-track"
                );
                match eval_config.offtrack_policy {
                    OffTrackPolicy::AutoCorrect => EvaluationApplyEffects {
                        next_status: TaskStatus::Ready,
                        last_error: None,
                        stm_injection: evaluation.suggested_action.as_ref().map(|action| {
                            (
                                EntryRole::Summary,
                                format!("[Evaluation AutoCorrect] {}", action),
                                EntryMetadata {
                                    keywords: vec![
                                        "evaluation".to_string(),
                                        "offtrack".to_string(),
                                        "autocorrect".to_string(),
                                    ],
                                    ..Default::default()
                                },
                            )
                        }),
                        system_message: None,
                    },
                    OffTrackPolicy::AskUser => {
                        let summary = format!(
                            "[Evaluation AskUser] 任务偏航：{}；建议操作：{}",
                            evaluation.reasoning,
                            evaluation.suggested_action.as_deref().unwrap_or("无")
                        );

                        EvaluationApplyEffects {
                            next_status: TaskStatus::Waiting(WaitingReason::User),
                            last_error: None,
                            stm_injection: Some((
                                EntryRole::Summary,
                                summary,
                                EntryMetadata {
                                    keywords: vec![
                                        "evaluation".to_string(),
                                        "offtrack".to_string(),
                                        "askuser".to_string(),
                                    ],
                                    ..Default::default()
                                },
                            )),
                            system_message: Some(format!(
                                "任务偏航：{}\n建议操作：{}",
                                evaluation.reasoning,
                                evaluation.suggested_action.as_deref().unwrap_or("无")
                            )),
                        }
                    }
                    OffTrackPolicy::Fail => EvaluationApplyEffects {
                        next_status: TaskStatus::Failed(FailureReason::AgentError),
                        last_error: Some(format!("Evaluation OffTrack: {}", evaluation.reasoning)),
                        stm_injection: None,
                        system_message: None,
                    },
                }
            }
        };

        // 第二阶段：应用效果
        task.status = effects.next_status;
        task.updated_at = now;
        if let Some(err) = effects.last_error {
            task.last_error = Some(err);
        }

        let task_id = task.id;
        // 释放 task 借用，以便后续再次查询 tasks
        drop(task);

        // 注入纠偏上下文到 STM（AutoCorrect / AskUser 均适用）
        if let Some((role, content, metadata)) = effects.stm_injection
            && let Some((_, _, Some(mut stm), _)) =
                tasks.iter_mut().find(|(_, t, _, _)| t.id == task_id)
        {
            stm.add_entry(role, &content, metadata);
        }

        // 发送系统通知（仅 AskUser）
        if let Some(msg) = effects.system_message {
            commands.spawn(SystemOutputMessage {
                task_id,
                content: msg,
            });
        }

        debug!(
            event = "TaskStatusTransition",
            task_id = %task_id,
            from_status = ?old_status,
            reason = "evaluation_result",
            work_item_id = %work_item.id,
            "task status updated by evaluation"
        );
    }

    // 清理 WorkItem 和结果消息
    commands.entity(work_item_entity).despawn();
    commands.entity(result_entity).despawn();
}

/// 处理 Summarization WorkItem 的执行结果
#[allow(clippy::too_many_arguments)]
fn handle_summarization_work_item_result(
    commands: &mut Commands,
    tasks: &mut Query<(
        Entity,
        &mut Task,
        Option<&mut ShortTermMemory>,
        Option<&ChatSession>,
    )>,
    result_entity: Entity,
    work_item_entity: Entity,
    work_item: &WorkItem,
    result: &crate::domain::AgentExecutionResult,
    now: chrono::DateTime<chrono::Utc>,
    config: &MemoryConfig,
) {
    let task_id = work_item.task_id;

    match &result.result {
        Ok(AgentExecutionOutput {
            content: OutputContent::Text(summary),
            ..
        }) => {
            // 更新摘要前缀（查找任意一个带 ShortTermMemory 的任务）
            // 注意：这里简化处理，假设全局只有一个 STM（当前架构确实如此）
            for (_, _, short_term, _) in tasks.iter_mut() {
                if let Some(mut memory) = short_term {
                    memory.summary_prefix = Some(summary.clone());

                    // 移除已压缩的 entries（保留最近 N 轮）
                    let preserve_count = (config.preserve_recent_turns * 2) as usize;
                    let removed = if memory.entries.len() > preserve_count {
                        let removed = memory.entries.len() - preserve_count;
                        memory.entries.drain(0..removed);
                        removed
                    } else {
                        0
                    };

                    // 重新计算 token
                    memory.recalculate_tokens();

                    debug!(
                        event = "SummarizationCompleted",
                        task_id = %task_id,
                        summary_len = summary.len(),
                        summary = %summary,
                        removed_entries = removed,
                        remaining_entries = memory.entries.len(),
                        new_tokens = memory.estimated_tokens,
                        "summarization completed"
                    );
                    break;
                }
            }

            // 发送系统通知（不进入 STM）
            commands.spawn(SystemOutputMessage {
                task_id,
                content: format!("📝 摘要完成\n\n{}", summary),
            });

            // 恢复任务状态：从 Waiting(Summarization) 恢复为 Waiting(User)
            // 这适用于 UserCommand 和 TokenThreshold 触发的摘要
            if let Some((_, mut task, _, _)) = tasks.iter_mut().find(|(_, t, _, _)| t.id == task_id)
                && matches!(
                    task.status,
                    TaskStatus::Waiting(WaitingReason::Summarization)
                )
            {
                let old_status = task.status.clone();
                task.status = TaskStatus::Waiting(WaitingReason::User);
                task.updated_at = now;
                debug!(
                    event = "TaskStatusRestoredAfterSummarization",
                    task_id = %task.id,
                    from_status = ?old_status,
                    to_status = ?task.status,
                    "task restored to waiting for user after summarization"
                );
            }
        }
        Ok(_) => {
            warn!(
                event = "SummarizationInvalidOutput",
                task_id = %task_id,
                work_item_id = %work_item.id,
                "summarization returned non-text output"
            );

            // 发送系统通知（不进入 STM）
            commands.spawn(SystemOutputMessage {
                task_id,
                content: "⚠️ 摘要失败：返回了非文本输出".to_string(),
            });

            // 即使摘要失败，也恢复任务状态，避免任务卡住
            if let Some((_, mut task, _, _)) = tasks.iter_mut().find(|(_, t, _, _)| t.id == task_id)
                && matches!(
                    task.status,
                    TaskStatus::Waiting(WaitingReason::Summarization)
                )
            {
                let old_status = task.status.clone();
                task.status = TaskStatus::Waiting(WaitingReason::User);
                task.updated_at = now;
                debug!(
                    event = "TaskStatusRestoredAfterSummarizationFailed",
                    task_id = %task.id,
                    from_status = ?old_status,
                    to_status = ?task.status,
                    "task restored to waiting for user after summarization failed"
                );
            }
        }
        Err(error) => {
            debug!(
                event = "SummarizationFailed",
                task_id = %task_id,
                work_item_id = %work_item.id,
                error = %error.message(),
                error_type = std::any::type_name_of_val(error),
                "summarization failed"
            );

            // 发送系统通知（不进入 STM）
            commands.spawn(SystemOutputMessage {
                task_id,
                content: format!("⚠️ 摘要失败：{}", error.message()),
            });

            // 即使摘要失败，也恢复任务状态，避免任务卡住
            if let Some((_, mut task, _, _)) = tasks.iter_mut().find(|(_, t, _, _)| t.id == task_id)
                && matches!(
                    task.status,
                    TaskStatus::Waiting(WaitingReason::Summarization)
                )
            {
                let old_status = task.status.clone();
                task.status = TaskStatus::Waiting(WaitingReason::User);
                task.updated_at = now;
                debug!(
                    event = "TaskStatusRestoredAfterSummarizationFailed",
                    task_id = %task.id,
                    from_status = ?old_status,
                    to_status = ?task.status,
                    "task restored to waiting for user after summarization failed"
                );
            }
        }
    }

    // 清理 WorkItem 和结果消息
    commands.entity(work_item_entity).despawn();
    commands.entity(result_entity).despawn();
}

/// LLM 响应处理 System
///
/// 处理 LLM 的响应，更新任务状态，处理 Tool 调用。
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub fn llm_response_system(
    clock: Res<Clock>,
    settings: Res<HarnessSettings>,
    config: Res<MemoryConfig>,
    eval_config: Res<crate::domain::TaskEvaluationConfig>,
    mut commands: Commands,
    mut tasks: Query<(
        Entity,
        &mut Task,
        Option<&mut ShortTermMemory>,
        Option<&ChatSession>,
    )>,
    results: Query<(Entity, &AgentExecutionResultMessage)>,
    calling_states: Query<(Entity, &ToolCallingState)>,
    mut work_items: Query<(Entity, &mut WorkItem)>,
    experience_store: Res<ExperienceStore>,
) {
    // Pre-collect ToolCallingState info to avoid mutable borrow conflicts
    struct CallingStateInfo {
        entity: Entity,
        task_id: crate::domain::TaskId,
        iteration: u32,
        max_iterations: u32,
        conversation: Vec<ConversationMessage>,
        tools: Vec<ToolDefinition>,
        request_kind: AgentRequestKind,
        work_item_id: Option<uuid::Uuid>,
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
            request_kind: s.request_kind.clone(),
            work_item_id: s.work_item_id,
        })
        .collect();

    for (entity, result_message) in &results {
        let result = &result_message.result;

        // 处理 WorkItem 结果（Evaluation、Summarization 等）
        if let Some(work_item_id) = result.work_item_id
            && let Some((work_item_entity, work_item)) =
                work_items.iter().find(|(_, wi)| wi.id == work_item_id)
        {
            match work_item.work_type {
                WorkItemType::Evaluation => {
                    handle_evaluation_work_item_result(
                        &mut commands,
                        &mut tasks,
                        entity,
                        work_item_entity,
                        work_item,
                        result,
                        clock.0,
                        &eval_config,
                    );
                    continue;
                }
                WorkItemType::Summarization => {
                    handle_summarization_work_item_result(
                        &mut commands,
                        &mut tasks,
                        entity,
                        work_item_entity,
                        work_item,
                        result,
                        clock.0,
                        &config,
                    );
                    continue;
                }
                WorkItemType::ExperienceCollection => {
                    match &result.result {
                        Ok(AgentExecutionOutput {
                            content: OutputContent::ToolCalls(_),
                            ..
                        }) => {
                            // 不 continue，让下面的 tool calling loop 处理 tool calls
                        }
                        Ok(_) => {
                            // LLM 返回普通文本：检查是否有候选提交
                            let had_submission =
                                has_experience_submission(&experience_store, work_item.task_id);

                            let completed_task_id = work_item.task_id;
                            let completed_parent_task_id = work_item.parent_task_id;
                            let completed_agent_id =
                                work_item.assigned_agent.unwrap_or(uuid::Uuid::nil());
                            let governing_agent_id =
                                work_item.governing_agent_id.unwrap_or(completed_agent_id);

                            if let Ok(mut wi) = work_items.get_mut(work_item_entity) {
                                if had_submission {
                                    wi.1.complete();
                                    commands.entity(work_item_entity).insert(
                                        WorkItemLifecycleHookPending(
                                            HookPoint::OnWorkItemCompleted,
                                        ),
                                    );
                                } else {
                                    wi.1.fail();
                                    commands.entity(work_item_entity).insert(
                                        WorkItemLifecycleHookPending(HookPoint::OnWorkItemFailed),
                                    );
                                }
                            }

                            commands.spawn(ExperienceCollectionCompletedMessage {
                                task_id: completed_task_id,
                                parent_task_id: completed_parent_task_id,
                                agent_id: completed_agent_id,
                                governing_agent_id,
                            });

                            commands.entity(work_item_entity).despawn();
                            commands.entity(entity).despawn();
                            continue;
                        }
                        Err(_) => {
                            if let Ok(mut wi) = work_items.get_mut(work_item_entity) {
                                wi.1.fail();
                                commands.entity(work_item_entity).insert(
                                    WorkItemLifecycleHookPending(HookPoint::OnWorkItemFailed),
                                );
                            }
                            commands.entity(work_item_entity).despawn();
                            commands.entity(entity).despawn();
                            continue;
                        }
                    }
                }
                _ => {}
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

                    // 若当前任务是 chat_with_agent 子任务，则进入 Waiting(ChatAgent) 并触发 ChatRoundReadyMessage
                    if let Some(parent_task_id) = task.parent_task_id
                        && let Some(chat_session) = chat_session
                    {
                        let response_text = content.clone();

                        task.status = TaskStatus::Waiting(WaitingReason::ChatAgent);
                        task.result_summary = response_text.clone();
                        task.updated_at = clock.0;

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
                            request_kind: info.request_kind.clone(),
                            work_item_id: info.work_item_id,
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
                            request_kind: result.request_kind.clone(),
                            work_item_id: result.work_item_id,
                        });
                    }

                    // Spawn ToolExecutionRequestMessage for each call
                    for call in calls {
                        let tool_input: serde_json::Value = serde_json::from_str(&call.arguments)
                            .unwrap_or(serde_json::Value::Null);
                        commands.spawn((
                            ToolCalledHookPending,
                            ToolExecutionRequestMessage {
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
                                    work_item_id: None,
                                },
                                tool_name: call.name.clone(),
                                tool_input,
                                pending_confirmation_id: None,
                                tool_call_id: Some(call.id.clone()),
                                pending_confirmation_options: None,
                            },
                        ));
                    }

                    // Set task to Waiting(ToolExecution) — but not for ExperienceCollection WorkItems
                    // since their original task is already in terminal state
                    if result.work_item_id.is_none() {
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

        // 仅在任务处于”由 tool calling loop 驱动的等待态”时继续，否则跳过
        // ExperienceCollection WorkItem 的原任务可能已是终态，但仍需继续 tool calling
        let is_work_item = state.work_item_id.is_some();
        let task_is_waiting = tasks.iter().any(|t| {
            t.id == state.task_id
                && matches!(
                    t.status,
                    TaskStatus::Waiting(
                        WaitingReason::ToolExecution
                            | WaitingReason::Session { .. }
                            | WaitingReason::SubTaskBatch { .. }
                    )
                )
        });
        if !task_is_waiting && !is_work_item {
            continue;
        }
        // For WorkItem tool calls, skip if there are no tool results yet even if task isn't waiting
        if is_work_item && !task_is_waiting {
            // Check if there are any collected results for this state
            let has_results = tool_results.iter().any(|r| {
                r.1.tool_call_id
                    .as_ref()
                    .is_some_and(|id| state.pending_tool_call_ids.contains(id))
            });
            if !has_results {
                continue;
            }
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
            // ExperienceCollection WorkItem 失败不应修改原任务状态
            if state.work_item_id.is_none()
                && let Some(mut task) = tasks.iter_mut().find(|t| t.id == state.task_id)
            {
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

        // 在 follow-up 中保留当前通道上下文，避免多轮 tool calling 后丢失来源信息。
        let system_prompt = tasks
            .iter()
            .find(|task| task.id == state.task_id)
            .map(|task| task.origin_channel.to_prompt_context());

        // Spawn follow-up LLM request with conversation
        let request = AgentExecutionRequest {
            task_id: state.task_id,
            agent_id: state.agent_id,
            request_kind: state.request_kind.clone(),
            prompt: String::new(),
            system_prompt,
            tools: state.tools.clone(),
            conversation: Some(state.conversation.clone()),
            work_item_id: state.work_item_id,
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

        commands.spawn((
            AgentExecutionRequestMessage { request },
            MessageDispatchedHookPending,
        ));

        // Set task back to Waiting(Agent) — 但不修改 ExperienceCollection 关联的原任务状态
        if state.work_item_id.is_none()
            && let Some(mut task) = tasks.iter_mut().find(|t| t.id == state.task_id)
            && matches!(
                task.status,
                TaskStatus::Waiting(
                    WaitingReason::ToolExecution
                        | WaitingReason::Session { .. }
                        | WaitingReason::SubTaskBatch { .. }
                )
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
