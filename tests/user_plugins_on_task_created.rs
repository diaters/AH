//! Task 16 集成测试：on_task_created hook 接入。
//!
//! 验证：在 `HARNESS_PLUGINS_DIR` 下放一个订阅 `on_task_created` 的插件，
//! 发送一条用户消息触发 task 创建后，companion 系统能派发 hook 并 flush
//! `WorldCommand`，不 panic，且 `NewlyCreatedTask` 标记被移除。
//!
//! hook 脚本同时调用 `task_set_metadata`（用于验证 deferred 分支不 panic）
//! 与 `get_task_ids()`（用于验证 snapshot 注入）。

use std::sync::Arc;

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

    // 用 HARNESS_PLUGINS_DIR 指向临时插件目录。注意：本测试只在一个测试函数内使用，
    // 同二进制无其它测试并行读取此环境变量。
    // SAFETY: 单线程测试，无并发 set_var/read。
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
