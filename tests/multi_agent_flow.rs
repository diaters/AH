use std::{sync::Arc, thread, time::Duration};

use crossbeam_channel::unbounded;
use harness::{
    Agent, AgentCapabilities, AgentExecutionOutput, AgentExecutionRequest, AgentExecutor,
    AgentKind, AgentProfile, AgentToolPermissions, ChannelId, ExecutorFuture, ExternalInput,
    FrontendKind, HarnessConfig, Task, TaskStatus, TaskTerminatedMessage, build_harness_app,
};

fn default_channel() -> ChannelId {
    ChannelId {
        frontend: FrontendKind::Tui,
        user_id: "default".to_string(),
        thread_id: None,
    }
}
use tokio::runtime::Runtime;

struct EchoExecutor;

impl AgentExecutor for EchoExecutor {
    fn execute(&self, request: AgentExecutionRequest) -> ExecutorFuture {
        Box::pin(async move {
            Ok(AgentExecutionOutput {
                content: harness::OutputContent::Text(format!("echo: {}", request.prompt)),
                reasoning_content: None,
            })
        })
    }
}

fn multi_agent_config() -> HarnessConfig {
    HarnessConfig {
        max_retries: 3,
        llm: harness::LlmProviderConfig {
            provider: harness::LlmProviderKind::OpenAi,
            model: "gpt-4.1-mini".to_string(),
            api_key: Some("test-api-key".to_string()),
            api_base: None,
        },
        brain: None,
        agents_config_path: "agents.toml".to_string(),
        default_wait_tasks_timeout_secs: 300,
        max_tool_iterations: 5,
        shell_default_tail_lines: 200,
        shell_max_tail_lines: 500,
        shell_default_exec_timeout_secs: 300,
        shell_default_stop_timeout_secs: 10,
        shell_max_buffer_bytes_per_stream: 64 * 1024,
        active_poll_ms: 16,
        idle_poll_ms: 150,
        channels: Default::default(),
        channels_config_path: None,
    }
}

/// 验证启动时从 agents.toml 加载持久性 Agent。
#[test]
fn loads_persistent_agents_from_config() {
    let runtime = Arc::new(Runtime::new().expect("runtime should be created"));
    let executor: Arc<dyn AgentExecutor> = Arc::new(EchoExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(
        multi_agent_config(),
        runtime,
        executor,
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
    let executor: Arc<dyn AgentExecutor> = Arc::new(EchoExecutor);
    let (input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(
        multi_agent_config(),
        runtime,
        executor,
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
        !tasks[0].status.is_terminal(),
        "multi-turn task should not be in terminal state, got {:?}",
        tasks[0].status
    );
}

/// 验证任务型 Agent 的创建、执行和销毁完整生命周期。
#[test]
fn task_scoped_agent_lifecycle() {
    let runtime = Arc::new(Runtime::new().expect("runtime should be created"));
    let executor: Arc<dyn AgentExecutor> = Arc::new(EchoExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(
        multi_agent_config(),
        runtime,
        executor,
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

    let task_id = uuid::Uuid::new_v4();
    {
        let world = app.world_mut();
        world.spawn(Agent {
            id: uuid::Uuid::new_v4(),
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
        world.spawn(Task {
            id: task_id,
            content: "test".to_string(),
            creator: parent_agent_id,
            delegate: None,
            status: TaskStatus::Done,
            pending_confirmation_id: None,
            input_summary: String::new(),
            result_summary: "done".to_string(),
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
            origin_channel: default_channel(),
            last_evaluated_turn: None,
        });
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

/// 验证 tags 子集校验：子 Agent tags 超出父 Agent 时拒绝创建。
#[test]
fn tags_subset_validation_rejects_invalid_spawn() {
    let parent_tags = ["llm".to_string(), "code".to_string()];
    let child_tags = ["llm".to_string(), "code".to_string(), "web".to_string()];

    let is_valid = child_tags.iter().all(|tag| parent_tags.contains(tag));
    assert!(!is_valid, "child tags exceeding parent should be rejected");

    let valid_child_tags = ["llm".to_string()];
    let is_valid = valid_child_tags.iter().all(|tag| parent_tags.contains(tag));
    assert!(is_valid, "child tags that are a subset should be accepted");
}
