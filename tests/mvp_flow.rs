mod common;

use std::{sync::Arc, thread, time::Duration};

use common::mock_executor::PromptEchoExecutor;
use crossbeam_channel::unbounded;
use harness::{
    app::build_harness_app, domain::Agent, domain::AgentCapabilities, domain::AgentExecutor,
    domain::AgentKind, domain::AgentProfile, domain::AgentToolPermissions, domain::ChannelId,
    domain::DispatchHint, domain::DispatchKind, domain::DispatchStrategy, domain::FrontendKind,
    domain::LongTermMemory, domain::PendingDispatch, domain::Task, domain::TaskStatus,
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

fn test_config() -> HarnessConfig {
    HarnessConfig {
        max_retries: 3,
        llm: harness::llm::LlmProviderConfig {
            provider: harness::domain::LlmProviderKind::OpenAi,
            model: Some("gpt-4.1-mini".to_string()),
            api_key: Some("test-api-key".to_string()),
            api_base: None,
        },
        brain: None,
        agents_config_path: "/nonexistent_agents.toml".to_string(),
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
        providers_config_path: "/nonexistent_providers.toml".to_string(),
    }
}

/// Helper function to spawn a default agent for tests
fn spawn_default_agent(app: &mut bevy_app::App) {
    app.world_mut().spawn((
        Agent {
            id: harness::domain::AgentId::new(),
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
        },
        LongTermMemory::default(),
    ));
}

/// 验证单轮输入可以沿着 MVP 主链路完成闭环。
#[test]
fn completes_single_turn_conversation_flow() {
    let runtime = Arc::new(Runtime::new().expect("runtime should be created"));
    let executor: Arc<dyn AgentExecutor> = Arc::new(PromptEchoExecutor);
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

    // 初始化应用
    app.update();
    spawn_default_agent(&mut app);

    // 创建一个 Ready 状态的任务（单轮场景）
    // 注：统一 dispatch_system 要求 Task 携带 PendingDispatch 才会派发，
    // 这里附加 PendingDispatch(DirectDelegate) 直接委派给 default-llm-agent
    // （test_config 中 brain: None，无法走 BrainLlm 路径）。
    let task = Task::from_user_input_ready("你好，Harness", 3, default_channel());
    app.world_mut().spawn((
        task,
        harness::domain::ShortTermMemory::default(),
        PendingDispatch {
            kind: DispatchKind::Task,
            hint: DispatchHint {
                strategy: DispatchStrategy::DirectDelegate,
                preferred_agent_name: Some("default-llm-agent".to_string()),
                required_skill_id: None,
                agent_spawn_spec: None,
            },
        },
    ));

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
    assert_eq!(tasks[0].status(), &TaskStatus::Done);
    assert_eq!(
        tasks[0].result_summary,
        format!(
            "echo: [Current channel]\nchannel=tui, chat_id=default\n\nWhen the user asks to send a file or message back, use the `channel_send` tool with channel='tui' and omit the target; {}\n\n[Current request]\n你好，Harness",
            harness::domain::ATTACHMENT_MARKER_SYNTAX_HINT
        )
    );
}
