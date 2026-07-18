//! 摘要系统
//!
//! 处理记忆压缩的摘要请求和结果处理。

use crate::prelude::*;
use tracing::{debug, info};

use crate::{
    app::Clock,
    domain::{
        DispatchHint, DispatchKind, DispatchStrategy, PendingDispatch, SummarizationRequestMessage,
        SummarizationTrigger, Task, TaskStatus, WaitingReason, WorkItem, WorkItemType,
    },
};

/// 摘要调度系统：将摘要请求转为 WorkItem
///
/// 仅负责将 SummarizationRequestMessage 转换为 WorkItem，
/// Agent 选择由 `dispatch_system` 统一处理（WorkItem 派发路径）。
pub(crate) fn summarization_dispatch_system(
    clock: Res<Clock>,
    mut commands: Commands,
    requests: Query<(Entity, &SummarizationRequestMessage)>,
    mut tasks: Query<&mut Task>,
) {
    for (entity, request) in &requests {
        // 对于非 TaskComplete 触发的摘要，标记任务为等待摘要
        // TaskComplete 触发的摘要不需要改变任务状态（任务已是终态）
        if request.trigger != SummarizationTrigger::TaskComplete
            && let Some(mut task) = tasks.iter_mut().find(|t| t.id == request.task_id)
            && !task.status.is_terminal()
        {
            let old_status = task.status.clone();
            task.status = TaskStatus::Waiting(WaitingReason::Summarization);
            task.updated_at = clock.0;
            debug!(
                event = "TaskWaitingForSummarization",
                task_id = %task.id,
                from_status = ?old_status,
                to_status = ?task.status,
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

#[cfg(test)]
mod tests {
    use crate::domain::{SummarizationTrigger, WorkItem, WorkItemType};

    #[test]
    fn summarization_workitem_preserves_trigger() {
        let task_id = uuid::Uuid::nil();
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
