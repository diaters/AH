//! Task 24-25 集成测试：`on_agent_started` / `on_agent_stopped` hook 接入。
//!
//! 验证在 `HARNESS_PLUGINS_DIR` 下放置订阅对应 hook 的插件后：
//! - 新 Agent entity spawn 后，companion 系统能派发 `on_agent_started`；
//! - Agent entity 标记 `AgentStoppingHookPending` 后，companion 系统能派发
//!   `on_agent_stopped` 并 despawn entity。
//!
//! hook 脚本调用 `get_task_ids()` 验证 snapshot 注入，以无 panic 即视为派发成功。

use std::sync::{Arc, Mutex};

use crossbeam_channel::unbounded;
use tempfile::TempDir;
use tokio::runtime::Runtime;

use harness::{
    Agent, AgentCapabilities, AgentExecutionOutput, AgentExecutionRequest, AgentExecutor,
    AgentKind, AgentProfile, AgentStoppingHookPending, AgentToolPermissions, ChannelId,
    ExecutorFuture, FrontendKind, HarnessConfig, build_harness_app, llm::ExecutorRegistry,
};

#[allow(dead_code)]
fn default_channel() -> ChannelId {
    ChannelId {
        frontend: FrontendKind::Tui,
        user_id: "default".to_string(),
        thread_id: None,
    }
}

/// 构造一个占位 Agent 用于测试。
fn make_agent(kind: AgentKind) -> Agent {
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
        kind,
        parent_id: None,
        bound_task_id: None,
        tool_permissions: AgentToolPermissions::default(),
        system_prompt: None,
    }
}

struct EchoExecutor;

impl AgentExecutor for EchoExecutor {
    fn execute(&self, _request: AgentExecutionRequest) -> ExecutorFuture {
        Box::pin(async move {
            Ok(AgentExecutionOutput {
                content: harness::OutputContent::Text("echo".to_string()),
                reasoning_content: None,
            })
        })
    }
}

/// 进程内串行化 HARNESS_PLUGINS_DIR 访问的全局锁。
/// `std::env::set_var` 并非线程安全，Rust 默认并行运行测试函数，
/// 故需要全局 Mutex 串行化所有触碰该 env 的测试。
static PLUGIN_ENV_LOCK: Mutex<()> = Mutex::new(());

/// 写入同时订阅 Agent 生命周期 hook 的插件。
fn write_agent_lifecycle_plugin(dir: &std::path::Path) {
    let plugin_dir = dir.join("alpha");
    std::fs::create_dir_all(plugin_dir.join("hooks")).unwrap();
    std::fs::write(
        plugin_dir.join("manifest.toml"),
        r#"
id = "alpha"
api_version = 1
[[hooks]]
event = "on_agent_started"
script = "hooks/on_started.rhai"
[[hooks]]
event = "on_agent_stopped"
script = "hooks/on_stopped.rhai"
"#,
    )
    .unwrap();
    std::fs::write(
        plugin_dir.join("hooks/on_started.rhai"),
        r#"
let ids = get_task_ids();
log_info("on_agent_started: task count = " + ids.len());
"#,
    )
    .unwrap();
    std::fs::write(
        plugin_dir.join("hooks/on_stopped.rhai"),
        r#"
let ids = get_task_ids();
log_info("on_agent_stopped: task count = " + ids.len());
"#,
    )
    .unwrap();
}

#[test]
fn on_agent_started_dispatches_on_new_agent() {
    let dir = TempDir::new().unwrap();
    write_agent_lifecycle_plugin(dir.path());

    let _env_guard = PLUGIN_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    // SAFETY: `PLUGIN_ENV_LOCK` 全局 Mutex 强制本二进制中所有触碰此 env 的测试
    // 串行执行；HARNESS_PLUGINS_DIR 指向临时目录，存活至本函数结束。
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

    // 在世界中 spawn 一个新的 Agent entity（模拟 Agent 创建）
    let agent = make_agent(AgentKind::Persistent);
    let entity = app.world_mut().spawn(agent).id();

    // 推帧让 agent_started_hook_system 派发 hook
    for _ in 0..5 {
        app.update();
    }

    // Agent entity 仍存在（on_agent_started 不 despawn）
    assert!(
        app.world().get_entity(entity).is_ok(),
        "Agent entity 应在 on_agent_started 派发后仍存在"
    );
}

#[test]
fn on_agent_stopped_dispatches_before_despawn() {
    let dir = TempDir::new().unwrap();
    write_agent_lifecycle_plugin(dir.path());

    let _env_guard = PLUGIN_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    // SAFETY: 同上。
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

    // Spawn 一个 Agent 并立即标记为即将停止
    let agent = make_agent(AgentKind::TaskScoped);
    let entity = app
        .world_mut()
        .spawn((agent, AgentStoppingHookPending))
        .id();

    // 推帧让 agent_stopped_hook_system 派发 hook 并 despawn
    for _ in 0..5 {
        app.update();
    }

    // Agent entity 应已被 despawn
    assert!(
        app.world().get_entity(entity).is_err(),
        "带 AgentStoppingHookPending 的 Agent entity 应在 on_agent_stopped 派发后 despawn"
    );
}
