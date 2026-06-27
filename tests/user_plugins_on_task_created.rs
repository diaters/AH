//! Task 16 集成测试：on_task_created hook 接入。
//!
//! 验证：在 `HARNESS_PLUGINS_DIR` 下放一个订阅 `on_task_created` 的插件，
//! 发送一条用户消息触发 task 创建后，companion 系统能派发 hook 并 flush
//! `WorldCommand`，不 panic，且 `NewlyCreatedTask` 标记被移除。
//!
//! hook 脚本同时调用 `task_set_metadata`（用于验证 deferred 分支不 panic）
//! 与 `get_task_ids()`（用于验证 snapshot 注入）。

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use crossbeam_channel::unbounded;
use tempfile::TempDir;
use tokio::runtime::Runtime;

use harness::{
    AgentExecutionOutput, AgentExecutionRequest, AgentExecutor, ChannelId, ExecutorFuture,
    ExternalInput, FrontendKind, HarnessConfig, NewlyCreatedTask, Task, build_harness_app,
};

fn default_channel() -> ChannelId {
    ChannelId {
        frontend: FrontendKind::Tui,
        user_id: "default".to_string(),
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
///
/// `std::env::set_var` / `std::env::var` 都是进程级全局状态。Rust 测试二进制默认
/// 并行运行测试函数，若多个测试同时读写同一 env var 会触发 UB（即使 var
/// 只在一个测试里用）。这里用一把全局 Mutex 约束：本二进制中任何需要触碰
/// `HARNESS_PLUGINS_DIR` 的测试都必须先持锁，从而把并发 set_var 串行化。这不是
/// “环境变量是线程安全的” 的证据——恰恰相反，正是因为环境变量并非线程安全，
/// 才需要用 Mutex 在测试层面强制独占。
static PLUGIN_ENV_LOCK: Mutex<()> = Mutex::new(());

/// 写入一个订阅 `on_task_created` 的最小插件到 dir/alpha。
fn write_alpha_plugin(dir: &std::path::Path) {
    let plugin_dir = dir.join("alpha");
    std::fs::create_dir_all(plugin_dir.join("hooks")).unwrap();
    std::fs::write(
        plugin_dir.join("manifest.toml"),
        r#"
id = "alpha"
api_version = 1
[[hooks]]
event = "on_task_created"
script = "hooks/on_task_created.rhai"
"#,
    )
    .unwrap();
    std::fs::write(
        plugin_dir.join("hooks/on_task_created.rhai"),
        // 同时调用 deferred 分支的 host API 与 snapshot 查询，确保不 panic。
        r#"
let ids = get_task_ids();
log_info("on_task_created: task count = " + ids.len());
task_set_metadata("00000000-0000-0000-0000-000000000000", "k", "v");
"#,
    )
    .unwrap();
}

#[test]
fn on_task_created_hook_dispatches_without_panic_and_clears_marker() {
    let dir = TempDir::new().unwrap();
    write_alpha_plugin(dir.path());

    // 用 HARNESS_PLUGINS_DIR 指向临时插件目录。通过 `PLUGIN_ENV_LOCK` 进程内串行化，
    // 避免与同二进制其它测试并发 set_var/read env 触发 UB。
    //
    // 曾经以 `unsafe { std::env::set_var(..) }` 配“单线程测试” SAFETY 论证是不对的：
    // Rust 测试二进制默认并行运行多个 test 函数，`set_var` 并非单线程独有。
    // 取而代之以 Mutex 序列化所有触碰 HARNESS_PLUGINS_DIR 的测试，锁析构后变量仍可能
    // 残留进程级状态，但不影响正确性：后续若新增同 env 的测试，持锁后即可安全覆写。
    let _env_guard = PLUGIN_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    // SAFETY: `std::env::set_var` 操作的是进程级全局 environ。Rust 测试二进制默认
    // 并行执行多个 test 函数，若不串行化则同进程内并发的 set/var 即为 UB。此处
    // 通过 `PLUGIN_ENV_LOCK` 全局 Mutex 强制本二进制中所有触碰此 env 的测试串行
    // 运行，且本测试在持锁期间不会向其它线程分发读取此 env 的工作。临时目录由
    // `TempDir` 持有到本函数结束，env 指向的路径在 set 之后到测试结束之间均合法。
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
        harness::channels::ChannelManager::empty().0,
    );

    // 初始化应用（让 Startup 阶段加载插件）
    app.update();

    // 验证插件已被加载
    {
        let reg = app
            .world()
            .resource::<harness::user_plugins::registry::PluginRegistry>();
        assert!(
            reg.plugins().iter().any(|p| p.manifest.id == "alpha"),
            "alpha 插件应被加载"
        );
    }

    // 发送一条用户消息触发 task 创建
    input_tx
        .send(ExternalInput::TextWithChannel {
            channel: default_channel(),
            content: "trigger on_task_created".to_string(),
        })
        .unwrap();

    // 多步推帧让 Ingress -> Transform（user_message_to_task_system -> on_task_created_hook_system）
    for _ in 0..6 {
        app.update();
    }

    let world = app.world_mut();
    let task_count = world.query::<&Task>().iter(world).count();
    assert!(task_count >= 1, "应至少创建一个 Task，实际 {}", task_count);

    // companion 系统本应移除 NewlyCreatedTask 标记。验证无标记残留。
    let marker_count = world.query::<&NewlyCreatedTask>().iter(world).count();
    assert_eq!(
        marker_count, 0,
        "NewlyCreatedTask 标记应在 hook 派发后全部移除"
    );
}
