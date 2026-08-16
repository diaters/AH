mod common;

use std::{sync::Arc, thread, time::Duration};

use common::mock_executor::PromptEchoExecutor;
use crossbeam_channel::unbounded;
use harness::{
    AgentExecutor, ChannelId, ExternalInput, FrontendKind, HarnessConfig, Task, build_harness_app,
    llm::ExecutorRegistry,
};
use tokio::runtime::Runtime;

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

/// 验证通过 TUI 入口的 ExternalInput 正确保留 origin_channel。
#[test]
fn tui_input_preserves_origin_channel() {
    let runtime = Arc::new(Runtime::new().expect("runtime should be created"));
    let executor: Arc<dyn AgentExecutor> = Arc::new(PromptEchoExecutor);
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

    let tui_channel = ChannelId {
        frontend: FrontendKind::Tui,
        user_id: "default".to_string(),
        thread_id: None,
    };
    input_tx
        .send(ExternalInput::TextWithChannel {
            channel: tui_channel.clone(),
            content: "hello from TUI".to_string(),
        })
        .expect("send");

    for _ in 0..10 {
        app.update();
        thread::sleep(Duration::from_millis(20));
    }

    let tasks: Vec<Task> = {
        let world = app.world_mut();
        let mut query = world.query::<&Task>();
        query.iter(world).cloned().collect()
    };

    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].origin_channel, Some(tui_channel));
}

/// 验证通过 Telegram 入口的 ExternalInput 正确保留 origin_channel。
#[test]
fn telegram_input_preserves_origin_channel() {
    let runtime = Arc::new(Runtime::new().expect("runtime should be created"));
    let executor: Arc<dyn AgentExecutor> = Arc::new(PromptEchoExecutor);
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

    let tg_channel = ChannelId {
        frontend: FrontendKind::Telegram,
        user_id: "123456".to_string(),
        thread_id: None,
    };
    input_tx
        .send(ExternalInput::TextWithChannel {
            channel: tg_channel.clone(),
            content: "hello from Telegram".to_string(),
        })
        .expect("send");

    for _ in 0..10 {
        app.update();
        thread::sleep(Duration::from_millis(20));
    }

    let tasks: Vec<Task> = {
        let world = app.world_mut();
        let mut query = world.query::<&Task>();
        query.iter(world).cloned().collect()
    };

    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].origin_channel, Some(tg_channel));
}
