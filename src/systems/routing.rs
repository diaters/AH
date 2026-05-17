use bevy::prelude::*;

use crate::{
    app::Clock,
    domain::{
        ContinueTaskMessage, CreateTaskMessage, Task, TaskStatus, UserCommand, UserInputMessage,
        WaitingReason,
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
        let waiting_task = tasks
            .iter()
            .find(|t| t.status == TaskStatus::Waiting(WaitingReason::User));

        if let Some(task) = waiting_task {
            // 继续现有任务
            commands.spawn(ContinueTaskMessage {
                task_id: task.id,
                user_input: input.content.clone(),
            });
        } else {
            // 创建新任务
            commands.spawn(CreateTaskMessage {
                content: input.content.clone(),
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
    mut tasks: Query<&mut Task>,
) {
    for (entity, msg) in &continue_messages {
        if let Some(mut task) = tasks.iter_mut().find(|t| t.id == msg.task_id) {
            // 更新任务状态为 Ready
            task.status = TaskStatus::Ready;
            task.updated_at = clock.0;
        }
        commands.entity(entity).despawn();
    }
}
