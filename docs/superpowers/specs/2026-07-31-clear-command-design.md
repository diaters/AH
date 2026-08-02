# /clear 命令设计

> **状态：当前有效**

## 背景

当前 `/finish` 命令将任务标记为 `Done`，触发完整的终态处理链路：

1. `finish_task_system` → `task.mark_done()`
2. `task_termination_system` → 清理 `ToolCallingState` + 停止 shell sessions + spawn `TaskTerminatedMessage` + 触发 `SummarizationRequestMessage`
3. `task_completion_hook_system` → 派发 `OnTaskCompleted` hook（经验治理等）

用户需要一种"静默删除"方式——直接移除 task 而不触发任何终态下游操作（摘要、经验收集、hook 派发等）。

## 语义

`/clear` 直接 despawn 当前 task entity 及其附属组件（STM、ToolCallingState 等），不经过终态转换，不触发任何下游操作。

## 设计

### 1. 命令解析层

`UserCommand` 新增变体：

```rust
/// /clear - 删除当前任务（不触发终态处理链路）
ClearCurrentTask,
```

`parse` 方法识别 `/clear`，映射到 `ClearCurrentTask`。

### 2. 消息层

新增 `ClearTaskMessage`：

```rust
/// /clear 命令产生的消息，用于通知 clear_task_system 执行清理
#[derive(Component)]
pub struct ClearTaskMessage {
    pub task_id: TaskId,
}
```

### 3. 命令处理层

`command_parse_system` 处理 `ClearCurrentTask`：查找同通道的活跃 task，spawn `ClearTaskMessage`。

### 4. 清理系统层

新增 `clear_task_system`：

1. 通过 `EntityIndex` 查找 task entity
2. 停止关联 shell sessions
3. Despawn 关联的 `ToolCallingState` entity
4. Despawn task entity 自身（含 STM、PreviousTaskStatus 等所有附属组件）
5. Despawn `ClearTaskMessage`
6. 在 despawn 前读取任务 `routing_policy.output_channel`，向该通道推送 `EngineEvent::TaskCleared`，通知前端移除对应展示（TUI 据此删除任务及其子任务）

**关键属性**：despawn 不会触发 `Changed<Task>`，因此 `task_termination_system`、`task_completion_hook_system` 均不会被触发，summarization、experience collection 等下游操作也不会执行。前端移除通知（`EngineEvent::TaskCleared`）不属于终态处理链路，仅用于同步展示。

### 5. 系统注册

`clear_task_system` 注册到 Transform schedule，与 `finish_task_system` 同集。

### 6. 测试覆盖

- `UserCommand::parse("/clear")` → `ClearCurrentTask`
- `clear_task_system` 正确 despawn task 及 ToolCallingState
- `clear_task_system` 不触发 `TaskTerminatedMessage`
- `clear_task_system` 不触发 `SummarizationRequestMessage`
- `clear_task_system` 推送 `EngineEvent::TaskCleared`（含正确的 `target` 与 `task_id`）
- TUI 收到 `TaskCleared` 后移除任务及其子任务展示
- 通道隔离：不同通道的 `/clear` 不影响其他通道的 task

## 涉及文件

| 文件 | 变更 |
|---|---|
| `src/domain/command.rs` | 新增 `ClearCurrentTask` 变体 + `parse` 分支 |
| `src/domain/message.rs` | 新增 `ClearTaskMessage` |
| `src/domain/mod.rs` | 导出 `ClearTaskMessage` |
| `src/systems/command.rs` | 处理 `ClearCurrentTask`，spawn `ClearTaskMessage` |
| `src/systems/transform/task_lifecycle.rs` | 新增 `clear_task_system`，推送 `EngineEvent::TaskCleared` |
| `src/systems/mod.rs` | 注册 `clear_task_system` |
| `src/domain/frontend.rs` | 新增 `EngineEvent::TaskCleared` 变体 |
| `src/tui/app.rs` | `handle_engine_event` 处理 `TaskCleared`，移除任务及子任务展示 |
