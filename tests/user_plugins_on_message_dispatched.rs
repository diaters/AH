//! Task 26 集成测试：`on_message_dispatched` hook 接入。
//!
//! 验证在 `HARNESS_PLUGINS_DIR` 下放置订阅 `on_message_dispatched` 的插件后，
//! `AgentExecutionRequestMessage` spawn 附带 `MessageDispatchedHookPending` 标记后，
//! companion 系统能派发 hook 并移除标记。

use std::sync::{Arc, Mutex};

use crossbeam_channel::unbounded;
use tempfile::TempDir;
use tokio::runtime::Runtime;

use common::mock_executor::EchoExecutor;
use harness::{
    AgentExecutionRequest, AgentExecutionRequestMessage, AgentExecutor, AgentRequestKind,
    HarnessConfig, MessageDispatchedHookPending, build_harness_app, llm::ExecutorRegistry,
};

mod common;

/// 进程内串行化 HARNESS_PLUGINS_DIR 访问的全局锁。
static PLUGIN_ENV_LOCK: Mutex<()> = Mutex::new(());

fn write_message_dispatched_plugin(dir: &std::path::Path) {
    let plugin_dir = dir.join("alpha");
    std::fs::create_dir_all(plugin_dir.join("hooks")).unwrap();
    std::fs::write(
        plugin_dir.join("manifest.toml"),
        r#"
id = "alpha"
api_version = 1
[[hooks]]
event = "on_message_dispatched"
script = "hooks/on_dispatched.rhai"
"#,
    )
    .unwrap();
    std::fs::write(
        plugin_dir.join("hooks/on_dispatched.rhai"),
        r#"
let ids = get_task_ids();
log_info("on_message_dispatched: task count = " + ids.len());
"#,
    )
    .unwrap();
}

#[test]
fn on_message_dispatched_removes_marker() {
    let dir = TempDir::new().unwrap();
    write_message_dispatched_plugin(dir.path());

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

    // spawn 一个带标记的 AgentExecutionRequestMessage
    let msg = AgentExecutionRequestMessage {
        request: AgentExecutionRequest {
            task_id: uuid::Uuid::new_v4(),
            agent_id: uuid::Uuid::new_v4(),
            request_kind: AgentRequestKind::LlmCompletion,
            prompt: "test".to_string(),
            system_prompt: None,
            tools: vec![],
            conversation: None,
            work_item_id: None,
            model_override: None,
        },
    };
    let entity = app
        .world_mut()
        .spawn((msg, MessageDispatchedHookPending))
        .id();

    // 推帧让 companion 系统派发 hook
    for _ in 0..5 {
        app.update();
    }

    // 标记应被移除
    assert!(
        app.world()
            .get::<MessageDispatchedHookPending>(entity)
            .is_none(),
        "MessageDispatchedHookPending 应在 hook 派发后移除"
    );
}
