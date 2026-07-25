//! LLM 响应处理 System
//!
//! 处理 LLM 响应和 Tool 调用循环。

use crate::prelude::*;
use tracing::{debug, trace, warn};

use crate::{
    app::{Clock, HarnessSettings, MemoryConfig},
    domain::{
        AgentExecutionOutput, AgentExecutionRequest, AgentExecutionRequestMessage,
        AgentExecutionResult, AgentExecutionResultMessage, AgentId, AgentRequestKind,
        ChatRoundReadyMessage, ChatSession, ConversationMessage, EntryMetadata, EntryRole,
        ExperienceCollectionCompletedMessage, ExperienceStore, FailureReason,
        MessageDispatchedHookPending, OffTrackPolicy, OutputContent, ProfileGenerationContext,
        ShortTermMemory, SystemOutputMessage, Task, TaskId, TaskStatus, ToolCalledHookPending,
        ToolCallingState, ToolDefinition, ToolExecutionRequestMessage, ToolExecutionResultMessage,
        ToolReturnedHookPending, UserOutputMessage, WaitingReason, WorkItem,
        WorkItemLifecycleHookPending, WorkItemType,
    },
    user_plugins::hook_point::HookPoint,
};

/// 绝对硬上限倍数：iteration 超过此值 × max_iterations 时强制失败任务
const HARD_LIMIT_MULTIPLIER: u32 = 2;

/// `ToolCallingState` 的快照，用于在 `llm_response_system` 内避开 Bevy 借用冲突。
///
/// 字段语义与 `ToolCallingState` 一致；`work_item_id` 区分 Task 级（None）与
/// WorkItem 级（Some）调用循环，是 `(task_id, work_item_id)` 索引键的一部分。
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

/// 在 `state_info` 中按 `(task_id, work_item_id)` 严格匹配查找 ToolCallingState。
///
/// 设计原则：WorkItem 是执行单位，Task 是组织单位。同一 Task 下不同 WorkItem
/// 的工具循环互不复用 State。这避免了"collector 残留 State 被 skill-updater
/// 误复用"类的循环 bug。
///
/// `work_item_id = None` 表示 Task 级调用，仅匹配 `work_item_id = None` 的 State；
/// `work_item_id = Some(x)` 表示 WorkItem 级调用，仅匹配 `work_item_id = Some(x)`
/// 的 State。跨 Task / 跨 WorkItem 都不会误匹配。
fn find_calling_state(
    state_info: &[CallingStateInfo],
    task_id: crate::domain::TaskId,
    work_item_id: Option<uuid::Uuid>,
) -> Option<&CallingStateInfo> {
    state_info
        .iter()
        .find(|i| i.task_id == task_id && i.work_item_id == work_item_id)
}

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

/// 处理 ProfileGeneration WorkItem LLM 响应无效的情况。
///
/// 触发条件（均为 LLM 异常，占用 exception_count）：
/// - 返回普通文本（未调用工具）
/// - 同时调用 submit_profile_update 和 skip_profile_update（互斥冲突）
/// - 调用非 submit/skip 的其他工具（违规）
/// - LLM 调用报错（Err）
///
/// 行为：
/// - exception_count + 1 < MAX_PROFILE_EXCEPTIONS：spawn 重试请求，feedback 注入系统提示
/// - exception_count + 1 >= MAX_PROFILE_EXCEPTIONS：spawn 失败完成消息，
///   由 completion_system 根据 exception_count 判断走失败路径
///
/// `ctx`：通过 Query 从 WorkItem Entity 读取的 `ProfileGenerationContext` Component 引用。
///   若为 None，表示 context 已被前序完成消息消费（如 skip/submit 已完成），直接清理 WorkItem。
fn handle_profile_generation_invalid(
    commands: &mut Commands,
    experience_store: &ExperienceStore,
    ctx: Option<&ProfileGenerationContext>,
    work_item: &WorkItem,
    result_entity: Entity,
    work_item_entity: Entity,
    invalid_reason: &str,
) {
    use crate::domain::{
        ExperienceCandidateStatus, MAX_PROFILE_EXCEPTIONS, ProfileGenerationCompletedMessage,
        ProfileGenerationRequestMessage,
    };

    let Some(ctx) = ctx else {
        // context 已被前序 ProfileGenerationCompletedMessage 消费
        // （例如 skip_profile_update 或 submit_profile_update 已完成流程），
        // 无需再次 spawn 完成消息，直接清理 WorkItem
        warn!(
            event = "ProfileGenerationContextMissing",
            task_id = %work_item.task_id,
            "profile generation context already consumed, cleaning up work item without spawning completion message"
        );
        commands
            .entity(work_item_entity)
            .insert(WorkItemLifecycleHookPending(HookPoint::OnWorkItemFailed));
        commands.entity(work_item_entity).despawn();
        commands.entity(result_entity).despawn();
        return;
    };

    let new_exception_count = ctx.exception_count + 1;
    let kind = ctx.kind.clone();
    let existing_profile = ctx.existing_profile.clone();
    let governing_agent_id = work_item.governing_agent_id.unwrap_or(uuid::Uuid::nil());

    if new_exception_count < MAX_PROFILE_EXCEPTIONS {
        // 未达上限：收集候选 ID 并 spawn 重试请求
        let candidate_ids: Vec<uuid::Uuid> = experience_store
            .candidates
            .values()
            .filter(|c| {
                c.producer_task_id == work_item.task_id
                    && c.status == ExperienceCandidateStatus::ProfileGenerationPending
            })
            .map(|c| c.candidate_id)
            .collect();

        let system_notice = format!(
            "你上一轮未正确调用 submit_profile_update 或 skip_profile_update 工具（{}）。\
             本轮必须调用其中一个工具提交结果。两个工具不能同时调用。",
            invalid_reason
        );
        commands.spawn(ProfileGenerationRequestMessage {
            task_id: work_item.task_id,
            agent_id: governing_agent_id,
            candidate_ids,
            existing_profile,
            kind,
            feedback: Some(system_notice),
            exception_count: new_exception_count,
        });
        debug!(
            event = "ProfileGenerationRetryRequested",
            task_id = %work_item.task_id,
            exception_count = new_exception_count,
            reason = invalid_reason,
            "spawning profile generation retry due to invalid LLM response"
        );
    } else {
        // 达到上限：spawn 失败完成消息（generated_profile: None），
        // completion_system 会根据 exception_count >= MAX 判断走失败路径
        commands.spawn(ProfileGenerationCompletedMessage {
            task_id: work_item.task_id,
            agent_id: governing_agent_id,
            generated_profile: None,
            kind,
        });
        warn!(
            event = "ProfileGenerationMaxExceptionsReached",
            task_id = %work_item.task_id,
            exception_count = new_exception_count,
            reason = invalid_reason,
            "profile generation failed due to max exceptions reached"
        );
    }

    // 标记 WorkItem 失败并 despawn
    commands
        .entity(work_item_entity)
        .insert(WorkItemLifecycleHookPending(HookPoint::OnWorkItemFailed));
    commands.entity(work_item_entity).despawn();
    commands.entity(result_entity).despawn();
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

/// 生成合成 ToolExecutionResultMessage，用于向 LLM 传达工具预算已耗尽
///
/// 合成结果跳过 ToolCalledHookPending（不 spawn），因此不会触发 on_tool_called hook。
/// 合成结果附加 ToolReturnedHookPending，会进入 on_tool_returned hook 流水线。
fn spawn_synthetic_limit_result(
    commands: &mut Commands,
    task_id: TaskId,
    agent_id: AgentId,
    tool_call_id: &str,
    tool_name: &str,
    iteration: u32,
    max_iterations: u32,
) {
    let tool_output = Ok(serde_json::json!({
        "exit_code": 1,
        "status": "tool_budget_exhausted",
        "output": format!(
            "[TOOL_BUDGET_EXHAUSTED] 本轮工具调用次数已达上限 ({}/{})。请总结你目前取得的进展，并向用户说明下一步需要什么，等待用户决策是否继续。",
            iteration, max_iterations
        )
    }));

    let result = AgentExecutionResult {
        task_id,
        agent_id,
        request_kind: AgentRequestKind::ToolExecution {
            tool_name: tool_name.to_string(),
        },
        result: Ok(AgentExecutionOutput {
            content: OutputContent::Text(String::new()),
            reasoning_content: None,
        }),
        prompt: String::new(),
        system_prompt: None,
        tools: vec![],
        reasoning_content: None,
        work_item_id: None,
    };

    commands.spawn((
        ToolExecutionResultMessage {
            result,
            tool_name: tool_name.to_string(),
            tool_output,
            tool_call_id: Some(tool_call_id.to_string()),
            processed: false,
            original_tool_output: None,
        },
        ToolReturnedHookPending,
    ));
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
    profile_contexts: Query<&ProfileGenerationContext>,
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
                            &mut tasks,
                            entity,
                            work_item_entity,
                            &work_item,
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
                                            WorkItemLifecycleHookPending(
                                                HookPoint::OnWorkItemFailed,
                                            ),
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
                    WorkItemType::ProfileGeneration => {
                        // 通过 Query 从 WorkItem Entity 读取 ProfileGenerationContext Component
                        let pg_ctx = profile_contexts.get(work_item_entity).ok();
                        match &result.result {
                            Ok(AgentExecutionOutput {
                                content: OutputContent::ToolCalls(calls),
                                ..
                            }) => {
                                // 互斥检测 + 非相关工具检测
                                let has_submit =
                                    calls.iter().any(|c| c.name == "submit_profile_update");
                                let has_skip =
                                    calls.iter().any(|c| c.name == "skip_profile_update");
                                let has_other = calls.iter().any(|c| {
                                    c.name != "submit_profile_update"
                                        && c.name != "skip_profile_update"
                                });

                                if has_submit && has_skip {
                                    // 互斥冲突：两个工具同时调用
                                    handle_profile_generation_invalid(
                                        &mut commands,
                                        &experience_store,
                                        pg_ctx,
                                        &work_item,
                                        entity,
                                        work_item_entity,
                                        "tool_conflict",
                                    );
                                    continue;
                                }
                                if has_other || (!has_submit && !has_skip) {
                                    // 调用非相关工具，或未调用任何相关工具
                                    handle_profile_generation_invalid(
                                        &mut commands,
                                        &experience_store,
                                        pg_ctx,
                                        &work_item,
                                        entity,
                                        work_item_entity,
                                        "non_relevant_tool",
                                    );
                                    continue;
                                }
                                // 单一工具调用（submit 或 skip）：放行给 orchestrator 处理
                            }
                            Ok(_) => {
                                // LLM 返回普通文本（未调用工具）：异常
                                handle_profile_generation_invalid(
                                    &mut commands,
                                    &experience_store,
                                    pg_ctx,
                                    &work_item,
                                    entity,
                                    work_item_entity,
                                    "no_tool_call",
                                );
                                continue;
                            }
                            Err(_) => {
                                // LLM 调用报错：异常
                                handle_profile_generation_invalid(
                                    &mut commands,
                                    &experience_store,
                                    pg_ctx,
                                    &work_item,
                                    entity,
                                    work_item_entity,
                                    "llm_error",
                                );
                                continue;
                            }
                        }
                    }
                    WorkItemType::SkillUpdate => {
                        // - ToolCalls（submit_skill_update）：fall through，让 tool calling loop 处理，
                        //   orchestrator 会 spawn SkillUpdateCompletedMessage，由
                        //   skill_update_completion_system 消费并 despawn WorkItem。
                        // - text / error：LLM 未调用工具，无 operations 可应用；
                        //   直接 despawn WorkItem + SkillUpdateContext entity + result entity，
                        //   候选状态保持 GovernanceResolved，由后续治理重新评估。
                        match &result.result {
                            Ok(AgentExecutionOutput {
                                content: OutputContent::ToolCalls(_),
                                ..
                            }) => {
                                // 不 continue，让下面的 tool calling loop 处理 tool calls
                            }
                            Ok(_) => {
                                warn!(
                                    event = "SkillUpdateWorkItemNoToolCall",
                                    work_item_id = %work_item.id,
                                    task_id = %work_item.task_id,
                                    error = "LLM returned text without calling submit_skill_update",
                                    error_type = "NoToolCall",
                                    "skill update LLM finished without tool call, cleaning up work item"
                                );
                                commands.entity(work_item_entity).insert(
                                    WorkItemLifecycleHookPending(HookPoint::OnWorkItemFailed),
                                );
                                commands.entity(work_item_entity).despawn();
                                commands.entity(entity).despawn();
                                continue;
                            }
                            Err(_) => {
                                warn!(
                                    event = "SkillUpdateWorkItemLlmFailed",
                                    work_item_id = %work_item.id,
                                    task_id = %work_item.task_id,
                                    error = "LLM execution returned Err",
                                    error_type = "LlmExecutionFailed",
                                    "skill update LLM failed, cleaning up work item"
                                );
                                commands.entity(work_item_entity).insert(
                                    WorkItemLifecycleHookPending(HookPoint::OnWorkItemFailed),
                                );
                                commands.entity(work_item_entity).despawn();
                                commands.entity(entity).despawn();
                                continue;
                            }
                        }
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
                    // Check for existing ToolCallingState (follow-up iteration).
                    // 按 (task_id, work_item_id) 严格匹配：避免误复用同 Task 下
                    // 其他 WorkItem 残留的 State（如 collector 残留被 skill-updater 误用）。
                    let existing = find_calling_state(&state_info, task.id, result.work_item_id);

                    if let Some(info) = existing {
                        let new_iteration = info.iteration + 1;
                        // Despawn old state and create updated one
                        let mut new_conversation = info.conversation.clone();
                        new_conversation.push(ConversationMessage::Assistant {
                            content: None,
                            tool_calls: calls.clone(),
                            reasoning_content: reasoning_content.clone(),
                        });

                        if new_iteration > info.max_iterations {
                            // 绝对硬上限：任何情况下都强制失败，防止无限循环
                            if new_iteration > HARD_LIMIT_MULTIPLIER * info.max_iterations {
                                warn!(
                                    event = "ToolCallingHardLimitExceeded",
                                    task_id = %task.id,
                                    iteration = new_iteration,
                                    max_iterations = info.max_iterations,
                                    "tool calling exceeded absolute hard limit"
                                );
                                if info.work_item_id.is_none() {
                                    task.last_error = Some(format!(
                                        "tool calling exceeded absolute hard limit ({}/{})",
                                        new_iteration, info.max_iterations
                                    ));
                                    task.status = TaskStatus::Failed(FailureReason::AgentError);
                                    task.updated_at = clock.0;
                                }
                                commands.entity(info.entity).despawn();
                                break;
                            }

                            if info.work_item_id.is_some() {
                                // WorkItem 保持硬失败语义，但不修改原任务状态
                                warn!(
                                    event = "ToolCallingLimitExceeded",
                                    task_id = %task.id,
                                    work_item_id = ?info.work_item_id,
                                    iteration = new_iteration,
                                    max_iterations = info.max_iterations,
                                    "work item tool calling exceeded max iterations"
                                );
                                commands.entity(info.entity).despawn();
                                break;
                            }

                            // 普通任务：生成合成 tool result
                            debug!(
                                event = "ToolBudgetExhausted",
                                task_id = %task.id,
                                iteration = new_iteration,
                                max_iterations = info.max_iterations,
                                "tool budget exhausted, returning synthetic result"
                            );

                            for call in calls {
                                spawn_synthetic_limit_result(
                                    &mut commands,
                                    task.id,
                                    result.agent_id,
                                    &call.id,
                                    &call.name,
                                    info.iteration,
                                    info.max_iterations,
                                );
                            }

                            // 更新 ToolCallingState，记录这些 tool_call_id 正在等待合成结果
                            let pending_ids: Vec<String> =
                                calls.iter().map(|c| c.id.clone()).collect();
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

                            // 不生成真实 ToolExecutionRequestMessage，避免真实工具执行
                            // 将任务设为 Waiting(ToolExecution)，使 tool_calling_orchestrator_system
                            // 入口检查允许此状态继续处理合成结果
                            if info.work_item_id.is_none() && !task.status.is_terminal() {
                                task.status = TaskStatus::Waiting(WaitingReason::ToolExecution);
                                task.updated_at = clock.0;
                            }
                            continue;
                        }

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
                    // 反查 WorkItem entity：用于将 SkillUpdateCompletedMessage 等"工具产物"
                    // 直接 insert 到 WorkItem entity 上（替代用 work_item_id 反查）。
                    let work_item_entity: Option<Entity> = result.work_item_id.and_then(|wid| {
                        work_items
                            .iter()
                            .find(|(_, wi)| wi.id == wid)
                            .map(|(e, _)| e)
                    });
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
                                    work_item_id: result.work_item_id,
                                    model_override: None,
                                },
                                tool_name: call.name.clone(),
                                tool_input,
                                pending_confirmation_id: None,
                                tool_call_id: Some(call.id.clone()),
                                pending_confirmation_options: None,
                                work_item_entity,
                                confirmed_once: false,
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
            // 绝对硬上限
            if state.iteration > HARD_LIMIT_MULTIPLIER * state.max_iterations {
                warn!(
                    event = "ToolCallingHardLimitExceeded",
                    task_id = %state.task_id,
                    iteration = state.iteration,
                    max_iterations = state.max_iterations,
                    "tool calling exceeded absolute hard limit on result collection"
                );
                // WorkItem 不修改原任务状态
                if state.work_item_id.is_none()
                    && let Some(mut task) = tasks.iter_mut().find(|t| t.id == state.task_id)
                {
                    task.last_error = Some(format!(
                        "tool calling exceeded absolute hard limit ({}/{})",
                        state.iteration, state.max_iterations
                    ));
                    task.status = TaskStatus::Failed(FailureReason::AgentError);
                    task.updated_at = clock.0;
                }
                commands.entity(state_entity).despawn();
                continue;
            }

            if state.work_item_id.is_some() {
                // WorkItem 保持现有隔离语义：不修改原任务状态，停止 follow-up
                warn!(
                    event = "ToolCallingLimitExceeded",
                    task_id = %state.task_id,
                    work_item_id = ?state.work_item_id,
                    iteration = state.iteration,
                    max_iterations = state.max_iterations,
                    "work item tool calling reached max iterations"
                );
                commands.entity(state_entity).despawn();
                continue;
            } else {
                // 普通任务：允许 LLM 再响应一次，用于总结和询问用户
                // 任务当前处于 Waiting(ToolExecution) 状态，
                // tool_calling_orchestrator_system 入口检查允许此状态继续处理
                debug!(
                    event = "ToolBudgetExhaustedAllowingSummary",
                    task_id = %state.task_id,
                    iteration = state.iteration,
                    max_iterations = state.max_iterations,
                    "tool budget exhausted, allowing LLM to summarize"
                );
            }
        }

        // 在 follow-up 中保留当前通道上下文，避免多轮 tool calling 后丢失来源信息。
        let system_prompt = tasks
            .iter()
            .find(|task| task.id == state.task_id)
            .and_then(|task| task.origin_channel.as_ref())
            .map(|ch| ch.to_prompt_context());

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
            model_override: None,
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

    /// 构造一个最小 CallingStateInfo 用于查找测试。
    fn make_state_info(
        task_id: crate::domain::TaskId,
        work_item_id: Option<uuid::Uuid>,
    ) -> CallingStateInfo {
        CallingStateInfo {
            entity: Entity::PLACEHOLDER,
            task_id,
            iteration: 1,
            max_iterations: 10,
            conversation: vec![],
            tools: vec![],
            request_kind: AgentRequestKind::LlmCompletion,
            work_item_id,
        }
    }

    /// 同 Task 下不同 WorkItem 的 ToolCallingState 不应互相复用。
    /// 这是本次 bug 修复的核心：skill-updater 不应误用 collector 残留的 State。
    #[test]
    fn find_calling_state_strict_matches_work_item_id() {
        let task_id = uuid::Uuid::new_v4();
        let collector_wi = uuid::Uuid::new_v4();
        let skill_updater_wi = uuid::Uuid::new_v4();

        // 模拟 collector 残留 State（work_item_id = collector_wi）
        // 与 skill-updater 当前响应（work_item_id = skill_updater_wi）
        let state_info = vec![
            make_state_info(task_id, Some(collector_wi)),
            make_state_info(task_id, Some(skill_updater_wi)),
        ];

        // 查找 skill-updater 的 State：必须严格匹配 work_item_id
        let found = find_calling_state(&state_info, task_id, Some(skill_updater_wi));
        assert!(
            found.is_some(),
            "should find state for matching work_item_id"
        );
        assert_eq!(
            found.unwrap().work_item_id,
            Some(skill_updater_wi),
            "must not reuse state from a different work item"
        );

        // 查找 collector 的 State：仍能找到
        let found_collector = find_calling_state(&state_info, task_id, Some(collector_wi));
        assert!(found_collector.is_some());
        assert_eq!(found_collector.unwrap().work_item_id, Some(collector_wi));

        // 查找不存在的 work_item_id：返回 None
        let other_wi = uuid::Uuid::new_v4();
        let found_other = find_calling_state(&state_info, task_id, Some(other_wi));
        assert!(
            found_other.is_none(),
            "should not return state for non-matching work_item_id"
        );
    }

    /// Task 级调用（work_item_id = None）应能找到 Task 级 State（work_item_id = None），
    /// 但不应误匹配 WorkItem 级 State。
    #[test]
    fn find_calling_state_task_level_excludes_work_item_states() {
        let task_id = uuid::Uuid::new_v4();
        let work_item_id = uuid::Uuid::new_v4();

        // 混合：一个 WorkItem 级 State + 一个 Task 级 State
        let state_info = vec![
            make_state_info(task_id, Some(work_item_id)),
            make_state_info(task_id, None),
        ];

        // Task 级查找：返回 Task 级 State，不返回 WorkItem 级
        let found = find_calling_state(&state_info, task_id, None);
        assert!(found.is_some(), "should find task-level state");
        assert_eq!(
            found.unwrap().work_item_id,
            None,
            "must not return work-item state when looking for task-level state"
        );
    }

    /// 只存在 WorkItem 级 State 时，Task 级查找不应误匹配。
    /// 这防止"Task 级调用复用 WorkItem 残留 State"的反向 bug。
    #[test]
    fn find_calling_state_task_level_returns_none_if_only_work_item_states_exist() {
        let task_id = uuid::Uuid::new_v4();
        let work_item_id = uuid::Uuid::new_v4();

        let state_info = vec![make_state_info(task_id, Some(work_item_id))];

        let found = find_calling_state(&state_info, task_id, None);
        assert!(
            found.is_none(),
            "must not return work-item state for task-level lookup"
        );
    }
}
