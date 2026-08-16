mod common;

use std::sync::Arc;

use common::mock_executor::BrainAwareEchoExecutor;
use crossbeam_channel::unbounded;
use harness::{
    Agent, AgentCapabilities, AgentExecutor, AgentKind, AgentProfile, AgentToolPermissions,
    ChannelId, EntityIndex, ExternalInput, FrontendKind, HarnessConfig, LongTermMemory,
    ShortTermMemory, Task, TaskRoutingPolicy, TaskStatus, WaitingReason, build_harness_app,
    llm::ExecutorRegistry,
};

fn default_channel() -> ChannelId {
    ChannelId {
        frontend: FrontendKind::Tui,
        user_id: "default".to_string(),
        thread_id: None,
    }
}
use tokio::runtime::Runtime;

fn test_config() -> HarnessConfig {
    HarnessConfig::default()
}

/// Helper: spawn brain + default-llm-agent（统一 dispatch 架构要求）
///
/// 同时写入 `EntityIndex.agents`，模拟 `spawn_agent` 封装的索引维护，
/// 供 `dispatch_system` 等 O(1) 解析 AgentId → Entity（ADR-005 §3 阶段 2）。
fn spawn_default_agents(app: &mut bevy_app::App) {
    let brain_agent = Agent {
        id: uuid::Uuid::new_v4(),
        profile: AgentProfile {
            name: "brain".to_string(),
            model: "gpt-4.1-mini".to_string(),
        },
        capabilities: AgentCapabilities {
            tags: vec!["brain".to_string()],
            description: "Brain Agent".to_string(),
        },
        kind: AgentKind::Persistent,
        parent_id: None,
        bound_task_id: None,
        tool_permissions: AgentToolPermissions::default(),
        system_prompt: None,
    };
    let brain_id = brain_agent.id;
    let brain_entity = app
        .world_mut()
        .spawn((brain_agent, LongTermMemory::default()))
        .id();
    app.world_mut()
        .resource_mut::<EntityIndex>()
        .agents
        .insert(brain_id, brain_entity);

    let default_agent = Agent {
        id: uuid::Uuid::new_v4(),
        profile: AgentProfile {
            name: "default-llm-agent".to_string(),
            model: "gpt-4.1-mini".to_string(),
        },
        capabilities: AgentCapabilities {
            tags: vec!["llm".to_string(), "default".to_string()],
            description: "Default LLM Agent".to_string(),
        },
        kind: AgentKind::Persistent,
        parent_id: None,
        bound_task_id: None,
        tool_permissions: AgentToolPermissions::default(),
        system_prompt: None,
    };
    let default_id = default_agent.id;
    let default_entity = app
        .world_mut()
        .spawn((default_agent, LongTermMemory::default()))
        .id();
    app.world_mut()
        .resource_mut::<EntityIndex>()
        .agents
        .insert(default_id, default_entity);
}

#[test]
fn user_input_creates_new_task_when_no_waiting_task() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(BrainAwareEchoExecutor::new("echo"));
    let executor_registry = ExecutorRegistry::from_single_executor(executor, "default");
    let (input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(
        test_config(),
        runtime,
        executor_registry,
        input_rx,
        vec![],
        harness::channels::ChannelManager::empty().0,
    );

    app.update();

    input_tx
        .send(ExternalInput::TextWithChannel {
            channel: default_channel(),
            content: "new task".to_string(),
        })
        .unwrap();

    for _ in 0..5 {
        app.update();
    }

    let task_count = app.world_mut().query::<&Task>().iter(app.world()).count();

    assert!(task_count >= 1, "should create at least one task");
}

#[test]
fn user_input_continues_waiting_task() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(BrainAwareEchoExecutor::new("echo"));
    let executor_registry = ExecutorRegistry::from_single_executor(executor, "default");
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(
        test_config(),
        runtime,
        executor_registry,
        input_rx,
        vec![],
        harness::channels::ChannelManager::empty().0,
    );

    app.update();
    spawn_default_agents(&mut app);

    // Create a task in Waiting(User) state (multi-turn)
    let task_id = uuid::Uuid::new_v4();
    let task_entity = app
        .world_mut()
        .spawn((
            Task {
                id: task_id,
                content: "existing task".to_string(),
                creator: uuid::Uuid::nil(),
                delegate: None,
                status: TaskStatus::Waiting(WaitingReason::User),
                pending_confirmation_id: None,
                input_summary: "existing task".to_string(),
                result_summary: String::new(),
                priority: 0,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
                retry_count: 0,
                max_retries: 3,
                next_retry_at: None,
                last_error: None,
                multi_turn: true,
                parent_task_id: None,
                batch_id: None,
                origin_channel: Some(default_channel()),
                routing_policy: TaskRoutingPolicy::conversational(default_channel()),
                last_evaluated_turn: None,
            },
            ShortTermMemory::default(),
        ))
        .id();
    // 测试夹具绕过 spawn_task 封装直接 spawn，需手动写入 EntityIndex
    app.world_mut()
        .resource_mut::<EntityIndex>()
        .tasks
        .insert(task_id, task_entity);

    // Simulate user input
    app.world_mut().spawn(harness::UserInputMessage {
        content: "continue input".to_string(),
        origin_channel: default_channel(),
    });

    for _ in 0..10 {
        app.update();
    }

    // Should have exactly ONE task (the continued one, not a new one)
    let task_count = app.world_mut().query::<&Task>().iter(app.world()).count();
    assert_eq!(
        task_count, 1,
        "should have exactly one task, not create a new one"
    );

    let task = app
        .world_mut()
        .query::<&Task>()
        .iter(app.world())
        .find(|t| t.id == task_id)
        .cloned();

    assert!(task.is_some(), "original waiting task should still exist");
    // 多轮对话任务会在处理完响应后回到 Waiting(User) 状态
    let task = task.unwrap();
    assert!(
        !task.status.is_terminal(),
        "task should not be in terminal state, got {:?}",
        task.status
    );
}

#[test]
fn evaluation_triggered_on_turn_limit() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(BrainAwareEchoExecutor::new("echo"));
    let executor_registry = ExecutorRegistry::from_single_executor(executor, "default");
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(
        test_config(),
        runtime,
        executor_registry,
        input_rx,
        vec![],
        harness::channels::ChannelManager::empty().0,
    );

    // Configure evaluation with max_turns = 2
    app.insert_resource(harness::TaskEvaluationConfig {
        enabled: true,
        max_turns: Some(2),
        evaluator_agent_name: "evaluator".to_string(),
        offtrack_policy: harness::OffTrackPolicy::AskUser,
    });

    app.update();

    // Add evaluator agent
    app.world_mut().spawn(harness::Agent {
        id: uuid::Uuid::new_v4(),
        profile: harness::AgentProfile {
            name: "evaluator".to_string(),
            model: "gpt-4.1-mini".to_string(),
        },
        capabilities: harness::AgentCapabilities {
            tags: vec!["evaluation".to_string()],
            description: "evaluator agent".to_string(),
        },
        kind: harness::AgentKind::Persistent,
        parent_id: None,
        bound_task_id: None,
        tool_permissions: harness::AgentToolPermissions::default(),
        system_prompt: None,
    });

    // Create a task with turn_count = 2 (4 entries = 2 turns)
    let task_id = uuid::Uuid::new_v4();
    let mut stm = ShortTermMemory {
        entries: vec![],
        estimated_tokens: 100,
        summary_prefix: None,
        last_cached_tokens: None,
    };
    // 第一轮
    stm.add_entry(
        harness::EntryRole::User,
        "user message 1",
        harness::EntryMetadata::default(),
    );
    stm.add_entry(
        harness::EntryRole::Assistant,
        "assistant response 1",
        harness::EntryMetadata::default(),
    );
    // 第二轮
    stm.add_entry(
        harness::EntryRole::User,
        "user message 2",
        harness::EntryMetadata::default(),
    );
    stm.add_entry(
        harness::EntryRole::Assistant,
        "assistant response 2",
        harness::EntryMetadata::default(),
    );

    app.world_mut().spawn((
        Task {
            id: task_id,
            content: "test task".to_string(),
            creator: uuid::Uuid::nil(),
            delegate: None,
            status: TaskStatus::Running,
            pending_confirmation_id: None,
            input_summary: "test".to_string(),
            result_summary: String::new(),
            priority: 0,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            retry_count: 0,
            max_retries: 3,
            next_retry_at: None,
            last_error: None,
            multi_turn: true,
            parent_task_id: None,
            batch_id: None,
            origin_channel: Some(default_channel()),
            routing_policy: TaskRoutingPolicy::conversational(default_channel()),
            last_evaluated_turn: None,
        },
        stm,
    ));

    app.update();

    // Check for WorkItem instead of EvaluationRequestMessage
    let has_evaluation_workitem = app
        .world_mut()
        .query::<&harness::WorkItem>()
        .iter(app.world())
        .any(|wi| wi.work_type == harness::WorkItemType::Evaluation);

    // This test verifies the trigger logic creates WorkItem
    assert!(
        has_evaluation_workitem,
        "should create evaluation workitem when evaluator agent exists"
    );
}

/// 验证多个 Waiting(User) 任务时，用户输入只路由到其中一个。
/// 接收输入的任务会完成一轮对话后回到 Waiting(User)，但只有它的 STM 会包含新条目。
#[test]
fn multiple_waiting_user_tasks_routes_to_one() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(BrainAwareEchoExecutor::new("echo"));
    let executor_registry = ExecutorRegistry::from_single_executor(executor, "default");
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(
        test_config(),
        runtime,
        executor_registry,
        input_rx,
        vec![],
        harness::channels::ChannelManager::empty().0,
    );

    app.update();
    spawn_default_agents(&mut app);

    // 创建两个 Waiting(User) 状态的任务
    let task_id_1 = uuid::Uuid::new_v4();
    let task_entity_1 = app
        .world_mut()
        .spawn((
            Task {
                id: task_id_1,
                content: "first waiting task".to_string(),
                creator: uuid::Uuid::nil(),
                delegate: None,
                status: TaskStatus::Waiting(WaitingReason::User),
                pending_confirmation_id: None,
                input_summary: "first".to_string(),
                result_summary: String::new(),
                priority: 0,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
                retry_count: 0,
                max_retries: 3,
                next_retry_at: None,
                last_error: None,
                multi_turn: true,
                parent_task_id: None,
                batch_id: None,
                origin_channel: Some(default_channel()),
                routing_policy: TaskRoutingPolicy::conversational(default_channel()),
                last_evaluated_turn: None,
            },
            ShortTermMemory::default(),
        ))
        .id();
    // 测试夹具绕过 spawn_task 封装直接 spawn，需手动写入 EntityIndex
    app.world_mut()
        .resource_mut::<EntityIndex>()
        .tasks
        .insert(task_id_1, task_entity_1);

    let task_id_2 = uuid::Uuid::new_v4();
    let task_entity_2 = app
        .world_mut()
        .spawn((
            Task {
                id: task_id_2,
                content: "second waiting task".to_string(),
                creator: uuid::Uuid::nil(),
                delegate: None,
                status: TaskStatus::Waiting(WaitingReason::User),
                pending_confirmation_id: None,
                input_summary: "second".to_string(),
                result_summary: String::new(),
                priority: 0,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
                retry_count: 0,
                max_retries: 3,
                next_retry_at: None,
                last_error: None,
                multi_turn: true,
                parent_task_id: None,
                batch_id: None,
                origin_channel: Some(default_channel()),
                routing_policy: TaskRoutingPolicy::conversational(default_channel()),
                last_evaluated_turn: None,
            },
            ShortTermMemory::default(),
        ))
        .id();
    // 测试夹具绕过 spawn_task 封装直接 spawn，需手动写入 EntityIndex
    app.world_mut()
        .resource_mut::<EntityIndex>()
        .tasks
        .insert(task_id_2, task_entity_2);

    // 模拟用户输入
    app.world_mut().spawn(harness::UserInputMessage {
        content: "hello".to_string(),
        origin_channel: default_channel(),
    });

    for _ in 0..10 {
        app.update();
    }

    // 验证只有一个任务的 STM 包含 "hello"（即接收了用户输入）
    let tasks_with_input: Vec<_> = app
        .world_mut()
        .query::<(&Task, &ShortTermMemory)>()
        .iter(app.world())
        .filter(|(_, stm)| stm.entries.iter().any(|e| e.content == "hello"))
        .collect();

    assert_eq!(
        tasks_with_input.len(),
        1,
        "exactly one task should have received the user input"
    );
    assert_eq!(
        tasks_with_input[0].0.id, task_id_1,
        "the first Waiting(User) task should receive the input"
    );
}

/// 验证 /finish 命令能结束多轮对话任务。
#[test]
fn finish_command_ends_multi_turn_conversation() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(BrainAwareEchoExecutor::new("echo"));
    let executor_registry = ExecutorRegistry::from_single_executor(executor, "default");
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(
        test_config(),
        runtime,
        executor_registry,
        input_rx,
        vec![],
        harness::channels::ChannelManager::empty().0,
    );

    app.update();

    // 创建 Waiting(User) 状态的多轮对话任务
    let task_id = uuid::Uuid::new_v4();
    let task_entity = app
        .world_mut()
        .spawn((
            Task {
                id: task_id,
                content: "active multi-turn task".to_string(),
                creator: uuid::Uuid::nil(),
                delegate: None,
                status: TaskStatus::Waiting(WaitingReason::User),
                pending_confirmation_id: None,
                input_summary: "active task".to_string(),
                result_summary: String::new(),
                priority: 0,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
                retry_count: 0,
                max_retries: 3,
                next_retry_at: None,
                last_error: None,
                multi_turn: true,
                parent_task_id: None,
                batch_id: None,
                origin_channel: Some(default_channel()),
                routing_policy: TaskRoutingPolicy::conversational(default_channel()),
                last_evaluated_turn: None,
            },
            ShortTermMemory::default(),
        ))
        .id();
    // 经 spawn 后同步写 EntityIndex（模拟 spawn_task 封装的索引维护），
    // 供 finish_task_system 等 O(1) 解析 TaskId → Entity。
    app.world_mut()
        .resource_mut::<harness::ecs::EntityIndex>()
        .tasks
        .insert(task_id, task_entity);

    // 模拟用户输入 /finish
    app.world_mut().spawn(harness::UserInputMessage {
        content: "/finish".to_string(),
        origin_channel: default_channel(),
    });

    for _ in 0..10 {
        app.update();
    }

    // 验证任务已终止
    let task = app
        .world_mut()
        .query::<&Task>()
        .iter(app.world())
        .find(|t| t.id == task_id);

    if let Some(task) = task {
        assert!(
            task.status.is_terminal(),
            "task should be in terminal state after /finish, got {:?}",
            task.status
        );
    }
    // 任务可能已被清理，找不到也说明已正常终止
}
