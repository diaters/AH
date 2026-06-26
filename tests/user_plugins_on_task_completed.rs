//! Task 17/18 集成测试：`on_task_completed` + `on_task_failed` hook 接入。
//!
//! 验证在 `HARNESS_PLUGINS_DIR` 下放置同时订阅两个终态 hook 的插件后：
//! - 通过 `/finish` 让 Task 进入 Done，可派发 `on_task_completed`；
//! - 直接将 Task 置为 Failed，可派发 `on_task_failed`；
//! - 终态去重 `TaskTerminalDispatched` 集合应包含对应 task id。
//!
//! hook 脚本不依赖 LLM 真实返回，host API 副作用通过 log_info 路径以无 panic
//! 即视为派发成功（与 Task 16 同样的最小可观测策略）。

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use crossbeam_channel::unbounded;
use tempfile::TempDir;
use tokio::runtime::Runtime;

use harness::{
    AgentExecutionOutput, AgentExecutionRequest, AgentExecutor, ChannelId, ExecutorFuture,
    ExternalInput, FrontendKind, HarnessConfig, ShortTermMemory, Task, TaskStatus,
    build_harness_app,
};

fn default_channel() -> ChannelId {
    ChannelId {
        frontend: FrontendKind::Tui,
        user_id: "default".to_string(),
    }
}

/// 把任务状态从外部置为终态会通过 `Changed<Task>` 触发 `task_completion_hook_system`。
fn make_ready_task() -> Task {
    Task::from_user_input_ready("ready-task", 0, default_channel())
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

/// 进程内串行化 HARNESS_PLUGINS_DIR 访问，参考 Task 16 测试的同名锁。
/// `std::env::set_var` 并非线程安全，Rust 默认并行运行测试函数，故需要
/// 全局 Mutex 串行化所有触碰该 env 的测试。
static PLUGIN_ENV_LOCK: Mutex<()> = Mutex::new(());

/// 写入同时订阅 `on_task_completed` 与 `on_task_failed` 的插件。
fn write_terminal_plugin(dir: &std::path::Path) {
    let plugin_dir = dir.join("alpha");
    std::fs::create_dir_all(plugin_dir.join("hooks")).unwrap();
    std::fs::write(
        plugin_dir.join("manifest.toml"),
        r#"
id = "alpha"
api_version = 1
[[hooks]]
event = "on_task_completed"
script = "hooks/on_completed.rhai"
[[hooks]]
event = "on_task_failed"
script = "hooks/on_failed.rhai"
"#,
    )
    .unwrap();
    std::fs::write(
        plugin_dir.join("hooks/on_completed.rhai"),
        r#"
let ids = get_task_ids();
log_info("on_task_completed: count = " + ids.len());
"#,
    )
    .unwrap();
    std::fs::write(
        plugin_dir.join("hooks/on_failed.rhai"),
        r#"
let ids = get_task_ids();
log_info("on_task_failed: count = " + ids.len());
"#,
    )
    .unwrap();
}

#[test]
fn on_task_completed_dispatches_on_finish_command() {
    let dir = TempDir::new().unwrap();
    write_terminal_plugin(dir.path());

    let _env_guard = PLUGIN_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    // SAFETY: `PLUGIN_ENV_LOCK` 全局 Mutex 强制本二进制中所有触碰此 env 的测试
    // 串行执行；HARNESS_PLUGINS_DIR 指向临时目录，存活至本函数结束。
    unsafe {
        std::env::set_var("HARNESS_PLUGINS_DIR", dir.path());
    }

    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(EchoExecutor);
    let (input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(
        HarnessConfig::default(),
        runtime,
        executor,
        input_rx,
        vec![],
        harness::channels::ChannelManager::empty(),
    );

    // 初始化应用（让 Startup 阶段加载插件）
    app.update();

    // 构造一个 Ready → 等待用户态的 task，发 `/finish` 触发 mark_done。
    let mut task = make_ready_task();
    task.status = TaskStatus::Waiting(harness::WaitingReason::User);
    let task_id = task.id;
    app.world_mut()
        .spawn((task, HarnessIdPlaceholder, ShortTermMemory::default()));

    // 发送 /finish 命令
    input_tx
        .send(ExternalInput::TextWithChannel {
            channel: default_channel(),
            content: "/finish".to_string(),
        })
        .unwrap();

    // 推足够多帧让 command_parse -> finish_task_system(mark done)
    // -> task_termination_system -> task_completion_hook_system 全部跑完。
    for _ in 0..10 {
        app.update();
    }

    // 验证：task 进入 Done，且去重集合包含其 task id。
    // 通过 world 快照验证 Task 状态为 Done
    let task_status = app
        .world_mut()
        .query::<&Task>()
        .iter(app.world())
        .find(|t| t.id == task_id)
        .map(|t| t.status.clone());
    match task_status {
        Some(TaskStatus::Done) => {}
        other => panic!("预期 Task 进入 Done，实际：{:?}", other),
    }

    let set: &harness::systems::TaskTerminalDispatched = app
        .world()
        .get_resource()
        .expect("TaskTerminalDispatched 应被 init_resource 注入");
    assert!(
        set.0.contains(&task_id),
        "on_task_completed 应将 task id 写入去重集合"
    );
}

#[test]
fn on_task_failed_dispatches_on_direct_failure_mutation() {
    let dir = TempDir::new().unwrap();
    write_terminal_plugin(dir.path());

    let _env_guard = PLUGIN_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    // SAFETY: 同上，`PLUGIN_ENV_LOCK` 串行化 set_var。
    unsafe {
        std::env::set_var("HARNESS_PLUGINS_DIR", dir.path());
    }

    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(EchoExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(
        HarnessConfig::default(),
        runtime,
        executor,
        input_rx,
        vec![],
        harness::channels::ChannelManager::empty(),
    );

    app.update();

    // spawn 一个非终态 task。
    let task = make_ready_task();
    let task_id = task.id;
    app.world_mut()
        .spawn((task, HarnessIdPlaceholder, ShortTermMemory::default()));

    // 先跑一帧确保 task 不被 task_termination_system 当作终态处理。
    app.update();

    // 通过 world system 将 task 直接改为 Failed：通过命令模式触发 `Changed<Task>`。
    // 这里用 `World::resource_scope` 不可，直接 world mut query 修改。
    {
        let mut task_q = app.world_mut().query::<&mut Task>();
        for mut t in task_q.iter_mut(app.world_mut()) {
            if t.id == task_id {
                t.status = TaskStatus::Failed(harness::FailureReason::AgentError);
            }
        }
    }

    // 推帧让 task_termination_system / task_completion_hook_system 触发。
    for _ in 0..3 {
        app.update();
    }

    // 验证去重集合包含 task id，说明 on_task_failed 已派发。
    let set: &harness::systems::TaskTerminalDispatched = app
        .world()
        .get_resource()
        .expect("TaskTerminalDispatched 应被 init_resource 注入");
    assert!(
        set.0.contains(&task_id),
        "on_task_failed 应将 task id 写入去重集合"
    );
}

/// 占位 component，仅作为 placeholder（让测试 entity 满足 `&HarnessIdPlaceholder`
/// 形式以确保类型不同；不影响系统逻辑）。
#[derive(Component, Default)]
struct HarnessIdPlaceholder;
