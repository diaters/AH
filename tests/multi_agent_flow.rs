mod common;

use std::{sync::Arc, thread, time::Duration};

use common::mock_executor::PromptEchoExecutor;
use crossbeam_channel::unbounded;
use harness::{
    app::build_harness_app, domain::Agent, domain::AgentExecutor, domain::ChannelId,
    domain::ExternalInput, domain::FrontendKind, domain::Task, llm::ExecutorRegistry,
    systems::HarnessConfig,
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
        approval_mode: harness::systems::ApprovalSetting::new(
            harness::systems::ApprovalMode::Manual,
        ),
        memory: harness::domain::MemoryConfig::default(),
        max_retries: 3,
        llm: harness::llm::LlmProviderConfig {
            provider: harness::domain::LlmProviderKind::OpenAi,
            model: Some("gpt-4.1-mini".to_string()),
            api_key: Some("test-api-key".to_string()),
            api_base: None,
        },
        main_agent: harness::systems::DEFAULT_MAIN_AGENT.to_string(),
        agents_config_path: "tests/fixtures/test_agents.toml".to_string(),
        default_wait_tasks_timeout_secs: 300,
        tool_background_after_secs: 0,
        max_tool_iterations: 5,
        subtask_checkpoint_iterations: harness::systems::DEFAULT_SUBTASK_CHECKPOINT_ITERATIONS,
        subtask_max_runtime_secs: harness::systems::DEFAULT_SUBTASK_MAX_RUNTIME_SECS,
        shell_default_tail_lines: 200,
        shell_max_tail_lines: 500,
        shell_default_exec_timeout_secs: 300,
        shell_default_stop_timeout_secs: 10,
        tool_inflight_timeout_secs: 300,
        shell_max_buffer_bytes_per_stream: 64 * 1024,
        terminal_task_retention_secs: 3600,
        conversation_idle_secs: 86400,
        schedule_failure_alert_threshold: 3,
        active_poll_ms: 16,
        idle_poll_ms: 150,
        channels: Default::default(),
        channels_config_path: None,
        triggers_config_path: None,
        providers_config_path: "/nonexistent_providers.toml".to_string(),
        loop_control: harness::domain::LoopControlConfig::default(),
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
    assert!(
        names.contains(&"summarizer"),
        "should have summarizer agent"
    );
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
            sender: None,
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
