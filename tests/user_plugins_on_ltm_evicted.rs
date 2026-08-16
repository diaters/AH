//! Task 30 集成测试：`on_long_term_memory_evicted` hook 接入。
//!
//! 验证在 `HARNESS_PLUGINS_DIR` 下放置订阅 `on_long_term_memory_evicted` 的插件后，
//! Agent entity 附带 `LtmEvictedHookPending` 标记后，
//! companion 系统能派发 hook 并移除标记。

use std::sync::{Arc, Mutex};

use crossbeam_channel::unbounded;
use tempfile::TempDir;
use tokio::runtime::Runtime;

use common::mock_executor::EchoExecutor;
use harness::{
    Agent, AgentCapabilities, AgentExecutor, AgentKind, AgentProfile, AgentToolPermissions,
    HarnessConfig, LtmEvictedHookPending, build_harness_app, llm::ExecutorRegistry,
};

mod common;

fn make_agent() -> Agent {
    Agent {
        id: uuid::Uuid::new_v4(),
        profile: AgentProfile {
            name: "test-agent".to_string(),
            model: "test-model".to_string(),
        },
        capabilities: AgentCapabilities {
            tags: vec![],
            description: "test".to_string(),
        },
        kind: AgentKind::Persistent,
        parent_id: None,
        bound_task_id: None,
        tool_permissions: AgentToolPermissions::default(),
        system_prompt: None,
    }
}

/// 进程内串行化 HARNESS_PLUGINS_DIR 访问的全局锁。
static PLUGIN_ENV_LOCK: Mutex<()> = Mutex::new(());

fn write_ltm_evicted_plugin(dir: &std::path::Path) {
    let plugin_dir = dir.join("alpha");
    std::fs::create_dir_all(plugin_dir.join("hooks")).unwrap();
    std::fs::write(
        plugin_dir.join("manifest.toml"),
        r#"
id = "alpha"
api_version = 1
[[hooks]]
event = "on_long_term_memory_evicted"
script = "hooks/on_ltm_evicted.rhai"
"#,
    )
    .unwrap();
    std::fs::write(
        plugin_dir.join("hooks/on_ltm_evicted.rhai"),
        r#"
let ids = get_task_ids();
log_info("on_long_term_memory_evicted: task count = " + ids.len());
"#,
    )
    .unwrap();
}

#[test]
fn on_ltm_evicted_removes_marker() {
    let dir = TempDir::new().unwrap();
    write_ltm_evicted_plugin(dir.path());

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

    // spawn 一个带 LtmEvictedHookPending 标记的 Agent entity
    let agent = make_agent();
    let entity = app.world_mut().spawn((agent, LtmEvictedHookPending)).id();

    // 推帧让 companion 系统派发 hook
    for _ in 0..5 {
        app.update();
    }

    // 标记应被移除
    assert!(
        app.world().get::<LtmEvictedHookPending>(entity).is_none(),
        "LtmEvictedHookPending 应在 hook 派发后移除"
    );
}
