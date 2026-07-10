//! Task 20 集成测试：on_tool_returned 观察 hook 接入。
//!
//! 验证：
//! - 插件观察工具结果但不调用 `tool_set_result` 时，原始 `tool_output` 不变。
//! - 插件调用 `tool_set_result` 替换结果时，`tool_output` 被替换，
//!   原始值保留在 `original_tool_output` 审计字段。
//!
//! 测试通过 `ToolCallingState` 阻止 `tool_result_system` 提前 despawn 结果 entity，
//! 保证在推帧后仍可检查 `ToolExecutionResultMessage` 的字段值。

use std::sync::{Arc, Mutex};

use crossbeam_channel::unbounded;
use harness::prelude::*;
use tempfile::TempDir;
use tokio::runtime::Runtime;

use harness::{
    AgentExecutionOutput, AgentExecutionRequest, AgentExecutor, ChannelId, ExecutorFuture,
    FrontendKind, HarnessConfig, ToolCallingState, ToolExecutionResultMessage,
    ToolReturnedHookPending, build_harness_app,
};

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
/// 参见 `tests/user_plugins_on_task_created.rs` 中 `PLUGIN_ENV_LOCK` 的说明。
static PLUGIN_ENV_LOCK: Mutex<()> = Mutex::new(());

/// 写入一个订阅 `on_tool_returned` 的观察型插件到 dir/observer。
fn write_observer_plugin(dir: &std::path::Path) {
    let plugin_dir = dir.join("observer");
    std::fs::create_dir_all(plugin_dir.join("hooks")).unwrap();
    std::fs::write(
        plugin_dir.join("manifest.toml"),
        r#"
id = "observer"
api_version = 1
[[hooks]]
event = "on_tool_returned"
script = "hooks/on_tool_returned.rhai"
"#,
    )
    .unwrap();
    std::fs::write(
        plugin_dir.join("hooks/on_tool_returned.rhai"),
        // 仅观察，不调用 tool_set_result
        r#"
let ids = get_task_ids();
log_info("on_tool_returned: task count = " + ids.len());
"#,
    )
    .unwrap();
}

/// 写入一个订阅 `on_tool_returned` 的替换型插件到 dir/replacer。
fn write_replacer_plugin(dir: &std::path::Path) {
    let plugin_dir = dir.join("replacer");
    std::fs::create_dir_all(plugin_dir.join("hooks")).unwrap();
    std::fs::write(
        plugin_dir.join("manifest.toml"),
        r#"
id = "replacer"
api_version = 1
[[hooks]]
event = "on_tool_returned"
script = "hooks/on_tool_returned.rhai"
"#,
    )
    .unwrap();
    std::fs::write(
        plugin_dir.join("hooks/on_tool_returned.rhai"),
        // 调用 tool_set_result 替换输出
        // 注：v1 host API 的 tool_set_result 将 Rhai Dynamic 转为字符串形式，
        // 因此传入 "replaced" 产生 serde_json::Value::String("replaced")。
        r#"
tool_set_result("replaced");
"#,
    )
    .unwrap();
}

/// 手动向 World 注入一个带标记的 `ToolExecutionResultMessage`，
/// 模拟工具执行完成后产出结果消息。
///
/// 同时添加 `ToolCallingState` 使 `tool_result_system` 保留结果 entity
/// （不 despawn），允许后续检查字段值。
fn inject_result_entity(world: &mut World, tool_output: serde_json::Value) -> Entity {
    let task_id = uuid::Uuid::new_v4();
    let agent_id = uuid::Uuid::nil();
    // 确保 Task 存在（tool_result_system 需要匹配 task_id）。
    let channel = ChannelId {
        frontend: FrontendKind::Tui,
        user_id: "test".to_string(),
        thread_id: None,
    };
    let mut task = harness::Task::from_user_input("test", 0, channel);
    task.id = task_id;
    world.spawn(task);

    // 添加 ToolCallingState 使 tool_result_system 保留结果 entity。
    world.spawn(ToolCallingState {
        task_id,
        agent_id,
        pending_tool_call_ids: vec!["call-1".to_string()],
        iteration: 1,
        max_iterations: 10,
        conversation: vec![],
        tools: vec![],
        request_kind: harness::AgentRequestKind::LlmCompletion,
        work_item_id: None,
    });

    let execution_result = harness::AgentExecutionResult {
        task_id,
        agent_id,
        request_kind: harness::AgentRequestKind::LlmCompletion,
        result: Ok(harness::AgentExecutionOutput {
            content: harness::OutputContent::Text("tool executed".to_string()),
            reasoning_content: None,
        }),
        prompt: String::new(),
        system_prompt: None,
        tools: vec![],
        reasoning_content: None,
        work_item_id: None,
    };

    world
        .spawn((
            ToolExecutionResultMessage {
                result: execution_result,
                tool_name: "shell_exec".to_string(),
                tool_output: Ok(tool_output),
                tool_call_id: Some("call-1".to_string()),
                processed: false,
                original_tool_output: None,
            },
            ToolReturnedHookPending,
        ))
        .id()
}

#[test]
fn tool_result_observed_without_modification() {
    let dir = TempDir::new().unwrap();
    write_observer_plugin(dir.path());

    let _env_guard = PLUGIN_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    // SAFETY: 同 `user_plugins_on_task_created.rs` 中 PLUGIN_ENV_LOCK 的论证。
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
        harness::channels::ChannelManager::empty().0,
    );

    // 初始化应用（让 Startup 阶段加载插件）
    app.update();

    // 注入一个带标记的工具结果
    let entity = inject_result_entity(app.world_mut(), serde_json::json!({"count": 3}));

    // 推帧让 companion 系统和 tool_result_system 处理
    app.update();

    let world = app.world();

    // 标记应被移除
    assert!(
        world.get::<ToolReturnedHookPending>(entity).is_none(),
        "ToolReturnedHookPending 标记应在 hook 派发后移除"
    );

    // tool_output 不应被修改
    let msg = world.get::<ToolExecutionResultMessage>(entity).unwrap();
    assert_eq!(msg.tool_output, Ok(serde_json::json!({"count": 3})));
    assert!(
        msg.original_tool_output.is_none(),
        "未替换时 original_tool_output 应为 None"
    );
}

#[test]
fn tool_result_replaced_when_plugin_sets_result() {
    let dir = TempDir::new().unwrap();
    write_replacer_plugin(dir.path());

    let _env_guard = PLUGIN_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    // SAFETY: 同 `user_plugins_on_task_created.rs` 中 PLUGIN_ENV_LOCK 的论证。
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
        harness::channels::ChannelManager::empty().0,
    );

    // 初始化应用（让 Startup 阶段加载插件）
    app.update();

    // 注入一个带标记的工具结果
    let entity = inject_result_entity(app.world_mut(), serde_json::json!({"count": 3}));

    // 推帧让 companion 系统和 tool_result_system 处理
    app.update();

    let world = app.world();

    // 标记应被移除
    assert!(
        world.get::<ToolReturnedHookPending>(entity).is_none(),
        "ToolReturnedHookPending 标记应在 hook 派发后移除"
    );

    // tool_output 应被替换为插件提供的值（v1 host API 将字符串转为 JSON String）
    let msg = world.get::<ToolExecutionResultMessage>(entity).unwrap();
    assert_eq!(msg.tool_output, Ok(serde_json::json!("replaced")));

    // 原始输出应保留在审计字段
    assert_eq!(
        msg.original_tool_output,
        Some(serde_json::json!({"count": 3})),
        "原始 tool_output 应保留在 original_tool_output 审计字段"
    );
}
