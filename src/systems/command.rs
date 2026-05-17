use bevy::prelude::*;
use tracing::info;

use crate::domain::{
    CreateTaskMessage, ShortTermMemory, Task, TaskStatus, TaskTerminatedMessage, UserCommand,
    UserInputMessage,
};

/// 命令解析系统：解析用户输入中的指令
pub(crate) fn command_parse_system(
    mut commands: Commands,
    user_inputs: Query<(Entity, &UserInputMessage)>,
    tasks: Query<&Task>,
) {
    for (entity, input) in &user_inputs {
        let cmd = UserCommand::parse(&input.content);

        match cmd {
            UserCommand::NewTask { topic } => {
                // /btw - 创建子任务承接新话题
                // 查找当前活跃的任务作为父任务
                let parent_task = tasks
                    .iter()
                    .find(|t| !t.status.is_terminal() && t.status != TaskStatus::Pending);

                if let Some(parent) = parent_task {
                    info!(
                        parent_id = %parent.id,
                        topic = %topic,
                        "creating sub-task via /btw command"
                    );
                    // 创建子任务（Pending 状态）
                    let child_task = Task::from_user_input(
                        if topic.is_empty() {
                            &input.content
                        } else {
                            &topic
                        },
                        parent.max_retries,
                    );
                    commands.spawn((child_task, ShortTermMemory::default()));
                } else {
                    // 没有父任务，创建普通任务
                    commands.spawn(CreateTaskMessage {
                        content: input.content.clone(),
                    });
                }
                commands.entity(entity).despawn();
            }
            UserCommand::FinishCurrentTask => {
                // /finish - 结束当前任务
                let current_task = tasks.iter().find(|t| !t.status.is_terminal());

                if let Some(task) = current_task {
                    info!(task_id = %task.id, "finishing current task via /finish command");
                    // 触发任务终止，后续会被 contribution 系统处理
                    commands.spawn(TaskTerminatedMessage { task_id: task.id });
                }
                commands.entity(entity).despawn();
            }
            UserCommand::Summarize => {
                // /summarize - 触发总结
                // TODO: 实现总结触发
                info!("summarize command received - to be implemented");
                commands.entity(entity).despawn();
            }
            UserCommand::PlainText(_) => {
                // 普通输入，交给路由系统处理
                // 不 despawn，让 user_input_routing_system 处理
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::domain::UserCommand;

    #[test]
    fn parse_btw_with_topic() {
        let cmd = UserCommand::parse("/btw new topic");
        assert_eq!(
            cmd,
            UserCommand::NewTask {
                topic: "new topic".to_string()
            }
        );
        assert!(cmd.is_command());
    }

    #[test]
    fn parse_btw_without_topic() {
        let cmd = UserCommand::parse("/btw");
        assert_eq!(
            cmd,
            UserCommand::NewTask {
                topic: String::new()
            }
        );
        assert!(cmd.is_command());
    }

    #[test]
    fn parse_finish() {
        let cmd = UserCommand::parse("/finish");
        assert_eq!(cmd, UserCommand::FinishCurrentTask);
        assert!(cmd.is_command());
    }

    #[test]
    fn parse_summarize() {
        let cmd = UserCommand::parse("/summarize");
        assert_eq!(cmd, UserCommand::Summarize);
        assert!(cmd.is_command());
    }

    #[test]
    fn parse_plain_text() {
        let cmd = UserCommand::parse("Hello, how are you?");
        assert_eq!(
            cmd,
            UserCommand::PlainText("Hello, how are you?".to_string())
        );
        assert!(!cmd.is_command());
    }
}
