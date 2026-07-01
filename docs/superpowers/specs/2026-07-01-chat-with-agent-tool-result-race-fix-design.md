# 修复 chat_with_agent Tool Result 路径竞态

__当前有效__

## 问题

`chat_with_agent` 子任务完成时，`chat_round_completion_system` 在同一帧中同时恢复父任务状态并生成 `ToolExecutionResultMessage`，导致 `tool_calling_orchestrator_system` 无法正确收集 tool result，最终引发 LLM 400 错误。

### 现象

日志 `harness_2026-07-01_20-02-54.jsonl` 中，任务 `9eae25a4` 在调用 `chat_with_agent` 后反复收到 DeepSeek 400 错误：

```
An assistant message with 'tool_calls' must be followed by tool messages
responding to each 'tool_call_id'. (insufficient tool messages following
tool_calls message)
```

任务在 `Running` → `Waiting(RetryBackoff)` 之间循环，重试 3 次后耗尽。

### 根因

`chat_with_agent` 的 tool result 回填存在路径割裂：

1. 普通工具（shell_exec 等）走 `ToolCallingState` 管线：
   - LLM 返回 tool_calls → 创建 `ToolCallingState`（含 conversation）→ 工具执行 → `ToolExecutionResultMessage` → `tool_calling_orchestrator_system` 收集结果追加 `ConversationMessage::Tool` → 构建 follow-up 请求

2. chat_with_agent 走独立路径：
   - LLM 返回 chat_with_agent tool_call → 创建 `ToolCallingState` → `handle_tool_action` 创建子任务 → 父任务进入 `Waiting(SubTaskBatch)` → 子任务完成 → `ChatRoundReadyMessage` → `chat_round_completion_system` 生成 `ToolExecutionResultMessage` **并**恢复父任务到 `Ready`

关键竞态：`chat_round_completion_system` 在生成 `ToolExecutionResultMessage` 的同一帧将父任务从 `Waiting(SubTaskBatch)` 恢复为 `Ready`。而 `tool_calling_orchestrator_system` 只在父任务处于 `Waiting(SubTaskBatch | ToolExecution | Session)` 时处理 tool result。父任务已是 `Ready`，orchestrator 跳过，tool result 永远不会被收集进 `ToolCallingState.conversation`。

下次 LLM 请求时，conversation 中有 `Assistant(tool_calls)` 但缺少对应 `Tool` 消息，触发 API 400 错误。

## 修复方案

将父任务状态恢复的职责从 `chat_round_completion_system` 移到 `tool_calling_orchestrator_system`，使 chat_with_agent 的 tool result 走与普通工具完全相同的收集路径。

### 改动清单

#### 1. `src/systems/transform/chat_round.rs` — `chat_round_completion_system`

- 删除修改父任务状态的代码（`parent.status = TaskStatus::Ready` 和 `parent.updated_at = clock.0`）
- 仅保留生成 `ToolExecutionResultMessage` 的逻辑
- 父任务保持 `Waiting(SubTaskBatch)` 直到 orchestrator 收集完结果

改动前：

```rust
if let Some(mut parent) = tasks.iter_mut().find(|t| t.id == msg.parent_task_id) {
    parent.status = TaskStatus::Ready;        // ← 删除
    parent.updated_at = clock.0;               // ← 删除
    debug!(event = "ChatRoundCompleted", ...);
}
```

改动后：将 `mut tasks: Query<&mut Task>` 改为 `tasks: Query<&Task>`（只读），移除 `clock: Res<Clock>` 参数。删除状态修改代码，保留 debug 日志。

#### 2. `src/systems/transform/llm_response.rs` — `tool_calling_orchestrator_system`

无需修改核心逻辑。当前代码 1171-1193 行已经处理了 `Waiting(SubTaskBatch)` → `Waiting(Agent)` 的状态转换：

```rust
if state.work_item_id.is_none()
    && let Some(mut task) = tasks.iter_mut().find(|t| t.id == state.task_id)
    && matches!(
        task.status,
        TaskStatus::Waiting(
            WaitingReason::ToolExecution
                | WaitingReason::Session { .. }
                | WaitingReason::SubTaskBatch { .. }  // ← 已在匹配列表中
        )
    )
{
    task.status = TaskStatus::Waiting(WaitingReason::Agent);
    ...
}
```

修复后，父任务将保持 `Waiting(SubTaskBatch)` 直到 orchestrator 收集完 tool result 并将其转为 `Waiting(Agent)`。

#### 3. ECS 系统执行顺序确认

确认 `chat_round_completion_system` 在 `tool_calling_orchestrator_system` 之前执行，或在同一 Bevy 系统集中运行。由于两者都在 `HarnessSet::Transform` 中，需确认子集排序。如果 completion 在 orchestrator 之后执行，`ToolExecutionResultMessage` 将在下一帧才被处理——这也是正确的（Bevy 会在下一帧处理）。

### 修复后流程

```
ChatRoundReadyMessage
  → chat_round_completion_system
    → 生成 ToolExecutionResultMessage（tool_call_id = parent_tool_call_id）
    → 父任务保持 Waiting(SubTaskBatch)
  → tool_result_system
    → 记录 tool result 到父任务 STM
    → 保留 ToolExecutionResultMessage 实体（ToolCallingState 正在跟踪）
  → tool_calling_orchestrator_system
    → 收集 ToolExecutionResultMessage
    → 追加 ConversationMessage::Tool 到 conversation
    → spawn follow-up LLM 请求
    → 恢复父任务 Waiting(SubTaskBatch) → Waiting(Agent)
```

与 shell_exec 等普通工具的路径完全一致。

## 测试

- 修改 `tests/chat_with_agent_flow.rs` 中的集成测试，验证 chat_with_agent 完成后：
  - 父任务 conversation 中包含 `ConversationMessage::Tool`
  - 不出现 400 错误
  - follow-up 请求正确构建
- 验证 `chat_round_completion_system` 不再修改父任务状态
- 验证 `tool_calling_orchestrator_system` 正确处理 `Waiting(SubTaskBatch)` 状态的父任务

## 不涉及

- chat_with_agent 重复创建子任务（LLM 行为问题，暂不修复）
- multi-turn 轮次限制（暂不修复）
