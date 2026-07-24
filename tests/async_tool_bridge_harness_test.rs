//! 异步工具桥共享测试 harness 自验证。
//!
//! 这一组测试不验证业务逻辑，只验证 harness 本身可用：
//! - `setup_bridge_world` 装好 AsyncRuntime / Clock / 通道资源
//! - `now` / `advance_clock` 提供唯一时间源与假时钟推进
//! - `wait_for_tool_result` 能从通道轮询到一条结果
//!
//! 后续所有 async-tool-bridge Task 复用本 harness 时，这组测试
//! 充当“地基塌了就先红”的回归保护。

mod common;
use common::async_tool_bridge::*;

#[test]
fn harness_world_has_all_resources() {
    let world = setup_bridge_world();
    assert!(world.contains_resource::<harness::app::AsyncRuntime>());
    assert!(world.contains_resource::<harness::app::Clock>());
    assert!(world.contains_resource::<harness::domain::ToolResultSender>());
    assert!(world.contains_resource::<harness::domain::ToolResultReceiver>());
}

#[test]
fn advance_clock_moves_world_now() {
    let mut world = setup_bridge_world();
    let before = now(&world);
    advance_clock(&mut world, 100);
    let after = now(&world);
    assert_eq!(after.signed_duration_since(before).num_seconds(), 100);
}

#[test]
fn channel_roundtrip() {
    let mut world = setup_bridge_world();
    let sender = world.resource::<harness::domain::ToolResultSender>();
    sender
        .0
        .send(harness::domain::ToolAsyncResult::completed(
            "call-1",
            Ok(serde_json::json!({"ok": true})),
        ))
        .unwrap();
    let got = wait_for_tool_result(&mut world, 100).expect("result within 100ms");
    assert_eq!(got.tool_call_id, "call-1");
}
