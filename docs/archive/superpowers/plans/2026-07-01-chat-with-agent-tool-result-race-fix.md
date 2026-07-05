> **状态：已归档** — 对应功能已合并到 main，归档于 2026-07-05

# chat_with_agent Tool Result 路径竞态修复 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 修复 chat_with_agent 子任务完成后 `chat_round_completion_system` 提前恢复父任务状态，导致 `tool_calling_orchestrator_system` 无法收集 tool result，最终引发 LLM 400 错误的竞态问题。

**Architecture:** 将父任务状态恢复职责从 `chat_round_completion_system` 移到 `tool_calling_orchestrator_system`，使 chat_with_agent 的 tool result 走与普通工具完全相同的收集路径。同时增加 ECS 系统执行顺序约束，确保 completion system 的输出在 orchestrator 处理前可用。

**Tech Stack:** Rust, Bevy ECS, existing test infrastructure

## Global Constraints

- 遵循 Conventional Commits
- 通过 `cargo clippy --all-targets --all-features -- -D warnings`
- 通过 `cargo fmt --all --check`
- 通过 `cargo test --all-features`
- 修改代码与文档放在同一提交中
- 使用中文撰写项目文档，可夹杂英文术语

---

### Task 1: 修改 `chat_round_completion_system` 不再恢复父任务状态

**Files:**
- Modify: `src/systems/transform/chat_round.rs:47-104`
- Test: `tests/chat_with_agent_flow.rs`

**Interfaces:**
- Consumes: `ChatRoundReadyMessage`（由 `llm_response_system` 生成）
- Produces: `ToolExecutionResultMessage`（由 `tool_calling_orchestrator_system` 消费，`tool_call_id` 匹配 `ToolCallingState.pending_tool_call_ids`）

- [ ] **Step 1: 修改 `chat_round_completion_system` 签名和实现**

将 `src/systems/transform/chat_round.rs` 的 `chat_round_completion_system` 改为只读 Query，移除父任务状态修改：

```rust
/// 消费 ChatRoundReadyMessage，生成 ToolExecutionResultMessage 回填父任务。
/// 父任务状态恢复由 tool_calling_orchestrator_system 统一处理。
pub fn chat_round_completion_system(
    mut commands: Commands,
    tasks: Query<&Task>,
    ready: Query<(Entity, &ChatRoundReadyMessage)>,
) {
    for (entity, msg) in &ready {
        if tasks.iter().any(|t| t.id == msg.parent_task_id) {
            debug!(
                event = "ChatRoundCompleted",
                parent_task_id = %msg.parent_task_id,
                child_task_id = %msg.child_task_id,
                batch_id = %msg.batch_id,
                "chat round completed, spawning tool result for orchestrator"
            );
        } else {
            warn!(
                event = "ChatRoundParentNotFound",
                parent_task_id = %msg.parent_task_id,
                "parent task not found for chat round completion"
            );
        }

        let execution_result = AgentExecutionResult {
            task_id: msg.parent_task_id,
            agent_id: msg.parent_agent_id,
            request_kind: AgentRequestKind::LlmCompletion,
            result: Ok(AgentExecutionOutput {
                content: OutputContent::Text(msg.response.clone()),
                reasoning_content: None,
            }),
            prompt: String::new(),
            system_prompt: None,
            tools: vec![],
            reasoning_content: None,
            work_item_id: None,
        };

        commands.spawn((
            ToolExecutionResultMessage {
                result: execution_result,
                tool_name: "chat_with_agent".to_string(),
                tool_output: Ok(serde_json::json!({
                    "handle": msg.child_task_id.to_string(),
                    "response": msg.response,
                    "agent": msg.child_agent_name
                })),
                tool_call_id: Some(msg.parent_tool_call_id.clone()),
                processed: false,
                original_tool_output: None,
            },
            ToolReturnedHookPending,
        ));
        commands.entity(entity).despawn();
    }
}
```

关键变更：
- `mut tasks: Query<&mut Task>` → `tasks: Query<&Task>`（只读）
- 移除 `clock: Res<Clock>` 参数
- 移除 `parent.status = TaskStatus::Ready` 和 `parent.updated_at = clock.0`
- 更新 doc comment 和 debug 日志文案

- [ ] **Step 2: 清理不再需要的 import**

移除 `src/systems/transform/chat_round.rs` 中不再使用的 import：
- `Clock`（`chat_round_completion_system` 不再使用 `clock`）
- `TaskStatus`（不再修改状态）
- `WaitingReason`（不再构造 `WaitingReason`）

保留 `chat_round_block_system` 仍需的 `Clock`、`TaskStatus`、`WaitingReason`。最终 import 列表：

```rust
use crate::{
    app::Clock,
    domain::{
        AgentExecutionOutput, AgentExecutionResult, AgentRequestKind, ChatRoundReadyMessage,
        ChatRoundStartedMessage, ChatSession, OutputContent, Task, TaskStatus,
        TaskTerminatedMessage, ToolExecutionResultMessage, ToolReturnedHookPending, WaitingReason,
    },
};
```

实际上 `Clock`、`TaskStatus`、`WaitingReason` 仍被 `chat_round_block_system` 使用，所以 import 不变。只需确认 `chat_round_completion_system` 的函数签名中不再有 `clock: Res<Clock>`。

- [ ] **Step 3: 运行 `cargo clippy` 和 `cargo test` 验证编译**

Run: `cargo clippy --all-targets --all-features -- -D warnings && cargo test --all-features chat_with_agent 2>&1 | tail -20`
Expected: 编译通过，现有测试可能因行为变化而失败（这是预期的，Task 2 修复测试）

- [ ] **Step 4: Commit**

```bash
git add src/systems/transform/chat_round.rs
git commit -m "refactor: remove parent status restoration from chat_round_completion_system"
```

---

### Task 2: 添加 ECS 系统执行顺序约束

**Files:**
- Modify: `src/plugins/task_runtime.rs:51-54`

**Interfaces:**
- Consumes: Bevy ECS scheduling系统
- Produces: 确保 `chat_round_completion_system` 在 `tool_calling_orchestrator_system` 之前执行

当前系统中 `chat_round_completion_system` 和 `tool_calling_orchestrator_system` 都在 `HarnessSet::Transform` 中，但没有显式排序。修复后需要 `chat_round_completion_system` 在 `tool_calling_orchestrator_system` 之前执行，保证 `ToolExecutionResultMessage` 在 orchestrator 检查时已存在。

- [ ] **Step 1: 添加 `.before(tool_calling_orchestrator_system)` 约束**

修改 `src/plugins/task_runtime.rs`，在 `chat_round_completion_system` 的调度中增加 `before(tool_calling_orchestrator_system)`：

```rust
chat_round_completion_system
    .in_set(HarnessSet::Transform)
    .after(tool_result_system)
    .before(chat_round_block_system)
    .before(tool_calling_orchestrator_system),
```

- [ ] **Step 2: 验证编译**

Run: `cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -5`
Expected: 编译通过，无 clippy 警告

- [ ] **Step 3: Commit**

```bash
git add src/plugins/task_runtime.rs
git commit -m "fix: ensure chat_round_completion_system runs before tool_calling_orchestrator"
```

---

### Task 3: 修改集成测试验证修复后行为

**Files:**
- Modify: `tests/chat_with_agent_flow.rs`

**Interfaces:**
- Consumes: `build_harness_app`、`Task`、`ChatSession`、`ToolCallingState`、`ToolExecutionResultMessage`
- Produces: 验证 chat_with_agent 完成后父任务状态和 conversation 正确

现有测试 `chat_with_agent_creates_chat_subtask` 和 `chat_with_agent_multi_round_via_handle` 只验证子任务创建，没有验证 chat round 完成后父任务的 tool result 收集。需要新增测试验证修复后的完整流程。

- [ ] **Step 1: 添加测试验证 chat_round_completion 后父任务保持 Waiting(SubTaskBatch)**

在 `tests/chat_with_agent_flow.rs` 末尾添加新测试：

```rust
/// 验证 chat_round_completion_system 不再直接恢复父任务到 Ready，
/// 而是保持 Waiting(SubTaskBatch) 等待 tool_calling_orchestrator_system 收集结果。
#[test]
fn chat_round_completion_preserves_parent_waiting_status() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(EchoExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(
        test_config(),
        runtime,
        executor,
        input_rx,
        vec![],
        harness::channels::ChannelManager::empty().0,
    );

    let reviewer_id = Uuid::new_v4();
    app.world_mut().spawn((
        Agent {
            id: reviewer_id,
            profile: AgentProfile {
                name: "reviewer".to_string(),
                model: "test-model".to_string(),
            },
            capabilities: AgentCapabilities {
                tags: vec!["review".to_string()],
                description: "reviewer agent".to_string(),
            },
            kind: AgentKind::Persistent,
            parent_id: None,
            bound_task_id: None,
            tool_permissions: AgentToolPermissions::default(),
        },
        harness::LongTermMemory::default(),
    ));

    let parent_agent_id = Uuid::new_v4();
    let perms = AgentToolPermissions {
        default_permission: ToolPermission::Allow,
        ..Default::default()
    };
    app.world_mut().spawn((
        Agent {
            id: parent_agent_id,
            profile: AgentProfile {
                name: "parent-agent".to_string(),
                model: "test-model".to_string(),
            },
            capabilities: AgentCapabilities {
                tags: vec!["general".to_string()],
                description: "parent agent".to_string(),
            },
            kind: AgentKind::Persistent,
            parent_id: None,
            bound_task_id: None,
            tool_permissions: perms,
        },
        harness::LongTermMemory::default(),
    ));

    let parent_task_id = Uuid::new_v4();
    let batch_id = Uuid::new_v4();
    let child_task_id = Uuid::new_v4();

    // 直接创建一个处于 Waiting(SubTaskBatch) 的父任务
    app.world_mut().spawn((
        Task {
            id: parent_task_id,
            content: "test".to_string(),
            creator: parent_agent_id,
            delegate: Some(parent_agent_id),
            status: TaskStatus::Waiting(harness::WaitingReason::SubTaskBatch { batch_id }),
            input_summary: "test".to_string(),
            result_summary: String::new(),
            priority: 0,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            retry_count: 0,
            max_retries: 3,
            next_retry_at: None,
            last_error: None,
            multi_turn: true,
            parent_task_id: None,
            batch_id: None,
            origin_channel: default_channel(),
            last_evaluated_turn: None,
        },
        harness::ShortTermMemory::default(),
    ));

    // 模拟子任务完成，发出 ChatRoundReadyMessage
    app.world_mut().spawn(harness::ChatRoundReadyMessage {
        child_task_id,
        parent_task_id,
        parent_agent_id,
        batch_id,
        parent_tool_call_id: "call_chat_test".to_string(),
        response: "test response".to_string(),
        child_agent_name: "reviewer".to_string(),
    });

    // 运行一帧让 chat_round_completion_system 处理
    app.update();

    // 验证父任务状态仍然是 Waiting(SubTaskBatch)，而非 Ready
    let parent_status: harness::TaskStatus = {
        let world = app.world_mut();
        let mut query = world.query::<&harness::Task>();
        query
            .iter(world)
            .find(|t| t.id == parent_task_id)
            .map(|t| t.status.clone())
            .expect("parent task should exist")
    };

    assert!(
        matches!(parent_status, harness::TaskStatus::Waiting(harness::WaitingReason::SubTaskBatch { .. })),
        "parent task should still be Waiting(SubTaskBatch) after chat_round_completion, got {:?}",
        parent_status
    );

    // 验证 ToolExecutionResultMessage 已生成
    let tool_result_count: usize = {
        let world = app.world_mut();
        let mut query = world.query::<&harness::ToolExecutionResultMessage>();
        query
            .iter(world)
            .filter(|r| r.tool_call_id.as_deref() == Some("call_chat_test"))
            .count()
    };

    assert_eq!(
        tool_result_count, 1,
        "exactly one ToolExecutionResultMessage should be spawned for the chat round"
    );
}
```

- [ ] **Step 2: 运行新测试验证通过**

Run: `cargo test --all-features chat_round_completion_preserves_parent_waiting_status -- --nocapture 2>&1 | tail -10`
Expected: PASS

- [ ] **Step 3: 运行全部 chat_with_agent 测试**

Run: `cargo test --all-features chat_with_agent -- --nocapture 2>&1 | tail -15`
Expected: 所有测试通过

- [ ] **Step 4: Commit**

```bash
git add tests/chat_with_agent_flow.rs
git commit -m "test: verify chat_round_completion preserves parent waiting status"
```

---

### Task 4: 全量回归测试与清理

**Files:**
- 无新增修改

- [ ] **Step 1: 运行完整测试套件**

Run: `cargo test --all-features 2>&1 | tail -20`
Expected: 所有测试通过

- [ ] **Step 2: 运行 clippy 和 fmt 检查**

Run: `cargo clippy --all-targets --all-features -- -D warnings && cargo fmt --all --check`
Expected: 无警告，格式正确

- [ ] **Step 3: 更新 spec 文档状态标注**

在 `docs/superpowers/specs/2026-07-01-chat-with-agent-tool-result-race-fix-design.md` 确认状态为 `__当前有效__`，无需修改（实施后仍为当前有效，待归档时再更新）。

- [ ] **Step 4: 最终 Commit（如有格式调整）**

```bash
git add -A
git commit -m "chore: final cleanup for chat_with_agent tool result race fix"
```

（仅在 Step 1/2 有修复时才需要此步骤）
