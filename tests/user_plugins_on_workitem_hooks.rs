//! Task 21-23 集成测试：`on_workitem_started` / `on_workitem_completed` /
//! `on_workitem_failed` hook 接入。
//!
//! 验证在 `HARNESS_PLUGINS_DIR` 下放置订阅对应 hook 的插件后：
//! - 对带 `WorkItemLifecycleHookPending(OnWorkItemStarted)` 标记的 WorkItem，
//!   companion 系统能派发 `on_workitem_started` 并移除标记；
//! - 对带 `WorkItemLifecycleHookPending(OnWorkItemCompleted)` 标记的 WorkItem，
//!   companion 系统能派发 `on_workitem_completed` 并移除标记；
//! - 对带 `WorkItemLifecycleHookPending(OnWorkItemFailed)` 标记的 WorkItem，
//!   companion 系统能派发 `on_workitem_failed` 并移除标记。
//!
//! hook 脚本调用 `get_task_ids()` 验证 snapshot 注入，以无 panic 即视为派发成功。

use std::sync::{Arc, Mutex};

use crossbeam_channel::unbounded;
use tempfile::TempDir;
use tokio::runtime::Runtime;

use common::mock_executor::EchoExecutor;
use harness::user_plugins::hook_point::HookPoint;
use harness::{
    AgentExecutor, ChannelId, FrontendKind, HarnessConfig, WorkItem, WorkItemInput,
    WorkItemLifecycleHookPending, WorkItemOrigin, WorkItemStatus, WorkItemType,
    WorkItemWritebackTarget, build_harness_app, llm::ExecutorRegistry,
};

mod common;

#[allow(dead_code)]
fn default_channel() -> ChannelId {
    ChannelId {
        frontend: FrontendKind::Tui,
        user_id: "default".to_string(),
        thread_id: None,
    }
}

/// 构造指定状态的 WorkItem。
fn make_work_item(status: WorkItemStatus) -> WorkItem {
    let mut wi = WorkItem::new(
        uuid::Uuid::nil(),
        WorkItemType::Evaluation,
        WorkItemInput::new("test".to_string()),
        WorkItemOrigin::Evaluation,
        WorkItemWritebackTarget::TaskResult,
    );
    wi.status = status;
    wi
}

/// 进程内串行化 HARNESS_PLUGINS_DIR 访问的全局锁。
/// `std::env::set_var` 并非线程安全，Rust 默认并行运行测试函数，
/// 故需要全局 Mutex 串行化所有触碰该 env 的测试。
static PLUGIN_ENV_LOCK: Mutex<()> = Mutex::new(());

/// 写入同时订阅三个 WorkItem 生命周期 hook 的插件。
fn write_workitem_lifecycle_plugin(dir: &std::path::Path) {
    let plugin_dir = dir.join("alpha");
    std::fs::create_dir_all(plugin_dir.join("hooks")).unwrap();
    std::fs::write(
        plugin_dir.join("manifest.toml"),
        r#"
id = "alpha"
api_version = 1
[[hooks]]
event = "on_workitem_started"
script = "hooks/on_started.rhai"
[[hooks]]
event = "on_workitem_completed"
script = "hooks/on_completed.rhai"
[[hooks]]
event = "on_workitem_failed"
script = "hooks/on_failed.rhai"
"#,
    )
    .unwrap();
    std::fs::write(
        plugin_dir.join("hooks/on_started.rhai"),
        r#"
let ids = get_task_ids();
log_info("on_workitem_started: task count = " + ids.len());
"#,
    )
    .unwrap();
    std::fs::write(
        plugin_dir.join("hooks/on_completed.rhai"),
        r#"
let ids = get_task_ids();
log_info("on_workitem_completed: task count = " + ids.len());
"#,
    )
    .unwrap();
    std::fs::write(
        plugin_dir.join("hooks/on_failed.rhai"),
        r#"
let ids = get_task_ids();
log_info("on_workitem_failed: task count = " + ids.len());
"#,
    )
    .unwrap();
}

#[test]
fn on_workitem_started_dispatches_and_clears_marker() {
    let dir = TempDir::new().unwrap();
    write_workitem_lifecycle_plugin(dir.path());

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

    // 构造一个 Running 状态的 WorkItem 并附带 Started 标记
    let work_item = make_work_item(WorkItemStatus::Running);
    let entity = app
        .world_mut()
        .spawn((
            work_item,
            WorkItemLifecycleHookPending(HookPoint::OnWorkItemStarted),
        ))
        .id();

    // 推帧让 workitem_lifecycle_hook_system 派发 hook 并移除标记
    for _ in 0..5 {
        app.update();
    }

    // 验证标记已被移除
    assert!(
        app.world_mut()
            .query::<&WorkItemLifecycleHookPending>()
            .get(app.world(), entity)
            .is_err(),
        "WorkItemLifecycleHookPending(OnWorkItemStarted) 标记应在 hook 派发后移除"
    );

    // WorkItem entity 仍存在
    assert!(
        app.world().get_entity(entity).is_ok(),
        "WorkItem entity 应仍存在"
    );
}

#[test]
fn on_workitem_completed_dispatches_and_clears_marker() {
    let dir = TempDir::new().unwrap();
    write_workitem_lifecycle_plugin(dir.path());

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

    app.update();

    // 构造一个 Completed 状态的 WorkItem 并附带 Completed 标记
    let work_item = make_work_item(WorkItemStatus::Completed);
    let entity = app
        .world_mut()
        .spawn((
            work_item,
            WorkItemLifecycleHookPending(HookPoint::OnWorkItemCompleted),
        ))
        .id();

    for _ in 0..5 {
        app.update();
    }

    assert!(
        app.world_mut()
            .query::<&WorkItemLifecycleHookPending>()
            .get(app.world(), entity)
            .is_err(),
        "WorkItemLifecycleHookPending(OnWorkItemCompleted) 标记应在 hook 派发后移除"
    );

    assert!(
        app.world().get_entity(entity).is_ok(),
        "WorkItem entity 应仍存在"
    );
}

#[test]
fn on_workitem_failed_dispatches_and_clears_marker() {
    let dir = TempDir::new().unwrap();
    write_workitem_lifecycle_plugin(dir.path());

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

    app.update();

    // 构造一个 Failed 状态的 WorkItem 并附带 Failed 标记
    let work_item = make_work_item(WorkItemStatus::Failed);
    let entity = app
        .world_mut()
        .spawn((
            work_item,
            WorkItemLifecycleHookPending(HookPoint::OnWorkItemFailed),
        ))
        .id();

    for _ in 0..5 {
        app.update();
    }

    assert!(
        app.world_mut()
            .query::<&WorkItemLifecycleHookPending>()
            .get(app.world(), entity)
            .is_err(),
        "WorkItemLifecycleHookPending(OnWorkItemFailed) 标记应在 hook 派发后移除"
    );

    assert!(
        app.world().get_entity(entity).is_ok(),
        "WorkItem entity 应仍存在"
    );
}
