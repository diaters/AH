//! 异步工具桥共享测试 harness。
//!
//! 为后续所有 async-tool-bridge Task 提供统一的测试基础设施：
//! - `setup_bridge_world`：装好 `AsyncRuntime`（真实 multi-thread Runtime）+
//!   `Clock`（假时钟，起点为真实 `Utc::now()`）+ 工具结果通道
//! - `now` / `advance_clock`：测试内唯一时间源 + 假时钟推进
//! - `wait_for_tool_result`：同步测试体轮询通道等一条异步结果
//!
//! 设计要点：
//! - Clock 起点用 `Utc::now()`（与生产时间源同型同语义），不用 epoch；
//!   测试体内一切“现在”都经 `now(&world)` 取，保证 started_at 与 sweeper
//!   判定用同一时间源（D6 全局唯一时间源）。
//! - 一律 `#[test]`，禁止 `#[tokio::test]`，避免 runtime 嵌套 panic；
//!   异步等待通过 `try_recv` 轮询实现。
//! - 跑 system 一律 `world.run_system_once(...)`（既有先例：
//!   `src/systems/experience/approval.rs:349`）。

use std::sync::Arc;

use bevy_ecs::prelude::*;
use chrono::{DateTime, Utc};
use tokio::runtime::Runtime;
use tokio::sync::mpsc;

use harness::app::{AsyncRuntime, Clock};
use harness::domain::{ToolAsyncResult, ToolResultReceiver, ToolResultSender};

/// 建测试 World：真实 multi-thread Runtime + 假时钟（起点为真实 now）+ 工具结果通道。
pub fn setup_bridge_world() -> World {
    let mut world = World::new();

    let rt = Runtime::new().expect("create tokio runtime");
    world.insert_resource(AsyncRuntime(Arc::new(rt)));

    // 关键：起点用 Utc::now()，测试里一切“现在”都经 now(&world) 取，
    // 保证 started_at 与 sweeper 判定用同一时间源。
    world.insert_resource(Clock(Utc::now()));

    let (tx, rx) = mpsc::unbounded_channel::<ToolAsyncResult>();
    world.insert_resource(ToolResultSender(tx));
    world.insert_resource(ToolResultReceiver(rx));

    world
}

/// 测试内唯一的“现在”。
///
/// 部分测试 binary（如 `async_dispatch_test`）不直接读 Clock，
/// 但 sweeper / ingest 后续测试会用到，故保留并 allow 在未使用 target 内的 dead_code。
#[allow(dead_code)]
pub fn now(world: &World) -> DateTime<Utc> {
    world.resource::<Clock>().0
}

/// 推进假时钟（sweeper 超时测试用）。
///
/// 当前 Task 2 类型测试未使用本函数，但后续 sweeper / ingest 测试会用到，
/// 故保留并 allow 在未使用 target 内的 dead_code。
#[allow(dead_code)]
pub fn advance_clock(world: &mut World, secs: i64) {
    let mut clock = world.resource_mut::<Clock>();
    clock.0 += chrono::Duration::seconds(secs);
}

/// 轮询通道等一条结果（同步测试体内等异步 worker）。
///
/// 每 1ms `try_recv` 一次，超过 `timeout_ms` 仍未拿到则返回 `None`。
/// 用 `try_recv` + `sleep` 而非 `block_on`，避免在测试线程里
/// 嵌套进入 tokio runtime。
///
/// 当前 Task 2 类型测试未使用本函数，但后续 dispatch / ingest 测试会用到，
/// 故保留并 allow 在未使用 target 内的 dead_code。
#[allow(dead_code)]
pub fn wait_for_tool_result(world: &mut World, timeout_ms: u64) -> Option<ToolAsyncResult> {
    let start = std::time::Instant::now();
    loop {
        {
            let mut receiver = world.resource_mut::<ToolResultReceiver>();
            if let Ok(result) = receiver.0.try_recv() {
                return Some(result);
            }
        }
        if start.elapsed().as_millis() >= timeout_ms as u128 {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}
