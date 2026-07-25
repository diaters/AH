//! sweeper 失联兜底行为测试。
//!
//! 验证 D4 claim 语义：
//! - 超时：发 error 入通道 + 摘除 InFlightToolCall（claim）
//! - 不 despawn 挂起实体（落地 + despawn 只在 ingest 单点）
//! - claim 后时钟再推进，不再重扫该实体（防重复发 error）
//! - 未超时：保持原状不动

mod common;
use bevy_ecs::system::RunSystemOnce;
use common::async_tool_bridge::*;
use harness::domain::{InFlightToolCall, ToolRequestPending, ToolWorkerPayload};

fn spawn_inflight(
    world: &mut bevy_ecs::prelude::World,
    call_id: &str,
    age_secs: i64,
    timeout_secs: i64,
) -> bevy_ecs::prelude::Entity {
    let req = harness::domain::AgentExecutionRequest {
        task_id: uuid::Uuid::new_v4(),
        agent_id: uuid::Uuid::new_v4(),
        request_kind: harness::domain::AgentRequestKind::ToolExecution {
            tool_name: "t".into(),
        },
        prompt: String::new(),
        system_prompt: None,
        tools: vec![],
        conversation: None,
        work_item_id: None,
        model_override: None,
    };
    world
        .spawn((
            ToolRequestPending {
                tool_call_id: call_id.into(),
                tool_name: "t".into(),
                original_request: std::sync::Arc::new(req),
            },
            InFlightToolCall {
                // 关键：从世界时钟取基准，禁止 Utc::now()
                started_at: now(world) - chrono::Duration::seconds(age_secs),
                timeout: chrono::Duration::seconds(timeout_secs),
                cancel: tokio_util::sync::CancellationToken::new(),
            },
        ))
        .id()
}

#[test]
fn sweeper_sends_error_and_claims_but_does_not_despawn() {
    let mut world = setup_bridge_world();
    let e = spawn_inflight(&mut world, "sw1", 400, 300);

    world
        .run_system_once(harness::systems::sweep_inflight_tool_calls)
        .unwrap();

    // error 结果入通道
    let result = wait_for_tool_result(&mut world, 100).expect("sweeper error result");
    assert_eq!(result.tool_call_id, "sw1");
    match result.payload {
        ToolWorkerPayload::Completed(Err(harness::domain::ToolError::Timeout(_))) => {}
        other => panic!("expected Timeout error, got {:?}", other),
    }

    // claim：InFlightToolCall 摘除，挂起实体保留（等 ingest 落地后 despawn）
    assert!(world.get::<InFlightToolCall>(e).is_none());
    assert!(world.get::<ToolRequestPending>(e).is_some());
}

#[test]
fn sweeper_leaves_non_timeout_alone() {
    let mut world = setup_bridge_world();
    let e = spawn_inflight(&mut world, "sw2", 100, 300);

    world
        .run_system_once(harness::systems::sweep_inflight_tool_calls)
        .unwrap();

    assert!(wait_for_tool_result(&mut world, 20).is_none());
    assert!(world.get::<InFlightToolCall>(e).is_some());
    assert!(world.get::<ToolRequestPending>(e).is_some());
}

#[test]
fn sweeper_does_not_reclaim_after_clock_advances() {
    let mut world = setup_bridge_world();
    spawn_inflight(&mut world, "sw3", 400, 300);

    // 第一次 sweep：claim 掉
    world
        .run_system_once(harness::systems::sweep_inflight_tool_calls)
        .unwrap();
    let first = wait_for_tool_result(&mut world, 100);
    assert!(first.is_some());

    // 时钟再推进 1000s，第二次 sweep：不得重发（InFlight 已摘除）
    advance_clock(&mut world, 1000);
    world
        .run_system_once(harness::systems::sweep_inflight_tool_calls)
        .unwrap();
    assert!(wait_for_tool_result(&mut world, 20).is_none());
}
