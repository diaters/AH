//! Task 28 集成测试：`on_llm_response` hook 接入。
//!
//! 验证在 `HARNESS_PLUGINS_DIR` 下放置订阅 `on_llm_response` 的插件后，
//! `AgentExecutionResultMessage` spawn 附带 `LlmResponseHookPending` 标记后，
//! companion 系统能派发 hook 并移除标记。

use std::sync::{Arc, Mutex};

use crossbeam_channel::unbounded;
use tempfile::TempDir;
use tokio::runtime::Runtime;

use common::mock_executor::EchoExecutor;
use harness::{
    AgentExecutionOutput, AgentExecutionResult, AgentExecutionResultMessage, AgentExecutor,
    AgentRequestKind, HarnessConfig, LlmResponseHookPending, OutputContent, build_harness_app,
    llm::ExecutorRegistry,
};

mod common;

/// 进程内串行化 HARNESS_PLUGINS_DIR 访问的全局锁。
static PLUGIN_ENV_LOCK: Mutex<()> = Mutex::new(());

fn write_llm_response_plugin(dir: &std::path::Path) {
    let plugin_dir = dir.join("alpha");
    std::fs::create_dir_all(plugin_dir.join("hooks")).unwrap();
    std::fs::write(
        plugin_dir.join("manifest.toml"),
        r#"
id = "alpha"
api_version = 1
[[hooks]]
event = "on_llm_response"
script = "hooks/on_llm_response.rhai"
"#,
    )
    .unwrap();
    std::fs::write(
        plugin_dir.join("hooks/on_llm_response.rhai"),
        r#"
let ids = get_task_ids();
log_info("on_llm_response: task count = " + ids.len());
"#,
    )
    .unwrap();
}

#[test]
fn on_llm_response_removes_marker() {
    let dir = TempDir::new().unwrap();
    write_llm_response_plugin(dir.path());

    let _env_guard = PLUGIN_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    unsafe {
        std::env::set_var("HARNESS_PLUGINS_DIR", dir.path());
    }

    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(EchoExecutor);
    let executor_registry = ExecutorRegistry::from_single_executor(executor, "default");
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(
        HarnessConfig::default(),
        runtime,
        executor_registry,
        input_rx,
        vec![],
        harness::channels::ChannelManager::empty().0,
    );

    // 初始化应用（让 Startup 阶段加载插件）
    app.update();

    // spawn 一个带标记的 AgentExecutionResultMessage
    let msg = AgentExecutionResultMessage {
        result: AgentExecutionResult {
            task_id: uuid::Uuid::new_v4(),
            agent_id: uuid::Uuid::new_v4(),
            request_kind: AgentRequestKind::LlmCompletion,
            result: Ok(AgentExecutionOutput {
                content: OutputContent::Text("test response".to_string()),
                reasoning_content: None,
            }),
            prompt: "test".to_string(),
            system_prompt: None,
            tools: vec![],
            reasoning_content: None,
            work_item_id: None,
            conversation: None,
        },
    };
    let entity = app.world_mut().spawn((msg, LlmResponseHookPending)).id();

    // 推帧让 companion 系统派发 hook
    for _ in 0..5 {
        app.update();
    }

    // 标记应被移除
    assert!(
        app.world().get::<LlmResponseHookPending>(entity).is_none(),
        "LlmResponseHookPending 应在 hook 派发后移除"
    );
}
