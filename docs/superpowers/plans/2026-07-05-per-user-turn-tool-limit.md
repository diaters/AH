# 按用户输入轮次的工具调用软限制 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 将工具调用超限行为从硬失败改为软限制——返回合成 tool result 让 LLM 总结并询问用户，同时保留绝对硬上限防止无限循环。

**Architecture:** 在 `llm_response.rs` 的两处超限检查点（801 行新 iteration 创建前、1114 行结果收集后）插入分层判断：绝对硬上限 → WorkItem 硬失败 → 普通任务软限制。新增 `spawn_synthetic_limit_result` 辅助函数构造合成结果。新增 `tool_calling_turn_reset_system` 作为安全网。

**Tech Stack:** Rust, Bevy ECS, tracing

## Global Constraints

- 语言：Rust，遵循官方风格指南
- 架构：Bevy ECS
- 提交遵循 Conventional Commits
- WorkItem 超限时不得修改原任务状态（ExperienceCollection 隔离语义）
- 绝对硬上限倍数通过 `HARD_LIMIT_MULTIPLIER` 常量（值为 2）控制，作为内部常量不暴露为配置
- 不修改 `HARNESS_MAX_TOOL_ITERATIONS` 默认值
- 不新增 `ToolCallingState` 字段（移除 `turn_exhausted` 方案已在规格中确认）

---

## File Structure

| 文件 | 职责 |
|------|------|
| `src/systems/transform/llm_response.rs` | 修改两处超限检查逻辑，新增 `HARD_LIMIT_MULTIPLIER` 常量和 `spawn_synthetic_limit_result` 辅助函数 |
| `src/systems/transform/task_lifecycle.rs` | 新增 `tool_calling_turn_reset_system`，补充 `WaitingReason` import |
| `src/systems/transform/mod.rs` | 导出 `tool_calling_turn_reset_system` |
| `src/plugins/task_runtime.rs` | 注册 `tool_calling_turn_reset_system` |
| `tests/llm_tool_calling_flow.rs` | 新增软限制、绝对硬上限、WorkItem 隔离、turn reset 等集成测试 |

---

### Task 1: 新增 `tool_calling_turn_reset_system` 并注册

**Files:**
- Modify: `src/systems/transform/task_lifecycle.rs`（在文件末尾追加函数，补充 import）
- Modify: `src/systems/transform/mod.rs`（追加导出）
- Modify: `src/plugins/task_runtime.rs`（追加 import 和系统注册）

**Interfaces:**
- Consumes: `Task`（读取 `status` 和 `pending_confirmation_id`）、`ToolCallingState`（读取 `task_id`）
- Produces: `tool_calling_turn_reset_system` 函数，供 `TaskRuntimePlugin` 注册

- [ ] **Step 1: 在 `task_lifecycle.rs` 补充 import 并追加 `tool_calling_turn_reset_system`**

修改 `src/systems/transform/task_lifecycle.rs` 文件头部的 import 块。现有 import 为：

```rust
use crate::{
    app::{Clock, MemoryConfig},
    contracts::SessionBackend,
    domain::{
        FailureReason, FinishTaskMessage, RetryReadyMessage, ShortTermMemory, SubTaskConfig,
        SummarizationRequestMessage, SummarizationTrigger, Task, TaskStatus, TaskTerminatedMessage,
        ToolCallingState,
    },
    systems::NativeProcessBackend,
};
```

追加 `WaitingReason`：

```rust
use crate::{
    app::{Clock, MemoryConfig},
    contracts::SessionBackend,
    domain::{
        FailureReason, FinishTaskMessage, RetryReadyMessage, ShortTermMemory, SubTaskConfig,
        SummarizationRequestMessage, SummarizationTrigger, Task, TaskStatus, TaskTerminatedMessage,
        ToolCallingState, WaitingReason,
    },
    systems::NativeProcessBackend,
};
```

在文件末尾追加：

```rust
/// User Turn 结束时重置 ToolCallingState（安全网）
///
/// 核心重置已由 LLM 产出文本时的 ToolCallingState despawn 完成。
/// 本 system 处理边界场景：任务已进入 Waiting(User) 但 ToolCallingState
/// 仍残留（如外部信号直接修改了任务状态）。
pub fn tool_calling_turn_reset_system(
    mut commands: Commands,
    tasks: Query<&Task>,
    calling_states: Query<(Entity, &ToolCallingState)>,
) {
    for (state_entity, state) in &calling_states {
        if let Some(task) = tasks.iter().find(|t| t.id == state.task_id) {
            if task.status == TaskStatus::Waiting(WaitingReason::User)
                && task.pending_confirmation_id.is_none()
            {
                debug!(
                    event = "ToolCallingStateTurnReset",
                    task_id = %state.task_id,
                    "despawning residual ToolCallingState on Waiting(User)"
                );
                commands.entity(state_entity).despawn();
            }
        }
    }
}
```

- [ ] **Step 2: 在 `src/systems/transform/mod.rs` 追加导出**

修改第 25 行的 `pub use task_lifecycle::{...};` 为：

```rust
pub use task_lifecycle::{
    finish_task_system, retry_ready_system, task_termination_system,
    tool_calling_turn_reset_system,
};
```

- [ ] **Step 3: 在 `src/plugins/task_runtime.rs` 追加 import 和系统注册**

修改第 10-16 行的 import 块，追加 `tool_calling_turn_reset_system`：

```rust
use crate::{
    app::MemoryConfig,
    domain::TaskEvaluationConfig,
    systems::{
        HarnessSet, chat_round_block_system, chat_round_completion_system,
        chat_session_cleanup_system, llm_response_system, on_tool_returned_hook_system,
        retry_ready_system, sub_task_batch_block_system, sub_task_completion_system,
        task_completion_hook_system, task_termination_system, tool_calling_orchestrator_system,
        tool_calling_turn_reset_system, tool_result_system,
    },
};
```

在 `app.add_systems(Update, (...))` 调用中（第 34-64 行），在 `retry_ready_system.in_set(HarnessSet::Transform),` 之后追加：

```rust
                // 用户输入轮次重置（安全网）
                tool_calling_turn_reset_system.in_set(HarnessSet::Transform),
```

- [ ] **Step 4: 运行编译检查**

Run: `cargo check --all-features 2>&1 | head -50`
Expected: 无编译错误

- [ ] **Step 5: Commit**

```bash
git add src/systems/transform/task_lifecycle.rs src/systems/transform/mod.rs src/plugins/task_runtime.rs
git commit -m "feat: add tool_calling_turn_reset_system as safety net for ToolCallingState cleanup"
```

---

### Task 2: 新增 `HARD_LIMIT_MULTIPLIER` 常量和 `spawn_synthetic_limit_result` 辅助函数

**Files:**
- Modify: `src/systems/transform/llm_response.rs`（在文件适当位置追加常量和函数）

**Interfaces:**
- Consumes: `AgentExecutionResult`、`AgentExecutionOutput`、`OutputContent`、`ToolExecutionResultMessage`、`ToolReturnedHookPending`
- Produces: `HARD_LIMIT_MULTIPLIER` 常量、`spawn_synthetic_limit_result` 函数，供 Task 3/4 的超限逻辑调用

- [ ] **Step 1: 在 `llm_response.rs` 文件中追加 `HARD_LIMIT_MULTIPLIER` 常量和 `spawn_synthetic_limit_result` 函数**

在 `llm_response_system` 函数之前（文件中第一个 `pub fn` 之前）追加常量和辅助函数：

```rust
/// 绝对硬上限倍数：当 iteration 超过 HARD_LIMIT_MULTIPLIER × max_iterations 时强制失败
const HARD_LIMIT_MULTIPLIER: u32 = 2;

/// 生成合成 ToolExecutionResultMessage，用于向 LLM 传达工具预算已耗尽
///
/// 合成结果跳过 ToolCalledHookPending（不 spawn），因此不会触发 on_tool_called hook。
/// 合成结果会正常进入 on_tool_returned hook 流水线（通过 ToolReturnedHookPending）。
fn spawn_synthetic_limit_result(
    commands: &mut Commands,
    task_id: TaskId,
    agent_id: AgentId,
    tool_call_id: &str,
    tool_name: &str,
    iteration: u32,
    max_iterations: u32,
) {
    let tool_output = Ok(serde_json::json!({
        "exit_code": 1,
        "status": "tool_budget_exhausted",
        "output": format!(
            "[TOOL_BUDGET_EXHAUSTED] 本轮工具调用次数已达上限 ({}/{})。请总结你目前取得的进展，并向用户说明下一步需要什么，等待用户决策是否继续。",
            iteration, max_iterations
        )
    }));

    let result = AgentExecutionResult {
        task_id,
        agent_id,
        request_kind: AgentRequestKind::ToolExecution {
            tool_name: tool_name.to_string(),
        },
        result: Ok(AgentExecutionOutput {
            content: OutputContent::Text(String::new()),
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
            result,
            tool_name: tool_name.to_string(),
            tool_output,
            tool_call_id: Some(tool_call_id.to_string()),
            processed: false,
            original_tool_output: None,
        },
        ToolReturnedHookPending,
    ));
}
```

注意：检查文件头部是否已有 `use crate::domain::{..., ToolReturnedHookPending, ...}` import。若缺少，在现有 domain import 块中追加 `ToolReturnedHookPending`。

- [ ] **Step 2: 运行编译检查**

Run: `cargo check --all-features 2>&1 | head -50`
Expected: 无编译错误（函数未调用不应影响编译，但需确认 import 正确）

- [ ] **Step 3: Commit**

```bash
git add src/systems/transform/llm_response.rs
git commit -m "feat: add HARD_LIMIT_MULTIPLIER constant and spawn_synthetic_limit_result helper"
```

---

### Task 3: 修改 801 行超限检查——新 iteration 创建前的分层判断

**Files:**
- Modify: `src/systems/transform/llm_response.rs:801-817`

**Interfaces:**
- Consumes: `spawn_synthetic_limit_result`（Task 2 产出）、`HARD_LIMIT_MULTIPLIER`（Task 2 产出）
- Produces: 修改后的超限逻辑——绝对硬上限 → WorkItem 硬失败 → 普通任务合成结果

- [ ] **Step 1: 替换 801 行的超限检查逻辑**

将以下原代码（`llm_response.rs` 第 801-817 行）：

```rust
                        if new_iteration > info.max_iterations {
                            warn!(
                                event = "ToolCallingLimitExceeded",
                                task_id = %task.id,
                                iteration = new_iteration,
                                max_iterations = info.max_iterations,
                                "tool calling exceeded max iterations"
                            );
                            task.last_error = Some(format!(
                                "tool calling exceeded max iterations ({})",
                                info.max_iterations
                            ));
                            task.status = TaskStatus::Failed(FailureReason::AgentError);
                            task.updated_at = clock.0;
                            commands.entity(info.entity).despawn();
                            break;
                        }
```

替换为：

```rust
                        if new_iteration > info.max_iterations {
                            // 绝对硬上限：任何情况下都强制失败，防止无限循环
                            if new_iteration > HARD_LIMIT_MULTIPLIER * info.max_iterations {
                                warn!(
                                    event = "ToolCallingHardLimitExceeded",
                                    task_id = %task.id,
                                    iteration = new_iteration,
                                    max_iterations = info.max_iterations,
                                    "tool calling exceeded absolute hard limit"
                                );
                                if info.work_item_id.is_none() {
                                    task.last_error = Some(format!(
                                        "tool calling exceeded absolute hard limit ({}/{})",
                                        new_iteration, info.max_iterations
                                    ));
                                    task.status = TaskStatus::Failed(FailureReason::AgentError);
                                    task.updated_at = clock.0;
                                }
                                commands.entity(info.entity).despawn();
                                break;
                            }

                            if info.work_item_id.is_some() {
                                // WorkItem 保持硬失败语义，但不修改原任务状态
                                warn!(
                                    event = "ToolCallingLimitExceeded",
                                    task_id = %task.id,
                                    work_item_id = ?info.work_item_id,
                                    iteration = new_iteration,
                                    max_iterations = info.max_iterations,
                                    "work item tool calling exceeded max iterations"
                                );
                                commands.entity(info.entity).despawn();
                                break;
                            }

                            // 普通任务：生成合成 tool result
                            debug!(
                                event = "ToolBudgetExhausted",
                                task_id = %task.id,
                                iteration = new_iteration,
                                max_iterations = info.max_iterations,
                                "tool budget exhausted, returning synthetic result"
                            );

                            for call in &calls {
                                spawn_synthetic_limit_result(
                                    &mut commands,
                                    task.id,
                                    result.agent_id,
                                    &call.id,
                                    &call.name,
                                    info.iteration,
                                    info.max_iterations,
                                );
                            }

                            // 更新 ToolCallingState，记录这些 tool_call_id 正在等待合成结果
                            let pending_ids: Vec<String> =
                                calls.iter().map(|c| c.id.clone()).collect();
                            commands.entity(info.entity).despawn();
                            commands.spawn(ToolCallingState {
                                task_id: task.id,
                                agent_id: result.agent_id,
                                pending_tool_call_ids: pending_ids,
                                iteration: new_iteration,
                                max_iterations: info.max_iterations,
                                conversation: new_conversation,
                                tools: info.tools.clone(),
                                request_kind: info.request_kind.clone(),
                                work_item_id: info.work_item_id,
                            });

                            // 不生成真实 ToolExecutionRequestMessage，避免真实工具执行
                            // 注意：此时任务状态仍为 Waiting(ToolExecution)，
                            // 后续 ToolCallingState 存在时 orchestrator 允许继续
                            continue;
                        }
```

注意：`new_conversation` 在原代码第 820 行之后才构造。替换后需确保 `new_conversation` 的构造代码（原 820-825 行）移到此代码块之前。具体做法：将以下代码提前到 `if new_iteration > info.max_iterations {` 之前：

```rust
                        let mut new_conversation = info.conversation.clone();
                        new_conversation.push(ConversationMessage::Assistant {
                            content: None,
                            tool_calls: calls.clone(),
                            reasoning_content: reasoning_content.clone(),
                        });
```

- [ ] **Step 2: 运行编译检查**

Run: `cargo check --all-features 2>&1 | head -50`
Expected: 无编译错误

- [ ] **Step 3: Commit**

```bash
git add src/systems/transform/llm_response.rs
git commit -m "feat: replace hard failure with soft limit + hard cap at 801 line iteration check"
```

---

### Task 4: 修改 1114 行超限检查——结果收集后的分层判断

**Files:**
- Modify: `src/systems/transform/llm_response.rs:1114-1135`

**Interfaces:**
- Consumes: `HARD_LIMIT_MULTIPLIER`（Task 2 产出）
- Produces: 修改后的超限逻辑——绝对硬上限 → WorkItem despawn → 普通任务允许 follow-up

- [ ] **Step 1: 替换 1114 行的超限检查逻辑**

将以下原代码（`llm_response.rs` 第 1114-1135 行）：

```rust
        if state.iteration >= state.max_iterations {
            warn!(
                event = "ToolCallingLimitExceeded",
                task_id = %state.task_id,
                iteration = state.iteration,
                max_iterations = state.max_iterations,
                "tool calling reached max iterations"
            );
            // ExperienceCollection WorkItem 失败不应修改原任务状态
            if state.work_item_id.is_none()
                && let Some(mut task) = tasks.iter_mut().find(|t| t.id == state.task_id)
            {
                task.last_error = Some(format!(
                    "tool calling reached max iterations ({})",
                    state.max_iterations
                ));
                task.status = TaskStatus::Failed(FailureReason::AgentError);
                task.updated_at = clock.0;
            }
            commands.entity(state_entity).despawn();
            continue;
        }
```

替换为：

```rust
        if state.iteration >= state.max_iterations {
            // 绝对硬上限
            if state.iteration > HARD_LIMIT_MULTIPLIER * state.max_iterations {
                warn!(
                    event = "ToolCallingHardLimitExceeded",
                    task_id = %state.task_id,
                    iteration = state.iteration,
                    max_iterations = state.max_iterations,
                    "tool calling exceeded absolute hard limit on result collection"
                );
                // WorkItem 不修改原任务状态
                if state.work_item_id.is_none()
                    && let Some(mut task) = tasks.iter_mut().find(|t| t.id == state.task_id)
                {
                    task.last_error = Some(format!(
                        "tool calling exceeded absolute hard limit ({}/{})",
                        state.iteration, state.max_iterations
                    ));
                    task.status = TaskStatus::Failed(FailureReason::AgentError);
                    task.updated_at = clock.0;
                }
                commands.entity(state_entity).despawn();
                continue;
            }

            if state.work_item_id.is_some() {
                // WorkItem 保持现有隔离语义：不修改原任务状态，despawn 阻止 follow-up
                warn!(
                    event = "ToolCallingLimitExceeded",
                    task_id = %state.task_id,
                    work_item_id = ?state.work_item_id,
                    iteration = state.iteration,
                    max_iterations = state.max_iterations,
                    "work item tool calling reached max iterations"
                );
                commands.entity(state_entity).despawn();
                continue;
            }

            // 普通任务：允许 LLM 再响应一次，用于总结和询问用户
            // 任务仍处于 Waiting(ToolExecution)，ToolCallingState 存在时
            // tool_calling_orchestrator_system 允许继续
            debug!(
                event = "ToolBudgetExhaustedAllowingSummary",
                task_id = %state.task_id,
                iteration = state.iteration,
                max_iterations = state.max_iterations,
                "tool budget exhausted, allowing LLM to summarize"
            );
        }
```

关键区别：WorkItem 分支必须 `despawn` + `continue` 阻止 follow-up 请求生成；普通任务不再 `despawn` + `continue`，而是让代码落入后续的 follow-up LLM 请求生成逻辑，LLM 可以输出总结文本。只有绝对硬上限才 `despawn` + `continue`。

- [ ] **Step 2: 运行编译检查**

Run: `cargo check --all-features 2>&1 | head -50`
Expected: 无编译错误

- [ ] **Step 3: Commit**

```bash
git add src/systems/transform/llm_response.rs
git commit -m "feat: replace hard failure with soft limit + hard cap at 1114 line iteration check"
```

---

### Task 5: 集成测试——普通任务软限制不失败

**Files:**
- Modify: `tests/llm_tool_calling_flow.rs`

**Interfaces:**
- Consumes: `InfiniteToolCallExecutor`（已有）、`build_harness_app`（已有）、`ToolExecutionResultMessage`、`ToolCallingState`
- Produces: 测试用例 `tool_calling_soft_limit_returns_synthetic_result`

- [ ] **Step 1: 新增 `BudgetAwareMockExecutor` 和软限制测试**

在 `tests/llm_tool_calling_flow.rs` 的 `InfiniteToolCallExecutor` 之后追加：

```rust
/// Mock executor: 持续返回 ToolCalls，但 conversation 中出现 TOOL_BUDGET_EXHAUSTED 后返回 Text
struct BudgetAwareMockExecutor;

impl AgentExecutor for BudgetAwareMockExecutor {
    fn execute(&self, request: AgentExecutionRequest) -> harness::ExecutorFuture {
        let has_budget_exhausted = request
            .conversation
            .as_ref()
            .is_some_and(|conv| {
                conv.iter().any(|m| {
                    matches!(m, harness::ConversationMessage::Tool { content, .. }
                        if content.contains("TOOL_BUDGET_EXHAUSTED"))
                })
            });
        let iteration = request
            .conversation
            .as_ref()
            .map(|c| {
                c.iter()
                    .filter(|m| matches!(m, harness::ConversationMessage::Tool { .. }))
                    .count()
            })
            .unwrap_or(0);
        let call_id = format!("call_iter_{}", iteration);

        if has_budget_exhausted {
            Box::pin(async move {
                Ok(AgentExecutionOutput {
                    content: harness::OutputContent::Text(
                        "我已达到工具调用上限，请决定是否继续。".to_string(),
                    ),
                    reasoning_content: None,
                })
            })
        } else {
            Box::pin(async move {
                Ok(AgentExecutionOutput {
                    content: harness::OutputContent::ToolCalls(vec![LlmToolCall {
                        id: call_id,
                        name: "knowledge_search".to_string(),
                        arguments: r#"{"query":"loop"}"#.to_string(),
                    }]),
                    reasoning_content: None,
                })
            })
        }
    }
}
```

在文件末尾追加测试：

```rust
/// 测试：普通任务达到软限制后不失败，而是让 LLM 总结
#[test]
fn tool_calling_soft_limit_returns_synthetic_result() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(BudgetAwareMockExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(
        test_config(),
        runtime,
        executor,
        input_rx,
        vec![],
        harness::channels::ChannelManager::empty().0,
    );

    app.update();

    let agent_id = create_test_agent(
        app.world_mut(),
        AgentToolPermissions {
            default_permission: ToolPermission::Allow,
            overrides: HashMap::new(),
        },
    );
    create_test_tool_registry(app.world_mut());

    let tools = get_all_tools(app.world());

    let (task_entity, task) = spawn_task_with_stm(app.world_mut());
    let task_id = task.id;

    // Task::from_user_input_ready 设置 multi_turn: false，
    // 但 llm_response.rs:745 只在 multi_turn == true 时进入 Waiting(User)，
    // 因此必须显式开启 multi_turn 以测试完整软限制流程
    app.world_mut().get_mut::<Task>(task_entity).unwrap().multi_turn = true;

    let request = AgentExecutionRequest {
        task_id,
        agent_id,
        request_kind: AgentRequestKind::LlmCompletion,
        prompt: "keep calling tools".to_string(),
        system_prompt: None,
        tools,
        conversation: None,
        work_item_id: None,
    };
    app.world_mut()
        .spawn(harness::AgentExecutionRequestMessage { request });

    for _ in 0..50 {
        app.update();
    }

    let task = app.world().get::<Task>(task_entity).unwrap();
    // 任务不应 Failed，而应 Waiting(User)（LLM 总结后进入等待用户状态）
    assert!(
        matches!(task.status, TaskStatus::Waiting(WaitingReason::User)),
        "Task should be Waiting(User) after soft limit, got {:?}",
        task.status
    );
}
```

- [ ] **Step 2: 运行测试验证**

Run: `cargo test --all-features tool_calling_soft_limit_returns_synthetic_result -- --nocapture 2>&1 | tail -30`
Expected: PASS

- [ ] **Step 3: Commit**

```bash
git add tests/llm_tool_calling_flow.rs
git commit -m "test: add soft limit integration test for normal task tool budget exhaustion"
```

---

### Task 5.5: 集成测试——WorkItem 超限不修改原任务状态

**Files:**
- Modify: `tests/llm_tool_calling_flow.rs`

**Interfaces:**
- Consumes: `InfiniteToolCallExecutor`（已有）、`build_harness_app`（已有）、`WorkItem`
- Produces: 测试用例 `work_item_tool_limit_does_not_modify_task_status`

- [ ] **Step 1: 新增 WorkItem 隔离测试**

在 `tests/llm_tool_calling_flow.rs` 文件末尾追加：

```rust
/// 测试：WorkItem 超限时不应修改原任务状态
#[test]
fn work_item_tool_limit_does_not_modify_task_status() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(InfiniteToolCallExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(
        test_config(),
        runtime,
        executor,
        input_rx,
        vec![],
        harness::channels::ChannelManager::empty().0,
    );

    app.update();

    let agent_id = create_test_agent(
        app.world_mut(),
        AgentToolPermissions {
            default_permission: ToolPermission::Allow,
            overrides: HashMap::new(),
        },
    );
    create_test_tool_registry(app.world_mut());

    let tools = get_all_tools(app.world());

    let (task_entity, task) = spawn_task_with_stm(app.world_mut());
    let task_id = task.id;

    let work_item_id = uuid::Uuid::new_v4();

    let request = AgentExecutionRequest {
        task_id,
        agent_id,
        request_kind: AgentRequestKind::LlmCompletion,
        prompt: "work item calling tools".to_string(),
        system_prompt: None,
        tools,
        conversation: None,
        work_item_id: Some(work_item_id),
    };
    app.world_mut()
        .spawn(harness::AgentExecutionRequestMessage { request });

    for _ in 0..50 {
        app.update();
    }

    let task = app.world().get::<Task>(task_entity).unwrap();
    // WorkItem 超限不应修改原任务状态为 Failed
    assert!(
        !matches!(task.status, TaskStatus::Failed(_)),
        "Task should NOT be Failed when WorkItem exceeds limit, got {:?}",
        task.status
    );
}
```

- [ ] **Step 2: 运行测试验证**

Run: `cargo test --all-features work_item_tool_limit_does_not_modify_task_status -- --nocapture 2>&1 | tail -30`
Expected: PASS

- [ ] **Step 3: Commit**

```bash
git add tests/llm_tool_calling_flow.rs
git commit -m "test: add WorkItem isolation test verifying task status unchanged on WorkItem limit"
```

---

### Task 5.6: 单元测试——`tool_calling_turn_reset_system` 行为验证

**Files:**
- Modify: `tests/llm_tool_calling_flow.rs`

**Interfaces:**
- Consumes: `tool_calling_turn_reset_system`、`Task`、`ToolCallingState`、`WaitingReason`
- Produces: 测试用例 `tool_calling_turn_reset_system_cleans_on_waiting_user`

- [ ] **Step 1: 新增 turn reset system 单元测试**

在 `tests/llm_tool_calling_flow.rs` 文件末尾追加：

```rust
/// 测试：tool_calling_turn_reset_system 在任务 Waiting(User) 时清理残留 ToolCallingState，
/// 在 Waiting 确认中和其他状态时不清理。
#[test]
fn tool_calling_turn_reset_system_cleans_on_waiting_user() {
    use harness::{AgentId, TaskId};

    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(ToolCallingMockExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(
        test_config(),
        runtime,
        executor,
        input_rx,
        vec![],
        harness::channels::ChannelManager::empty().0,
    );

    app.update();

    // 场景 1: 任务处于 Waiting(User) 且无 pending_confirmation_id → 应清理
    let task_id_1 = TaskId::new();
    let agent_id = AgentId::new();
    let mut task1 = Task::from_user_input_ready("test", 3, default_channel());
    task1.id = task_id_1;
    task1.status = TaskStatus::Waiting(WaitingReason::User);
    let task1_entity = app.world_mut().spawn((task1, ShortTermMemory::default())).id();

    let state1_entity = app
        .world_mut()
        .spawn(ToolCallingState {
            task_id: task_id_1,
            agent_id,
            pending_tool_call_ids: vec!["call_1".to_string()],
            iteration: 1,
            max_iterations: 3,
            conversation: vec![],
            tools: vec![],
            request_kind: AgentRequestKind::LlmCompletion,
            work_item_id: None,
        })
        .id();

    // 场景 2: 任务处于 Waiting(User) 但有 pending_confirmation_id → 不应清理
    let task_id_2 = TaskId::new();
    let mut task2 = Task::from_user_input_ready("test", 3, default_channel());
    task2.id = task_id_2;
    task2.status = TaskStatus::Waiting(WaitingReason::User);
    task2.pending_confirmation_id = Some("confirm_1".to_string());
    let task2_entity = app.world_mut().spawn((task2, ShortTermMemory::default())).id();

    let state2_entity = app
        .world_mut()
        .spawn(ToolCallingState {
            task_id: task_id_2,
            agent_id,
            pending_tool_call_ids: vec!["call_2".to_string()],
            iteration: 1,
            max_iterations: 3,
            conversation: vec![],
            tools: vec![],
            request_kind: AgentRequestKind::LlmCompletion,
            work_item_id: None,
        })
        .id();

    // 场景 3: 任务处于 Waiting(ChatAgent) → 不应清理
    let task_id_3 = TaskId::new();
    let mut task3 = Task::from_user_input_ready("test", 3, default_channel());
    task3.id = task_id_3;
    task3.status = TaskStatus::Waiting(WaitingReason::ChatAgent);
    let task3_entity = app.world_mut().spawn((task3, ShortTermMemory::default())).id();

    let state3_entity = app
        .world_mut()
        .spawn(ToolCallingState {
            task_id: task_id_3,
            agent_id,
            pending_tool_call_ids: vec!["call_3".to_string()],
            iteration: 1,
            max_iterations: 3,
            conversation: vec![],
            tools: vec![],
            request_kind: AgentRequestKind::LlmCompletion,
            work_item_id: None,
        })
        .id();

    // 运行 system 足够次确保 turn reset system 执行
    for _ in 0..5 {
        app.update();
    }

    // 场景 1: ToolCallingState 应已被 despawn
    assert!(
        app.world().get::<ToolCallingState>(state1_entity).is_none(),
        "ToolCallingState should be despawned when task is Waiting(User) without confirmation"
    );

    // 场景 2: ToolCallingState 应保留
    assert!(
        app.world().get::<ToolCallingState>(state2_entity).is_some(),
        "ToolCallingState should remain when task has pending_confirmation_id"
    );

    // 场景 3: ToolCallingState 应保留
    assert!(
        app.world().get::<ToolCallingState>(state3_entity).is_some(),
        "ToolCallingState should remain when task is Waiting(ChatAgent)"
    );
}
```

- [ ] **Step 2: 运行测试验证**

Run: `cargo test --all-features tool_calling_turn_reset_system_cleans_on_waiting_user -- --nocapture 2>&1 | tail -30`
Expected: PASS

- [ ] **Step 3: Commit**

```bash
git add tests/llm_tool_calling_flow.rs
git commit -m "test: add unit test for tool_calling_turn_reset_system cleanup behavior"
```

---

### Task 6: 集成测试——绝对硬上限强制失败

**Files:**
- Modify: `tests/llm_tool_calling_flow.rs`

**Interfaces:**
- Consumes: `InfiniteToolCallExecutor`（已有，始终返回 ToolCalls 不产生 Text）
- Produces: 测试用例 `tool_calling_hard_limit_forces_failure`

- [ ] **Step 1: 新增绝对硬上限测试**

在 `tests/llm_tool_calling_flow.rs` 文件末尾追加：

```rust
/// 测试：绝对硬上限（HARD_LIMIT_MULTIPLIER * max_iterations）时强制失败任务
#[test]
fn tool_calling_hard_limit_forces_failure() {
    let runtime = Arc::new(Runtime::new().unwrap());
    // InfiniteToolCallExecutor 始终返回 ToolCalls，即使收到 TOOL_BUDGET_EXHAUSTED 也不产出 Text
    let executor: Arc<dyn AgentExecutor> = Arc::new(InfiniteToolCallExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(
        test_config(), // max_tool_iterations: 3, hard limit = 6
        runtime,
        executor,
        input_rx,
        vec![],
        harness::channels::ChannelManager::empty().0,
    );

    app.update();

    let agent_id = create_test_agent(
        app.world_mut(),
        AgentToolPermissions {
            default_permission: ToolPermission::Allow,
            overrides: HashMap::new(),
        },
    );
    create_test_tool_registry(app.world_mut());

    let tools = get_all_tools(app.world());

    let (task_entity, task) = spawn_task_with_stm(app.world_mut());
    let task_id = task.id;

    let request = AgentExecutionRequest {
        task_id,
        agent_id,
        request_kind: AgentRequestKind::LlmCompletion,
        prompt: "keep calling tools forever".to_string(),
        system_prompt: None,
        tools,
        conversation: None,
        work_item_id: None,
    };
    app.world_mut()
        .spawn(harness::AgentExecutionRequestMessage { request });

    for _ in 0..50 {
        app.update();
    }

    let task = app.world().get::<Task>(task_entity).unwrap();
    assert!(
        matches!(task.status, TaskStatus::Failed(_)),
        "Task should be Failed after exceeding absolute hard limit, got {:?}",
        task.status
    );
    assert!(
        task.last_error
            .as_ref()
            .is_some_and(|e| e.contains("absolute hard limit")),
        "Error should mention absolute hard limit, got: {:?}",
        task.last_error
    );
}
```

- [ ] **Step 2: 运行测试验证**

Run: `cargo test --all-features tool_calling_hard_limit_forces_failure -- --nocapture 2>&1 | tail -30`
Expected: PASS

- [ ] **Step 3: Commit**

```bash
git add tests/llm_tool_calling_flow.rs
git commit -m "test: add absolute hard limit integration test"
```

---

### Task 7: 修改现有 `tool_calling_exceeds_max_iterations` 测试以适配新行为

**Files:**
- Modify: `tests/llm_tool_calling_flow.rs:254-266`

**Interfaces:**
- Consumes: 无
- Produces: 更新后的测试用例，验证 `InfiniteToolCallExecutor` 下任务在绝对硬上限处失败

- [ ] **Step 1: 更新 `tool_calling_exceeds_max_iterations` 测试断言**

将 `tests/llm_tool_calling_flow.rs` 中 `tool_calling_exceeds_max_iterations` 测试（第 254-266 行）的断言更新：

原断言检查 `TaskStatus::Failed` 且 `last_error` 包含 `"max iterations"`。在新的软限制行为下，`InfiniteToolCallExecutor`（始终返回 ToolCalls 不产生 Text）会先触发软限制（合成结果），然后 LLM 再次请求工具时再次触发软限制，直到达到绝对硬上限 `HARD_LIMIT_MULTIPLIER * max_iterations`。

修改断言：

```rust
    let task = app.world().get::<Task>(task_entity).unwrap();
    assert!(
        matches!(task.status, TaskStatus::Failed(_)),
        "Task should be Failed after exceeding absolute hard limit, got {:?}",
        task.status
    );
    assert!(
        task.last_error
            .as_ref()
            .is_some_and(|e| e.contains("absolute hard limit")),
        "Error should mention absolute hard limit, got: {:?}",
        task.last_error
    );
```

- [ ] **Step 2: 运行测试验证**

Run: `cargo test --all-features tool_calling_exceeds_max_iterations -- --nocapture 2>&1 | tail -30`
Expected: PASS

- [ ] **Step 3: Commit**

```bash
git add tests/llm_tool_calling_flow.rs
git commit -m "test: update existing max iterations test for soft limit + hard cap behavior"
```

---

### Task 8: 全量测试与 lint 检查

**Files:**
- 无代码修改，仅运行验证

**Interfaces:**
- Consumes: 所有前序 Task 的产出
- Produces: 无

- [ ] **Step 1: 运行 cargo fmt 检查**

Run: `cargo fmt --all --check 2>&1 | head -20`
Expected: 无输出（格式正确）

- [ ] **Step 2: 运行 cargo clippy 检查**

Run: `cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -30`
Expected: 无 warning

- [ ] **Step 3: 运行全量测试**

Run: `cargo test --all-features 2>&1 | tail -50`
Expected: 所有测试 PASS

- [ ] **Step 4: Commit（如有格式修正）**

仅在 fmt/clippy 产生修正时提交：

```bash
git add -A
git commit -m "chore: fix formatting and clippy warnings"
```

---

### Task 9: 文档同步

**Files:**
- Modify: `docs/current-state.md`（补充软限制能力说明）
- Modify: `docs/configuration.md`（确认 max_iterations 描述一致）
- Modify: `docs/superpowers/specs/2026-07-05-per-user-turn-tool-limit-design.md`（标注状态为"已实施"）

**Interfaces:**
- Consumes: 规格文档中的描述
- Produces: 同步后的文档

- [ ] **Step 1: 在 `docs/current-state.md` 补充能力说明**

在"已实现"部分追加：

```markdown
- 工具调用软限制：单轮用户输入内达到 `HARNESS_MAX_TOOL_ITERATIONS` 后返回合成 tool result，让 LLM 总结并询问用户；绝对硬上限（2 × max_iterations）时强制失败
```

- [ ] **Step 2: 确认 `docs/configuration.md` 描述一致**

检查第 75 行附近 `HARNESS_MAX_TOOL_ITERATIONS` 的描述是否为"单轮工具调用最大迭代次数"或"单次用户输入后，LLM 工具调用最大迭代次数"。若不一致则修正为后者。

- [ ] **Step 3: 更新规格文档状态**

将 `docs/superpowers/specs/2026-07-05-per-user-turn-tool-limit-design.md` 顶部的 `> **状态：当前有效**` 保留不变（设计仍然有效，只是已实施）。

- [ ] **Step 4: Commit**

```bash
git add docs/current-state.md docs/configuration.md
git commit -m "docs: sync documentation for per-user-turn tool calling soft limit"
```
