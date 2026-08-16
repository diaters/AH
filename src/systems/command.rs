use crate::prelude::*;
use tracing::debug;

use crate::app::MemoryConfig;
use crate::domain::{
    Agent, ClearTaskMessage, CreateTaskMessage, DispatchHint, DispatchKind, DispatchStrategy,
    FinishTaskMessage, NewlyCreatedTask, PendingDispatch, PendingKnowledgeWriteHooks,
    ReloadPluginsMessage, ReloadTriggersMessage, SharedKnowledgeBase, SharedKnowledgeEntry,
    ShortTermMemory, SummarizationRequestMessage, SummarizationTrigger, Task, TaskRoutingPolicy,
    TaskStatus, UserCommand, UserInputMessage,
};
use crate::ecs::EntityIndex;

/// 命令解析系统：解析用户输入中的指令
pub(crate) fn command_parse_system(
    mut commands: Commands,
    mut knowledge: ResMut<SharedKnowledgeBase>,
    mut pending_writes: ResMut<PendingKnowledgeWriteHooks>,
    config: Res<MemoryConfig>,
    index: Res<EntityIndex>,
    user_inputs: Query<(Entity, &UserInputMessage)>,
    tasks: Query<(&Task, Option<&ShortTermMemory>)>,
    agents: Query<&Agent>,
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
                let parent_task = tasks.iter().find(|(t, _)| {
                    !t.status.is_terminal()
                        && t.status != TaskStatus::Pending
                        && t.origin_channel == Some(input.origin_channel.clone())
                });

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
                        input.origin_channel.clone(),
                    );
                    commands.spawn((
                        child_task,
                        ShortTermMemory::default(),
                        NewlyCreatedTask,
                        PendingDispatch {
                            kind: DispatchKind::Task,
                            hint: DispatchHint {
                                strategy: DispatchStrategy::BrainLlm,
                                preferred_agent_name: None,
                                required_skill_id: None,
                                agent_spawn_spec: None,
                            },
                        },
                    ));
                } else {
                    debug!(
                        event = "NoParentTask",
                        topic = %topic,
                        "no active parent task, creating normal task"
                    );
                    // 没有父任务，创建普通任务
                    commands.spawn(CreateTaskMessage {
                        content: input.content.clone(),
                        origin_channel: Some(input.origin_channel.clone()),
                        routing_policy: TaskRoutingPolicy::conversational(
                            input.origin_channel.clone(),
                        ),
                    });
                }
                commands.entity(entity).despawn();
            }
            UserCommand::FinishCurrentTask => {
                // /finish - 结束当前任务
                let current_task = tasks.iter().find(|(t, _)| {
                    !t.status.is_terminal()
                        && t.origin_channel == Some(input.origin_channel.clone())
                });

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
                let active_task = tasks.iter().find(|(t, _)| {
                    !t.status.is_terminal()
                        && t.origin_channel == Some(input.origin_channel.clone())
                });

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
            UserCommand::ReloadTriggers => {
                // /reload-triggers - 重新加载事件触发配置
                debug!(
                    event = "ReloadTriggersCommandReceived",
                    "spawning ReloadTriggersMessage"
                );
                commands.spawn(ReloadTriggersMessage);
                commands.entity(entity).despawn();
            }
            UserCommand::ClearCurrentTask => {
                // /clear - 删除当前任务（不触发终态处理链路）
                let current_task = tasks.iter().find(|(t, _)| {
                    !t.status.is_terminal()
                        && t.origin_channel == Some(input.origin_channel.clone())
                });

                if let Some((task, _)) = current_task {
                    debug!(
                        event = "ClearCommandReceived",
                        task_id = %task.id,
                        task_status = ?task.status,
                        task_content = %task.content,
                        "clearing current task via /clear command"
                    );
                    commands.spawn(ClearTaskMessage { task_id: task.id });
                } else {
                    debug!(event = "ClearCommandNoTask", "no active task to clear");
                }
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
            UserCommand::CreateSkill { intent } => {
                // /skill - 为当前任务的 Agent 创建新 skill
                if intent.is_empty() {
                    eprintln!("[skill] usage: /skill <intent description>");
                } else {
                    // 查找当前活跃任务的 Agent（与 /finish 同逻辑：同 channel、非终态）
                    let current_task = tasks.iter().find(|(t, _)| {
                        !t.status.is_terminal()
                            && t.origin_channel == Some(input.origin_channel.clone())
                    });

                    if let Some((task, _)) = current_task {
                        // 优先使用 task.delegate（执行 Agent），回退到 task.creator
                        let agent_id = task.delegate.unwrap_or(task.creator);
                        let agent_name = index
                            .get_agent(&agent_id)
                            .and_then(|e| agents.get(e).ok())
                            .map(|a| a.profile.name.clone())
                            .unwrap_or_default();

                        debug!(
                            event = "SkillCreationCommandReceived",
                            task_id = %task.id,
                            agent_id = %agent_id,
                            agent_name = %agent_name,
                            intent = %intent,
                            "spawning skill creation request"
                        );
                        commands.spawn(crate::domain::SkillCreationRequestMessage {
                            task_id: task.id,
                            agent_id,
                            agent_name,
                            intent,
                        });
                    } else {
                        eprintln!("[skill] no active task — /skill requires an active task");
                    }
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
    let mut messages: Vec<crate::prelude::Entity> = Vec::new();
    {
        let mut query =
            world.query_filtered::<crate::prelude::Entity, With<ReloadPluginsMessage>>();
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

/// /reload-triggers 伴生系统：消费 `ReloadTriggersMessage` 实体，执行触发器重载。
///
/// 与 `ReloadPluginsMessage` 同模式：`command_parse_system` spawn 此消息实体，
/// 由此系统在下一帧消费并执行重载。
pub(crate) fn reload_triggers_message_consumer_system(world: &mut World) {
    let mut messages: Vec<crate::prelude::Entity> = Vec::new();
    {
        let mut query =
            world.query_filtered::<crate::prelude::Entity, With<ReloadTriggersMessage>>();
        for entity in query.iter(world) {
            messages.push(entity);
        }
    }

    if messages.is_empty() {
        return;
    }

    // 执行重载
    crate::triggers::reload_triggers_system(world);

    // despawn 消息实体
    for entity in messages {
        world.despawn(entity);
    }
}

#[cfg(test)]
mod tests {
    use crate::prelude::*;

    use super::command_parse_system;
    use crate::{
        app::MemoryConfig,
        domain::{
            Agent, ChannelId, CreateTaskMessage, KnowledgeValidationStatus,
            PendingKnowledgeWriteHooks, SharedKnowledgeBase, ShortTermMemory, Task, TaskStatus,
            UserCommand, UserCommand::Remember, UserInputMessage,
        },
    };
    use crate::ecs::EntityIndex;

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
        app.insert_resource(EntityIndex::default());
        app.add_systems(Update, command_parse_system);
        app.world_mut().spawn(UserInputMessage {
            content: "/remember Docs should stay in Chinese".to_string(),
            origin_channel: crate::domain::ChannelId {
                frontend: crate::domain::FrontendKind::Tui,
                user_id: "default".to_string(),
                thread_id: None,
            },
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

    #[test]
    fn btw_picks_parent_only_in_same_channel() {
        use crate::domain::FrontendKind;

        let mut app = App::new();
        app.insert_resource(MemoryConfig::default());
        app.insert_resource(SharedKnowledgeBase::default());
        app.insert_resource(PendingKnowledgeWriteHooks::default());
        app.insert_resource(EntityIndex::default());
        app.add_systems(Update, command_parse_system);

        // QQ 通道的活跃任务
        let qq_channel = ChannelId {
            frontend: FrontendKind::QQ,
            user_id: "qq-user".to_string(),
            thread_id: None,
        };
        let now = chrono::Utc::now();
        app.world_mut().spawn((
            Task {
                id: uuid::Uuid::new_v4(),
                content: "qq active task".to_string(),
                creator: uuid::Uuid::nil(),
                delegate: None,
                status: TaskStatus::Ready,
                pending_confirmation_id: None,
                input_summary: "qq".to_string(),
                result_summary: String::new(),
                priority: 0,
                created_at: now,
                updated_at: now,
                retry_count: 0,
                max_retries: 3,
                next_retry_at: None,
                last_error: None,
                multi_turn: false,
                parent_task_id: None,
                batch_id: None,
                origin_channel: Some(qq_channel.clone()),
                routing_policy: crate::domain::TaskRoutingPolicy::conversational(
                    qq_channel.clone(),
                ),
                last_evaluated_turn: None,
            },
            ShortTermMemory::default(),
        ));

        // Telegram 通道发起 /btw
        let tg_channel = ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "tg-user".to_string(),
            thread_id: None,
        };
        app.world_mut().spawn(UserInputMessage {
            content: "/btw new topic".to_string(),
            origin_channel: tg_channel.clone(),
        });

        app.update();

        // 断言：无父任务，走 CreateTaskMessage 分支
        let create_msgs: Vec<&CreateTaskMessage> = app
            .world_mut()
            .query::<&CreateTaskMessage>()
            .iter(app.world())
            .collect();
        assert_eq!(
            create_msgs.len(),
            1,
            "Telegram /btw with no Telegram parent should fall back to CreateTaskMessage"
        );
        assert_eq!(create_msgs[0].origin_channel, Some(tg_channel));
    }

    #[test]
    fn btw_subtask_inherits_input_origin_channel() {
        use crate::domain::FrontendKind;

        let mut app = App::new();
        app.insert_resource(MemoryConfig::default());
        app.insert_resource(SharedKnowledgeBase::default());
        app.insert_resource(PendingKnowledgeWriteHooks::default());
        app.insert_resource(EntityIndex::default());
        app.add_systems(Update, command_parse_system);

        // QQ 通道的活跃父任务
        let qq_channel = ChannelId {
            frontend: FrontendKind::QQ,
            user_id: "qq-user".to_string(),
            thread_id: None,
        };
        let now = chrono::Utc::now();
        app.world_mut().spawn((
            Task {
                id: uuid::Uuid::new_v4(),
                content: "qq parent".to_string(),
                creator: uuid::Uuid::nil(),
                delegate: None,
                status: TaskStatus::Ready,
                pending_confirmation_id: None,
                input_summary: "qq".to_string(),
                result_summary: String::new(),
                priority: 0,
                created_at: now,
                updated_at: now,
                retry_count: 0,
                max_retries: 3,
                next_retry_at: None,
                last_error: None,
                multi_turn: false,
                parent_task_id: None,
                batch_id: None,
                origin_channel: Some(qq_channel.clone()),
                routing_policy: crate::domain::TaskRoutingPolicy::conversational(
                    qq_channel.clone(),
                ),
                last_evaluated_turn: None,
            },
            ShortTermMemory::default(),
        ));

        app.world_mut().spawn(UserInputMessage {
            content: "/btw child topic".to_string(),
            origin_channel: qq_channel.clone(),
        });

        app.update();

        // /btw 子任务使用 topic 作为 content（若 topic 为空则使用 input.content）
        let child_task = app
            .world_mut()
            .query::<&Task>()
            .iter(app.world())
            .find(|t| t.content == "child topic");
        assert!(
            child_task.is_some(),
            "should spawn child task with topic as content"
        );
        assert_eq!(
            child_task.unwrap().origin_channel,
            Some(qq_channel),
            "child task should inherit input origin_channel"
        );
    }

    #[test]
    fn finish_does_not_finish_other_channel_task() {
        use crate::domain::{FinishTaskMessage, FrontendKind, Task, TaskStatus};

        let mut app = App::new();
        app.insert_resource(MemoryConfig::default());
        app.insert_resource(SharedKnowledgeBase::default());
        app.insert_resource(PendingKnowledgeWriteHooks::default());
        app.insert_resource(EntityIndex::default());
        app.add_systems(Update, command_parse_system);

        let qq_channel = ChannelId {
            frontend: FrontendKind::QQ,
            user_id: "qq-user".to_string(),
            thread_id: None,
        };
        let now = chrono::Utc::now();
        app.world_mut().spawn((
            Task {
                id: uuid::Uuid::new_v4(),
                content: "qq active task".to_string(),
                creator: uuid::Uuid::nil(),
                delegate: None,
                status: TaskStatus::Ready,
                pending_confirmation_id: None,
                input_summary: "qq".to_string(),
                result_summary: String::new(),
                priority: 0,
                created_at: now,
                updated_at: now,
                retry_count: 0,
                max_retries: 3,
                next_retry_at: None,
                last_error: None,
                multi_turn: false,
                parent_task_id: None,
                batch_id: None,
                origin_channel: Some(qq_channel.clone()),
                routing_policy: crate::domain::TaskRoutingPolicy::conversational(qq_channel),
                last_evaluated_turn: None,
            },
            ShortTermMemory::default(),
        ));

        let tg_channel = ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "tg-user".to_string(),
            thread_id: None,
        };
        app.world_mut().spawn(UserInputMessage {
            content: "/finish".to_string(),
            origin_channel: tg_channel,
        });

        app.update();

        // 断言：未生成 FinishTaskMessage
        let finish_count = app
            .world_mut()
            .query::<&FinishTaskMessage>()
            .iter(app.world())
            .count();
        assert_eq!(finish_count, 0, "Telegram /finish should not touch QQ task");
    }

    #[test]
    fn summarize_does_not_summarize_other_channel_task() {
        use crate::domain::{FrontendKind, SummarizationRequestMessage, Task, TaskStatus};

        let mut app = App::new();
        app.insert_resource(MemoryConfig::default());
        app.insert_resource(SharedKnowledgeBase::default());
        app.insert_resource(PendingKnowledgeWriteHooks::default());
        app.insert_resource(EntityIndex::default());
        app.add_systems(Update, command_parse_system);

        let qq_channel = ChannelId {
            frontend: FrontendKind::QQ,
            user_id: "qq-user".to_string(),
            thread_id: None,
        };
        let now = chrono::Utc::now();
        let mut stm = ShortTermMemory::default();
        stm.add_entry(
            crate::domain::EntryRole::User,
            "some content long enough",
            crate::domain::EntryMetadata::default(),
        );
        app.world_mut().spawn((
            Task {
                id: uuid::Uuid::new_v4(),
                content: "qq active task".to_string(),
                creator: uuid::Uuid::nil(),
                delegate: None,
                status: TaskStatus::Ready,
                pending_confirmation_id: None,
                input_summary: "qq".to_string(),
                result_summary: String::new(),
                priority: 0,
                created_at: now,
                updated_at: now,
                retry_count: 0,
                max_retries: 3,
                next_retry_at: None,
                last_error: None,
                multi_turn: false,
                parent_task_id: None,
                batch_id: None,
                origin_channel: Some(qq_channel.clone()),
                routing_policy: crate::domain::TaskRoutingPolicy::conversational(qq_channel),
                last_evaluated_turn: None,
            },
            stm,
        ));

        let tg_channel = ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "tg-user".to_string(),
            thread_id: None,
        };
        app.world_mut().spawn(UserInputMessage {
            content: "/summarize".to_string(),
            origin_channel: tg_channel,
        });

        app.update();

        // 断言：未生成 SummarizationRequestMessage
        let summarize_count = app
            .world_mut()
            .query::<&SummarizationRequestMessage>()
            .iter(app.world())
            .count();
        assert_eq!(
            summarize_count, 0,
            "Telegram /summarize should not touch QQ task"
        );
    }

    #[test]
    fn clear_command_spawns_clear_task_message() {
        use crate::domain::{ClearTaskMessage, FrontendKind, Task, TaskStatus};

        let mut app = App::new();
        app.insert_resource(MemoryConfig::default());
        app.insert_resource(SharedKnowledgeBase::default());
        app.insert_resource(PendingKnowledgeWriteHooks::default());
        app.insert_resource(EntityIndex::default());
        app.add_systems(Update, command_parse_system);

        let channel = ChannelId {
            frontend: FrontendKind::Tui,
            user_id: "test".to_string(),
            thread_id: None,
        };
        let now = chrono::Utc::now();
        let task_id = uuid::Uuid::new_v4();
        app.world_mut().spawn((
            Task {
                id: task_id,
                content: "active task".to_string(),
                creator: uuid::Uuid::nil(),
                delegate: None,
                status: TaskStatus::Running,
                pending_confirmation_id: None,
                input_summary: "test".to_string(),
                result_summary: String::new(),
                priority: 0,
                created_at: now,
                updated_at: now,
                retry_count: 0,
                max_retries: 3,
                next_retry_at: None,
                last_error: None,
                multi_turn: false,
                parent_task_id: None,
                batch_id: None,
                origin_channel: Some(channel.clone()),
                routing_policy: crate::domain::TaskRoutingPolicy::conversational(channel.clone()),
                last_evaluated_turn: None,
            },
            ShortTermMemory::default(),
        ));

        app.world_mut().spawn(UserInputMessage {
            content: "/clear".to_string(),
            origin_channel: channel,
        });

        app.update();

        let clear_msgs: Vec<&ClearTaskMessage> = app
            .world_mut()
            .query::<&ClearTaskMessage>()
            .iter(app.world())
            .collect();
        assert_eq!(clear_msgs.len(), 1);
        assert_eq!(clear_msgs[0].task_id, task_id);
    }

    #[test]
    fn skill_command_spawns_creation_request() {
        use crate::domain::{FrontendKind, SkillCreationRequestMessage, Task, TaskStatus};

        let mut app = App::new();
        app.insert_resource(MemoryConfig::default());
        app.insert_resource(SharedKnowledgeBase::default());
        app.insert_resource(PendingKnowledgeWriteHooks::default());
        app.insert_resource(EntityIndex::default());
        app.add_systems(Update, command_parse_system);

        let channel = ChannelId {
            frontend: FrontendKind::Tui,
            user_id: "test".to_string(),
            thread_id: None,
        };
        let now = chrono::Utc::now();
        let task_id = uuid::Uuid::new_v4();
        let creator_id = uuid::Uuid::new_v4();
        app.world_mut().spawn((
            Task {
                id: task_id,
                content: "active task".to_string(),
                creator: creator_id,
                delegate: None,
                status: TaskStatus::Running,
                pending_confirmation_id: None,
                input_summary: "test".to_string(),
                result_summary: String::new(),
                priority: 0,
                created_at: now,
                updated_at: now,
                retry_count: 0,
                max_retries: 3,
                next_retry_at: None,
                last_error: None,
                multi_turn: false,
                parent_task_id: None,
                batch_id: None,
                origin_channel: Some(channel.clone()),
                routing_policy: crate::domain::TaskRoutingPolicy::conversational(channel.clone()),
                last_evaluated_turn: None,
            },
            ShortTermMemory::default(),
        ));

        app.world_mut().spawn(UserInputMessage {
            content: "/skill 做代码审查".to_string(),
            origin_channel: channel,
        });

        app.update();

        let skill_msgs: Vec<&SkillCreationRequestMessage> = app
            .world_mut()
            .query::<&SkillCreationRequestMessage>()
            .iter(app.world())
            .collect();
        assert_eq!(skill_msgs.len(), 1);
        assert_eq!(skill_msgs[0].task_id, task_id);
        assert_eq!(skill_msgs[0].intent, "做代码审查");
        // delegate 为 None，回退到 creator；无 Agent entity，agent_name 为空
        assert_eq!(skill_msgs[0].agent_id, creator_id);
        assert!(skill_msgs[0].agent_name.is_empty());
    }

    #[test]
    fn skill_command_uses_delegate_agent_name() {
        use crate::domain::{FrontendKind, SkillCreationRequestMessage, Task, TaskStatus};

        let mut app = App::new();
        app.insert_resource(MemoryConfig::default());
        app.insert_resource(SharedKnowledgeBase::default());
        app.insert_resource(PendingKnowledgeWriteHooks::default());
        app.insert_resource(EntityIndex::default());
        app.add_systems(Update, command_parse_system);

        let channel = ChannelId {
            frontend: FrontendKind::Tui,
            user_id: "test".to_string(),
            thread_id: None,
        };
        let now = chrono::Utc::now();
        let task_id = uuid::Uuid::new_v4();
        let delegate_id = uuid::Uuid::new_v4();
        // 注册 Agent 实体
        let agent_entity = app.world_mut().spawn(Agent {
            id: delegate_id,
            profile: crate::domain::AgentProfile {
                name: "browser-operator".to_string(),
                model: "gpt-4".to_string(),
            },
            capabilities: crate::domain::AgentCapabilities {
                tags: vec![],
                description: String::new(),
            },
            kind: crate::domain::AgentKind::Persistent,
            parent_id: None,
            bound_task_id: None,
            tool_permissions: crate::domain::AgentToolPermissions::default(),
            system_prompt: None,
        }).id();
        app.world_mut().resource_mut::<EntityIndex>().agents.insert(delegate_id, agent_entity);

        app.world_mut().spawn((
            Task {
                id: task_id,
                content: "active task".to_string(),
                creator: uuid::Uuid::new_v4(),
                delegate: Some(delegate_id),
                status: TaskStatus::Running,
                pending_confirmation_id: None,
                input_summary: "test".to_string(),
                result_summary: String::new(),
                priority: 0,
                created_at: now,
                updated_at: now,
                retry_count: 0,
                max_retries: 3,
                next_retry_at: None,
                last_error: None,
                multi_turn: false,
                parent_task_id: None,
                batch_id: None,
                origin_channel: Some(channel.clone()),
                routing_policy: crate::domain::TaskRoutingPolicy::conversational(channel.clone()),
                last_evaluated_turn: None,
            },
            ShortTermMemory::default(),
        ));

        app.world_mut().spawn(UserInputMessage {
            content: "/skill 创建测试 skill".to_string(),
            origin_channel: channel,
        });

        app.update();

        let skill_msgs: Vec<&SkillCreationRequestMessage> = app
            .world_mut()
            .query::<&SkillCreationRequestMessage>()
            .iter(app.world())
            .collect();
        assert_eq!(skill_msgs.len(), 1);
        assert_eq!(skill_msgs[0].task_id, task_id);
        assert_eq!(skill_msgs[0].agent_id, delegate_id);
        assert_eq!(skill_msgs[0].agent_name, "browser-operator");
    }

    #[test]
    fn skill_command_empty_intent_no_request() {
        use crate::domain::{FrontendKind, SkillCreationRequestMessage, Task, TaskStatus};

        let mut app = App::new();
        app.insert_resource(MemoryConfig::default());
        app.insert_resource(SharedKnowledgeBase::default());
        app.insert_resource(PendingKnowledgeWriteHooks::default());
        app.insert_resource(EntityIndex::default());
        app.add_systems(Update, command_parse_system);

        let channel = ChannelId {
            frontend: FrontendKind::Tui,
            user_id: "test".to_string(),
            thread_id: None,
        };
        let now = chrono::Utc::now();
        app.world_mut().spawn((
            Task {
                id: uuid::Uuid::new_v4(),
                content: "active task".to_string(),
                creator: uuid::Uuid::nil(),
                delegate: None,
                status: TaskStatus::Running,
                pending_confirmation_id: None,
                input_summary: "test".to_string(),
                result_summary: String::new(),
                priority: 0,
                created_at: now,
                updated_at: now,
                retry_count: 0,
                max_retries: 3,
                next_retry_at: None,
                last_error: None,
                multi_turn: false,
                parent_task_id: None,
                batch_id: None,
                origin_channel: Some(channel.clone()),
                routing_policy: crate::domain::TaskRoutingPolicy::conversational(channel.clone()),
                last_evaluated_turn: None,
            },
            ShortTermMemory::default(),
        ));

        app.world_mut().spawn(UserInputMessage {
            content: "/skill".to_string(),
            origin_channel: channel,
        });

        app.update();

        let skill_count = app
            .world_mut()
            .query::<&SkillCreationRequestMessage>()
            .iter(app.world())
            .count();
        assert_eq!(
            skill_count, 0,
            "/skill with no intent should not spawn request"
        );
    }

    #[test]
    fn clear_does_not_clear_other_channel_task() {
        use crate::domain::{ClearTaskMessage, FrontendKind, Task, TaskStatus};

        let mut app = App::new();
        app.insert_resource(MemoryConfig::default());
        app.insert_resource(SharedKnowledgeBase::default());
        app.insert_resource(PendingKnowledgeWriteHooks::default());
        app.insert_resource(EntityIndex::default());
        app.add_systems(Update, command_parse_system);

        let qq_channel = ChannelId {
            frontend: FrontendKind::QQ,
            user_id: "qq-user".to_string(),
            thread_id: None,
        };
        let now = chrono::Utc::now();
        app.world_mut().spawn((
            Task {
                id: uuid::Uuid::new_v4(),
                content: "qq active task".to_string(),
                creator: uuid::Uuid::nil(),
                delegate: None,
                status: TaskStatus::Ready,
                pending_confirmation_id: None,
                input_summary: "qq".to_string(),
                result_summary: String::new(),
                priority: 0,
                created_at: now,
                updated_at: now,
                retry_count: 0,
                max_retries: 3,
                next_retry_at: None,
                last_error: None,
                multi_turn: false,
                parent_task_id: None,
                batch_id: None,
                origin_channel: Some(qq_channel.clone()),
                routing_policy: crate::domain::TaskRoutingPolicy::conversational(qq_channel),
                last_evaluated_turn: None,
            },
            ShortTermMemory::default(),
        ));

        let tg_channel = ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "tg-user".to_string(),
            thread_id: None,
        };
        app.world_mut().spawn(UserInputMessage {
            content: "/clear".to_string(),
            origin_channel: tg_channel,
        });

        app.update();

        let clear_count = app
            .world_mut()
            .query::<&ClearTaskMessage>()
            .iter(app.world())
            .count();
        assert_eq!(clear_count, 0, "Telegram /clear should not touch QQ task");
    }
}
