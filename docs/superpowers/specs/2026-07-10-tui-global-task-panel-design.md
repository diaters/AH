# TUI 全局 Task 面板设计

> __当前有效__

## 背景

当前 TUI 的 Task 面板只显示来自 TUI 通道自身的任务状态变化。原因在于 `TuiFrontend.push_event()` 按通道过滤事件——`TaskStatusChanged` 事件以 `Directed(task.routing_policy.output_channel)` 发出，TUI 只接收目标为自己通道的事件。来自 QQ、Telegram 等其他通道的任务状态变化被完全忽略。

TUI 作为系统的本地控制台，天然应该看到所有任务的全局状态概览。

## 设计目标

1. TUI Task 面板显示系统中所有通道的任务状态
2. 每个任务显示来源通道标签，便于区分任务归属
3. 已完成/失败的任务自动清理，保持面板清爽
4. Chat 面板行为不变，仍只显示 TUI 通道的对话

## 变更范围

### 1. 事件层变更

#### `EngineEvent::TaskStatusChanged` 新增 `origin_channel` 字段

```rust
TaskStatusChanged {
    target: EventTarget,
    task_id: TaskId,
    name: String,
    status: TaskStatusKind,
    old_status: Option<TaskStatusKind>,
    result: Option<String>,
    parent_id: Option<TaskId>,
    origin_channel: Option<ChannelId>,  // 新增
}
```

`origin_channel` 取自 `task.origin_channel`，表示任务的前端来源通道。事件任务的 `origin_channel` 为 `None`。

#### `frontend_output_system` 填充 `origin_channel`

在构建 `TaskStatusChanged` 事件时，将 `task.origin_channel` 传入新字段。

#### `TuiFrontend.push_event()` 放宽过滤

对 `TaskStatusChanged` 事件：无论 `target` 是 `Broadcast` 还是 `Directed`（即使不匹配 TUI 通道），都接收并转发给 App。其他事件类型（Text、ApprovalRequest 等）保持原有过滤逻辑不变。

去重保护：`handle_engine_event` 已通过 `find(|t| t.id == task_id)` 实现先查找再更新或插入，同一 task_id 不会重复添加。

### 2. 状态层变更

#### `TaskState` 新增字段

```rust
pub struct TaskState {
    pub id: uuid::Uuid,
    pub name: String,
    pub status: TaskStatusKind,
    pub result: Option<String>,
    pub parent_id: Option<uuid::Uuid>,
    pub subtask_count: u32,
    pub completed_count: u32,
    pub origin_channel: Option<ChannelId>,  // 新增：来源通道
    pub completed_at: Option<std::time::Instant>,  // 新增：终态记录时间
}
```

- `origin_channel`：从事件中获取，用于渲染来源标签
- `completed_at`：任务进入 Done/Failed 时记录，用于自动清理

#### 已完成任务自动清理

- `handle_engine_event` 中任务变为 Done/Failed 时记录 `completed_at = Some(Instant::now())`
- `render()` 前调用 `cleanup_completed_tasks()`，移除 `completed_at` 超过 5 秒的任务
- 子任务随主任务一起被清理（清理主任务时，其子任务也一并移除）

### 3. 渲染层变更

#### 来源通道标签

在任务名称前添加来源通道标签：

```
Tasks
 ● [TUI] 分析代码结构
   │ ● 读取文件
   │ ✓ 解析依赖
 ● [QQ]  每日报告
 ✓ [TG]  审核部署
```

标签颜色与文本：
- TUI → `[TUI]` Green
- QQ → `[QQ]` Magenta
- Telegram → `[TG]` Blue
- 事件任务（origin_channel 为 None）→ `[EVT]` DarkGray
- 其他前端 → `[Web]`/`[FS]` 等 DarkGray

#### 终态任务渲染

保持已有行为：终态任务颜色变踏（DarkGray）。新增的自动清理会在 5 秒后移除。

## 不变更的部分

- Chat 面板：仍只显示 TUI 通道的对话消息
- 其他 Channel 前端（ChannelFrontend）：保持原有过滤逻辑，只接收目标为自己通道的事件
- Agent 状态：已是 Broadcast，无需变更
- ApprovalRequest/ApprovalResult：保持原有定向路由逻辑

## 测试覆盖

- `TaskState` 新增字段的正确填充
- `TuiFrontend.push_event()` 对 `TaskStatusChanged` 的放宽过滤
- 已完成任务自动清理逻辑（超时移除、未超时保留）
- 来源通道标签的渲染输出
- 去重：同一 task_id 不会重复添加到 `App.tasks`
- 其他 Channel 前端不受影响
