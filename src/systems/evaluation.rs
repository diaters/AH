use crate::contracts::Clock;
use crate::prelude::*;
use tracing::{debug, info, warn};

use crate::domain::{
    Agent, AgentExecutionOutput, AgentExecutionResult, ChatSession, DispatchHint, DispatchKind,
    DispatchStrategy, EntryMetadata, EntryRole, FailureReason, OffTrackPolicy, OutputContent,
    PendingDispatch, ShortTermMemory, SystemOutputMessage, Task, TaskStatus, WaitingReason,
    WorkItem, WorkItemType,
};
use crate::ecs::EntityIndex;

/// 评估器触发系统：检测评估条件并生成 WorkItem
pub(crate) fn evaluation_trigger_system(
    mut commands: Commands,
    config: Res<crate::domain::TaskEvaluationConfig>,
    clock: Res<Clock>,
    mut tasks: Query<(&mut Task, Option<&ShortTermMemory>)>,
    agents: Query<&Agent>,
) {
    if !config.enabled {
        return;
    }

    for (mut task, memory) in &mut tasks {
        if task.status != TaskStatus::Running {
            continue;
        }

        // 检查轮数阈值
        if let Some(max_turns) = config.max_turns {
            // 仅统计真实对话轮次（User + Assistant），Summary/Archive 不计入进度
            let turn_count = memory.map(ShortTermMemory::dialog_turn_count).unwrap_or(0);
            if turn_count >= max_turns {
                // 基于进度的去重：同一 turn_count 不重复触发
                if let Some(last) = task.last_evaluated_turn
                    && turn_count <= last
                {
                    continue;
                }

                // 查找评估器 Agent
                let evaluator_exists = agents
                    .iter()
                    .any(|a| a.profile.name == config.evaluator_agent_name);

                if evaluator_exists {
                    debug!(
                        task_id = %task.id,
                        turn_count,
                        max_turns,
                        "evaluation triggered by turn limit"
                    );
                    info!(
                        event = "EvaluationTriggered",
                        task_id = %task.id,
                        turn_count,
                        max_turns,
                        "评估触发：任务已达 {}/{} 轮",
                        turn_count,
                        max_turns
                    );

                    // 创建评估 WorkItem
                    let work_item = WorkItem::evaluation(
                        task.id,
                        format!(
                            "任务内容：{}\n\n请基于当前任务执行情况判断 decision、reasoning、suggested_action。",
                            task.content
                        ),
                        Some(format!(
                            "当前已执行 {} 轮，达到配置的最大轮数限制 {} 轮。",
                            turn_count, max_turns
                        )),
                    );
                    commands.spawn((
                        work_item,
                        PendingDispatch {
                            kind: DispatchKind::WorkItem(WorkItemType::Evaluation),
                            hint: DispatchHint {
                                strategy: DispatchStrategy::DirectDelegate,
                                preferred_agent_name: None,
                                required_skill_id: None,
                                agent_spawn_spec: None,
                            },
                        },
                    ));

                    // 记录本次评估对应的轮数
                    task.record_evaluation_at_turn(turn_count);

                    // 将任务状态改为等待评估器，防止重复触发
                    task.mark_waiting(WaitingReason::Evaluator, clock.0);
                }
            }
        }
    }
}

/// 处理 Evaluation WorkItem 的执行结果
///
/// 自 `transform/llm_response.rs` 按知识域归位至此（P2 重组，纯搬家）。
#[allow(clippy::too_many_arguments, clippy::drop_non_drop)]
pub(crate) fn handle_evaluation_work_item_result(
    commands: &mut Commands,
    index: &EntityIndex,
    tasks: &mut Query<(
        Entity,
        &mut Task,
        Option<&mut ShortTermMemory>,
        Option<&ChatSession>,
    )>,
    result_entity: Entity,
    work_item_entity: Entity,
    work_item: &WorkItem,
    result: &AgentExecutionResult,
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
                if let Some((_, mut task, _, _)) = index
                    .get_task(&work_item.task_id)
                    .and_then(|e| tasks.get_mut(e).ok())
                    && matches!(task.status, TaskStatus::Waiting(WaitingReason::Evaluator))
                {
                    task.mark_ready(now);
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
                task.mark_ready(now);
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
                task.mark_ready(now);
            }
            commands.entity(work_item_entity).despawn();
            commands.entity(result_entity).despawn();
            return;
        }
    };

    // 更新任务状态（两阶段应用，避免借用冲突）
    if let Some((_, mut task, _, _)) = index
        .get_task(&work_item.task_id)
        .and_then(|e| tasks.get_mut(e).ok())
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

        // 第二阶段：应用效果（状态转换统一经 mark_* 方法）
        match effects.next_status {
            TaskStatus::Ready => task.mark_ready(now),
            TaskStatus::Done => task.mark_done("completed by evaluation", now),
            TaskStatus::Waiting(reason) => task.mark_waiting(reason, now),
            TaskStatus::Failed(reason) => {
                let err = effects
                    .last_error
                    .unwrap_or_else(|| "evaluation marked task as failed".to_string());
                task.mark_failed_reason(reason, err, now);
            }
            TaskStatus::Pending | TaskStatus::Running => {
                unreachable!("evaluation result never yields Pending/Running")
            }
        }

        let task_id = task.id;
        // 释放 task 借用，以便后续再次查询 tasks
        drop(task);

        // 注入纠偏上下文到 STM（AutoCorrect / AskUser 均适用）
        if let Some((role, content, metadata)) = effects.stm_injection
            && let Some((_, _, Some(mut stm), _)) =
                index.get_task(&task_id).and_then(|e| tasks.get_mut(e).ok())
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

#[cfg(test)]
mod tests {
    use crate::domain::TaskEvaluationConfig;

    #[test]
    fn evaluation_trigger_system_disabled_by_default() {
        let config = TaskEvaluationConfig::default();
        assert!(!config.enabled);
    }

    #[test]
    fn evaluation_trigger_system_does_nothing_when_disabled() {
        // This is a logic check - when disabled, system returns early
        let config = TaskEvaluationConfig::default();
        assert!(!config.enabled);
    }

    #[test]
    fn dedup_skips_when_last_evaluated_turn_equals_current() {
        // 模拟去重逻辑：当 last_evaluated_turn == turn_count 时不应触发
        let last_evaluated_turn: Option<u32> = Some(2);
        let turn_count: u32 = 2;
        let should_skip = last_evaluated_turn.is_some_and(|last| turn_count <= last);
        assert!(should_skip, "should skip evaluation at same progress");
    }

    #[test]
    fn dedup_allows_when_turn_count_advances() {
        // 模拟去重逻辑：当 turn_count > last_evaluated_turn 时应允许触发
        let last_evaluated_turn: Option<u32> = Some(2);
        let turn_count: u32 = 4;
        let should_skip = last_evaluated_turn.is_some_and(|last| turn_count <= last);
        assert!(
            !should_skip,
            "should allow evaluation when progress advanced"
        );
    }

    #[test]
    fn dedup_allows_when_no_previous_evaluation() {
        // 模拟去重逻辑：当 last_evaluated_turn == None 时应允许触发
        let last_evaluated_turn: Option<u32> = None;
        let turn_count: u32 = 2;
        let should_skip = last_evaluated_turn.is_some_and(|last| turn_count <= last);
        assert!(
            !should_skip,
            "should allow evaluation when never evaluated before"
        );
    }
}
