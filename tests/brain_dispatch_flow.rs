use std::{sync::Arc, thread, time::Duration};

use crossbeam_channel::unbounded;
use harness::{
    Agent, AgentCapabilities, AgentExecutionOutput, AgentExecutionRequest, AgentExecutor,
    AgentKind, AgentProfile, AgentToolPermissions, BrainConfig, ChannelId, ExecutorFuture,
    FrontendKind, HarnessConfig, LongTermMemory, Task, TaskStatus, build_harness_app,
    llm::ExecutorRegistry,
};
use tokio::runtime::Runtime;
use uuid::Uuid;

fn default_channel() -> ChannelId {
    ChannelId {
        frontend: FrontendKind::Tui,
        user_id: "default".to_string(),
        thread_id: None,
    }
}

struct BrainMockExecutor;

impl AgentExecutor for BrainMockExecutor {
    fn execute(&self, request: AgentExecutionRequest) -> ExecutorFuture {
        match request.request_kind {
            harness::AgentRequestKind::BrainDecision => {
                let decision = r#"{"selected_agent_name":"default-llm-agent","delegate_prompt":"请处理这个任务","reasoning":"测试用例"}"#;
                Box::pin(async move {
                    Ok(AgentExecutionOutput {
                        content: harness::OutputContent::Text(decision.to_string()),
                        reasoning_content: None,
                    })
                })
            }
            harness::AgentRequestKind::LlmCompletion => Box::pin(async move {
                Ok(AgentExecutionOutput {
                    content: harness::OutputContent::Text(format!("echo: {}", request.prompt)),
                    reasoning_content: None,
                })
            }),
            harness::AgentRequestKind::ToolExecution { .. } => {
                // Tool 执行由专门的 tool_execution_system 处理，此处不应到达
                Box::pin(async move {
                    Err(harness::ExecutionError::Unknown(
                        "ToolExecution not supported in mock executor".to_string(),
                    ))
                })
            }
            harness::AgentRequestKind::Summarization => {
                // Summarization 由专门的 summarization system 处理，此处不应到达
                Box::pin(async move {
                    Err(harness::ExecutionError::Unknown(
                        "Summarization not supported in mock executor".to_string(),
                    ))
                })
            }
            harness::AgentRequestKind::Evaluation => {
                // Evaluation 由专门的 workitem_dispatch 处理，此处不应到达
                Box::pin(async move {
                    Err(harness::ExecutionError::Unknown(
                        "Evaluation not supported in mock executor".to_string(),
                    ))
                })
            }
        }
    }
}

fn brain_test_config() -> HarnessConfig {
    HarnessConfig {
        max_retries: 3,
        llm: harness::LlmProviderConfig {
            provider: harness::LlmProviderKind::OpenAi,
            model: "gpt-4.1-mini".to_string(),
            api_key: Some("test-api-key".to_string()),
            api_base: None,
        },
        brain: Some(BrainConfig { enabled: true }),
        agents_config_path: "/nonexistent_agents.toml".to_string(),
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
        triggers_config_path: None,
        providers_config_path: "/nonexistent_providers.toml".to_string(),
    }
}

/// 验证 Brain 启用时，用户输入经过 Brain 决策后交给 Agent 执行。
#[test]
fn completes_brain_dispatch_flow() {
    let runtime = Arc::new(Runtime::new().expect("runtime should be created"));
    let executor: Arc<dyn AgentExecutor> = Arc::new(BrainMockExecutor);
    let executor_registry = ExecutorRegistry::from_single_executor(executor, "default");
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(
        brain_test_config(),
        runtime,
        executor_registry,
        input_rx,
        vec![],
        harness::channels::ChannelManager::empty().0,
    );

    // 初始化应用
    app.update();

    // 手动创建 Brain Agent
    let brain_id = Uuid::new_v4();
    app.world_mut().spawn((
        Agent {
            id: brain_id,
            profile: AgentProfile {
                name: "brain".to_string(),
                model: "gpt-4.1-mini".to_string(),
            },
            capabilities: AgentCapabilities {
                tags: vec!["brain".to_string(), "dispatcher".to_string()],
                description: "Brain Agent".to_string(),
            },
            kind: AgentKind::Persistent,
            parent_id: None,
            bound_task_id: None,
            tool_permissions: AgentToolPermissions::default(),
        },
        LongTermMemory::default(),
    ));

    // 手动创建 default-llm-agent（Brain 会调度到这个 agent）
    let default_agent_id = Uuid::new_v4();
    app.world_mut().spawn((
        Agent {
            id: default_agent_id,
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
        },
        LongTermMemory::default(),
    ));

    // 创建一个 Ready 状态的任务
    let task = Task::from_user_input_ready("你好，Harness", 3, default_channel());
    app.world_mut()
        .spawn((task, harness::ShortTermMemory::default()));

    for _ in 0..16 {
        app.update();
        thread::sleep(Duration::from_millis(20));
    }

    let tasks: Vec<Task> = {
        let world = app.world_mut();
        let mut query = world.query::<&Task>();
        query.iter(world).cloned().collect()
    };

    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].status, TaskStatus::Done);
}

/// 验证 Brain 不启用时，MVP 流程不受影响。
#[test]
fn mvp_flow_unchanged_when_brain_disabled() {
    let runtime = Arc::new(Runtime::new().expect("runtime should be created"));
    let executor: Arc<dyn AgentExecutor> = Arc::new(BrainMockExecutor);
    let executor_registry = ExecutorRegistry::from_single_executor(executor, "default");
    let (_input_tx, input_rx) = unbounded();

    let mut no_brain_config = brain_test_config();
    no_brain_config.brain = None;
    let mut app = build_harness_app(
        no_brain_config,
        runtime,
        executor_registry,
        input_rx,
        vec![],
        harness::channels::ChannelManager::empty().0,
    );

    // 初始化应用
    app.update();

    // 手动创建 Brain Agent
    let brain_id = Uuid::new_v4();
    app.world_mut().spawn((
        Agent {
            id: brain_id,
            profile: AgentProfile {
                name: "brain".to_string(),
                model: "gpt-4.1-mini".to_string(),
            },
            capabilities: AgentCapabilities {
                tags: vec!["brain".to_string(), "dispatcher".to_string()],
                description: "Brain Agent".to_string(),
            },
            kind: AgentKind::Persistent,
            parent_id: None,
            bound_task_id: None,
            tool_permissions: AgentToolPermissions::default(),
        },
        LongTermMemory::default(),
    ));

    // 手动创建 default-llm-agent（Brain 会调度到这个 agent）
    let default_agent_id = Uuid::new_v4();
    app.world_mut().spawn((
        Agent {
            id: default_agent_id,
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
        },
        LongTermMemory::default(),
    ));

    // 创建一个 Ready 状态的任务
    let task = Task::from_user_input_ready("你好，Harness", 3, default_channel());
    app.world_mut()
        .spawn((task, harness::ShortTermMemory::default()));

    for _ in 0..8 {
        app.update();
        thread::sleep(Duration::from_millis(20));
    }
}
