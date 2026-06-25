use bevy::prelude::*;
use tracing::debug;

use crate::app::MemoryConfig;
use crate::domain::{
    ChannelId, CreateTaskMessage, FinishTaskMessage, FrontendKind, NewlyCreatedTask,
    PendingKnowledgeWriteHooks, ReloadPluginsMessage, SharedKnowledgeBase, SharedKnowledgeEntry,
    ShortTermMemory, SummarizationRequestMessage, SummarizationTrigger, Task, TaskStatus,
    UserCommand, UserInputMessage,
};

/// 命令解析系统：解析用户输入中的指令
pub(crate) fn command_parse_system(
    mut commands: Commands,
    mut knowledge: ResMut<SharedKnowledgeBase>,
    mut pending_writes: ResMut<PendingKnowledgeWriteHooks>,
    config: Res<MemoryConfig>,
    user_inputs: Query<(Entity, &UserInputMessage)>,
    tasks: Query<(&Task, Option<&ShortTermMemory>)>,
    plugin_registry: Option<Res<crate::user_plugins::registry::PluginRegistry>>,
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
                    // 创建子任务（Pending 状态）。附带 NewlyCreatedTask 标记，
                    // 使 on_task_created_hook_system 能对称地为 /btw 子任务派发 hook。
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
                    commands.spawn((child_task, ShortTermMemory::default(), NewlyCreatedTask));
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
                    let entry = SharedKnowledgeEntry::approved_from_user_input(content.clone());
                    knowledge.entries.push(entry.clone());
                    // 推入待派发队列，由 companion 系统触发 on_shared_knowledge_write hook。
                    pending_writes.0.push(entry);
                }
                commands.entity(entity).despawn();
            }
            UserCommand::PlainText(_) => {
                // 普通输入，交给路由系统处理
                // 不 despawn，让 user_input_routing_system 处理
            }
            UserCommand::ListPlugins => {
                // /plugins - 列出已加载的插件
                if let Some(registry) = &plugin_registry {
                    let plugins: Vec<String> = registry
                        .plugins()
                        .iter()
                        .map(|p| {
                            let name = p.manifest.name.as_deref().unwrap_or(&p.manifest.id);
                            let version = p.manifest.version.as_deref().unwrap_or("?");
                            format!("  {} v{} — {}", p.manifest.id, version, name)
                        })
                        .collect();
                    if plugins.is_empty() {
                        eprintln!("[plugins] no plugins loaded");
                    } else {
                        eprintln!("[plugins] loaded plugins ({}):", plugins.len());
                        for line in &plugins {
                            eprintln!("{}", line);
                        }
                    }
                    let failures: Vec<String> = registry
                        .failures()
                        .iter()
                        .map(|f| {
                            format!("  {}: {}", f.plugin_id.as_deref().unwrap_or("?"), f.error)
                        })
                        .collect();
                    if !failures.is_empty() {
                        eprintln!("[plugins] failed plugins ({}):", failures.len());
                        for line in &failures {
                            eprintln!("{}", line);
                        }
                    }
                } else {
                    eprintln!("[plugins] plugin system not initialized");
                }
                commands.entity(entity).despawn();
            }
            UserCommand::ReloadPlugins => {
                // /reload-plugins - 重新加载所有插件
                // command_parse_system 使用 Commands，无法直接获取 &mut World，
                // 因此 spawn ReloadPluginsMessage 由独立系统消费。
                debug!(
                    event = "ReloadPluginsCommandReceived",
                    "spawning ReloadPluginsMessage"
                );
                commands.spawn(ReloadPluginsMessage);
                commands.entity(entity).despawn();
            }
            UserCommand::PluginCommand {
                plugin_id,
                command,
                args,
            } => {
                // 插件 slash command：/plugin_id:command [args]
                // v1 简化实现：记录日志，后续 Phase 8 补充完整 Rhai 脚本派发
                debug!(
                    event = "PluginCommandReceived",
                    plugin_id = %plugin_id,
                    command = %command,
                    args = %args,
                    "plugin slash command parsed (v1 stub)"
                );
                if let Some(registry) = &plugin_registry {
                    if registry.get(&plugin_id).is_some() {
                        eprintln!(
                            "[plugins] /{}:{} — command dispatch not yet implemented",
                            plugin_id, command
                        );
                    } else {
                        eprintln!(
                            "[plugins] unknown plugin: {} (no such plugin loaded)",
                            plugin_id
                        );
                    }
                } else {
                    eprintln!("[plugins] plugin system not initialized");
                }
                commands.entity(entity).despawn();
            }
        }
    }
}

/// /reload-plugins 伴生系统：消费 `ReloadPluginsMessage` 实体，执行插件重载。
///
/// `command_parse_system` 使用 `Commands` 无法直接获取 `&mut World`，
/// 因此 spawn `ReloadPluginsMessage`，由此系统在下一帧消费并执行重载。
pub(crate) fn reload_plugins_system(world: &mut World) {
    let mut messages: Vec<bevy::prelude::Entity> = Vec::new();
    {
        let mut query = world.query_filtered::<bevy::prelude::Entity, With<ReloadPluginsMessage>>();
        for entity in query.iter(world) {
            messages.push(entity);
        }
    }

    if messages.is_empty() {
        return;
    }

    // 执行重载
    crate::user_plugins::reload::reload_plugins(world);

    // despawn 消息实体
    for entity in messages {
        world.despawn(entity);
    }
}

#[cfg(test)]
mod tests {
    use bevy::prelude::*;

    use super::command_parse_system;
    use crate::{
        app::MemoryConfig,
        domain::{
            KnowledgeValidationStatus, PendingKnowledgeWriteHooks, SharedKnowledgeBase,
            UserCommand, UserCommand::Remember, UserInputMessage,
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
        app.insert_resource(PendingKnowledgeWriteHooks::default());
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
        // 待派发 hook 队列也应包含一条记录
        let pending = app.world().resource::<PendingKnowledgeWriteHooks>();
        assert_eq!(pending.0.len(), 1);
        assert_eq!(
            UserCommand::parse("/remember Docs should stay in Chinese"),
            Remember {
                content: "Docs should stay in Chinese".to_string()
            }
        );
    }

    #[test]
    fn parse_list_plugins() {
        let cmd = UserCommand::parse("/plugins");
        assert_eq!(cmd, UserCommand::ListPlugins);
        assert!(cmd.is_command());
    }

    #[test]
    fn parse_plugin_command_with_args() {
        let cmd = UserCommand::parse("/alpha:hello world");
        assert_eq!(
            cmd,
            UserCommand::PluginCommand {
                plugin_id: "alpha".to_string(),
                command: "hello".to_string(),
                args: "world".to_string(),
            }
        );
        assert!(cmd.is_command());
    }

    #[test]
    fn parse_plugin_command_without_args() {
        let cmd = UserCommand::parse("/alpha:hello");
        assert_eq!(
            cmd,
            UserCommand::PluginCommand {
                plugin_id: "alpha".to_string(),
                command: "hello".to_string(),
                args: String::new(),
            }
        );
        assert!(cmd.is_command());
    }

    #[test]
    fn parse_plugin_command_empty_plugin_id_falls_back() {
        // /:hello — 空 plugin_id，回退到 PlainText
        let cmd = UserCommand::parse("/:hello");
        assert!(matches!(cmd, UserCommand::PlainText(_)));
    }

    #[test]
    fn parse_plugin_command_empty_command_falls_back() {
        // /alpha: — 空 command，回退到 PlainText
        let cmd = UserCommand::parse("/alpha:");
        assert!(matches!(cmd, UserCommand::PlainText(_)));
    }

    #[test]
    fn parse_plugin_command_no_colon_falls_back() {
        // /alpha — 不含冒号，回退到 PlainText
        let cmd = UserCommand::parse("/alpha");
        assert!(matches!(cmd, UserCommand::PlainText(_)));
    }

    #[test]
    fn parse_reload_plugins() {
        let cmd = UserCommand::parse("/reload-plugins");
        assert_eq!(cmd, UserCommand::ReloadPlugins);
        assert!(cmd.is_command());
    }
}
