//! 异步工具桥 pilot e2e 验收。
//!
//! 本文件是 pilot 的退出判据：前 7 个 Task 交付的零件（dispatch / ingest /
//! sweeper / list_scheduled_tasks / 通道与挂起实体类型）装在一起能否跑通
//! 完整链路。覆盖：
//! - **Step 1 happy path**：dispatch → worker → ingest → tool_result_system 全链路
//! - **Step 2 失联路径一**：worker panic → catch_unwind 合成 ExecutionFailed
//! - **Step 3 失联路径二**：sweeper 超时兜底（`std::future::pending`）
//! - **Step 4 失联路径三**：通道断开（移除 `ToolResultReceiver`）→ sweeper 逐个 claim
//! - **Step 5 exactly-once race**：sweeper 先 claim，worker 晚到成功结果被 ingest drop
//! - **Step 6 barrier 部分结果**：未收齐不收口（跑 `tool_calling_orchestrator_system`）
//! - **Step 7 背压实验**：1000 条 buffered 结果的 RSS 占用（`#[ignore]`，手动跑）
//!
//! 设计要点：
//! - 一律 `#[test]`，禁止 `#[tokio::test]`（避免 runtime 嵌套 panic）
//! - 时间源唯一：测试体内一切「现在」都来自 `now(&world)`，禁止 `Utc::now()`
//!   出现在测试体（fixture 数据如 `created_at` 例外）
//! - 不重写 barrier 逻辑：复用 `tool_result_system` / `tool_calling_orchestrator_system`
//! - 失联三路径恰好一条 error 结果，exactly-once 由「挂起实体是否还在」唯一裁决

mod common;
use bevy_ecs::prelude::*;
use bevy_ecs::system::RunSystemOnce;
use common::async_tool_bridge::*;
use harness::domain::{
    AgentExecutionRequest, AgentExecutionRequestMessage, AgentRequestKind, BuiltinTool,
    BuiltinToolExecutors, ChannelId, FrontendKind, InFlightToolCall, OwnedToolContext,
    ShortTermMemory, Task, TaskStatus, ToolAction, ToolActionKind, ToolCallingState, ToolContext,
    ToolError, ToolExecutionRequestMessage, ToolExecutionResultMessage, ToolFuture,
    ToolRequestPending, ToolWorkerOutput,
};
use harness::systems::tools::builtin::scheduled::ListScheduledTasksTool;
use harness::triggers::scheduled_task::{
    DynamicScheduledTask, ScheduleSpec, ScheduledTaskInfo, ScheduledTaskRegistry, SchedulerState,
};
use uuid::Uuid;

// ============ 共享 fixture ============

fn test_channel() -> ChannelId {
    ChannelId {
        frontend: FrontendKind::Tui,
        user_id: "e2e".to_string(),
        thread_id: None,
    }
}

/// 构造一条 `ToolExecutionRequestMessage`（关联给定 task / agent）。
fn make_request(
    tool_name: &str,
    tool_call_id: &str,
    task_id: Uuid,
    agent_id: Uuid,
) -> ToolExecutionRequestMessage {
    ToolExecutionRequestMessage {
        request: AgentExecutionRequest {
            task_id,
            agent_id,
            request_kind: AgentRequestKind::ToolExecution {
                tool_name: tool_name.into(),
            },
            prompt: String::new(),
            system_prompt: None,
            tools: vec![],
            conversation: None,
            work_item_id: None,
            model_override: None,
        },
        tool_name: tool_name.into(),
        tool_input: serde_json::json!({}),
        pending_confirmation_id: None,
        tool_call_id: Some(tool_call_id.into()),
        pending_confirmation_options: None,
        work_item_entity: None,
        confirmed_once: false,
    }
}

/// spawn 一个 Waiting(ToolExecution) 的 Task + ShortTermMemory，返回 (task_entity, task_id)。
fn spawn_waiting_task(world: &mut World, content: &str) -> (Entity, Uuid) {
    let mut task = Task::from_user_input(content, 3, test_channel());
    let task_id = task.id;
    task.status = TaskStatus::Waiting(harness::domain::WaitingReason::ToolExecution);
    let entity = world.spawn((task, ShortTermMemory::default())).id();
    (entity, task_id)
}

/// spawn 一个 ToolCallingState（pending_tool_call_ids = given list）。
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
            tool_name: "list_scheduled_tasks".into(),
        },
        work_item_id: None,
    });
}

/// 装 list_scheduled_tasks 工具 + 双账本各一条任务，作为 happy path 数据源。
fn world_with_list_tool() -> World {
    let mut world = setup_bridge_world();

    let mut executors = BuiltinToolExecutors::default();
    executors.register(Box::new(ListScheduledTasksTool));
    world.insert_resource(executors);
    world.insert_resource(harness::app::HarnessSettings::default_test());

    // 双账本各一条任务（造 list 的数据源）—— dynamic_tasks 与 registry 一致
    let mut state = SchedulerState::default();
    state.dynamic_tasks_mut().push(DynamicScheduledTask {
        id: Uuid::new_v4(),
        kind: "daily".into(),
        schedule: ScheduleSpec::Once(now(&world)),
        created_at: now(&world),
    });
    world.insert_resource(state);

    let mut registry = ScheduledTaskRegistry::default();
    registry.insert(
        "daily",
        ScheduledTaskInfo {
            content: "每日报告".into(),
            output_channel: None,
            is_once: false,
        },
    );
    world.insert_resource(registry);

    world
}

/// 装一个自定义 Async 工具（无快照需求）。
fn world_with_async_tool(tool: Box<dyn BuiltinTool>) -> World {
    let mut world = setup_bridge_world();
    let mut executors = BuiltinToolExecutors::default();
    executors.register(tool);
    world.insert_resource(executors);
    world.insert_resource(harness::app::HarnessSettings::default_test());
    world
}

/// 轮询 ingest 直到指定 `tool_call_id` 的 `ToolExecutionResultMessage` 落地，或超时返回 None。
///
/// 必须按 call_id 等待而非「任一结果」：barrier 测试中世界里可能已存在前序 call_id 的
/// 结果实体（被 `ToolCallingState` 保留），「任一」语义会立刻返回 stale 实体导致 flaky。
fn poll_ingest_for_call_id(world: &mut World, call_id: &str, timeout_ms: u64) -> Option<Entity> {
    let start = std::time::Instant::now();
    loop {
        world
            .run_system_once(harness::systems::ingest_tool_results_system)
            .unwrap();
        let mut q = world.query::<(Entity, &ToolExecutionResultMessage)>();
        for (e, msg) in q.iter(world) {
            if msg.tool_call_id.as_deref() == Some(call_id) {
                return Some(e);
            }
        }
        if start.elapsed().as_millis() >= timeout_ms as u128 {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

// ============ Step 1: happy path ============

#[test]
fn e2e_full_chain_dispatch_to_restore() {
    let mut world = world_with_list_tool();

    // 1. Waiting(ToolExecution) Task + ToolCallingState(pending: ["e2e-call-1"])
    let (_task_entity, task_id) = spawn_waiting_task(&mut world, "list my tasks");
    let agent_id = Uuid::new_v4();
    spawn_calling_state(&mut world, task_id, agent_id, &["e2e-call-1"]);

    // 2. 预置双账本各一条任务已在 world_with_list_tool 完成
    // 3. spawn ToolExecutionRequestMessage（list_scheduled_tasks, "e2e-call-1"）
    let pending_entity = world
        .spawn(make_request(
            "list_scheduled_tasks",
            "e2e-call-1",
            task_id,
            agent_id,
        ))
        .id();

    // 4. dispatch：原地改造为挂起实体（摘请求消息 + 挂 Pending + InFlight）+ worker 起跑
    world
        .run_system_once(harness::systems::async_tool_dispatch_system)
        .unwrap();
    assert!(
        world
            .get::<ToolExecutionRequestMessage>(pending_entity)
            .is_none()
    );
    assert!(world.get::<ToolRequestPending>(pending_entity).is_some());
    assert!(world.get::<InFlightToolCall>(pending_entity).is_some());

    // 5. 轮询 ingest 直到 e2e-call-1 结果实体出现
    let result_entity = poll_ingest_for_call_id(&mut world, "e2e-call-1", 2000)
        .expect("result entity landed within 2000ms");

    // 6. 断言 ToolExecutionResultMessage 落地、tool_call_id 匹配、tool_output 含 tasks 数组
    let msg = world
        .get::<ToolExecutionResultMessage>(result_entity)
        .expect("result message");
    assert_eq!(msg.tool_call_id.as_deref(), Some("e2e-call-1"));
    let output = msg.tool_output.as_ref().expect("ok output");
    let tasks = output["tasks"]
        .as_array()
        .expect("tasks array in tool_output");
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0]["kind"], "daily");
    assert_eq!(tasks[0]["content"], "每日报告");

    // 挂起实体已 despawn（实体足迹闭环——dispatch 改造 + ingest despawn）
    assert!(world.get::<ToolRequestPending>(pending_entity).is_none());

    // 7. 复用既有 tool_result_system（barrier 真实实现的副作用：processed=true + STM 记录 +
    //    结果 entity 因 ToolCallingState.pending 含 e2e-call-1 而保留）
    world
        .run_system_once(harness::systems::tools::tool_result_system)
        .unwrap();

    let msg = world
        .get::<ToolExecutionResultMessage>(result_entity)
        .expect("result entity retained by ToolCallingState");
    assert!(msg.processed, "tool_result_system should mark processed");

    // 8. 复用 tool_calling_orchestrator_system 验证 barrier 收齐后行为：
    //    pending_tool_call_ids 清空 + 结果 entity despawn + follow-up 请求 spawn
    world
        .run_system_once(harness::systems::transform::tool_calling_orchestrator_system)
        .unwrap();

    // barrier 收齐 → pending 清空
    let mut q_state = world.query::<&ToolCallingState>();
    let state = q_state.iter(&world).next().expect("calling state");
    assert!(
        state.pending_tool_call_ids.is_empty(),
        "barrier should clear pending_tool_call_ids after collecting all results"
    );

    // barrier 收齐 → 已消费的结果 entity despawn
    assert!(
        world
            .get::<ToolExecutionResultMessage>(result_entity)
            .is_none(),
        "result entity should be despawned after barrier collection"
    );

    // barrier 收齐 → spawn follow-up AgentExecutionRequestMessage（异步路径的「恢复」语义）
    assert_eq!(
        count_entities::<AgentExecutionRequestMessage>(&mut world),
        1,
        "barrier should spawn exactly one follow-up request"
    );
}

// ============ Step 2: worker panic ============

struct PanicAsyncTool;
impl BuiltinTool for PanicAsyncTool {
    fn name(&self) -> &str {
        "panic_async"
    }
    fn kind(&self) -> ToolActionKind {
        ToolActionKind::Async
    }
    fn execute(&self, _: &serde_json::Value, _: &ToolContext) -> Result<ToolAction, ToolError> {
        unreachable!("async tool must not run on sync path")
    }
    fn run_async(&self, _: serde_json::Value, _: OwnedToolContext) -> ToolFuture {
        Box::pin(async { panic!("worker panic for e2e") })
    }
}

#[test]
fn e2e_worker_panic_yields_exactly_one_error_result() {
    let mut world = world_with_async_tool(Box::new(PanicAsyncTool));
    let (_task_entity, task_id) = spawn_waiting_task(&mut world, "trigger panic");
    let agent_id = Uuid::new_v4();
    spawn_calling_state(&mut world, task_id, agent_id, &["panic-1"]);

    let pending_entity = world
        .spawn(make_request("panic_async", "panic-1", task_id, agent_id))
        .id();

    world
        .run_system_once(harness::systems::async_tool_dispatch_system)
        .unwrap();

    // catch_unwind 兜底合成 ExecutionFailed error → ingest 落地
    let result_entity = poll_ingest_for_call_id(&mut world, "panic-1", 2000)
        .expect("panic error landed within 2000ms");

    let msg = world
        .get::<ToolExecutionResultMessage>(result_entity)
        .expect("result message");
    assert_eq!(msg.tool_call_id.as_deref(), Some("panic-1"));
    match &msg.tool_output {
        Err(ToolError::ExecutionFailed(reason)) => {
            assert!(
                reason.contains("panic"),
                "reason should mention panic: {reason}"
            )
        }
        other => panic!("expected Err(ExecutionFailed), got {:?}", other),
    }

    // 挂起实体 despawn
    assert!(world.get::<ToolRequestPending>(pending_entity).is_none());

    // 无第二条结果（exactly-once）
    let mut q = world.query::<&ToolExecutionResultMessage>();
    assert_eq!(q.iter(&world).count(), 1);
}

// ============ Step 3: sweeper 超时兜底 ============

struct PendingForeverTool;
impl BuiltinTool for PendingForeverTool {
    fn name(&self) -> &str {
        "pending_forever"
    }
    fn kind(&self) -> ToolActionKind {
        ToolActionKind::Async
    }
    fn execute(&self, _: &serde_json::Value, _: &ToolContext) -> Result<ToolAction, ToolError> {
        unreachable!("async tool must not run on sync path")
    }
    fn run_async(&self, _: serde_json::Value, _: OwnedToolContext) -> ToolFuture {
        Box::pin(std::future::pending())
    }
}

#[test]
fn e2e_sweeper_timeout_yields_error_and_barrier_continues() {
    let mut world = world_with_async_tool(Box::new(PendingForeverTool));
    let (_task_entity, task_id) = spawn_waiting_task(&mut world, "trigger pending forever");
    let agent_id = Uuid::new_v4();
    spawn_calling_state(&mut world, task_id, agent_id, &["to-1"]);

    let pending_entity = world
        .spawn(make_request("pending_forever", "to-1", task_id, agent_id))
        .id();

    world
        .run_system_once(harness::systems::async_tool_dispatch_system)
        .unwrap();

    // 推进假时钟超过 timeout（default_test = 300s）
    advance_clock(&mut world, 400);

    // sweeper claim：发 error 入通道 + 摘 InFlight
    world
        .run_system_once(harness::systems::sweep_inflight_tool_calls)
        .unwrap();

    // InFlight 已摘除，挂起实体保留（等 ingest 落地后 despawn）
    assert!(world.get::<InFlightToolCall>(pending_entity).is_none());
    assert!(world.get::<ToolRequestPending>(pending_entity).is_some());

    // ingest 落地 error + despawn
    world
        .run_system_once(harness::systems::ingest_tool_results_system)
        .unwrap();

    let mut q = world.query::<&ToolExecutionResultMessage>();
    let results: Vec<_> = q.iter(&world).collect();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].tool_call_id.as_deref(), Some("to-1"));
    match &results[0].tool_output {
        Err(ToolError::Timeout(_)) => {}
        other => panic!("expected Timeout error, got {:?}", other),
    }

    // 挂起实体 despawn
    assert!(world.get::<ToolRequestPending>(pending_entity).is_none());

    // 再跑一次 sweeper + ingest：无第二条结果（claim 防重）
    world
        .run_system_once(harness::systems::sweep_inflight_tool_calls)
        .unwrap();
    world
        .run_system_once(harness::systems::ingest_tool_results_system)
        .unwrap();
    let mut q = world.query::<&ToolExecutionResultMessage>();
    assert_eq!(
        q.iter(&world).count(),
        1,
        "no second result after re-sweep (claim prevents duplicate)"
    );
}

// ============ Step 4: 通道断开 ============

#[test]
fn e2e_channel_disconnect_is_swept() {
    // 第一阶段：移除 ToolResultReceiver 后 dispatch，worker send 失败但被吞（let _ =）
    let mut world = world_with_async_tool(Box::new(EchoAsyncTool));
    let (_task_entity, task_id) = spawn_waiting_task(&mut world, "channel disconnect");
    let agent_id = Uuid::new_v4();
    spawn_calling_state(&mut world, task_id, agent_id, &["dc-1"]);

    // 移除 receiver 模拟通道断开（worker send 失败静默）
    world.remove_resource::<harness::domain::ToolResultReceiver>();

    let pending_entity = world
        .spawn(make_request("echo_async", "dc-1", task_id, agent_id))
        .id();

    world
        .run_system_once(harness::systems::async_tool_dispatch_system)
        .unwrap();

    // worker 跑完 send 失败（let _ = 吞掉）——系统不 panic、不挂死
    // 等一小段时间让 worker 完成 send（失败的 send）
    std::thread::sleep(std::time::Duration::from_millis(50));

    // 推进假时钟触发 sweeper
    advance_clock(&mut world, 400);
    world
        .run_system_once(harness::systems::sweep_inflight_tool_calls)
        .unwrap();

    // sweeper claim：InFlight 摘除（sweeper 的 send 也失败但 claim 已完成）
    assert!(
        world.get::<InFlightToolCall>(pending_entity).is_none(),
        "sweeper should claim (remove InFlight) even if channel send fails"
    );
    // 挂起实体保留（等 ingest 落地后 despawn）
    assert!(world.get::<ToolRequestPending>(pending_entity).is_some());

    // 第二阶段：恢复通道（新建一对 channel），sweeper 之前发的 error 因通道断未送达，
    // 但 worker 也已 send 失败——本测试的真实含义是「通道断 = 全体失联，sweeper 逐个 claim」，
    // 系统不挂死、不 panic 即可。挂起实体保留等待（不会自行恢复）。
    // 用一个新的 world 验证「恢复通道后 sweeper error 落地」语义：
    let mut world2 = world_with_async_tool(Box::new(PendingForeverTool));
    let (_task_entity2, task_id2) = spawn_waiting_task(&mut world2, "channel recover");
    let agent_id2 = Uuid::new_v4();
    spawn_calling_state(&mut world2, task_id2, agent_id2, &["rc-1"]);
    let pending_entity2 = world2
        .spawn(make_request("pending_forever", "rc-1", task_id2, agent_id2))
        .id();
    world2
        .run_system_once(harness::systems::async_tool_dispatch_system)
        .unwrap();
    advance_clock(&mut world2, 400);
    world2
        .run_system_once(harness::systems::sweep_inflight_tool_calls)
        .unwrap();
    // 通道在：sweeper error 经 ingest 落地 + despawn
    world2
        .run_system_once(harness::systems::ingest_tool_results_system)
        .unwrap();
    assert!(world2.get::<ToolRequestPending>(pending_entity2).is_none());
    let mut q = world2.query::<&ToolExecutionResultMessage>();
    let results: Vec<_> = q.iter(&world2).collect();
    assert_eq!(results.len(), 1);
    match &results[0].tool_output {
        Err(ToolError::Timeout(_)) => {}
        other => panic!(
            "expected Timeout error after channel recovery, got {:?}",
            other
        ),
    }
}

// ============ Step 5: exactly-once race ============

struct SlowSuccessTool;
impl BuiltinTool for SlowSuccessTool {
    fn name(&self) -> &str {
        "slow_success"
    }
    fn kind(&self) -> ToolActionKind {
        ToolActionKind::Async
    }
    fn execute(&self, _: &serde_json::Value, _: &ToolContext) -> Result<ToolAction, ToolError> {
        unreachable!("async tool must not run on sync path")
    }
    fn run_async(&self, _: serde_json::Value, _: OwnedToolContext) -> ToolFuture {
        Box::pin(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            Ok(ToolWorkerOutput::Value(serde_json::json!({"slow": "ok"})))
        })
    }
}

#[test]
fn e2e_sweeper_error_first_worker_late_success_dropped() {
    let mut world = world_with_async_tool(Box::new(SlowSuccessTool));
    let (_task_entity, task_id) = spawn_waiting_task(&mut world, "race test");
    let agent_id = Uuid::new_v4();
    spawn_calling_state(&mut world, task_id, agent_id, &["race-1"]);

    let pending_entity = world
        .spawn(make_request("slow_success", "race-1", task_id, agent_id))
        .id();

    // 1. dispatch 起 worker（worker 50ms 后会成功）
    world
        .run_system_once(harness::systems::async_tool_dispatch_system)
        .unwrap();

    // 2. 立刻 advance_clock(400) + sweeper → error 先入通道（claim）
    advance_clock(&mut world, 400);
    world
        .run_system_once(harness::systems::sweep_inflight_tool_calls)
        .unwrap();

    // 3. ingest → error 落地 + despawn 挂起实体
    world
        .run_system_once(harness::systems::ingest_tool_results_system)
        .unwrap();

    let mut q = world.query::<&ToolExecutionResultMessage>();
    let results: Vec<_> = q.iter(&world).collect();
    assert_eq!(results.len(), 1);
    match &results[0].tool_output {
        Err(ToolError::Timeout(_)) => {}
        other => panic!("expected Timeout error from sweeper, got {:?}", other),
    }
    assert!(world.get::<ToolRequestPending>(pending_entity).is_none());

    // 4. 用 wait_for_tool_result 轮询通道确认 late Ok 真的到达——证明 race 被实际触发。
    //    若仅靠 thread::sleep(200ms) 等过后跑 ingest，CI 慢机器上 worker 50ms sleep
    //    可能未完成，通道为空、count==1 仍成立 → vacuous-pass（drop 路径从未执行）。
    let late_ok = wait_for_tool_result(&mut world, 1000)
        .expect("worker late Ok must arrive to prove race triggered");
    assert_eq!(late_ok.tool_call_id, "race-1");
    match &late_ok.payload {
        harness::domain::ToolWorkerPayload::Completed(Ok(v)) => {
            assert_eq!(
                v["slow"], "ok",
                "late Ok payload should match SlowSuccessTool output"
            );
        }
        other => panic!("expected Completed(Ok) from worker, got {:?}", other),
    }

    // 5. 把 late Ok 塞回通道，再跑 ingest：挂起实体已 despawn，late Ok 走 drop 路径
    world
        .resource::<harness::domain::ToolResultSender>()
        .0
        .send(late_ok)
        .unwrap();
    world
        .run_system_once(harness::systems::ingest_tool_results_system)
        .unwrap();

    // 6. 世界里恰好一条结果消息（error 那条）；第二条 late Ok 被 drop
    let mut q = world.query::<&ToolExecutionResultMessage>();
    assert_eq!(
        q.iter(&world).count(),
        1,
        "exactly-once: late worker success should be dropped"
    );
    let mut q = world.query::<&ToolExecutionResultMessage>();
    let still = q.iter(&world).next().unwrap();
    match &still.tool_output {
        Err(ToolError::Timeout(_)) => {}
        other => panic!(
            "the single result should still be the Timeout error, got {:?}",
            other
        ),
    }
}

// ============ Step 6: barrier 部分结果 ============

#[test]
fn e2e_barrier_waits_for_all_results_before_restore() {
    let mut world = world_with_async_tool(Box::new(EchoAsyncTool));
    let (_task_entity, task_id) = spawn_waiting_task(&mut world, "two tool calls batch");
    let agent_id = Uuid::new_v4();
    // 一批两个工具调用：pending = [c1, c2]
    spawn_calling_state(&mut world, task_id, agent_id, &["c1", "c2"]);

    // 用「分两次 dispatch」自然模拟 c1 先回、c2 后回的时序：
    // 先只 spawn c1 的请求；dispatch + ingest 落地 c1 后，再 spawn c2 的请求。
    let _p1 = world
        .spawn(make_request("echo_async", "c1", task_id, agent_id))
        .id();
    world
        .run_system_once(harness::systems::async_tool_dispatch_system)
        .unwrap();
    // 等 c1 的 worker 跑完——按 call_id 等待，避免与后续 c2 落地混淆
    let _c1_entity = poll_ingest_for_call_id(&mut world, "c1", 500).expect("c1 landed");

    // c1 已落地，c2 还在飞行中（甚至还没 dispatch）
    let mut q = world.query::<&ToolExecutionResultMessage>();
    let results: Vec<_> = q.iter(&world).collect();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].tool_call_id.as_deref(), Some("c1"));

    // 跑 orchestrator：barrier 未收齐（pending 仍含 c2）→ 不 spawn follow-up
    world
        .run_system_once(harness::systems::transform::tool_calling_orchestrator_system)
        .unwrap();
    assert_eq!(
        count_entities::<AgentExecutionRequestMessage>(&mut world),
        0,
        "barrier should not spawn follow-up when c2 not landed"
    );
    let mut q_state = world.query::<&ToolCallingState>();
    let state = q_state.iter(&world).next().expect("calling state");
    assert!(state.pending_tool_call_ids.contains(&"c2".to_string()));

    // 现在 spawn c2 的请求；dispatch + ingest 落地 c2
    let _p2 = world
        .spawn(make_request("echo_async", "c2", task_id, agent_id))
        .id();
    world
        .run_system_once(harness::systems::async_tool_dispatch_system)
        .unwrap();
    // 等 c2 落地——按 call_id 等待，避免立即返回 stale 的 c1 结果实体
    let _c2_entity = poll_ingest_for_call_id(&mut world, "c2", 500).expect("c2 landed");

    // c2 也落地：现在世界里有两条结果消息
    assert_eq!(
        count_entities::<ToolExecutionResultMessage>(&mut world),
        2,
        "both c1 and c2 results should be landed"
    );

    // 跑 orchestrator：barrier 收齐 → pending 清空 + despawn 结果 + spawn follow-up
    world
        .run_system_once(harness::systems::transform::tool_calling_orchestrator_system)
        .unwrap();
    let mut q_state = world.query::<&ToolCallingState>();
    let state = q_state.iter(&world).next().expect("calling state");
    assert!(
        state.pending_tool_call_ids.is_empty(),
        "barrier should clear pending after both results landed"
    );
    assert_eq!(
        count_entities::<AgentExecutionRequestMessage>(&mut world),
        1,
        "barrier should spawn follow-up after collecting all results"
    );
    // 结果 entity 被 barrier 消费 despawn
    assert_eq!(
        count_entities::<ToolExecutionResultMessage>(&mut world),
        0,
        "barrier should despawn consumed result entities"
    );
}

/// 数世界中带某 Component 的实体数。
fn count_entities<T: Component>(world: &mut World) -> usize {
    let mut q = world.query_filtered::<Entity, With<T>>();
    q.iter(world).count()
}

// ============ Step 7: 背压实验（手动跑） ============

#[test]
#[ignore = "backpressure experiment, run manually: cargo test -- --ignored"]
fn e2e_backpressure_experiment_1000_buffered_results() {
    let world = world_with_async_tool(Box::new(EchoAsyncTool));

    // 持有 receiver 不跑 ingest；直接向 sender 塞 1000 条
    let sender = world
        .resource::<harness::domain::ToolResultSender>()
        .0
        .clone();
    let n = 1000usize;

    let rss_before = current_rss_mb();

    for i in 0..n {
        let _ = sender.send(harness::domain::ToolAsyncResult::completed(
            format!("bp-{i}"),
            Ok(serde_json::json!({
                "index": i,
                "payload": "x".repeat(64),
            })),
        ));
    }

    let rss_after = current_rss_mb();
    let delta = if rss_after > rss_before {
        rss_after - rss_before
    } else {
        0.0
    };

    // 输出格式供报告摘抄
    println!(
        "BUFFERED={n} RSS_BEFORE={rss_before:.2}MB RSS_AFTER={rss_after:.2}MB DELTA={delta:.2}MB"
    );

    // 不再跑 ingest——保持 receiver 持有，让 world drop 时统一清理
    assert!(n > 0);
}

/// 读取当前进程 RSS（驻留集大小），单位 MB。
///
/// 用 `ps` 命令零依赖获取，跨 macOS / Linux 兼容。失败时返回 0
/// （背压实验的绝对值不重要，差值才有意义）。
fn current_rss_mb() -> f64 {
    let pid = std::process::id();
    let output = match std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
    {
        Ok(o) => o,
        Err(_) => return 0.0,
    };
    let s = String::from_utf8_lossy(&output.stdout);
    match s.trim().parse::<u64>() {
        // ps 返回的 rss 单位是 KB
        Ok(kb) => kb as f64 / 1024.0,
        Err(_) => 0.0,
    }
}

// EchoAsyncTool 已抽到 tests/common/async_tool_bridge.rs，经 glob 导入复用。

// ============ Step 8: delete_scheduled_task 全链路 ============

/// 装一个 delete_scheduled_task 工具 + 双账本各一条 "victim" 任务。
fn world_with_delete_tool() -> World {
    let mut world = setup_bridge_world();

    let mut executors = BuiltinToolExecutors::default();
    executors.register(Box::new(
        harness::systems::tools::builtin::scheduled::DeleteScheduledTaskTool,
    ));
    world.insert_resource(executors);
    world.insert_resource(harness::app::HarnessSettings::default_test());

    // 双账本各一条 "victim" 任务——dynamic_tasks 与 registry 一致
    let mut state = SchedulerState::default();
    state.dynamic_tasks_mut().push(DynamicScheduledTask {
        id: Uuid::new_v4(),
        kind: "victim".into(),
        schedule: ScheduleSpec::Once(now(&world)),
        created_at: now(&world),
    });
    world.insert_resource(state);

    let mut registry = ScheduledTaskRegistry::default();
    registry.insert(
        "victim",
        ScheduledTaskInfo {
            content: "to be deleted".into(),
            output_channel: None,
            is_once: true,
        },
    );
    world.insert_resource(registry);

    world
}

/// 跑 dispatch → ingest（spawn ToolEffectPending，挂起实体保留）→ commit（双账本删除，
/// 回送 existed=true）→ ingest（落地最终结果 + despawn 挂起实体）。
fn run_delete_full_chain(world: &mut World, tool_call_id: &str) -> Entity {
    let (_task_entity, task_id) = spawn_waiting_task(world, "delete victim");
    let agent_id = Uuid::new_v4();
    spawn_calling_state(world, task_id, agent_id, &[tool_call_id]);

    // 构造 delete_scheduled_task 请求，tool_input = {"kind": "victim"}
    let mut req = make_request("delete_scheduled_task", tool_call_id, task_id, agent_id);
    req.tool_input = serde_json::json!({"kind": "victim"});
    let pending_entity = world.spawn(req).id();

    // 1. dispatch：原地改造为挂起实体 + worker 起跑（worker 返回 Effect payload）
    world
        .run_system_once(harness::systems::async_tool_dispatch_system)
        .unwrap();

    // 2. 轮询 ingest 直到 ToolEffectPending 出现（worker 异步，结果未必立即可见）
    let effect_entity = poll_effect_pending_for_call_id(world, tool_call_id, 2000)
        .expect("ToolEffectPending should spawn within 2000ms");
    // 挂起实体必须还在（commit 还没跑）
    assert!(
        world.get::<ToolRequestPending>(pending_entity).is_some(),
        "pending entity must survive until commit lands final result"
    );

    // 3. commit：双账本删除，回送 existed=true 到通道
    world
        .run_system_once(harness::systems::commit_tool_effects_system)
        .unwrap();
    // ToolEffectPending 已 despawn
    assert!(
        world
            .get::<harness::domain::ToolEffectPending>(effect_entity)
            .is_none(),
        "ToolEffectPending should be despawned after commit"
    );

    // 4. ingest：落地最终结果 + despawn 挂起实体
    world
        .run_system_once(harness::systems::ingest_tool_results_system)
        .unwrap();
    assert!(
        world.get::<ToolRequestPending>(pending_entity).is_none(),
        "pending entity should be despawned after final result landed"
    );

    // 返回结果实体供调用者断言
    let mut q = world.query::<(Entity, &ToolExecutionResultMessage)>();
    q.iter(world)
        .find(|(_, m)| m.tool_call_id.as_deref() == Some(tool_call_id))
        .map(|(e, _)| e)
        .expect("result entity landed")
}

/// 轮询 ingest 直到指定 `tool_call_id` 的 `ToolEffectPending` 实体出现，或超时返回 None。
fn poll_effect_pending_for_call_id(
    world: &mut World,
    call_id: &str,
    timeout_ms: u64,
) -> Option<Entity> {
    let start = std::time::Instant::now();
    loop {
        world
            .run_system_once(harness::systems::ingest_tool_results_system)
            .unwrap();
        let mut q = world.query::<(Entity, &harness::domain::ToolEffectPending)>();
        for (e, p) in q.iter(world) {
            if p.tool_call_id == call_id {
                return Some(e);
            }
        }
        if start.elapsed().as_millis() >= timeout_ms as u128 {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

#[test]
fn e2e_delete_full_chain() {
    let mut world = world_with_delete_tool();

    let result_entity = run_delete_full_chain(&mut world, "del-1");

    // 断言最终结果：existed=true，deleted=victim
    let msg = world
        .get::<ToolExecutionResultMessage>(result_entity)
        .expect("result message");
    let output = msg.tool_output.as_ref().expect("ok output");
    assert_eq!(output["deleted"], "victim");
    assert_eq!(output["existed"], true);

    // 双账本都空了
    assert!(
        world
            .resource::<SchedulerState>()
            .dynamic_tasks()
            .is_empty(),
        "SchedulerState should be empty after delete"
    );
    assert!(
        world
            .resource::<ScheduledTaskRegistry>()
            .get("victim")
            .is_none(),
        "Registry should not contain victim after delete"
    );

    // 挂起实体与效果实体都不剩
    let mut qp = world.query::<&ToolRequestPending>();
    assert_eq!(qp.iter(&world).count(), 0);
    let mut qe = world.query::<&harness::domain::ToolEffectPending>();
    assert_eq!(qe.iter(&world).count(), 0);

    // 幂等可观测：再删一次 "victim" → existed=false
    let result_entity2 = run_delete_full_chain(&mut world, "del-2");
    let msg2 = world
        .get::<ToolExecutionResultMessage>(result_entity2)
        .expect("second result message");
    let output2 = msg2.tool_output.as_ref().expect("ok output");
    assert_eq!(output2["deleted"], "victim");
    assert_eq!(
        output2["existed"], false,
        "deleting absent kind should report existed=false (idempotent observable)"
    );
}
