# Spec: 重试分派修复

## 问题背景

### 现象

任务在 LLM 调用失败触发重试后，`retry_ready_system` 成功将任务状态从 `Waiting(RetryBackoff)` 转换为 `Ready`，但任务没有被重新分派给 Agent，导致任务悬空。

### 日志证据

```
06:06:27.645  mark_running        delegate = Some(f7bea689...)  ✅
06:06:44.952  schedule_retry      delegate = ?                  (未记录)
06:06:47.160  RetryReady #1       agent_delegate = null         ❌
06:06:47.321  RetryReady #2       agent_delegate = null         ❌
```

### 根因分析

1. **双重 RetryReady 事件**：`retry_wakeup_system` 触发了两次，导致 `retry_ready_system` 被调用两次。原因是 `retry_wakeup_system` 和 `signal_ingest_system` 在同一个 `HarnessSet::Signal` 集合中运行，没有明确的执行顺序约束。

2. **delegate 字段丢失**：在 `schedule_retry` → `RetryReady` 路径中，`delegate` 字段从 `Some(...)` 变为 `None`。代码审查未发现清除 delegate 的显式逻辑，推测为 Bevy ECS 内部行为或竞态条件。

3. **防御性编程缺失**：`retry_ready_system` 在 `task.delegate = None` 或 delegate 指向的 agent 不存在时，**静默跳过**，不插入 `PendingDispatch`，也不 fallback 到 `BrainLlm` 策略。

## 解决方案

### 修复 1：retry_ready_system 防御性 fallback

**目标**：确保重试任务始终被分派，无论 delegate 状态如何。

**修改位置**：`src/systems/transform/task_lifecycle.rs` 中的 `retry_ready_system`

**修改策略**：

```
if task.delegate = Some(agent_id):
    if agent 存在:
        → DirectDelegate 策略
    else:
        → BrainLlm 策略 (fallback)
else:
    → BrainLlm 策略
```

**新增日志**：

- `RetryDirectDispatch`：delegate 存在且 agent 找到
- `RetryDelegateAgentNotFound`：delegate 存在但 agent 未找到，fallback 到 BrainLlm
- `RetryBrainLlm`：delegate 为 None，走 BrainLlm

### 修复 2：防止重复 RetryWakeup

**目标**：避免 `retry_wakeup_system` 在同一任务的 `Waiting(RetryBackoff)` 状态期间重复触发。

**修改位置**：`src/systems/ingress.rs` 中的 `retry_wakeup_system`

**修改策略**：在 `signal_ingest_system` 处理 `RetryWakeup` Signal 时，立即将 `task.next_retry_at` 清除为 `None`。这样 `retry_wakeup_system` 在下一帧检查时不会再次触发。

**实现方式**：将 `next_retry_at` 的清除从 `mark_ready_for_retry` 移动到 `signal_ingest_system` 的 RetryWakeup 分支。

## 影响范围

### 正常路径（无影响）

- 用户续轮 (`Waiting(User)` → `Ready`)：delegate 在 `continue_task_system` 中被清除，然后立即插入 `PendingDispatch`，不经过 `retry_ready_system`。

### 重试路径（修复）

- LLM 调用失败 → `schedule_retry` → `RetryReady` → `retry_ready_system`
- **修复前**：delegate 为 None 时静默跳过，任务悬空
- **修复后**：delegate 为 None 时 fallback 到 `BrainLlm`，任务被重新分派

### 新增日志

| 事件 | 含义 |
|------|------|
| `RetryDirectDispatch` | 重试任务走 DirectDelegate 策略（delegate 有效） |
| `RetryDelegateAgentNotFound` | delegate 指向的 agent 不存在，fallback 到 BrainLlm |
| `RetryBrainLlm` | 无 delegate 或显式选择 BrainLlm 策略 |

## 测试覆盖

需要新增测试用例：

1. `retry_with_valid_delegate_dispatches_direct`：delegate 有效时走 DirectDelegate
2. `retry_with_missing_delegate_falls_back_to_brain_llm`：delegate 为 None 时 fallback 到 BrainLlm
3. `retry_with_stale_delegate_falls_back_to_brain_llm`：delegate 指向已不存在的 agent 时 fallback

## 实施计划

1. 修改 `retry_ready_system`，添加防御性 fallback 逻辑
2. 修改 `signal_ingest_system`，在 RetryWakeup 分支清除 `next_retry_at`
3. 新增测试用例
4. 运行现有测试确保无回归

## 相关文件

- `src/systems/transform/task_lifecycle.rs`：`retry_ready_system`
- `src/systems/transform/signal_ingest.rs`：`signal_ingest_system`
- `src/domain/task.rs`：`mark_ready_for_retry`
- `tests/error_handling_flow.rs`：现有重试相关测试
