//! 等待任务 System
//!
//! 处理 wait_tasks 工具的等待逻辑。

use crate::prelude::*;

use crate::{
    app::Clock,
    domain::{SubTaskCompletedMessage, Task, WaitingForTasksInfo},
};

use super::orchestrator::{collect_task_results, spawn_wait_result_message};

/// 子任务完成时检查是否有任务在等待（事件驱动优化）
pub fn on_subtask_completed_check_waiting(
    messages: Query<(Entity, &SubTaskCompletedMessage)>,
    waiting_tasks: Query<(Entity, &Task, &WaitingForTasksInfo)>,
    all_tasks: Query<&Task>,
    mut commands: Commands,
) {
    for (_msg_entity, msg) in &messages {
        // 检查是否有任务在等待这个完成的子任务
        for (entity, task, info) in &waiting_tasks {
            if info.target_task_ids.contains(&msg.child_task_id) {
                // 检查是否所有目标都完成
                let all_terminal = info.target_task_ids.iter().all(|id| {
                    all_tasks
                        .iter()
                        .any(|t| t.id == *id && t.status.is_terminal())
                });

                if all_terminal {
                    let results = collect_task_results(&info.target_task_ids, &all_tasks);
                    spawn_wait_result_message(&mut commands, task.id, info, results, false);
                    commands.entity(entity).remove::<WaitingForTasksInfo>();
                }
            }
        }
    }
}

/// 轮询检查等待中的任务（超时兜底）
pub fn check_waiting_tasks_system(
    clock: Res<Clock>,
    mut commands: Commands,
    waiting_tasks: Query<(Entity, &Task, &WaitingForTasksInfo)>,
    all_tasks: Query<&Task>,
) {
    for (entity, task, info) in &waiting_tasks {
        let timed_out = clock.0 >= info.timeout_at;

        // 检查所有目标任务是否都已终态
        let all_terminal = info.target_task_ids.iter().all(|id| {
            all_tasks
                .iter()
                .any(|t| t.id == *id && t.status.is_terminal())
        });

        if timed_out || all_terminal {
            let results = collect_task_results(&info.target_task_ids, &all_tasks);
            spawn_wait_result_message(&mut commands, task.id, info, results, timed_out);

            // 移除等待信息组件
            commands.entity(entity).remove::<WaitingForTasksInfo>();
        }
    }
}
