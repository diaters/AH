use bevy::prelude::*;
use tracing::debug;

use crate::app::MemoryConfig;
use crate::domain::{
    ChannelId, CreateTaskMessage, FinishTaskMessage, FrontendKind, SharedKnowledgeBase,
    SharedKnowledgeEntry, ShortTermMemory, SummarizationRequestMessage, SummarizationTrigger, Task,
    TaskStatus, TaskTerminatedMessage, UserCommand, UserInputMessage,
};

/// 命令解析系统：解析用户输入中的指令
pub(crate) fn command_parse_system(
    mut commands: Commands,
    mut knowledge: ResMut<SharedKnowledgeBase>,
    config: Res<MemoryConfig>,
    user_inputs: Query<(Entity, &UserInputMessage)>,
    tasks: Query<(&Task, Option<&ShortTermMemory>)>,
) {
    for (entity, input) in &user_inputs {
        let cmd = UserCommand::parse(&input.content);

        debug!(
            event = "CommandParsed",
            command = ?cmd,
            raw_input = %input.content,
            input_len = input.content.len(),
            "user command parsed"
        );

        match cmd {
            UserCommand::NewTask { topic } => {
                // /btw - 创建子任务承接新话题
                // 查找当前活跃的任务作为父任务
                let parent_task = tasks
                    .iter()
                    .find(|(t, _)| !t.status.is_terminal() && t.status != TaskStatus::Pending);

                if let Some((parent, _)) = parent_task {
                    debug!(
                        event = "SubTaskCreating",
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
                        ChannelId {
                            frontend: FrontendKind::Tui,
                            user_id: "default".to_string(),
                        },
                    );
                    commands.spawn((child_task, ShortTermMemory::default()));
                } else {
                    debug!(
                        event = "NoParentTask",
                        topic = %topic,
                        "no active parent task, creating normal task"
                    );
                    // 没有父任务，创建普通任务
                    commands.spawn(CreateTaskMessage {
                        content: input.content.clone(),
                    });
                }
                commands.entity(entity).despawn();
            }
            UserCommand::FinishCurrentTask => {
                // /finish - 结束当前任务
                let current_task = tasks.iter().find(|(t, _)| !t.status.is_terminal());

                if let Some((task, _)) = current_task {
                    debug!(
                        event = "FinishCommandReceived",
                        task_id = %task.id,
                        task_status = ?task.status,
                        task_content = %task.content,
                        "finishing current task via /finish command"
                    );
                    commands.spawn(TaskTerminatedMessage { task_id: task.id });
                    // 标记任务为完成
                    commands.spawn(FinishTaskMessage { task_id: task.id });
                } else {
                    debug!(event = "FinishCommandNoTask", "no active task to finish");
                }
                commands.entity(entity).despawn();
            }
            UserCommand::Summarize => {
                // /summarize - 触发总结
                let active_task = tasks.iter().find(|(t, _)| !t.status.is_terminal());

                if let Some((task, memory)) = active_task
                    && let Some(stm) = memory
                {
                    // 收集所有条目内容
                    let content: String = stm
                        .entries
                        .iter()
                        .map(|e| format!("{:?}: {}", e.role, e.content))
                        .collect::<Vec<_>>()
                        .join("\n");

                    if !content.is_empty() {
                        debug!(
                            event = "SummarizeCommandReceived",
                            task_id = %task.id,
                            stm_entries = stm.entries.len(),
                            stm_tokens = stm.estimated_tokens,
                            content_len = content.len(),
                            "triggering summarization via /summarize command"
                        );
                        commands.spawn(SummarizationRequestMessage {
                            task_id: task.id,
                            content_to_summarize: content,
                            target_tokens: config.summary_target_tokens,
                            trigger: SummarizationTrigger::UserCommand,
                        });
                    } else {
                        debug!(
                            event = "SummarizeCommandEmpty",
                            task_id = %task.id,
                            "stm is empty, nothing to summarize"
                        );
                    }
                } else {
                    debug!(
                        event = "SummarizeCommandNoTask",
                        "no active task to summarize"
                    );
                }
                commands.entity(entity).despawn();
            }
            UserCommand::Remember { content } => {
                // /remember - 以用户显式批准的共享知识条目写入共享知识库
                if content.is_empty() {
                    debug!(
                        event = "RememberCommandEmpty",
                        "remember command received with empty content - ignoring"
                    );
                } else {
                    debug!(
                        event = "RememberCommandReceived",
                        content = %content,
                        content_len = content.len(),
                        knowledge_entries_before = knowledge.entries.len(),
                        "adding knowledge via /remember command"
                    );
                    knowledge
                        .entries
                        .push(SharedKnowledgeEntry::approved_from_user_input(
                            content.clone(),
                        ));
                }
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
    use bevy::prelude::*;

    use super::command_parse_system;
    use crate::{
        app::MemoryConfig,
        domain::{
            KnowledgeValidationStatus, SharedKnowledgeBase, UserCommand, UserCommand::Remember,
            UserInputMessage,
        },
    };

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
    fn parse_remember_with_content() {
        let cmd = UserCommand::parse("/remember This is important knowledge");
        assert_eq!(
            cmd,
            UserCommand::Remember {
                content: "This is important knowledge".to_string()
            }
        );
        assert!(cmd.is_command());
    }

    #[test]
    fn parse_remember_without_content() {
        let cmd = UserCommand::parse("/remember");
        assert_eq!(
            cmd,
            UserCommand::Remember {
                content: String::new()
            }
        );
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

    #[test]
    fn remember_command_creates_approved_shared_knowledge_entry() {
        let mut app = App::new();
        app.insert_resource(MemoryConfig::default());
        app.insert_resource(SharedKnowledgeBase::default());
        app.add_systems(Update, command_parse_system);
        app.world_mut().spawn(UserInputMessage {
            content: "/remember Docs should stay in Chinese".to_string(),
        });

        app.update();

        let knowledge = app.world().resource::<SharedKnowledgeBase>();
        assert_eq!(knowledge.entries.len(), 1);
        assert_eq!(
            knowledge.entries[0].validation_status,
            KnowledgeValidationStatus::Approved
        );
        assert_eq!(
            knowledge.entries[0].approved_by.as_deref(),
            Some("user:/remember")
        );
        assert_eq!(
            UserCommand::parse("/remember Docs should stay in Chinese"),
            Remember {
                content: "Docs should stay in Chinese".to_string()
            }
        );
    }
}
