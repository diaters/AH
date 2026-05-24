use std::{sync::Arc, thread, time::Duration};

use crossbeam_channel::unbounded;
use harness::{
    AgentExecutionOutput, AgentExecutionRequest, AgentExecutor, BrainConfig, ExecutorFuture,
    HarnessConfig, OutputMessage, Task, TaskStatus, build_harness_app,
};
use tokio::runtime::Runtime;

struct BrainMockExecutor;

impl AgentExecutor for BrainMockExecutor {
    fn execute(&self, request: AgentExecutionRequest) -> ExecutorFuture {
        match request.request_kind {
            harness::AgentRequestKind::BrainDecision => {
                let decision = r#"{"selected_agent_name":"default-llm-agent","delegate_prompt":"请处理这个任务","reasoning":"测试用例"}"#;
                Box::pin(async move { Ok(AgentExecutionOutput::Text(decision.to_string())) })
            }
            harness::AgentRequestKind::LlmCompletion => {
                Box::pin(async move { Ok(AgentExecutionOutput::Text(format!("echo: {}", request.prompt))) })
            }
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
        brain: Some(BrainConfig {
            enabled: true,
            model: "test-brain-model".to_string(),
            agent_name: "brain".to_string(),
        }),
        agents_config_path: "agents.toml".to_string(),
        max_tool_iterations: 5,
    }
}

/// 验证 Brain 启用时，用户输入经过 Brain 决策后交给 Agent 执行。
#[test]
fn completes_brain_dispatch_flow() {
    let runtime = Arc::new(Runtime::new().expect("runtime should be created"));
    let executor: Arc<dyn AgentExecutor> = Arc::new(BrainMockExecutor);
    let (_input_tx, input_rx) = unbounded();
    let (output_tx, output_rx) = unbounded::<OutputMessage>();
    let mut app = build_harness_app(brain_test_config(), runtime, executor, input_rx, output_tx);

    // 初始化应用
    app.update();

    // 创建一个 Ready 状态的任务
    let task = Task::from_user_input_ready("你好，Harness", 3);
    app.world_mut()
        .spawn((task, harness::ShortTermMemory::default()));

    for _ in 0..16 {
        app.update();
        thread::sleep(Duration::from_millis(20));
    }

    let output = output_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("output should be produced");
    assert_eq!(output.content, "echo: 请处理这个任务");

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
    let (_input_tx, input_rx) = unbounded();
    let (output_tx, output_rx) = unbounded::<OutputMessage>();

    let mut no_brain_config = brain_test_config();
    no_brain_config.brain = None;
    let mut app = build_harness_app(no_brain_config, runtime, executor, input_rx, output_tx);

    // 初始化应用
    app.update();

    // 创建一个 Ready 状态的任务
    let task = Task::from_user_input_ready("你好，Harness", 3);
    app.world_mut()
        .spawn((task, harness::ShortTermMemory::default()));

    for _ in 0..8 {
        app.update();
        thread::sleep(Duration::from_millis(20));
    }

    let output = output_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("output should be produced");
    assert_eq!(output.content, "echo: 你好，Harness");
}
