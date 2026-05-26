use std::{sync::Arc, thread, time::Duration};

use crossbeam_channel::unbounded;
use harness::{
    AgentExecutionOutput, AgentExecutionRequest, AgentExecutor, ChannelId, ExecutorFuture,
    FrontendKind, HarnessConfig, OutputMessage, Task, TaskStatus, build_harness_app,
};

fn default_channel() -> ChannelId {
    ChannelId { frontend: FrontendKind::Tui, user_id: "default".to_string() }
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

fn test_config() -> HarnessConfig {
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
        max_tool_iterations: 5,
    }
}

/// 验证单轮输入可以沿着 MVP 主链路完成闭环。
#[test]
fn completes_single_turn_conversation_flow() {
    let runtime = Arc::new(Runtime::new().expect("runtime should be created"));
    let executor: Arc<dyn AgentExecutor> = Arc::new(EchoExecutor);
    let (_input_tx, input_rx) = unbounded();
    let (output_tx, output_rx) = unbounded::<OutputMessage>();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, output_tx);

    // 初始化应用
    app.update();

    // 创建一个 Ready 状态的任务（单轮场景）
    let task = Task::from_user_input_ready("你好，Harness", 3, default_channel());
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

    let tasks: Vec<Task> = {
        let world = app.world_mut();
        let mut query = world.query::<&Task>();
        query.iter(world).cloned().collect()
    };

    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].status, TaskStatus::Done);
    assert_eq!(tasks[0].result_summary, "echo: 你好，Harness");
}
