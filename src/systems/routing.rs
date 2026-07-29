use crate::prelude::*;
use tracing::debug;

use crate::ecs::EntityIndex;
use crate::{
    app::Clock,
    domain::{
        Agent, AgentKind, ContinueTaskMessage, CreateTaskMessage, DispatchHint, DispatchKind,
        DispatchStrategy, EntryMetadata, EntryRole, PendingDispatch, ShortTermMemory,
        SystemOutputMessage, Task, TaskRoutingPolicy, TaskStatus, ToolConfirmationResponseMessage,
        UserCommand, UserInputMessage, WaitingReason,
    },
};

fn parse_confirmation_option(content: &str) -> Option<String> {
    match content.trim().to_lowercase().as_str() {
        "1" => Some("allow_once".to_string()),
        "2" => Some("allow_always".to_string()),
        "3" => Some("deny".to_string()),
        _ => None,
    }
}

/// 用户输入路由系统：判断是创建新任务还是继续现有任务
pub(crate) fn user_input_routing_system(
    mut commands: Commands,
    user_inputs: Query<(Entity, &UserInputMessage)>,
    tasks: Query<&Task>,
) {
    for (entity, input) in &user_inputs {
        // 命令优先（即使在等待工具确认期间也允许 /finish 等指令）
        if UserCommand::parse(&input.content).is_command() {
            continue; // 跳过，由 command_parse_system 处理
        }

        // 优先处理处于 Waiting(User) 且正在等待工具确认的任务
        if let Some(task) = tasks.iter().find(|t| {
            t.status == TaskStatus::Waiting(WaitingReason::User)
                && t.origin_channel == Some(input.origin_channel.clone())
                && t.pending_confirmation_id.is_some()
        }) {
            let pending_id = task
                .pending_confirmation_id
                .expect("pending id confirmed above");
            match parse_confirmation_option(&input.content) {
                Some(option_id) => {
                    debug!(
                        event = "RoutingDecision",
                        decision = "confirmation_response",
                        input = %input.content,
                        selected_option = %option_id,
                        request_id = %pending_id,
                        task_id = %task.id,
                        "routing input as tool confirmation response"
                    );
                    commands.spawn(ToolConfirmationResponseMessage {
                        request_id: pending_id,
                        selected_option: option_id,
                        feedback: None,
                    });
                }
                None => {
                    debug!(
                        event = "RoutingDecision",
                        decision = "confirmation_retry_prompt",
                        input = %input.content,
                        task_id = %task.id,
                        "invalid confirmation option, prompting retry"
                    );
                    commands.spawn(SystemOutputMessage {
                        task_id: task.id,
                        content: "请输入有效选项：1=仅本次允许，2=永久允许，3=拒绝".to_string(),
                    });
                }
            }
            commands.entity(entity).despawn();
            continue;
        }

        // 查找是否有 Waiting(User) 状态的任务
        let waiting_tasks: Vec<_> = tasks
            .iter()
            .filter(|t| {
                t.status == TaskStatus::Waiting(WaitingReason::User)
                    && t.origin_channel == Some(input.origin_channel.clone())
            })
            .collect();

        if let Some(task) = waiting_tasks.first() {
            debug!(
                event = "RoutingDecision",
                decision = "continue_existing",
                input = %input.content,
                input_len = input.content.len(),
                selected_task_id = %task.id,
                waiting_tasks_count = waiting_tasks.len(),
                input_channel = ?input.origin_channel,
                task_channel = ?task.origin_channel,
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
                origin_channel: Some(input.origin_channel.clone()),
                routing_policy: TaskRoutingPolicy::conversational(input.origin_channel.clone()),
            });
        }

        commands.entity(entity).despawn();
    }
}

/// 继续任务系统：将用户输入追加到任务
pub(crate) fn continue_task_system(
    mut commands: Commands,
    clock: Res<Clock>,
    index: ResMut<EntityIndex>,
    continue_messages: Query<(Entity, &ContinueTaskMessage)>,
    agents: Query<&Agent>,
    mut tasks: Query<(Entity, &mut Task, Option<&mut ShortTermMemory>)>,
) {
    for (entity, msg) in &continue_messages {
        // 经 EntityIndex O(1) 解析 TaskId → Entity（替代全量线性扫描）
        if let Some((task_entity, mut task, short_term)) = index
            .get_task(&msg.task_id)
            .and_then(|e| tasks.get_mut(e).ok())
        {
            let stm_entries_before = short_term.as_ref().map(|s| s.entries.len()).unwrap_or(0);
            let stm_tokens_before = short_term.as_ref().map(|s| s.estimated_tokens).unwrap_or(0);

            let old_status = task.status.clone();
            let prev_content = task.content.clone();
            let prev_delegate = task.delegate;
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
                    prev_delegate = ?prev_delegate,
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
                    prev_delegate = ?prev_delegate,
                    has_stm = false,
                    "task continued (no STM attached)"
                );
            }

            // 续轮派发语义（恢复 2026-06-07 设计意图，修正 477c1bb 的静默反转）：
            // TopLevelTask（parent_task_id == None）续轮默认复用上一轮 delegate，
            // 走 DirectDelegate，避免每轮重跑 Brain（省一次 LLM 决策）。
            // 无 delegate / delegate 指向的 agent 已不存在（stale） / SubTask：
            // 回退到 BrainLlm，与原行为一致。
            // 无论哪条路径都先清空 delegate：dispatch_system 的 BrainLlm 分支对
            // "已有 delegate" 的任务会跳过，置 None 可避免误判；DirectDelegate 分支
            // 会按 preferred_agent_name 重新 mark_waiting_for_agent 重设 delegate。
            task.delegate = None;
            let reuse_hint = if task.parent_task_id.is_none() {
                match prev_delegate.and_then(|id| {
                    agents
                        .iter()
                        .find(|a| a.id == id && a.kind == AgentKind::Persistent)
                }) {
                    Some(agent) => DispatchHint {
                        strategy: DispatchStrategy::DirectDelegate,
                        preferred_agent_name: Some(agent.profile.name.clone()),
                        required_skill_id: None,
                        agent_spawn_spec: None,
                    },
                    None => DispatchHint {
                        strategy: DispatchStrategy::BrainLlm,
                        preferred_agent_name: None,
                        required_skill_id: None,
                        agent_spawn_spec: None,
                    },
                }
            } else {
                DispatchHint {
                    strategy: DispatchStrategy::BrainLlm,
                    preferred_agent_name: None,
                    required_skill_id: None,
                    agent_spawn_spec: None,
                }
            };
            commands.entity(task_entity).insert(PendingDispatch {
                kind: DispatchKind::Task,
                hint: reuse_hint,
            });
        }

        commands.entity(entity).despawn();
    }
}

#[cfg(test)]
mod tests {
    use crate::prelude::*;

    use super::{continue_task_system, user_input_routing_system};
    use crate::app::{Clock, MemoryConfig};
    use crate::domain::{
        Agent, AgentCapabilities, AgentKind, AgentProfile, AgentToolPermissions, ChannelId,
        ContinueTaskMessage, CreateTaskMessage, DispatchHint, DispatchStrategy, FinishTaskMessage,
        FrontendKind, PendingDispatch, PendingKnowledgeWriteHooks, SharedKnowledgeBase,
        SystemOutputMessage, Task, TaskStatus, ToolConfirmationResponseMessage, UserInputMessage,
        WaitingReason,
    };
    use crate::ecs::EntityIndex;
    use crate::systems::command::command_parse_system;

    fn telegram_channel() -> ChannelId {
        ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "tg-user".to_string(),
            thread_id: None,
        }
    }

    fn qq_channel() -> ChannelId {
        ChannelId {
            frontend: FrontendKind::QQ,
            user_id: "qq-user".to_string(),
            thread_id: None,
        }
    }

    fn make_waiting_task(channel: ChannelId) -> Task {
        let now = chrono::Utc::now();
        Task {
            id: uuid::Uuid::new_v4(),
            content: "waiting".to_string(),
            creator: uuid::Uuid::nil(),
            delegate: None,
            status: TaskStatus::Waiting(WaitingReason::User),
            pending_confirmation_id: None,
            input_summary: String::new(),
            result_summary: String::new(),
            priority: 0,
            created_at: now,
            updated_at: now,
            retry_count: 0,
            max_retries: 3,
            next_retry_at: None,
            last_error: None,
            multi_turn: true,
            parent_task_id: None,
            batch_id: None,
            origin_channel: Some(channel.clone()),
            routing_policy: crate::domain::TaskRoutingPolicy::conversational(channel),
            last_evaluated_turn: None,
        }
    }

    fn make_waiting_task_with_confirmation(channel: ChannelId, pending_id: uuid::Uuid) -> Task {
        let mut task = make_waiting_task(channel);
        task.pending_confirmation_id = Some(pending_id);
        task
    }

    fn make_agent(id: uuid::Uuid, name: &str, kind: AgentKind) -> Agent {
        Agent {
            id,
            profile: AgentProfile {
                name: name.to_string(),
                model: "test-model".to_string(),
            },
            capabilities: AgentCapabilities {
                tags: vec![],
                description: String::new(),
            },
            kind,
            parent_id: None,
            bound_task_id: None,
            tool_permissions: AgentToolPermissions::default(),
            system_prompt: None,
        }
    }

    fn make_task_with_delegate(
        channel: ChannelId,
        delegate: Option<uuid::Uuid>,
        parent: Option<uuid::Uuid>,
    ) -> Task {
        let mut task = make_waiting_task(channel);
        task.delegate = delegate;
        task.parent_task_id = parent;
        task
    }

    /// 运行 continue_task_system，并返回被附加到 Task 上的 PendingDispatch hint（如有）。
    fn run_continue_and_get_hint(
        app: &mut App,
        task: Task,
        agent: Option<Agent>,
    ) -> Option<DispatchHint> {
        let task_id = task.id;
        // 经 spawn 后同步写 EntityIndex
        let entity = app.world_mut().spawn(task).id();
        app.world_mut()
            .resource_mut::<EntityIndex>()
            .tasks
            .insert(task_id, entity);
        if let Some(a) = agent {
            app.world_mut().spawn(a);
        }
        app.world_mut().spawn(ContinueTaskMessage {
            task_id,
            user_input: "继续上一轮".to_string(),
        });
        app.update();
        let pending: Vec<&PendingDispatch> = app
            .world_mut()
            .query::<&PendingDispatch>()
            .iter(app.world())
            .collect();
        pending.first().map(|p| p.hint.clone())
    }

    #[test]
    fn cross_channel_input_not_routed_to_other_channel_waiting_task() {
        let mut app = App::new();
        app.add_systems(Update, user_input_routing_system);

        // Telegram 通道的 Waiting(User) 任务
        app.world_mut().spawn(make_waiting_task(telegram_channel()));

        // QQ 通道的纯文本输入
        app.world_mut().spawn(UserInputMessage {
            content: "hello from QQ".to_string(),
            origin_channel: qq_channel(),
        });

        app.update();

        // 断言：应生成 CreateTaskMessage（而非 ContinueTaskMessage）
        let create_count = app
            .world_mut()
            .query::<&CreateTaskMessage>()
            .iter(app.world())
            .count();
        let continue_count = app
            .world_mut()
            .query::<&ContinueTaskMessage>()
            .iter(app.world())
            .count();
        assert_eq!(
            create_count, 1,
            "QQ input should create new task, not continue Telegram task"
        );
        assert_eq!(
            continue_count, 0,
            "no ContinueTaskMessage should be spawned"
        );
    }

    #[test]
    fn same_channel_input_routed_to_waiting_task() {
        let mut app = App::new();
        app.add_systems(Update, user_input_routing_system);

        let task = make_waiting_task(telegram_channel());
        let task_id = task.id;
        app.world_mut().spawn(task);

        app.world_mut().spawn(UserInputMessage {
            content: "hello from Telegram".to_string(),
            origin_channel: telegram_channel(),
        });

        app.update();

        let continue_msgs: Vec<&ContinueTaskMessage> = app
            .world_mut()
            .query::<&ContinueTaskMessage>()
            .iter(app.world())
            .collect();
        assert_eq!(continue_msgs.len(), 1);
        assert_eq!(continue_msgs[0].task_id, task_id);
    }

    #[test]
    fn text_confirmation_option_2_maps_to_allow_always() {
        let mut app = App::new();
        app.add_systems(Update, user_input_routing_system);

        let pending_id = uuid::Uuid::new_v4();
        let task = make_waiting_task_with_confirmation(telegram_channel(), pending_id);
        let task_id = task.id;
        app.world_mut().spawn(task);

        app.world_mut().spawn(UserInputMessage {
            content: "2".to_string(),
            origin_channel: telegram_channel(),
        });

        app.update();

        let responses: Vec<&ToolConfirmationResponseMessage> = app
            .world_mut()
            .query::<&ToolConfirmationResponseMessage>()
            .iter(app.world())
            .collect();
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0].request_id, pending_id);
        assert_eq!(responses[0].selected_option, "allow_always");

        let continues: Vec<&ContinueTaskMessage> = app
            .world_mut()
            .query::<&ContinueTaskMessage>()
            .iter(app.world())
            .collect();
        assert!(
            continues.is_empty(),
            "should not continue task while pending confirmation"
        );

        let outputs: Vec<&SystemOutputMessage> = app
            .world_mut()
            .query::<&SystemOutputMessage>()
            .iter(app.world())
            .collect();
        assert!(
            outputs.is_empty(),
            "should not prompt retry for valid option"
        );

        let tasks: Vec<&Task> = app.world_mut().query::<&Task>().iter(app.world()).collect();
        assert_eq!(tasks[0].id, task_id);
        assert_eq!(tasks[0].pending_confirmation_id, Some(pending_id));
    }

    #[test]
    fn invalid_confirmation_text_prompts_retry() {
        let mut app = App::new();
        app.add_systems(Update, user_input_routing_system);

        let pending_id = uuid::Uuid::new_v4();
        app.world_mut().spawn(make_waiting_task_with_confirmation(
            telegram_channel(),
            pending_id,
        ));

        app.world_mut().spawn(UserInputMessage {
            content: "hello".to_string(),
            origin_channel: telegram_channel(),
        });

        app.update();

        let outputs: Vec<&SystemOutputMessage> = app
            .world_mut()
            .query::<&SystemOutputMessage>()
            .iter(app.world())
            .collect();
        assert_eq!(outputs.len(), 1);
        assert!(outputs[0].content.contains("1=仅本次允许"));

        let responses: Vec<&ToolConfirmationResponseMessage> = app
            .world_mut()
            .query::<&ToolConfirmationResponseMessage>()
            .iter(app.world())
            .collect();
        assert!(
            responses.is_empty(),
            "should not spawn confirmation response for invalid input"
        );

        let new_tasks: Vec<&CreateTaskMessage> = app
            .world_mut()
            .query::<&CreateTaskMessage>()
            .iter(app.world())
            .collect();
        assert!(
            new_tasks.is_empty(),
            "should not create new task while pending confirmation"
        );

        let continues: Vec<&ContinueTaskMessage> = app
            .world_mut()
            .query::<&ContinueTaskMessage>()
            .iter(app.world())
            .collect();
        assert!(
            continues.is_empty(),
            "should not continue task while pending confirmation"
        );
    }

    #[test]
    fn no_pending_confirmation_routes_to_continue_task() {
        let mut app = App::new();
        app.add_systems(Update, user_input_routing_system);

        let task = make_waiting_task(telegram_channel());
        let task_id = task.id;
        app.world_mut().spawn(task);

        app.world_mut().spawn(UserInputMessage {
            content: "继续".to_string(),
            origin_channel: telegram_channel(),
        });

        app.update();

        let continues: Vec<&ContinueTaskMessage> = app
            .world_mut()
            .query::<&ContinueTaskMessage>()
            .iter(app.world())
            .collect();
        assert_eq!(continues.len(), 1);
        assert_eq!(continues[0].task_id, task_id);

        let responses: Vec<&ToolConfirmationResponseMessage> = app
            .world_mut()
            .query::<&ToolConfirmationResponseMessage>()
            .iter(app.world())
            .collect();
        assert!(
            responses.is_empty(),
            "should not spawn confirmation response without pending id"
        );

        let outputs: Vec<&SystemOutputMessage> = app
            .world_mut()
            .query::<&SystemOutputMessage>()
            .iter(app.world())
            .collect();
        assert!(
            outputs.is_empty(),
            "should not prompt retry without pending id"
        );
    }

    #[test]
    fn command_during_pending_confirmation_still_executes() {
        let mut app = App::new();
        app.add_systems(Update, (user_input_routing_system, command_parse_system));
        app.insert_resource(MemoryConfig::default());
        app.insert_resource(SharedKnowledgeBase::default());
        app.insert_resource(PendingKnowledgeWriteHooks::default());

        let pending_id = uuid::Uuid::new_v4();
        let task = make_waiting_task_with_confirmation(telegram_channel(), pending_id);
        app.world_mut().spawn(task);

        app.world_mut().spawn(UserInputMessage {
            content: "/finish".to_string(),
            origin_channel: telegram_channel(),
        });

        app.update();

        let outputs: Vec<&SystemOutputMessage> = app
            .world_mut()
            .query::<&SystemOutputMessage>()
            .iter(app.world())
            .collect();
        assert!(
            outputs.is_empty(),
            "should not treat command as invalid confirmation option"
        );

        let responses: Vec<&ToolConfirmationResponseMessage> = app
            .world_mut()
            .query::<&ToolConfirmationResponseMessage>()
            .iter(app.world())
            .collect();
        assert!(
            responses.is_empty(),
            "should not spawn confirmation response for command input"
        );

        let finishes: Vec<&FinishTaskMessage> = app
            .world_mut()
            .query::<&FinishTaskMessage>()
            .iter(app.world())
            .collect();
        assert_eq!(
            finishes.len(),
            1,
            "command_parse_system should handle /finish while pending confirmation"
        );
    }

    // ---- continue_task_system 续轮派发语义测试 ----

    #[test]
    fn continue_top_level_task_reuses_persistent_delegate() {
        let mut app = App::new();
        app.insert_resource(Clock::default());
        app.init_resource::<EntityIndex>();
        app.add_systems(Update, continue_task_system);

        let agent_id = uuid::Uuid::new_v4();
        let task = make_task_with_delegate(telegram_channel(), Some(agent_id), None);
        let agent = make_agent(agent_id, "reused-agent", AgentKind::Persistent);

        let hint = run_continue_and_get_hint(&mut app, task, Some(agent));
        let hint = hint.expect("PendingDispatch should be attached after continue");

        assert!(
            matches!(hint.strategy, DispatchStrategy::DirectDelegate),
            "TopLevelTask with existing persistent delegate should reuse it via DirectDelegate"
        );
        assert_eq!(
            hint.preferred_agent_name.as_deref(),
            Some("reused-agent"),
            "preferred_agent_name should be the reused agent's name"
        );

        // delegate 必须在重派发前被清空
        let tasks: Vec<&Task> = app.world_mut().query::<&Task>().iter(app.world()).collect();
        assert!(
            tasks.iter().all(|t| t.delegate.is_none()),
            "delegate must be cleared before re-dispatch"
        );
    }

    #[test]
    fn continue_top_level_task_with_missing_delegate_agent_falls_back_to_brain_llm() {
        let mut app = App::new();
        app.insert_resource(Clock::default());
        app.init_resource::<EntityIndex>();
        app.add_systems(Update, continue_task_system);

        let agent_id = uuid::Uuid::new_v4();
        let task = make_task_with_delegate(telegram_channel(), Some(agent_id), None);
        // 故意不 spawn 对应 agent（delegate 指向的 agent 已不存在 → stale）

        let hint = run_continue_and_get_hint(&mut app, task, None);
        let hint = hint.expect("PendingDispatch should be attached after continue");
        assert!(
            matches!(hint.strategy, DispatchStrategy::BrainLlm),
            "stale delegate should fall back to BrainLlm"
        );
    }

    #[test]
    fn continue_subtask_always_uses_brain_llm() {
        let mut app = App::new();
        app.insert_resource(Clock::default());
        app.init_resource::<EntityIndex>();
        app.add_systems(Update, continue_task_system);

        let agent_id = uuid::Uuid::new_v4();
        let parent_id = uuid::Uuid::new_v4();
        let task = make_task_with_delegate(telegram_channel(), Some(agent_id), Some(parent_id));
        let agent = make_agent(agent_id, "sub-agent", AgentKind::Persistent);

        let hint = run_continue_and_get_hint(&mut app, task, Some(agent));
        let hint = hint.expect("PendingDispatch should be attached after continue");
        assert!(
            matches!(hint.strategy, DispatchStrategy::BrainLlm),
            "SubTask must always re-run Brain, even with a valid persistent delegate"
        );
    }

    #[test]
    fn continue_task_without_delegate_uses_brain_llm() {
        let mut app = App::new();
        app.insert_resource(Clock::default());
        app.init_resource::<EntityIndex>();
        app.add_systems(Update, continue_task_system);

        let task = make_task_with_delegate(telegram_channel(), None, None);

        let hint = run_continue_and_get_hint(&mut app, task, None);
        let hint = hint.expect("PendingDispatch should be attached after continue");
        assert!(
            matches!(hint.strategy, DispatchStrategy::BrainLlm),
            "no delegate should fall back to BrainLlm"
        );
    }
}
