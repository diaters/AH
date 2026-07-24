mod common;
use bevy_ecs::system::RunSystemOnce;
use common::async_tool_bridge::*;
use harness::domain::{
    AgentExecutionRequest, AgentRequestKind, InFlightToolCall, ToolAsyncResult, ToolEffect,
    ToolEffectPending, ToolError, ToolExecutionResultMessage, ToolRequestPending,
};

fn spawn_pending(world: &mut bevy_ecs::prelude::World, call_id: &str) -> bevy_ecs::prelude::Entity {
    let req = AgentExecutionRequest {
        task_id: uuid::Uuid::new_v4(),
        agent_id: uuid::Uuid::new_v4(),
        request_kind: AgentRequestKind::ToolExecution {
            tool_name: "t".into(),
        },
        prompt: "the prompt".into(),
        system_prompt: Some("sys".into()),
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
                started_at: now(world),
                timeout: chrono::Duration::seconds(300),
            },
        ))
        .id()
}

#[test]
fn ingest_lands_completed_result_and_despawns_pending() {
    let mut world = setup_bridge_world();
    let e = spawn_pending(&mut world, "call-i1");

    let sender = world.resource::<harness::domain::ToolResultSender>();
    sender
        .0
        .send(ToolAsyncResult::completed(
            "call-i1",
            Ok(serde_json::json!({"tasks": []})),
        ))
        .unwrap();

    world
        .run_system_once(harness::systems::ingest_tool_results_system)
        .unwrap();

    let mut q = world.query::<&ToolExecutionResultMessage>();
    let results: Vec<_> = q.iter(&world).collect();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].tool_call_id.as_deref(), Some("call-i1"));
    assert!(results[0].tool_output.is_ok());
    // 重建字段来自 original_request（9 字段逐项核）
    assert_eq!(results[0].result.prompt, "the prompt");
    assert_eq!(results[0].result.system_prompt.as_deref(), Some("sys"));
    assert!(world.get::<ToolRequestPending>(e).is_none());
}

#[test]
fn ingest_lands_error_result_with_tool_error_type() {
    let mut world = setup_bridge_world();
    spawn_pending(&mut world, "call-i2");

    let sender = world.resource::<harness::domain::ToolResultSender>();
    sender
        .0
        .send(ToolAsyncResult::completed(
            "call-i2",
            Err(ToolError::Timeout("timed out after 300s".into())),
        ))
        .unwrap();

    world
        .run_system_once(harness::systems::ingest_tool_results_system)
        .unwrap();

    let mut q = world.query::<&ToolExecutionResultMessage>();
    let results: Vec<_> = q.iter(&world).collect();
    assert_eq!(results.len(), 1);
    match &results[0].tool_output {
        Err(ToolError::Timeout(msg)) => assert!(msg.contains("300")),
        other => panic!("expected ToolError::Timeout, got {:?}", other),
    }
    assert!(results[0].result.result.is_err());
}

#[test]
fn ingest_drops_late_duplicate_when_pending_gone() {
    let mut world = setup_bridge_world();
    // 不 spawn pending（模拟 sweeper claim 后 ingest 已落地、despawn，worker 晚到的结果）
    let sender = world.resource::<harness::domain::ToolResultSender>();
    sender
        .0
        .send(ToolAsyncResult::completed(
            "ghost",
            Ok(serde_json::json!({})),
        ))
        .unwrap();

    world
        .run_system_once(harness::systems::ingest_tool_results_system)
        .unwrap();

    let mut q = world.query::<&ToolExecutionResultMessage>();
    assert_eq!(q.iter(&world).count(), 0);
}

#[test]
fn ingest_routes_effect_to_pending_entity_without_landing_result() {
    let mut world = setup_bridge_world();
    let e = spawn_pending(&mut world, "call-i3");

    let sender = world.resource::<harness::domain::ToolResultSender>();
    sender
        .0
        .send(ToolAsyncResult::effect(
            "call-i3",
            ToolEffect::DeleteScheduledTask { kind: "k".into() },
        ))
        .unwrap();

    world
        .run_system_once(harness::systems::ingest_tool_results_system)
        .unwrap();

    // 不落地结果、不 despawn 挂起实体
    let mut q = world.query::<&ToolExecutionResultMessage>();
    assert_eq!(q.iter(&world).count(), 0);
    assert!(world.get::<ToolRequestPending>(e).is_some());
    // spawn 了 ToolEffectPending
    let mut qe = world.query::<&ToolEffectPending>();
    let effects: Vec<_> = qe.iter(&world).collect();
    assert_eq!(effects.len(), 1);
    assert_eq!(effects[0].tool_call_id, "call-i3");
}
