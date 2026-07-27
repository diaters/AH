//! 子任务处理 System
//!
//! 处理子任务批次的创建和完成。

use crate::prelude::*;
use tracing::{debug, warn};

use crate::{
    app::Clock,
    domain::{
        BatchTaskState, SubTaskBatchCreatedMessage, SubTaskBatchState, SubTaskCompletedMessage,
        Task, TaskStatus, ToolCallingState, WaitingReason,
    },
    ecs::EntityIndex,
};

/// 子任务批次阻塞 System
///
/// 将父 Task 阻塞等待所有子任务完成。
pub fn sub_task_batch_block_system(
    mut commands: Commands,
    clock: Res<Clock>,
    index: Res<EntityIndex>,
    messages: Query<(Entity, &SubTaskBatchCreatedMessage)>,
    mut tasks: Query<&mut Task>,
) {
    for (entity, msg) in &messages {
        if let Some(mut parent_task) = index
            .get_task(&msg.parent_task_id)
            .and_then(|e| tasks.get_mut(e).ok())
        {
            debug!(
                event = "ParentTaskBlocked",
                parent_task_id = %msg.parent_task_id,
                batch_id = %msg.batch_id,
                task_count = msg.tasks.len(),
                "parent task blocked waiting for sub-task batch completion"
            );
            parent_task.status = TaskStatus::Waiting(WaitingReason::SubTaskBatch {
                batch_id: msg.batch_id,
            });
            parent_task.updated_at = clock.0;
        }
        commands.entity(entity).despawn();
    }
}

/// 子任务完成处理 System
///
/// 更新 SubTaskBatchState，检查是否全部完成。
pub fn sub_task_completion_system(
    mut commands: Commands,
    index: Res<EntityIndex>,
    messages: Query<(Entity, &SubTaskCompletedMessage)>,
    mut tasks: Query<&mut Task>,
    mut batch_states: Query<(Entity, &mut SubTaskBatchState)>,
    calling_states: Query<&ToolCallingState>,
) {
    for (entity, msg) in &messages {
        debug!(
            event = "SubTaskCompleted",
            parent_task_id = %msg.parent_task_id,
            batch_id = %msg.batch_id,
            child_task_id = %msg.child_task_id,
            child_name = %msg.child_task_name,
            success = msg.success,
            result_summary = %msg.result_summary,
            "sub-task completed, updating batch state"
        );

        // 更新 SubTaskBatchState
        let (batch_complete, batch_entity) = if let Some((bs_entity, mut batch_state)) =
            batch_states
                .iter_mut()
                .find(|(_, bs)| bs.batch_id == msg.batch_id)
        {
            let new_state = if msg.success {
                BatchTaskState::Done
            } else {
                BatchTaskState::Failed
            };
            batch_state.update_task_state(
                &msg.child_task_name,
                new_state,
                Some(msg.result_summary.clone()),
            );
            debug!(
                event = "BatchStateUpdated",
                batch_id = %msg.batch_id,
                completed = batch_state.completed_count,
                total = batch_state.total_count,
                "batch progress updated"
            );
            (batch_state.all_done(), Some(bs_entity))
        } else {
            warn!(
                event = "SubTaskBatchStateNotFound",
                batch_id = %msg.batch_id,
                child_task_id = %msg.child_task_id,
                "SubTaskBatchState not found for completed sub-task"
            );
            commands.entity(entity).despawn();
            continue;
        };

        if batch_complete {
            debug!(
                event = "SubTaskBatchComplete",
                parent_task_id = %msg.parent_task_id,
                batch_id = %msg.batch_id,
                "all sub-tasks in batch completed, unblocking parent"
            );

            // 清理 SubTaskBatchState
            if let Some(bs_entity) = batch_entity {
                commands.entity(bs_entity).despawn();
            }

            // 恢复父 Task 状态
            if let Some(mut parent_task) = index
                .get_task(&msg.parent_task_id)
                .and_then(|e| tasks.get_mut(e).ok())
            {
                let has_calling_state =
                    calling_states.iter().any(|cs| cs.task_id == parent_task.id);
                parent_task.status = if has_calling_state {
                    TaskStatus::Waiting(WaitingReason::ToolExecution)
                } else {
                    TaskStatus::Ready
                };
                debug!(
                    event = "ParentTaskUnblocked",
                    parent_task_id = %msg.parent_task_id,
                    new_status = ?parent_task.status,
                    "parent task unblocked after batch completion"
                );
            }
        }

        commands.entity(entity).despawn();
    }
}
