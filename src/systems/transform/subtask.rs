//! 子任务处理 System
//!
//! 处理子任务批次的创建和完成。

use crate::prelude::*;
use tracing::{debug, warn};

use crate::{
    contracts::Clock,
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
            parent_task.mark_waiting(
                WaitingReason::SubTaskBatch {
                    batch_id: msg.batch_id,
                },
                clock.0,
            );
        }
        commands.entity(entity).despawn();
    }
}

/// 子任务完成处理 System
///
/// 更新 SubTaskBatchState，检查是否全部完成。
pub fn sub_task_completion_system(
    mut commands: Commands,
    clock: Res<Clock>,
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

            // 恢复父 Task 状态：仅当父任务仍阻塞在本批次上时解除阻塞。
            // 父任务已继续执行、空闲（Waiting(User)）或等待后台任务时保持原状态，
            // 批次结果经 wait_tasks 或后台任务通知回流。
            if let Some(mut parent_task) = index
                .get_task(&msg.parent_task_id)
                .and_then(|e| tasks.get_mut(e).ok())
                .filter(|t| {
                    t.status
                        == TaskStatus::Waiting(WaitingReason::SubTaskBatch {
                            batch_id: msg.batch_id,
                        })
                })
            {
                let has_calling_state =
                    calling_states.iter().any(|cs| cs.task_id == parent_task.id);
                if has_calling_state {
                    parent_task.mark_waiting(WaitingReason::ToolExecution, clock.0);
                } else {
                    parent_task.mark_ready(clock.0);
                }
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::domain::{BatchTaskStatus, ChannelId, FrontendKind, TaskId};

    fn run_batch_completion(parent_status: TaskStatus) -> TaskStatus {
        let mut app = App::new();
        app.insert_resource(Clock::default());
        app.insert_resource(EntityIndex::default());
        app.add_systems(Update, sub_task_completion_system);

        let channel = ChannelId {
            frontend: FrontendKind::Tui,
            user_id: "local".to_string(),
            thread_id: None,
        };
        let mut parent = Task::from_user_input("parent", 3, channel);
        match parent_status {
            TaskStatus::Waiting(reason) => parent.mark_waiting(reason, chrono::Utc::now()),
            other => panic!("unsupported {other:?}"),
        }
        let parent_id = parent.id;
        let parent_entity = app.world_mut().spawn(parent).id();
        app.world_mut()
            .resource_mut::<EntityIndex>()
            .tasks
            .insert(parent_id, parent_entity);

        let batch_id = uuid::Uuid::from_u128(7);
        let child_id = TaskId::new();
        app.world_mut().spawn(SubTaskBatchState {
            batch_id,
            parent_tool_call_id: "call".to_string(),
            tasks: HashMap::from([(
                "child".to_string(),
                BatchTaskStatus {
                    task_id: child_id,
                    state: BatchTaskState::Pending,
                    result_summary: None,
                },
            )]),
            completed_count: 0,
            total_count: 1,
        });
        app.world_mut().spawn(SubTaskCompletedMessage {
            parent_task_id: parent_id,
            batch_id,
            child_task_id: child_id,
            child_task_name: "child".to_string(),
            result_summary: "ok".to_string(),
            success: true,
        });
        app.update();
        app.world()
            .get::<Task>(parent_entity)
            .unwrap()
            .status
            .clone()
    }

    #[test]
    fn batch_completion_unblocks_parent_blocked_on_that_batch() {
        let status = run_batch_completion(TaskStatus::Waiting(WaitingReason::SubTaskBatch {
            batch_id: uuid::Uuid::from_u128(7),
        }));
        assert_eq!(status, TaskStatus::Ready);
    }

    #[test]
    fn batch_completion_leaves_idle_parent_untouched() {
        let status = run_batch_completion(TaskStatus::Waiting(WaitingReason::User));
        assert_eq!(status, TaskStatus::Waiting(WaitingReason::User));
    }
}
