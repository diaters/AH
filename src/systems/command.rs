use crate::prelude::*;
use tracing::debug;

use crate::domain::MemoryConfig;
use crate::domain::{
    ClearTaskMessage, CreateTaskMessage, FinishTaskMessage, ReloadPluginsMessage,
    ReloadTriggersMessage, ShortTermMemory, StopTaskMessage, SummarizationRequestMessage,
    SummarizationTrigger, Task, TaskRoutingPolicy, UserCommand, UserInputMessage,
};

/// 命令解析系统：解析用户输入中的指令
#[allow(clippy::too_many_arguments)]
pub(crate) fn command_parse_system(
    mut commands: Commands,
    config: Res<MemoryConfig>,
    user_inputs: Query<(Entity, &UserInputMessage), Without<crate::domain::InputBlocked>>,
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

        if cmd.is_command() {
            let roots: Vec<_> = tasks
                .iter()
                .filter(|(task, _)| {
                    super::conversation::is_user_root(task, &input.origin_channel)
                        && !task.status.is_terminal()
                })
                .collect();
            if roots.len() > 1 {
                tracing::warn!(event = "ConversationSlotConflict", channel = ?input.origin_channel,
                    task_ids = ?roots.iter().map(|(task, _)| task.id).collect::<Vec<_>>(),
                    "multiple unfinished user roots reject ambiguous command");
                commands.spawn(crate::domain::SystemOutputMessage {
                    task_id: roots[0].0.id,
                    content: "此通道存在多个未结束的用户任务，无法确定命令归属；请先修复冲突任务。"
                        .into(),
                });
                commands.entity(entity).despawn();
                continue;
            }
        }

        match cmd {
            UserCommand::NewTask { topic } => {
                let roots: Vec<_> = tasks
                    .iter()
                    .filter(|(task, _)| {
                        super::conversation::is_user_root(task, &input.origin_channel)
                            && !task.status.is_terminal()
                    })
                    .collect();
                if let Some((task, _)) = roots.first() {
                    commands.spawn(crate::domain::SystemOutputMessage {
                        task_id: task.id,
                        content: "当前通道已有未结束的任务；请先用 /finish 结束，再开启新任务。"
                            .to_string(),
                    });
                } else {
                    commands.spawn(CreateTaskMessage {
                        content: if topic.is_empty() {
                            input.content.clone()
                        } else {
                            topic
                        },
                        origin_channel: Some(input.origin_channel.clone()),
                        routing_policy: TaskRoutingPolicy::conversational(
                            input.origin_channel.clone(),
                        ),
                        initiator: input.sender.clone(),
                    });
                }
                commands.entity(entity).despawn();
            }
            UserCommand::FinishCurrentTask => {
                // /finish - 结束当前任务
                let current_task = tasks.iter().find(|(t, _)| {
                    !t.status.is_terminal()
                        && super::conversation::is_user_root(t, &input.origin_channel)
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
            UserCommand::StopCurrentTask => {
                // /stop - 中断同通道唯一用户顶层任务的当前回合（保留上下文）
                let target = tasks.iter().find(|(t, _)| {
                    !t.status.is_terminal()
                        && t.parent_task_id.is_none()
                        && super::conversation::is_user_root(t, &input.origin_channel)
                });
                if let Some((task, _)) = target {
                    debug!(
                        event = "StopCommandReceived",
                        task_id = %task.id,
                        task_status = ?task.status,
                        "stopping current turn via /stop command"
                    );
                    commands.spawn(StopTaskMessage { task_id: task.id });
                } else {
                    debug!(event = "StopCommandNoTask", "no active task to stop");
                }
                commands.entity(entity).despawn();
            }
            UserCommand::Summarize => {
                // /summarize - 触发总结
                let active_task = tasks.iter().find(|(t, _)| {
                    !t.status.is_terminal()
                        && super::conversation::is_user_root(t, &input.origin_channel)
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
                        && super::conversation::is_user_root(t, &input.origin_channel)
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

    // 执行重载（user_plugins 侧完成 registry 与技能贡献的重扫），
    // 随后由 tools 系统主动拉取 registry 注册插件工具（方向反转）。
    crate::user_plugins::reload::reload_plugins(world);
    crate::systems::tools::register_plugin_tools_in_world(world);

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
    use crate::ecs::EntityIndex;
    use crate::{
        domain::MemoryConfig,
        domain::{
            ChannelId, CreateTaskMessage, ShortTermMemory, Task, TaskStatus, UserCommand,
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
    fn removed_remember_and_skill_are_plain_text() {
        for input in [
            "/remember Docs should stay in Chinese",
            "/skill code review",
        ] {
            assert_eq!(
                UserCommand::parse(input),
                UserCommand::PlainText(input.to_string())
            );
        }
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
                id: crate::domain::TaskId::new(),
                content: "qq active task".to_string(),
                creator: crate::domain::AgentId::nil(),
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
            sender: None,
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
    fn btw_rejects_new_topic_when_same_channel_task_is_unfinished() {
        use crate::domain::FrontendKind;

        let mut app = App::new();
        app.insert_resource(MemoryConfig::default());
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
                id: crate::domain::TaskId::new(),
                content: "qq parent".to_string(),
                creator: crate::domain::AgentId::nil(),
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
            sender: None,
            content: "/btw child topic".to_string(),
            origin_channel: qq_channel.clone(),
        });

        app.update();

        assert_eq!(
            app.world_mut().query::<&Task>().iter(app.world()).count(),
            1
        );
        assert_eq!(
            app.world_mut()
                .query::<&CreateTaskMessage>()
                .iter(app.world())
                .count(),
            0
        );
        let replies: Vec<_> = app
            .world_mut()
            .query::<&crate::domain::SystemOutputMessage>()
            .iter(app.world())
            .map(|message| message.content.clone())
            .collect();
        assert!(replies.iter().any(|reply| reply.contains("/finish")));
    }

    #[test]
    fn finish_does_not_finish_other_channel_task() {
        use crate::domain::{FinishTaskMessage, FrontendKind, Task, TaskStatus};

        let mut app = App::new();
        app.insert_resource(MemoryConfig::default());
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
                id: crate::domain::TaskId::new(),
                content: "qq active task".to_string(),
                creator: crate::domain::AgentId::nil(),
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
            sender: None,
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
                id: crate::domain::TaskId::new(),
                content: "qq active task".to_string(),
                creator: crate::domain::AgentId::nil(),
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
            sender: None,
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
        app.insert_resource(EntityIndex::default());
        app.add_systems(Update, command_parse_system);

        let channel = ChannelId {
            frontend: FrontendKind::Tui,
            user_id: "test".to_string(),
            thread_id: None,
        };
        let now = chrono::Utc::now();
        let task_id = crate::domain::TaskId::new();
        app.world_mut().spawn((
            Task {
                id: task_id,
                content: "active task".to_string(),
                creator: crate::domain::AgentId::nil(),
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
            sender: None,
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
    fn clear_does_not_clear_other_channel_task() {
        use crate::domain::{ClearTaskMessage, FrontendKind, Task, TaskStatus};

        let mut app = App::new();
        app.insert_resource(MemoryConfig::default());
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
                id: crate::domain::TaskId::new(),
                content: "qq active task".to_string(),
                creator: crate::domain::AgentId::nil(),
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
            sender: None,
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
