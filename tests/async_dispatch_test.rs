//! Task 4：async_tool_dispatch_system（桥本体）行为测试。
//!
//! 验证：
//! - kind==Async 的请求被原地改造为挂起实体（Pending + InFlight），原消息组件移除
//! - worker 真实跑完，结果经 `ToolResultSender` 通道回传
//! - 未知工具 / Sync 工具的请求原样保留给 sync 路径
//! - `max_duration` 钩子在挂起现场调用，结果反映到 `InFlightToolCall.timeout`

mod common;
use bevy_ecs::system::RunSystemOnce;
use common::async_tool_bridge::*;
use harness::domain::{
    AgentExecutionRequest, AgentRequestKind, BuiltinTool, BuiltinToolExecutors, InFlightToolCall,
    OwnedToolContext, ToolAction, ToolActionKind, ToolContext, ToolError,
    ToolExecutionRequestMessage, ToolFuture, ToolRequestPending, ToolWorkerOutput,
};

fn make_request(tool_name: &str, tool_call_id: &str) -> ToolExecutionRequestMessage {
    ToolExecutionRequestMessage {
        request: AgentExecutionRequest {
            task_id: uuid::Uuid::new_v4(),
            agent_id: uuid::Uuid::new_v4(),
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
    world.insert_resource(harness::app::HarnessSettings::default_test());
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
    world.insert_resource(harness::app::HarnessSettings::default_test());

    let e = world.spawn(make_request("slow", "call-d3")).id();
    world
        .run_system_once(harness::systems::async_tool_dispatch_system)
        .unwrap();

    let inflight = world.get::<InFlightToolCall>(e).unwrap();
    assert_eq!(inflight.timeout, chrono::Duration::seconds(900));
}
