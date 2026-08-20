//! Task 31-36 集成测试：`on_shared_knowledge_write`、经验候选和审批 hook 接入。
//!
//! 验证各 hook 点的 companion 系统能派发 hook 并清理标记/队列。

use std::sync::{Arc, Mutex};

use crossbeam_channel::unbounded;
use tempfile::TempDir;
use tokio::runtime::Runtime;

use common::mock_executor::EchoExecutor;
use harness::{
    app::build_harness_app, domain::AgentExecutor, llm::ExecutorRegistry, systems::HarnessConfig,
};

mod common;

/// 进程内串行化 HARNESS_PLUGINS_DIR 访问的全局锁。
static PLUGIN_ENV_LOCK: Mutex<()> = Mutex::new(());

// ============ Task 31: OnSharedKnowledgeWrite ============

fn write_knowledge_write_plugin(dir: &std::path::Path) {
    let plugin_dir = dir.join("kw-alpha");
    std::fs::create_dir_all(plugin_dir.join("hooks")).unwrap();
    std::fs::write(
        plugin_dir.join("manifest.toml"),
        r#"
id = "kw-alpha"
api_version = 1
[[hooks]]
event = "on_shared_knowledge_write"
script = "hooks/on_kw_write.rhai"
"#,
    )
    .unwrap();
    std::fs::write(
        plugin_dir.join("hooks/on_kw_write.rhai"),
        r#"
let ids = get_task_ids();
log_info("on_shared_knowledge_write: task count = " + ids.len());
"#,
    )
    .unwrap();
}

#[test]
fn on_shared_knowledge_write_drains_pending_queue() {
    let dir = TempDir::new().unwrap();
    write_knowledge_write_plugin(dir.path());

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

    // 手动向 PendingKnowledgeWriteHooks 推入一条记录
    {
        let mut pending = app
            .world_mut()
            .resource_mut::<harness::domain::PendingKnowledgeWriteHooks>();
        pending.0.push(
            harness::domain::SharedKnowledgeEntry::approved_from_user_input("test knowledge"),
        );
    }

    // 推帧让 companion 系统派发 hook
    for _ in 0..5 {
        app.update();
    }

    // 队列应被清空
    let pending = app
        .world()
        .resource::<harness::domain::PendingKnowledgeWriteHooks>();
    assert!(
        pending.0.is_empty(),
        "PendingKnowledgeWriteHooks 队列应在 hook 派发后清空"
    );
}

// ============ Task 32-34: Experience candidate hooks ============

fn write_experience_submitted_plugin(dir: &std::path::Path) {
    let plugin_dir = dir.join("exp-alpha");
    std::fs::create_dir_all(plugin_dir.join("hooks")).unwrap();
    std::fs::write(
        plugin_dir.join("manifest.toml"),
        r#"
id = "exp-alpha"
api_version = 1
[[hooks]]
event = "on_experience_candidate_submitted"
script = "hooks/on_exp.rhai"
"#,
    )
    .unwrap();
    std::fs::write(
        plugin_dir.join("hooks/on_exp.rhai"),
        r#"
let ids = get_task_ids();
log_info("on_experience_candidate_submitted: task count = " + ids.len());
"#,
    )
    .unwrap();
}

#[test]
fn on_experience_hook_drains_pending_queue() {
    let dir = TempDir::new().unwrap();
    write_experience_submitted_plugin(dir.path());

    let _env_guard = PLUGIN_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    unsafe {
        std::env::set_var("HARNESS_PLUGINS_DIR", dir.path());
    }

    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(EchoExecutor);
    let _executor_registry = ExecutorRegistry::from_single_executor(executor, "default");
    let (_input_tx, input_rx) = unbounded();
    let executor: Arc<dyn AgentExecutor> = Arc::new(EchoExecutor);
    let executor_registry = ExecutorRegistry::from_single_executor(executor, "default");
    let mut app = build_harness_app(
        HarnessConfig::default(),
        runtime,
        executor_registry,
        input_rx,
        vec![],
        harness::channels::ChannelManager::empty().0,
    );

    // 初始化应用
    app.update();

    // 手动向 PendingExperienceHooks 推入事件
    {
        let mut pending = app
            .world_mut()
            .resource_mut::<harness::domain::PendingExperienceHooks>();
        pending.0.push((
            harness::domain::HookPoint::OnExperienceCandidateSubmitted,
            uuid::Uuid::new_v4(),
        ));
    }

    // 推帧让 companion 系统派发 hook
    for _ in 0..5 {
        app.update();
    }

    // 队列应被清空
    let pending = app
        .world()
        .resource::<harness::domain::PendingExperienceHooks>();
    assert!(
        pending.0.is_empty(),
        "PendingExperienceHooks 队列应在 hook 派发后清空"
    );
}

// ============ Task 35-36: Approval hooks ============

fn write_approval_requested_plugin(dir: &std::path::Path) {
    let plugin_dir = dir.join("appr-alpha");
    std::fs::create_dir_all(plugin_dir.join("hooks")).unwrap();
    std::fs::write(
        plugin_dir.join("manifest.toml"),
        r#"
id = "appr-alpha"
api_version = 1
[[hooks]]
event = "on_approval_requested"
script = "hooks/on_appr.rhai"
"#,
    )
    .unwrap();
    std::fs::write(
        plugin_dir.join("hooks/on_appr.rhai"),
        r#"
let ids = get_task_ids();
log_info("on_approval_requested: task count = " + ids.len());
"#,
    )
    .unwrap();
}

#[test]
fn on_approval_requested_removes_marker() {
    let dir = TempDir::new().unwrap();
    write_approval_requested_plugin(dir.path());

    let _env_guard = PLUGIN_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    unsafe {
        std::env::set_var("HARNESS_PLUGINS_DIR", dir.path());
    }

    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(EchoExecutor);
    let _executor_registry = ExecutorRegistry::from_single_executor(executor, "default");
    let (_input_tx, input_rx) = unbounded();
    let executor: Arc<dyn AgentExecutor> = Arc::new(EchoExecutor);
    let executor_registry = ExecutorRegistry::from_single_executor(executor, "default");
    let mut app = build_harness_app(
        HarnessConfig::default(),
        runtime,
        executor_registry,
        input_rx,
        vec![],
        harness::channels::ChannelManager::empty().0,
    );

    // 初始化应用
    app.update();

    // spawn 一个带 ApprovalRequestedHookPending 标记的 ApprovalRequestMessage
    let entity = app
        .world_mut()
        .spawn((
            harness::domain::ApprovalRequestMessage {
                request_id: uuid::Uuid::new_v4(),
                source_task_id: harness::domain::TaskId::nil(),
                approval_task_id: harness::domain::TaskId::new(),
                parent_agent_id: harness::domain::AgentId::nil(),
                child_agent_id: harness::domain::AgentId::nil(),
                tool_name: "shell_exec".to_string(),
                tool_input: serde_json::json!({"command": "ls"}),
                context: String::new(),
            },
            harness::domain::ApprovalRequestedHookPending,
        ))
        .id();

    // 推帧让 companion 系统派发 hook
    for _ in 0..5 {
        app.update();
    }

    // 标记应被移除
    assert!(
        app.world()
            .get::<harness::domain::ApprovalRequestedHookPending>(entity)
            .is_none(),
        "ApprovalRequestedHookPending 应在 hook 派发后移除"
    );
}

#[test]
fn on_approval_resolved_removes_marker() {
    let dir = TempDir::new().unwrap();
    // 使用 on_approval_resolved 事件
    let plugin_dir = dir.path().join("appr-resolved");
    std::fs::create_dir_all(plugin_dir.join("hooks")).unwrap();
    std::fs::write(
        plugin_dir.join("manifest.toml"),
        r#"
id = "appr-resolved"
api_version = 1
[[hooks]]
event = "on_approval_resolved"
script = "hooks/on_appr_resolved.rhai"
"#,
    )
    .unwrap();
    std::fs::write(
        plugin_dir.join("hooks/on_appr_resolved.rhai"),
        r#"
let ids = get_task_ids();
log_info("on_approval_resolved: task count = " + ids.len());
"#,
    )
    .unwrap();

    let _env_guard = PLUGIN_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    unsafe {
        std::env::set_var("HARNESS_PLUGINS_DIR", dir.path());
    }

    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(EchoExecutor);
    let _executor_registry = ExecutorRegistry::from_single_executor(executor, "default");
    let (_input_tx, input_rx) = unbounded();
    let executor: Arc<dyn AgentExecutor> = Arc::new(EchoExecutor);
    let executor_registry = ExecutorRegistry::from_single_executor(executor, "default");
    let mut app = build_harness_app(
        HarnessConfig::default(),
        runtime,
        executor_registry,
        input_rx,
        vec![],
        harness::channels::ChannelManager::empty().0,
    );

    // 初始化应用
    app.update();

    // spawn 一个带 ApprovalResolvedHookPending 标记的 ApprovalResultMessage
    let entity = app
        .world_mut()
        .spawn((
            harness::domain::ApprovalResultMessage {
                request_id: uuid::Uuid::new_v4(),
                source_task_id: harness::domain::TaskId::nil(),
                approval_task_id: harness::domain::TaskId::new(),
                decision: harness::domain::ApprovalDecision::Approved,
                reasoning: "test".to_string(),
                grant_mode: harness::domain::GrantMode::Once,
            },
            harness::domain::ApprovalResolvedHookPending,
        ))
        .id();

    // 推帧让 companion 系统派发 hook
    for _ in 0..5 {
        app.update();
    }

    // 标记应被移除
    assert!(
        app.world()
            .get::<harness::domain::ApprovalResolvedHookPending>(entity)
            .is_none(),
        "ApprovalResolvedHookPending 应在 hook 派发后移除"
    );
}
