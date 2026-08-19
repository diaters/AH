//! 摘要系统
//!
//! 处理记忆压缩的摘要请求和结果处理。

use crate::prelude::*;
use tracing::{debug, info, warn};

use crate::{
    contracts::Clock,
    domain::{
        AgentExecutionOutput, AgentExecutionResult, ChatSession, DispatchHint, DispatchKind,
        DispatchStrategy, MemoryConfig, OutputContent, PendingDispatch, ShortTermMemory,
        SummarizationRequestMessage, SummarizationTrigger, SystemOutputMessage, Task, TaskStatus,
        WaitingReason, WorkItem, WorkItemType,
    },
    ecs::EntityIndex,
};

/// 摘要调度系统：将摘要请求转为 WorkItem
///
/// 仅负责将 SummarizationRequestMessage 转换为 WorkItem，
/// Agent 选择由 `dispatch_system` 统一处理（WorkItem 派发路径）。
pub(crate) fn summarization_dispatch_system(
    clock: Res<Clock>,
    mut commands: Commands,
    index: Res<EntityIndex>,
    requests: Query<(Entity, &SummarizationRequestMessage)>,
    mut tasks: Query<&mut Task>,
) {
    for (entity, request) in &requests {
        // 对于非 TaskComplete 触发的摘要，标记任务为等待摘要
        // TaskComplete 触发的摘要不需要改变任务状态（任务已是终态）
        // UUID 寻址改用 EntityIndex O(1) 解析
        if request.trigger != SummarizationTrigger::TaskComplete
            && let Some(mut task) = index
                .get_task(&request.task_id)
                .and_then(|e| tasks.get_mut(e).ok())
            && !task.status.is_terminal()
        {
            task.mark_waiting(WaitingReason::Summarization, clock.0);
            debug!(
                event = "TaskWaitingForSummarization",
                task_id = %task.id,
                trigger = ?request.trigger,
                "task waiting for summarization"
            );
        }

        // 创建 Summarization WorkItem
        let work_item = WorkItem::summarization(
            request.task_id,
            request.content_to_summarize.clone(),
            request.target_tokens as usize,
            request.trigger,
        );
        commands.spawn((
            work_item,
            PendingDispatch {
                kind: DispatchKind::WorkItem(WorkItemType::Summarization),
                hint: DispatchHint {
                    strategy: DispatchStrategy::DirectDelegate,
                    preferred_agent_name: None,
                    required_skill_id: None,
                    agent_spawn_spec: None,
                },
            },
        ));

        debug!(
            event = "SummarizationWorkItemCreated",
            task_id = %request.task_id,
            trigger = ?request.trigger,
            target_tokens = request.target_tokens,
            content_len = request.content_to_summarize.len(),
            "summarization work item created"
        );
        info!(
            event = "SummarizationRequested",
            task_id = %request.task_id,
            trigger = ?request.trigger,
            target_tokens = request.target_tokens,
            "摘要请求：{:?}，目标 tokens {}",
            request.trigger,
            request.target_tokens
        );
        commands.entity(entity).despawn();
    }
}

/// 处理 Summarization WorkItem 的执行结果
///
/// 自 `transform/llm_response.rs` 按知识域归位至此（P2 重组，纯搬家）。
/// 配对组选择逻辑的单一权威出处在 `domain::memory`
/// （`split_into_groups` / `compressible_entry_count` / `drain_compressed_groups`），
/// 与触发端 `memory_compression_system` 共用。
#[allow(clippy::too_many_arguments)]
pub(crate) fn handle_summarization_work_item_result(
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

                    // 移除已压缩的 entries：与触发端 memory_compression_system
                    // 共用配对组选择逻辑（见 domain::memory 的
                    // split_into_groups / compressible_entry_count），
                    // 保证压缩循环每轮必有进展、必然收敛
                    let removed = memory.drain_compressed_groups(config.preserve_recent_turns);

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
            if let Some((_, mut task, _, _)) =
                index.get_task(&task_id).and_then(|e| tasks.get_mut(e).ok())
                && matches!(
                    task.status,
                    TaskStatus::Waiting(WaitingReason::Summarization)
                )
            {
                task.mark_waiting(WaitingReason::User, now);
                debug!(
                    event = "TaskStatusRestoredAfterSummarization",
                    task_id = %task.id,
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
            if let Some((_, mut task, _, _)) =
                index.get_task(&task_id).and_then(|e| tasks.get_mut(e).ok())
                && matches!(
                    task.status,
                    TaskStatus::Waiting(WaitingReason::Summarization)
                )
            {
                task.mark_waiting(WaitingReason::User, now);
                debug!(
                    event = "TaskStatusRestoredAfterSummarizationFailed",
                    task_id = %task.id,
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
            if let Some((_, mut task, _, _)) =
                index.get_task(&task_id).and_then(|e| tasks.get_mut(e).ok())
                && matches!(
                    task.status,
                    TaskStatus::Waiting(WaitingReason::Summarization)
                )
            {
                task.mark_waiting(WaitingReason::User, now);
                debug!(
                    event = "TaskStatusRestoredAfterSummarizationFailed",
                    task_id = %task.id,
                    "task restored to waiting for user after summarization failed"
                );
            }
        }
    }

    // 清理 WorkItem 和结果消息
    commands.entity(work_item_entity).despawn();
    commands.entity(result_entity).despawn();
}

#[cfg(test)]
mod tests {
    use crate::domain::{SummarizationTrigger, WorkItem, WorkItemType};

    #[test]
    fn summarization_workitem_preserves_trigger() {
        let task_id = crate::domain::TaskId::nil();
        let work_item = WorkItem::summarization(
            task_id,
            "content".to_string(),
            100,
            SummarizationTrigger::TaskComplete,
        );
        assert_eq!(work_item.work_type, WorkItemType::Summarization);
        assert_eq!(work_item.task_id, task_id);
    }
}
