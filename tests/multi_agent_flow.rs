mod common;

use std::{sync::Arc, thread, time::Duration};

use common::mock_executor::PromptEchoExecutor;
use crossbeam_channel::unbounded;
use harness::{
    app::build_harness_app, domain::Agent, domain::AgentCapabilities, domain::AgentExecutor,
    domain::AgentKind, domain::AgentProfile, domain::AgentToolPermissions, domain::ChannelId,
    domain::ExternalInput, domain::FrontendKind, domain::Task, domain::TaskTerminatedMessage,
    llm::ExecutorRegistry, systems::HarnessConfig,
};

fn default_channel() -> ChannelId {
    ChannelId {
        frontend: FrontendKind::Tui,
        user_id: "default".to_string(),
        thread_id: None,
    }
}
use tokio::runtime::Runtime;

fn multi_agent_config() -> HarnessConfig {
    HarnessConfig {
        memory: harness::domain::MemoryConfig::default(),
        max_retries: 3,
        llm: harness::llm::LlmProviderConfig {
            provider: harness::domain::LlmProviderKind::OpenAi,
            model: Some("gpt-4.1-mini".to_string()),
            api_key: Some("test-api-key".to_string()),
            api_base: None,
        },
        brain: None,
        agents_config_path: "tests/fixtures/test_agents.toml".to_string(),
        default_wait_tasks_timeout_secs: 300,
        max_tool_iterations: 5,
        shell_default_tail_lines: 200,
        shell_max_tail_lines: 500,
        shell_default_exec_timeout_secs: 300,
        shell_default_stop_timeout_secs: 10,
        tool_inflight_timeout_secs: 300,
        shell_max_buffer_bytes_per_stream: 64 * 1024,
        active_poll_ms: 16,
        idle_poll_ms: 150,
        channels: Default::default(),
        channels_config_path: None,
        triggers_config_path: None,
        providers_config_path: "providers.toml".to_string(),
    }
}

/// 验证启动时从 agents.toml 加载持久性 Agent。
#[test]
fn loads_persistent_agents_from_config() {
    let runtime = Arc::new(Runtime::new().expect("runtime should be created"));
    let executor: Arc<dyn AgentExecutor> = Arc::new(PromptEchoExecutor);
    let executor_registry = ExecutorRegistry::from_single_executor(executor, "default");
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(
        multi_agent_config(),
        runtime,
        executor_registry,
        input_rx,
        vec![],
        harness::channels::ChannelManager::empty().0,
    );

    app.update();

    let agents: Vec<Agent> = {
        let world = app.world_mut();
        let mut query = world.query::<&Agent>();
        query.iter(world).cloned().collect()
    };

    assert!(
        agents.len() >= 2,
        "should load at least 2 agents from config"
    );

    let names: Vec<&str> = agents.iter().map(|a| a.profile.name.as_str()).collect();
    assert!(
        names.contains(&"default-llm-agent"),
        "should have default agent"
    );
    assert!(names.contains(&"brain"), "should have brain agent");

    for agent in &agents {
        assert_eq!(agent.kind, AgentKind::Persistent);
        assert_eq!(agent.parent_id, None);
        assert_eq!(agent.bound_task_id, None);
    }
}

/// 验证 tags 匹配选择 Agent。
#[test]
fn selects_agent_by_tags_match() {
    let runtime = Arc::new(Runtime::new().expect("runtime should be created"));
    let executor: Arc<dyn AgentExecutor> = Arc::new(PromptEchoExecutor);
    let _executor_registry = ExecutorRegistry::from_single_executor(executor, "default");
    let (input_tx, input_rx) = unbounded();
    let executor: Arc<dyn AgentExecutor> = Arc::new(PromptEchoExecutor);
    let executor_registry = ExecutorRegistry::from_single_executor(executor, "default");
    let mut app = build_harness_app(
        multi_agent_config(),
        runtime,
        executor_registry,
        input_rx,
        vec![],
        harness::channels::ChannelManager::empty().0,
    );

    input_tx
        .send(ExternalInput::TextWithChannel {
            channel: default_channel(),
            content: "帮我写一段 general 代码".to_string(),
        })
        .expect("input should be accepted");

    for _ in 0..8 {
        app.update();
        thread::sleep(Duration::from_millis(20));
    }

    let tasks: Vec<Task> = {
        let world = app.world_mut();
        let mut query = world.query::<&Task>();
        query.iter(world).cloned().collect()
    };
    assert_eq!(tasks.len(), 1);
    // 多轮对话任务会在响应后回到 Waiting(User) 状态
    assert!(
        !tasks[0].status().is_terminal(),
        "multi-turn task should not be in terminal state, got {:?}",
        tasks[0].status()
    );
}

/// 验证任务型 Agent 的创建、执行和销毁完整生命周期。
#[test]
fn task_scoped_agent_lifecycle() {
    let runtime = Arc::new(Runtime::new().expect("runtime should be created"));
    let executor: Arc<dyn AgentExecutor> = Arc::new(PromptEchoExecutor);
    let _executor_registry = ExecutorRegistry::from_single_executor(executor, "default");
    let (_input_tx, input_rx) = unbounded();
    let executor: Arc<dyn AgentExecutor> = Arc::new(PromptEchoExecutor);
    let executor_registry = ExecutorRegistry::from_single_executor(executor, "default");
    let mut app = build_harness_app(
        multi_agent_config(),
        runtime,
        executor_registry,
        input_rx,
        vec![],
        harness::channels::ChannelManager::empty().0,
    );

    app.update();

    let parent_agent_id = {
        let world = app.world_mut();
        let mut query = world.query::<&Agent>();
        let default_agent = query
            .iter(world)
            .find(|a| a.profile.name == "default-llm-agent")
            .expect("default agent should exist");
        default_agent.id
    };

    let task_id = harness::domain::TaskId::new();
    {
        let world = app.world_mut();
        world.spawn(Agent {
            id: harness::domain::AgentId::new(),
            profile: AgentProfile {
                name: "sub-agent".to_string(),
                model: "gpt-4.1-mini".to_string(),
            },
            capabilities: AgentCapabilities {
                tags: vec!["llm".to_string()],
                description: "子 Agent".to_string(),
            },
            kind: AgentKind::TaskScoped,
            parent_id: Some(parent_agent_id),
            bound_task_id: Some(task_id),
            tool_permissions: AgentToolPermissions::default(),
            system_prompt: None,
        });
    }

    let task_scoped_count = {
        let world = app.world_mut();
        let mut query = world.query::<&Agent>();
        query
            .iter(world)
            .filter(|a| a.kind == AgentKind::TaskScoped)
            .count()
    };
    assert_eq!(task_scoped_count, 1);

    {
        let world = app.world_mut();
        let mut task = Task::from_user_input("test".to_string(), 3, default_channel());
        task.id = task_id;
        task.creator = parent_agent_id;
        task.mark_done("done".to_string(), chrono::Utc::now());
        let task_entity = world.spawn(task).id();
        // 经 spawn 后同步写 EntityIndex（模拟 spawn_task 封装的索引维护），
        // 供 maintenance::handle_termination 等 O(1) 解析 TaskId → Entity（ADR-005 §3 阶段 2）。
        world
            .resource_mut::<harness::ecs::EntityIndex>()
            .tasks
            .insert(task_id, task_entity);
        world.spawn(TaskTerminatedMessage { task_id });
    }

    app.update();

    let task_scoped_count = {
        let world = app.world_mut();
        let mut query = world.query::<&Agent>();
        query
            .iter(world)
            .filter(|a| a.kind == AgentKind::TaskScoped)
            .count()
    };
    assert_eq!(
        task_scoped_count, 0,
        "task-scoped agent should be despawned after task termination"
    );
}
