//! Task 2：异步桥领域类型行为测试。
//!
//! 验证：
//! - `ToolAsyncResult::completed` 的错误侧是 `ToolError`（不是 String），
//!   与 `ToolExecutionResultMessage.tool_output` 同型，ingest 落地零转换。
//! - `ToolAsyncResult::effect` 携带声明式 `ToolEffect`，效果与值走同一通道，
//!   ingest 按 payload 枚举分流。
//! - `ToolRequestPending` + `InFlightToolCall` 可作为 Component spawn 进 World，
//!   挂起实体与在飞标记并存（claim 语义基础：摘除 InFlightToolCall 时实体保留）。
//! - `OwnedToolContext::empty_for_test` 返回无快照的最小上下文，
//!   新增的 scheduler_state / registry 字段缺省为 None。
//!
//! 不依赖 worker 真实运行；纯类型与 ECS spawn 行为验证。

mod common;
use chrono::Duration as ChronoDuration;
use common::async_tool_bridge::*;
use harness::domain::{
    AgentExecutionRequest, AgentRequestKind, InFlightToolCall, OwnedToolContext, ToolAsyncResult,
    ToolEffect, ToolError, ToolRequestPending, ToolWorkerPayload,
};

#[test]
fn async_result_completed_carries_tool_error() {
    let r = ToolAsyncResult::completed("c1", Err(ToolError::Timeout("boom".into())));
    match r.payload {
        ToolWorkerPayload::Completed(Err(ToolError::Timeout(msg))) => assert_eq!(msg, "boom"),
        other => panic!("unexpected {:?}", other),
    }
}

#[test]
fn async_result_can_carry_effect() {
    let r = ToolAsyncResult::effect("c2", ToolEffect::DeleteScheduledTask { kind: "k".into() });
    match r.payload {
        ToolWorkerPayload::Effect(ToolEffect::DeleteScheduledTask { kind }) => {
            assert_eq!(kind, "k")
        }
        other => panic!("unexpected {:?}", other),
    }
}

#[test]
fn pending_entity_spawn_and_query() {
    let mut world = setup_bridge_world();
    let req = AgentExecutionRequest {
        task_id: uuid::Uuid::new_v4(),
        agent_id: uuid::Uuid::new_v4(),
        request_kind: AgentRequestKind::ToolExecution {
            tool_name: "list_scheduled_tasks".into(),
        },
        prompt: "p".into(),
        system_prompt: None,
        tools: vec![],
        conversation: None,
        work_item_id: None,
        model_override: None,
    };
    let e = world
        .spawn((
            ToolRequestPending {
                tool_call_id: "call-9".into(),
                tool_name: "list_scheduled_tasks".into(),
                original_request: std::sync::Arc::new(req),
            },
            InFlightToolCall {
                started_at: now(&world),
                timeout: ChronoDuration::seconds(300),
            },
        ))
        .id();
    let pending = world.get::<ToolRequestPending>(e).unwrap();
    assert_eq!(pending.tool_call_id, "call-9");
    assert!(world.get::<InFlightToolCall>(e).is_some());
}

#[test]
fn owned_context_empty_for_test() {
    let ctx = OwnedToolContext::empty_for_test(300);
    assert_eq!(ctx.tool_inflight_timeout_secs, 300);
    assert!(ctx.scheduler_state.is_none());
    assert!(ctx.registry.is_none());
}
