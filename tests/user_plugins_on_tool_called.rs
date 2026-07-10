//! Task 19 集成测试：on_tool_called 前置 hook 接入（含拒绝能力）。
//!
//! 验证：
//! - 当插件订阅 `on_tool_called` 但不调用 `tool_deny` 时，工具正常执行，
//!   `ToolCalledHookPending` 标记被 companion 系统移除。
//! - 当插件调用 `tool_deny` 时，请求被替换为 `PermissionDenied` 错误结果，
//!   不产出成功的 `ToolExecutionResultMessage`，请求 entity 被销毁。

use std::sync::{Arc, Mutex};

use crossbeam_channel::unbounded;
use harness::prelude::*;
use tempfile::TempDir;
use tokio::runtime::Runtime;

use harness::{
    AgentExecutionOutput, AgentExecutionRequest, AgentExecutor, ChannelId, ExecutorFuture,
    FrontendKind, HarnessConfig, ToolCalledHookPending, ToolExecutionResultMessage,
    build_harness_app, llm::ExecutorRegistry,
};

fn default_channel() -> ChannelId {
    ChannelId {
        frontend: FrontendKind::Tui,
        user_id: "default".to_string(),
        thread_id: None,
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

/// 进程内串行化 `HARNESS_PLUGINS_DIR` 访问的全局锁。
///
/// 见 `tests/user_plugins_on_task_created.rs` 中对 env 非线程安全的背景说明。
static PLUGIN_ENV_LOCK: Mutex<()> = Mutex::new(());

/// 在 dir/alpha 下写入一个最小插件，订阅指定的 hook 点并加载指定 Rhai 脚本。
fn write_plugin(dir: &std::path::Path, event: &str, script_body: &str) {
    let plugin_dir = dir.join("alpha");
    std::fs::create_dir_all(plugin_dir.join("hooks")).unwrap();
    let manifest = format!(
        r#"
id = "alpha"
api_version = 1
[[hooks]]
event = "{event}"
script = "hooks/hook.rhai"
"#
    );
    std::fs::write(plugin_dir.join("manifest.toml"), manifest).unwrap();
    std::fs::write(plugin_dir.join("hooks/hook.rhai"), script_body).unwrap();
}

/// 直接在 world 中 spawn 一个带 `ToolCalledHookPending` 标记的占位
/// `ToolExecutionRequestMessage`，便于在不需要走 LLM 的前提下测试
/// companion 系统的 hook 派发路径。
fn spawn_tool_request(world: &mut World, tool_name: &str, task_id: harness::TaskId) {
    use harness::{
        AgentExecutionRequest, AgentRequestKind, ToolExecutionRequestMessage, llm::ExecutorRegistry,
    };

    world.spawn((
        ToolExecutionRequestMessage {
            request: AgentExecutionRequest {
                task_id,
                agent_id: uuid::Uuid::nil(),
                request_kind: AgentRequestKind::ToolExecution {
                    tool_name: tool_name.to_string(),
                },
                prompt: String::new(),
                system_prompt: None,
                tools: vec![],
                conversation: None,
                work_item_id: None,
                model_override: None,
            },
            tool_name: tool_name.to_string(),
            tool_input: serde_json::json!({}),
            pending_confirmation_id: None,
            tool_call_id: Some("test-call-id".to_string()),
            pending_confirmation_options: None,
        },
        ToolCalledHookPending,
    ));
}

/// 构造一个已 init 的 app，并把指定插件挂到 `HARNESS_PLUGINS_DIR`。
fn build_app(_dir: &tempfile::TempDir) -> App {
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
    app
}

#[test]
fn tool_call_runs_when_no_deny() {
    let dir = TempDir::new().unwrap();
    write_plugin(
        dir.path(),
        "on_tool_called",
        r#"
log_info("on_tool_called: observing, no deny");
"#,
    );

    let _env_guard = PLUGIN_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    // SAFETY: 同 `tests/user_plugins_on_task_created.rs` 中 SAFETY 论证。
    unsafe {
        std::env::set_var("HARNESS_PLUGINS_DIR", dir.path());
    }

    let mut app = build_app(&dir);

    let task_id = {
        use harness::Task;
        let channel = default_channel();
        let task = Task::from_user_input("owner-task", 0, channel);
        let id = task.id;
        // 把 task 加进 world，让 hook 的 snapshot 读出非空 task 列表。
        app.world_mut().spawn(task);
        id
    };

    spawn_tool_request(app.world_mut(), "shell_exec", task_id);

    // 推一帧：Dispatch 之前 companion 系统派发 hook，再由 tool_dispatch_system 处理。
    app.update();

    let world = app.world_mut();
    let marker_count = world.query::<&ToolCalledHookPending>().iter(world).count();
    assert_eq!(
        marker_count, 0,
        "未拒绝时 companion 系统应移除 ToolCalledHookPending 标记"
    );

    // 请求 entity 应仍存在并后续由 tool_dispatch_system 处理（或因没有注册的
    // executor 被打回 NotFound 错误）。这里只断言没有记录 tool_deny 引起的
    // PermissionDenied 错误路径。
    let denied_count = world
        .query::<&ToolExecutionResultMessage>()
        .iter(world)
        .filter(|m| matches!(&m.tool_output, Err(e) if matches!(e, harness::ToolError::PermissionDenied(_))))
        .count();
    assert_eq!(denied_count, 0, "未拒绝时不应出现 PermissionDenied 结果");
}

#[test]
fn tool_call_aborts_when_plugin_denies() {
    let dir = TempDir::new().unwrap();
    write_plugin(
        dir.path(),
        "on_tool_called",
        r#"
tool_deny("blocked by test");
"#,
    );

    let _env_guard = PLUGIN_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    // SAFETY: 同 `tests/user_plugins_on_task_created.rs` 中 SAFETY 论证。
    unsafe {
        std::env::set_var("HARNESS_PLUGINS_DIR", dir.path());
    }

    let mut app = build_app(&dir);

    let request_entity = {
        use harness::{
            AgentExecutionRequest, AgentRequestKind, ToolExecutionRequestMessage,
            llm::ExecutorRegistry,
        };
        let task_id = {
            use harness::Task;
            let channel = default_channel();
            let task = Task::from_user_input("owner-task", 0, channel);
            let id = task.id;
            app.world_mut().spawn(task);
            id
        };

        app.world_mut()
            .spawn((
                ToolExecutionRequestMessage {
                    request: AgentExecutionRequest {
                        task_id,
                        agent_id: uuid::Uuid::nil(),
                        request_kind: AgentRequestKind::ToolExecution {
                            tool_name: "shell_exec".to_string(),
                        },
                        prompt: String::new(),
                        system_prompt: None,
                        tools: vec![],
                        conversation: None,
                        work_item_id: None,
                        model_override: None,
                    },
                    tool_name: "shell_exec".to_string(),
                    tool_input: serde_json::json!({"command": "echo x"}),
                    pending_confirmation_id: None,
                    tool_call_id: Some("deny-call-id".to_string()),
                    pending_confirmation_options: None,
                },
                ToolCalledHookPending,
            ))
            .id()
    };

    app.update();

    let world = app.world_mut();

    // 请求 entity 应已被 companion 系统销毁（替换为错误结果）。
    assert!(
        world.get_entity(request_entity).is_err(),
        "被 hook 拒绝的 ToolExecutionRequestMessage entity 应已销毁"
    );

    // 应产出带 PermissionDenied 的结果消息。
    let denied_results: Vec<&ToolExecutionResultMessage> = world
        .query::<&ToolExecutionResultMessage>()
        .iter(world)
        .filter(|m| {
            matches!(
                &m.tool_output,
                Err(harness::ToolError::PermissionDenied(reason))
                    if reason.contains("denied by plugin")
            )
        })
        .collect();
    assert!(
        !denied_results.is_empty(),
        "应至少产出一条带 'denied by plugin' 的 PermissionDenied 结果"
    );

    // 不应存在 tool_output == Ok(..) 的成功结果。
    let ok_count = world
        .query::<&ToolExecutionResultMessage>()
        .iter(world)
        .filter(|m| m.tool_output.is_ok())
        .count();
    assert_eq!(
        ok_count, 0,
        "被拒绝时不应有 Ok 类型的 ToolExecutionResultMessage"
    );
}
