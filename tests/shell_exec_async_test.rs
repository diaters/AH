//! shell_exec 上桥后的「不堵帧」回归测试。
//!
//! Task 11 验收钉死之一：长命令执行期间帧不被拉长。
//!
//! 设计要点：
//! - spawn `sleep 3` 的 shell_exec 请求 → 跑 `async_tool_dispatch_system` 认领 +
//!   spawn worker → 连续跑 100 次空帧（模拟主循环空转）→ 断言：
//!   1. 请求被 async 路径认领（`ToolExecutionRequestMessage` 已被移除 + 挂起实体挂上）
//!   2. 100 帧总墙钟消耗 < 500ms（worker 在另一线程上 sleep 3s，主线程帧不被拉长）
//! - 一律 `#[test]`，禁止 `#[tokio::test]`（避免 runtime 嵌套 panic）
//! - 墙钟用 `std::time::Instant::now()` 计时（Clock 是假时钟，不能用于墙钟断言）
//! - 子进程 `sleep 3` 在 macOS / Linux 兼容

mod common;
use bevy_ecs::prelude::*;
use bevy_ecs::system::RunSystemOnce;
use common::async_tool_bridge::*;
use harness::domain::{
    AgentExecutionRequest, AgentRequestKind, BuiltinTool, BuiltinToolExecutors, ChannelId,
    FailureReason, FrontendKind, InFlightToolCall, ShortTermMemory, Task, TaskStatus,
    ToolActionKind, ToolCallingState, ToolExecutionRequestMessage, ToolRequestPending,
};
use harness::systems::tools::builtin::ShellExecTool;
use std::time::{Duration, Instant};
use uuid::Uuid;

// ============ 共享 fixture ============

fn test_channel() -> ChannelId {
    ChannelId {
        frontend: FrontendKind::Tui,
        user_id: "shell-exec-async".to_string(),
        thread_id: None,
    }
}

fn make_request(
    tool_input: serde_json::Value,
    tool_call_id: &str,
    task_id: Uuid,
    agent_id: Uuid,
) -> ToolExecutionRequestMessage {
    ToolExecutionRequestMessage {
        request: AgentExecutionRequest {
            task_id,
            agent_id,
            request_kind: AgentRequestKind::ToolExecution {
                tool_name: "shell_exec".into(),
            },
            prompt: String::new(),
            system_prompt: None,
            tools: vec![],
            conversation: None,
            work_item_id: None,
            model_override: None,
        },
        tool_name: "shell_exec".into(),
        tool_input,
        pending_confirmation_id: None,
        tool_call_id: Some(tool_call_id.into()),
        pending_confirmation_options: None,
        work_item_entity: None,
        confirmed_once: false,
    }
}

fn spawn_waiting_task(world: &mut World, content: &str) -> (Entity, Uuid) {
    let mut task = Task::from_user_input(content, 3, test_channel());
    let task_id = task.id;
    task.status = TaskStatus::Waiting(harness::domain::WaitingReason::ToolExecution);
    let entity = world.spawn((task, ShortTermMemory::default())).id();
    (entity, task_id)
}

fn spawn_calling_state(world: &mut World, task_id: Uuid, agent_id: Uuid, pending: &[&str]) {
    world.spawn(ToolCallingState {
        task_id,
        agent_id,
        pending_tool_call_ids: pending.iter().map(|s| s.to_string()).collect(),
        iteration: 1,
        max_iterations: 10,
        conversation: vec![],
        tools: vec![],
        request_kind: AgentRequestKind::ToolExecution {
            tool_name: "shell_exec".into(),
        },
        work_item_id: None,
    });
}

/// 装好 ShellExecTool + 异步桥基础设施的 world。
fn world_with_shell_exec() -> World {
    let mut world = setup_bridge_world();

    let mut executors = BuiltinToolExecutors::default();
    executors.register(Box::new(ShellExecTool));
    world.insert_resource(executors);
    world.insert_resource(harness::app::HarnessSettings::default_test());
    world.insert_resource(harness::NativeProcessBackend::default());

    world
}

/// 空帧 system：用来模拟主循环空转，验证 worker 不阻塞主线程。
fn noop_system() {}

// ============ Step 1: shell_exec 必须是 Async kind ============

#[test]
fn shell_exec_is_async_kind() {
    let tool = ShellExecTool;
    assert_eq!(
        tool.kind(),
        ToolActionKind::Async,
        "shell_exec must be migrated to async bridge (kind == Async) to avoid blocking the main loop"
    );
}

// ============ Step 2: 长命令期间帧不被拉长 ============

#[test]
fn long_command_does_not_block_frame() {
    let mut world = world_with_shell_exec();

    // 1. Waiting(ToolExecution) Task + ToolCallingState(pending: ["frame-1"])
    let (_task_entity, task_id) = spawn_waiting_task(&mut world, "run sleep 3");
    let agent_id = Uuid::new_v4();
    spawn_calling_state(&mut world, task_id, agent_id, &["frame-1"]);

    // 2. spawn ToolExecutionRequestMessage（shell_exec, "frame-1", command=sleep 3）
    let pending_entity = world
        .spawn(make_request(
            serde_json::json!({ "command": "sleep 3", "timeout_secs": 10 }),
            "frame-1",
            task_id,
            agent_id,
        ))
        .id();

    // 3. dispatch：原地改造为挂起实体 + worker 起跑（worker 在另一线程上 sleep 3s）
    let dispatch_start = Instant::now();
    world
        .run_system_once(harness::systems::async_tool_dispatch_system)
        .unwrap();
    let dispatch_elapsed = dispatch_start.elapsed();

    // 断言 1：dispatch 墙钟消耗远小于 3s（worker 不阻塞主线程）
    assert!(
        dispatch_elapsed < Duration::from_millis(500),
        "async_tool_dispatch_system should not block; dispatch took {:?}",
        dispatch_elapsed
    );

    // 断言 2：请求被 async 路径认领（ToolExecutionRequestMessage 已移除）
    assert!(
        world
            .get::<ToolExecutionRequestMessage>(pending_entity)
            .is_none(),
        "request should be reaped by async dispatch (ToolExecutionRequestMessage removed)"
    );
    // 断言 3：挂起实体挂上
    assert!(world.get::<ToolRequestPending>(pending_entity).is_some());
    assert!(world.get::<InFlightToolCall>(pending_entity).is_some());

    // 4. 连续跑 100 次空帧（模拟主循环空转，验证 worker 不阻塞主线程）
    let frames_start = Instant::now();
    for _ in 0..100 {
        world.run_system_once(noop_system).unwrap();
    }
    let frames_elapsed = frames_start.elapsed();

    // 断言 4：100 帧总墙钟消耗 < 500ms（worker 在另一线程 sleep 3s，主线程帧不被拉长）
    assert!(
        frames_elapsed < Duration::from_millis(500),
        "100 noop frames should complete in <500ms; took {:?}",
        frames_elapsed
    );

    // 5. 清理：显式 cancel worker，避免 spawn_blocking 线程泄漏（sleep 3 会跑满 3s）。
    //    cancel 后 worker 在 ≤10ms 内 kill 子进程退出。
    {
        let mut q = world.query::<&harness::domain::InFlightToolCall>();
        if let Some(inflight) = q.iter(&world).next() {
            inflight.cancel.cancel();
        }
    }
    std::thread::sleep(Duration::from_millis(50));
}

// ============ Step 3: 父任务取消 → worker 收信退出 ============

/// 父任务进入终态后，cancel_monitor 触发 token.cancel()，worker 收信 kill 子进程，
/// 返回 `Err(ExecutionFailed("cancelled"))`。
///
/// 验收钉死：
/// - cancel_monitor claim 后 InFlightToolCall 摘除，挂起实体保留（等 ingest 落地）
/// - worker 在 cancel 后快速返回（远快于 `sleep 5` 的 5s 自然完成）
/// - 结果内容含 "cancelled"
#[test]
fn parent_task_cancel_kills_worker_and_returns_cancelled_error() {
    let mut world = world_with_shell_exec();

    // 1. spawn task + request for `sleep 5`（足够长，确保 cancel 先于自然完成）
    let (task_entity, task_id) = spawn_waiting_task(&mut world, "run sleep 5");
    let agent_id = Uuid::new_v4();
    spawn_calling_state(&mut world, task_id, agent_id, &["cancel-1"]);

    let pending_entity = world
        .spawn(make_request(
            serde_json::json!({ "command": "sleep 5", "timeout_secs": 30 }),
            "cancel-1",
            task_id,
            agent_id,
        ))
        .id();

    // 2. dispatch：挂起 + worker 起跑（worker 在另一线程上 sleep 5s）
    world
        .run_system_once(harness::systems::async_tool_dispatch_system)
        .unwrap();

    // 确认 worker 已起跑（InFlightToolCall 挂上）
    assert!(world.get::<InFlightToolCall>(pending_entity).is_some());

    // 3. 父任务进入终态（UserCancelled）——模拟用户取消
    {
        let mut task = world.get_mut::<Task>(task_entity).unwrap();
        task.status = TaskStatus::Failed(FailureReason::UserCancelled);
    }

    // 4. cancel_monitor：扫到终态父任务 → token.cancel() + claim
    let cancel_start = Instant::now();
    world
        .run_system_once(harness::systems::cancel_monitor_system)
        .unwrap();

    // 断言 1：InFlightToolCall 已被 claim（摘除），挂起实体保留（等 ingest 落地）
    assert!(
        world.get::<InFlightToolCall>(pending_entity).is_none(),
        "cancel_monitor should claim InFlightToolCall"
    );
    assert!(
        world.get::<ToolRequestPending>(pending_entity).is_some(),
        "pending entity should remain for ingest"
    );

    // 5. 等 worker 回 cancelled 结果（应远快于 5s——cancel 触发 kill 子进程）
    let result = wait_for_tool_result(&mut world, 3000)
        .expect("worker should return cancelled result after cancel");

    let cancel_elapsed = cancel_start.elapsed();
    assert!(
        cancel_elapsed < Duration::from_secs(3),
        "cancel should kill subprocess quickly; took {:?}",
        cancel_elapsed
    );

    // 断言 2：结果内容是 cancelled error
    assert_eq!(result.tool_call_id, "cancel-1");
    match result.payload {
        harness::domain::ToolWorkerPayload::Completed(Err(
            harness::domain::ToolError::ExecutionFailed(msg),
        )) => {
            assert!(
                msg.contains("cancelled"),
                "error should mention 'cancelled', got: {}",
                msg
            );
        }
        other => panic!("expected cancelled error, got {:?}", other),
    }
}
