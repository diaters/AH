use bevy::prelude::*;
use tracing::debug;

use crate::{
    app::Clock,
    domain::{
        ContinueTaskMessage, CreateTaskMessage, EntryMetadata, EntryRole, ShortTermMemory, Task,
        TaskStatus, UserCommand, UserInputMessage, WaitingReason,
    },
};

/// 用户输入路由系统：判断是创建新任务还是继续现有任务
pub(crate) fn user_input_routing_system(
    mut commands: Commands,
    user_inputs: Query<(Entity, &UserInputMessage)>,
    tasks: Query<&Task>,
) {
    for (entity, input) in &user_inputs {
        // 检查是否是命令（命令由 command_parse_system 处理）
        if UserCommand::parse(&input.content).is_command() {
            continue; // 跳过，由 command_parse_system 处理
        }

        // 查找是否有 Waiting(User) 状态的任务
        let waiting_tasks: Vec<_> = tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Waiting(WaitingReason::User))
            .collect();

        if let Some(task) = waiting_tasks.first() {
            debug!(
                event = "RoutingDecision",
                decision = "continue_existing",
                input = %input.content,
                input_len = input.content.len(),
                selected_task_id = %task.id,
                waiting_tasks_count = waiting_tasks.len(),
                waiting_tasks = ?waiting_tasks.iter().map(|t| (t.id, t.status.clone())).collect::<Vec<_>>(),
                "routing input to existing Waiting(User) task"
            );
            // 继续现有任务
            commands.spawn(ContinueTaskMessage {
                task_id: task.id,
                user_input: input.content.clone(),
            });
        } else {
            debug!(
                event = "RoutingDecision",
                decision = "create_new",
                input = %input.content,
                input_len = input.content.len(),
                waiting_tasks_count = 0,
                "no waiting task, creating new task"
            );
            // 创建新任务
            commands.spawn(CreateTaskMessage {
                content: input.content.clone(),
                origin_channel: input.origin_channel.clone(),
            });
        }

        commands.entity(entity).despawn();
    }
}

/// 继续任务系统：将用户输入追加到任务
pub(crate) fn continue_task_system(
    mut commands: Commands,
    clock: Res<Clock>,
    continue_messages: Query<(Entity, &ContinueTaskMessage)>,
    mut tasks: Query<(&mut Task, Option<&mut ShortTermMemory>)>,
) {
    for (entity, msg) in &continue_messages {
        // 更新任务状态和追加用户输入到 ShortTermMemory
        if let Some((mut task, short_term)) = tasks.iter_mut().find(|(t, _)| t.id == msg.task_id) {
            let stm_entries_before = short_term.as_ref().map(|s| s.entries.len()).unwrap_or(0);
            let stm_tokens_before = short_term.as_ref().map(|s| s.estimated_tokens).unwrap_or(0);

            let old_status = task.status.clone();
            let prev_content = task.content.clone();
            task.status = TaskStatus::Ready;
            task.content = msg.user_input.clone();
            task.updated_at = clock.0;

            // 追加用户输入到 ShortTermMemory
            if let Some(mut stm) = short_term {
                stm.add_entry(EntryRole::User, &msg.user_input, EntryMetadata::default());
                let stm_entries_after = stm.entries.len();
                let stm_tokens_after = stm.estimated_tokens;
                debug!(
                    event = "TaskContinued",
                    task_id = %task.id,
                    user_input = %msg.user_input,
                    user_input_len = msg.user_input.len(),
                    prev_content = %prev_content,
                    new_content = %task.content,
                    old_status = ?old_status,
                    new_status = ?task.status,
                    stm_entries_before = stm_entries_before,
                    stm_entries_after = stm_entries_after,
                    stm_tokens_before = stm_tokens_before,
                    stm_tokens_after = stm_tokens_after,
                    "task continued with new user input"
                );
            } else {
                debug!(
                    event = "TaskContinued",
                    task_id = %task.id,
                    user_input = %msg.user_input,
                    prev_content = %prev_content,
                    new_content = %task.content,
                    old_status = ?old_status,
                    new_status = ?task.status,
                    has_stm = false,
                    "task continued (no STM attached)"
                );
            }
        }

        commands.entity(entity).despawn();
    }
}
