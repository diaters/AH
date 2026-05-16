use std::{sync::Arc, thread, time::Duration};

use crossbeam_channel::unbounded;
use harness::{
    build_harness_app, AgentExecutionRequest, AgentExecutor, ExecutorFuture, ExternalInput,
    HarnessConfig, OutputMessage, Task, TaskStatus,
};
use tokio::runtime::Runtime;

struct EchoExecutor;

impl AgentExecutor for EchoExecutor {
    fn execute(&self, request: AgentExecutionRequest) -> ExecutorFuture {
        Box::pin(async move { Ok(format!("echo: {}", request.prompt)) })
    }
}

fn test_config() -> HarnessConfig {
    HarnessConfig {
        max_retries: 3,
        llm: harness::LlmProviderConfig {
            provider: harness::LlmProviderKind::OpenAi,
            model: "gpt-4.1-mini".to_string(),
            api_key: "test-api-key".to_string(),
            api_base: None,
            org_id: None,
            project_id: None,
        },
        brain: None,
        agents_config_path: "agents.toml".to_string(),
    }
}

/// 验证单轮输入可以沿着 MVP 主链路完成闭环。
#[test]
fn completes_single_turn_conversation_flow() {
    let runtime = Arc::new(Runtime::new().expect("runtime should be created"));
    let executor: Arc<dyn AgentExecutor> = Arc::new(EchoExecutor);
    let (input_tx, input_rx) = unbounded();
    let (output_tx, output_rx) = unbounded::<OutputMessage>();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, output_tx);

    input_tx
        .send(ExternalInput::Text("你好，Harness".to_string()))
        .expect("input should be accepted");

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
