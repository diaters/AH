//! Task 46 集成测试：hook 派发行为。
//!
//! 验证：
//! - on_task_created hook 能观察新建的 task（通过 fixture test-plugin）
//! - on_tool_called hook 调用 tool_deny 能中止工具执行

use std::sync::{Arc, Mutex};

use crossbeam_channel::unbounded;
use tempfile::TempDir;
use tokio::runtime::Runtime;

use harness::{
    AgentExecutionOutput, AgentExecutionRequest, AgentExecutor, ChannelId, ExecutorFuture,
    ExternalInput, FrontendKind, HarnessConfig, NewlyCreatedTask, Task, ToolCalledHookPending,
    ToolExecutionResultMessage, build_harness_app, llm::ExecutorRegistry,
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

/// 进程内串行化 HARNESS_PLUGINS_DIR 访问的全局锁。
///
/// 见 `tests/user_plugins_on_task_created.rs` 中对 env 非线程安全的背景说明。
static PLUGIN_ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn on_task_created_hook_observes_new_task() {
    // 指向 fixture test-plugin 目录
    let fixtures_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("plugins");

    let _env_guard = PLUGIN_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    // SAFETY: 同 `tests/user_plugins_on_task_created.rs` 中 SAFETY 论证。
    unsafe {
        std::env::set_var("HARNESS_PLUGINS_DIR", &fixtures_dir);
    }

    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(EchoExecutor);
    let executor_registry = ExecutorRegistry::from_single_executor(executor, "default");
    let (input_tx, input_rx) = unbounded();
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

    // 验证 test-plugin 已被加载
    {
        let reg = app
            .world()
            .resource::<harness::user_plugins::registry::PluginRegistry>();
        assert!(
            reg.plugins().iter().any(|p| p.manifest.id == "test-plugin"),
            "test-plugin 应被加载"
        );
    }

    // 发送一条用户消息触发 task 创建
    input_tx
        .send(ExternalInput::TextWithChannel {
            channel: default_channel(),
            content: "trigger on_task_created via fixture".to_string(),
        })
        .unwrap();

    // 多步推帧让 Ingress -> Transform（user_message_to_task_system -> on_task_created_hook_system）
    for _ in 0..6 {
        app.update();
    }

    let world = app.world_mut();

    // 验证至少创建了一个 Task
    let task_count = world.query::<&Task>().iter(world).count();
    assert!(task_count >= 1, "应至少创建一个 Task，实际 {}", task_count);

    // companion 系统本应移除 NewlyCreatedTask 标记
    let marker_count = world.query::<&NewlyCreatedTask>().iter(world).count();
    assert_eq!(
        marker_count, 0,
        "NewlyCreatedTask 标记应在 hook 派发后全部移除"
    );
}

#[test]
fn on_tool_called_deny_aborts_execution() {
    let dir = TempDir::new().unwrap();
    // 创建一个调用 tool_deny 的插件
    let plugin_dir = dir.path().join("deny-plugin");
    std::fs::create_dir_all(plugin_dir.join("hooks")).unwrap();
    std::fs::write(
        plugin_dir.join("manifest.toml"),
        r#"
id = "deny-plugin"
api_version = 1
[[hooks]]
event = "on_tool_called"
script = "hooks/on_tool_called.rhai"
"#,
    )
    .unwrap();
    std::fs::write(
        plugin_dir.join("hooks/on_tool_called.rhai"),
        r#"
tool_deny("test deny reason");
"#,
    )
    .unwrap();

    let _env_guard = PLUGIN_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    // SAFETY: 同 `tests/user_plugins_on_task_created.rs` 中 SAFETY 论证。
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

    // spawn 一个带 ToolCalledHookPending 的请求
    let request_entity = {
        use harness::{AgentExecutionRequest, AgentRequestKind, ToolExecutionRequestMessage};
        let task_id = {
            let channel = default_channel();
            let task = Task::from_user_input("deny-test-task", 0, channel);
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
                    tool_input: serde_json::json!({}),
                    pending_confirmation_id: None,
                    tool_call_id: Some("deny-test-call-id".to_string()),
                    pending_confirmation_options: None,
                },
                ToolCalledHookPending,
            ))
            .id()
    };

    // 推一帧：companion 系统派发 hook → tool_deny → 替换为 PermissionDenied 错误
    app.update();

    let world = app.world_mut();

    // 请求 entity 应已被 companion 系统销毁
    assert!(
        world.get_entity(request_entity).is_err(),
        "被 hook 拒绝的 ToolExecutionRequestMessage entity 应已销毁"
    );

    // 应产出带 PermissionDenied 的结果消息
    let denied_count = world
        .query::<&ToolExecutionResultMessage>()
        .iter(world)
        .filter(|m| {
            matches!(
                &m.tool_output,
                Err(harness::ToolError::PermissionDenied(reason))
                    if reason.contains("denied by plugin")
            )
        })
        .count();
    assert!(
        denied_count >= 1,
        "应至少产出一条带 'denied by plugin' 的 PermissionDenied 结果"
    );

    // 不应存在 tool_output == Ok(..) 的成功结果
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
