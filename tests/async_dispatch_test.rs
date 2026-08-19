//! Task 4：async_tool_dispatch_system（桥本体）行为测试。
//!
//! 验证：
//! - kind==Async 的请求被原地改造为挂起实体（Pending + InFlight），原消息组件移除
//! - worker 真实跑完，结果经 `ToolResultSender` 通道回传
//! - 未知工具 / Sync 工具的请求原样保留给 sync 路径
//! - `max_duration` 钩子在挂起现场调用，结果反映到 `InFlightToolCall.timeout`
//! - dispatch 从 `Task.origin_channel` 注入 `OwnedToolContext.current_origin_channel`
//!   （Task 14 Step E）

mod common;
use bevy_ecs::system::RunSystemOnce;
use common::async_tool_bridge::*;
use harness::domain::{
    AgentExecutionRequest, AgentRequestKind, BuiltinTool, BuiltinToolExecutors, ChannelId,
    FrontendKind, InFlightToolCall, OwnedToolContext, SkillCreationContext, Task, ToolAction,
    ToolActionKind, ToolContext, ToolError, ToolExecutionRequestMessage, ToolFuture,
    ToolRequestPending, ToolWorkerOutput,
};

fn make_request(tool_name: &str, tool_call_id: &str) -> ToolExecutionRequestMessage {
    ToolExecutionRequestMessage {
        request: AgentExecutionRequest {
            task_id: harness::domain::TaskId::new(),
            agent_id: harness::domain::AgentId::new(),
            request_kind: AgentRequestKind::ToolExecution {
                tool_name: tool_name.into(),
            },
            prompt: "p".into(),
            system_prompt: None,
            tools: vec![],
            conversation: None,
            work_item_id: None,
            model_override: None,
        },
        tool_name: tool_name.into(),
        tool_input: serde_json::json!({"hello": "world"}),
        pending_confirmation_id: None,
        tool_call_id: Some(tool_call_id.into()),
        pending_confirmation_options: None,
        work_item_entity: None,
        confirmed_once: false,
    }
}

/// HarnessSettings 无现成测试构造器——本 Task 顺带在 src/app/mod.rs 补一个
/// `default_test()`（tool_inflight_timeout_secs=300，其余字段按 HarnessConfig
/// 现有测试惯例填默认值）。
fn world_with_echo_tool() -> bevy_ecs::prelude::World {
    let mut world = setup_bridge_world();
    let mut executors = BuiltinToolExecutors::default();
    executors.register(Box::new(EchoAsyncTool));
    world.insert_resource(executors);
    world.insert_resource(harness::systems::HarnessSettings::default_test());
    world
}

#[test]
fn dispatch_parks_request_and_worker_reports_back() {
    let mut world = world_with_echo_tool();
    let req_entity = world.spawn(make_request("echo_async", "call-d1")).id();

    world
        .run_system_once(harness::systems::async_tool_dispatch_system)
        .unwrap();

    // 请求实体被改造为挂起实体（原消息组件移除，挂上 Pending + InFlight）
    assert!(
        world
            .get::<ToolExecutionRequestMessage>(req_entity)
            .is_none()
    );
    let pending = world
        .get::<ToolRequestPending>(req_entity)
        .expect("pending component");
    assert_eq!(pending.tool_call_id, "call-d1");
    assert!(world.get::<InFlightToolCall>(req_entity).is_some());

    // worker 真实跑完并把结果送进通道
    let result = wait_for_tool_result(&mut world, 2000).expect("worker result");
    assert_eq!(result.tool_call_id, "call-d1");
    match result.payload {
        harness::domain::ToolWorkerPayload::Completed(Ok(v)) => {
            assert_eq!(v["hello"], "world")
        }
        other => panic!("unexpected {:?}", other),
    }
}

#[test]
fn dispatch_ignores_sync_tools_and_unknown_tools() {
    let mut world = world_with_echo_tool();
    // 未知工具：原样保留给 sync 路径报错
    let e = world.spawn(make_request("no_such_tool", "call-d2")).id();
    world
        .run_system_once(harness::systems::async_tool_dispatch_system)
        .unwrap();
    assert!(world.get::<ToolExecutionRequestMessage>(e).is_some());
    assert!(world.get::<ToolRequestPending>(e).is_none());
}

#[test]
fn dispatch_uses_max_duration_hook_for_inflight_timeout() {
    // 自定义 max_duration 的工具 → InFlightToolCall.timeout 反映钩子值
    struct SlowTool;
    impl BuiltinTool for SlowTool {
        fn name(&self) -> &str {
            "slow"
        }
        fn kind(&self) -> ToolActionKind {
            ToolActionKind::Async
        }
        fn max_duration(&self, _: &serde_json::Value, _default_secs: u64) -> std::time::Duration {
            std::time::Duration::from_secs(900)
        }
        fn execute(&self, _: &serde_json::Value, _: &ToolContext) -> Result<ToolAction, ToolError> {
            unreachable!()
        }
        fn run_async(&self, _: serde_json::Value, _: OwnedToolContext) -> ToolFuture {
            Box::pin(async { Ok(ToolWorkerOutput::Value(serde_json::json!({}))) })
        }
    }

    let mut world = setup_bridge_world();
    let mut executors = BuiltinToolExecutors::default();
    executors.register(Box::new(SlowTool));
    world.insert_resource(executors);
    world.insert_resource(harness::systems::HarnessSettings::default_test());

    let e = world.spawn(make_request("slow", "call-d3")).id();
    world
        .run_system_once(harness::systems::async_tool_dispatch_system)
        .unwrap();

    let inflight = world.get::<InFlightToolCall>(e).unwrap();
    assert_eq!(inflight.timeout, chrono::Duration::seconds(900));
}

/// Task 14 Step E：dispatch 应从 `Task.origin_channel` 注入
/// `OwnedToolContext.current_origin_channel`，让 schedule_task 等需要继承通道的
/// 异步工具能在 worker 内拿到真值。
///
/// 构造一个 Telegram origin_channel 的 Task + 一个捕获 ctx.current_origin_channel
/// 的 echo 工具，dispatch 后 worker 应把捕获到的 channel 回送。
#[test]
fn dispatch_injects_current_origin_channel_from_task() {
    // 捕获 ctx.current_origin_channel 并把它原样回送的探针工具
    struct ChannelProbeTool;
    impl BuiltinTool for ChannelProbeTool {
        fn name(&self) -> &str {
            "channel_probe"
        }
        fn kind(&self) -> ToolActionKind {
            ToolActionKind::Async
        }
        fn execute(&self, _: &serde_json::Value, _: &ToolContext) -> Result<ToolAction, ToolError> {
            unreachable!()
        }
        fn run_async(&self, _: serde_json::Value, ctx: OwnedToolContext) -> ToolFuture {
            Box::pin(async move {
                Ok(ToolWorkerOutput::Value(serde_json::json!({
                    "captured_origin_channel": match &ctx.current_origin_channel {
                        Some(ch) => serde_json::json!({
                            "frontend": format!("{:?}", ch.frontend),
                            "user_id": ch.user_id,
                        }),
                        None => serde_json::Value::Null,
                    }
                })))
            })
        }
    }

    let mut world = setup_bridge_world();
    let mut executors = BuiltinToolExecutors::default();
    executors.register(Box::new(ChannelProbeTool));
    world.insert_resource(executors);
    world.insert_resource(harness::systems::HarnessSettings::default_test());

    // 构造一个带 Telegram origin_channel 的 Task
    let telegram_channel = ChannelId {
        frontend: FrontendKind::Telegram,
        user_id: "tg-probe".to_string(),
        thread_id: None,
    };
    let task_id = harness::domain::TaskId::new();
    let mut task = Task::from_user_input("probe task", 3, telegram_channel.clone());
    task.id = task_id;
    task.multi_turn = false;
    let task_entity = world.spawn(task).id();
    world
        .resource_mut::<harness::ecs::EntityIndex>()
        .tasks
        .insert(task_id, task_entity);

    // 构造引用该 task_id 的请求
    let mut request = make_request("channel_probe", "call-channel-probe");
    request.request.task_id = task_id;
    world.spawn(request);

    world
        .run_system_once(harness::systems::async_tool_dispatch_system)
        .unwrap();

    let result = wait_for_tool_result(&mut world, 2000).expect("worker result");
    match result.payload {
        harness::domain::ToolWorkerPayload::Completed(Ok(v)) => {
            let captured = &v["captured_origin_channel"];
            assert!(
                !captured.is_null(),
                "current_origin_channel must be injected (got null); \
                 expected Telegram channel captured"
            );
            assert_eq!(captured["user_id"], "tg-probe");
            assert_eq!(captured["frontend"], "Telegram");
        }
        other => panic!("unexpected payload {:?}", other),
    }
}

/// Task 14 Step E：Task 不存在时，`current_origin_channel` 应为 `None`
/// 而非 panic（与「Test 世界可能缺 Task」的容忍语义一致）。
#[test]
fn dispatch_handles_missing_task_gracefully_for_origin_channel() {
    // 复用探针工具，但请求引用一个不存在的 task_id
    struct ChannelProbeTool;
    impl BuiltinTool for ChannelProbeTool {
        fn name(&self) -> &str {
            "channel_probe_missing"
        }
        fn kind(&self) -> ToolActionKind {
            ToolActionKind::Async
        }
        fn execute(&self, _: &serde_json::Value, _: &ToolContext) -> Result<ToolAction, ToolError> {
            unreachable!()
        }
        fn run_async(&self, _: serde_json::Value, ctx: OwnedToolContext) -> ToolFuture {
            Box::pin(async move {
                Ok(ToolWorkerOutput::Value(serde_json::json!({
                    "captured_origin_channel": if ctx.current_origin_channel.is_some() {
                        serde_json::Value::String("some".to_string())
                    } else {
                        serde_json::Value::Null
                    }
                })))
            })
        }
    }

    let mut world = setup_bridge_world();
    let mut executors = BuiltinToolExecutors::default();
    executors.register(Box::new(ChannelProbeTool));
    world.insert_resource(executors);
    world.insert_resource(harness::systems::HarnessSettings::default_test());

    // 故意不 spawn Task；request 引用一个随机 task_id
    let _entity = world
        .spawn(make_request("channel_probe_missing", "call-no-task"))
        .id();

    world
        .run_system_once(harness::systems::async_tool_dispatch_system)
        .unwrap();

    let result = wait_for_tool_result(&mut world, 2000).expect("worker result");
    match result.payload {
        harness::domain::ToolWorkerPayload::Completed(Ok(v)) => {
            assert!(
                v["captured_origin_channel"].is_null(),
                "missing Task should yield null current_origin_channel, not panic"
            );
        }
        other => panic!("unexpected payload {:?}", other),
    }
}

/// 回归修复：`async_tool_dispatch_system` 应从 WorkItem 的 `SkillCreationContext`
/// 注入 `OwnedToolContext.current_skill_dir`，让 `write_skill_file` 等 skill 工具
/// 在 worker 内拿到沙盒目录。同步路径 `dispatch.rs` 已正确注入，异步路径此前
/// 写死 `None`（bug：write_skill_file 报 "no skill directory in current context"）。
///
/// 构造一个带 SkillCreationContext 的 WorkItem entity + 捕获 ctx.current_skill_dir
/// 的探针工具，dispatch 后 worker 应把捕获到的 sandbox_dir 回送。
#[test]
fn dispatch_injects_current_skill_dir_from_skill_creation_context() {
    // 捕获 ctx.current_skill_dir 并把它原样回送的探针工具
    struct SkillDirProbeTool;
    impl BuiltinTool for SkillDirProbeTool {
        fn name(&self) -> &str {
            "skill_dir_probe"
        }
        fn kind(&self) -> ToolActionKind {
            ToolActionKind::Async
        }
        fn execute(&self, _: &serde_json::Value, _: &ToolContext) -> Result<ToolAction, ToolError> {
            unreachable!()
        }
        fn run_async(&self, _: serde_json::Value, ctx: OwnedToolContext) -> ToolFuture {
            Box::pin(async move {
                Ok(ToolWorkerOutput::Value(serde_json::json!({
                    "captured_skill_dir": match &ctx.current_skill_dir {
                        Some(d) => serde_json::Value::String(d.to_string_lossy().to_string()),
                        None => serde_json::Value::Null,
                    }
                })))
            })
        }
    }

    let mut world = setup_bridge_world();
    let mut executors = BuiltinToolExecutors::default();
    executors.register(Box::new(SkillDirProbeTool));
    world.insert_resource(executors);
    world.insert_resource(harness::systems::HarnessSettings::default_test());

    // 构造带 SkillCreationContext 的 WorkItem entity（sandbox_dir 应注入 ctx）
    let task_id = harness::domain::TaskId::new();
    let agent_id = harness::domain::AgentId::new();
    let sandbox = std::path::PathBuf::from("/tmp/probe-sandbox");
    let wi_entity = world
        .spawn(SkillCreationContext {
            task_id,
            agent_id,
            agent_name: "skill-creator".to_string(),
            sandbox_dir: sandbox.clone(),
            skill_name: "probe".to_string(),
        })
        .id();

    // 请求携带 work_item_entity 引用
    let mut request = make_request("skill_dir_probe", "call-skill-dir");
    request.work_item_entity = Some(wi_entity);
    world.spawn(request);

    world
        .run_system_once(harness::systems::async_tool_dispatch_system)
        .unwrap();

    let result = wait_for_tool_result(&mut world, 2000).expect("worker result");
    match result.payload {
        harness::domain::ToolWorkerPayload::Completed(Ok(v)) => {
            let captured = &v["captured_skill_dir"];
            assert!(
                !captured.is_null(),
                "current_skill_dir must be injected from SkillCreationContext (got null); \
                 expected sandbox_dir {}",
                sandbox.display()
            );
            assert_eq!(
                captured.as_str().unwrap(),
                "/tmp/probe-sandbox",
                "current_skill_dir should match SkillCreationContext.sandbox_dir"
            );
        }
        other => panic!("unexpected payload {:?}", other),
    }
}
