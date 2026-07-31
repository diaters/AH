# 前端展示优化设计

> **状态：当前有效**
>
> 日期：2026-07-30

## 背景

当前 TUI 与 IM 通道（QQ/Telegram）的前端展示存在四个问题：

1. TUI 侧边栏显示 Agent 列表，但 Agent 状态信息对监控价值低且占用垂直空间
2. TUI Task 列表每条 task 不显示被指派的 Agent 名，监控信息不足
3. TUI 侧边栏信息密度低，对话区域占用过多，与"TUI 主要用于监控后台"的定位不符
4. IM 通道在 task 状态变化时仅推送 `[short_id] 状态: X → Y`，信息单薄；工具调用情况完全不可见

## 目标

- 去除 TUI Agent 列表，释放侧边栏垂直空间
- Task 列表附带被指派的 Agent 名
- 加宽 TUI 侧边栏，强化监控信息，弱化对话
- IM 通道 task 状态变化推送附带 task name 与 agent name
- IM 通道工具调用开始时推送工具调用具体情况（不推送 result）

## 非目标

- 不修改 `App.agents` 字段与 `AgentStatusChanged` 事件处理逻辑（仅去除渲染）
- 不推送 `ToolCallFinished` 事件（用户明确要求不显示 result）
- 不调整 ChatPanel 的渲染逻辑（仅调整布局宽度）
- 不引入飞书通道相关改动

## 设计

### 事件结构变更

位于 `src/domain/frontend.rs`。

#### 扩展 `TaskStatusChanged`

新增两个字段：

```rust
TaskStatusChanged {
    target: EventTarget,
    task_id: TaskId,
    name: String,
    status: TaskStatusKind,
    old_status: Option<TaskStatusKind>,
    result: Option<String>,
    parent_id: Option<TaskId>,
    origin_channel: Option<ChannelId>,
    agent_name: Option<String>,                // 新增：被指派 agent 的 profile.name
    waiting_reason: Option<WaitingReasonKind>, // 新增：等待原因（仅 status==Waiting 时有意义）
}
```

- `agent_name`：通过 `EntityIndex::get_agent(&task.delegate)` 查询 agent 的 `profile.name`。`delegate` 为 `None` 时填 `None`
- `waiting_reason`：当 `task.status` 为 `Waiting(reason)` 时映射为 `WaitingReasonKind`，否则为 `None`

#### 新增 `WaitingReasonKind` 枚举

精简版，只暴露前端展示所需的语义，避免泄露 ECS 内部 `WaitingReason` 的全部细节：

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitingReasonKind {
    Agent,  // 等待 agent 调度
    Tool,   // 等待工具执行
    User,   // 等待用户确认
    Retry,  // 重试退避
    Other,
}
```

映射规则（在 `frontend_output_system` 中实现）：

| `WaitingReason` | `WaitingReasonKind` |
|---|---|
| `Agent` | `Agent` |
| `ToolExecution` / `Session { .. }` / `SubTaskBatch { .. }` | `Tool` |
| `User` / `Approval` | `User` |
| `RetryBackoff` | `Retry` |
| `Evaluator` / `Summarization` / `ChatAgent` | `Other` |

注：`Session`（等待 shell 会话）与 `SubTaskBatch`（等待子任务批次）本质都是等待工具/子任务执行，归为 `Tool` 语义；`Approval` 本质是等待用户审批确认，归为 `User` 语义。

#### 新增 `ToolCallStarted` 事件

```rust
ToolCallStarted {
    target: EventTarget,
    task_id: TaskId,
    agent_name: String,
    tool_name: String,
    tool_input_summary: String,
}
```

- 路由目标：复用 task 的 `routing_policy.output_channel`
- `tool_input_summary`：由 `summarize_tool_input` 函数生成（见下文）
- 不推送 `ToolCallFinished`（符合"不显示 result"要求）

### 事件产生

#### TaskStatusChanged 构造调整

位于 `src/systems/frontend_output.rs` 的 `frontend_output_system`。

在构造 `TaskStatusChanged` 事件时（当前行 133-142），增加：

1. 通过 `index.get_agent(&task.delegate)` 查询 agent entity，读取 `agent.profile.name`
2. 从 `task.status` 提取 `waiting_reason`：若 `Waiting(reason)` 则映射，否则 `None`

`index.get_agent` 返回 `None` 时，`agent_name` 填 `None`，不影响事件推送。

#### ToolCallStarted 推送

位于 `src/systems/tools/dispatch.rs` 的 `tool_dispatch_system`，仅在 `Allow` 路径（权限检查直接通过）推送。

不在 `Confirm` / `Approval` 路径推送，原因：这两条路径会 spawn `ToolConfirmationRequestMessage`，由 `frontend_output_system` 转为 `ApprovalRequest` 事件推送，已携带 `tool_name` 与 `tool_input`，重复推送无意义。审批通过后的工具执行在 `approval_result_system` 中，此时 task 状态从 `Waiting(User)` → `Running`，`TaskStatusChanged` 已会推送。

推送方式：通过 `FrontendRegistry` 资源遍历所有 frontend 调用 `push_event`，与现有 `frontend_output_system` 的推送模式一致。

需访问 `EntityIndex` 查询 task 的 `routing_policy.output_channel` 作为 `target`，以及 agent 的 `profile.name`。

#### `summarize_tool_input` 函数

位于 `src/domain/frontend.rs`，作为前端展示辅助函数。

```rust
pub fn summarize_tool_input(tool_name: &str, tool_input: &serde_json::Value) -> String
```

规则：

- `shell_exec` / `shell_start`：取 `command` 字段值，截断到 80 字符
- `channel_send`：取 `channel` 字段 + `content` 前 50 字符，格式 `channel=qq content=...`
- `create_tasks`：取 `tasks` 数组长度，格式 `N 个子任务`
- `wait_tasks`：取 `task_ids` 数组长度，格式 `等待 N 个任务`
- 其他工具：JSON 序列化后取前 100 字符
- JSON 解析失败或字段缺失：返回空字符串

### TUI 调整

#### 去除 Agents 段

位于 `src/tui/status.rs` 行 71-105。

删除 Agents 段的标题与渲染循环。`App.agents` 字段与 `AgentStatusChanged` 事件处理保留（`AgentStatusKind` 仍可能被复用，避免破坏现有逻辑）。

#### TaskState 扩展

位于 `src/tui/app.rs` 行 46-59。

新增两个字段：

```rust
pub struct TaskState {
    // ... 现有字段 ...
    pub agent_name: Option<String>,
    pub waiting_reason: Option<WaitingReasonKind>,
}
```

事件处理（`EngineEvent::TaskStatusChanged` 分支）写入这两个字段。

#### 侧边栏加宽

位于 `src/tui/app.rs` 行 734-738。

`content_layout` 的侧边栏约束从 `Constraint::Length(30)` 改为 `Constraint::Length(48)`。

#### Task 渲染格式

位于 `src/tui/status.rs` 行 107-205。

主任务行格式：

```text
{icon} [QQ] task_name @agent_name (1/3) ⏳tool
```

- `{icon}`：状态图标（保留现有）
- `[QQ]`：来源通道标签（保留现有）
- `task_name`：任务名（保留现有）
- `@agent_name`：仅当 `agent_name` 存在时显示
- `(1/3)`：仅当有子任务时显示（保留现有）
- `⏳tool`：仅当 `waiting_reason` 存在时显示，文本为 `⏳agent` / `⏳tool` / `⏳user` / `⏳retry` / `⏳other`，颜色为 Cyan

子任务行格式：

```text
  │ {icon} subtask_name @agent_name
```

- `@agent_name`：仅当 `agent_name` 存在时显示

#### ToolCallStarted 在 TUI

TUI 全局接收 `ToolCallStarted`（同 `TaskStatusChanged`，跨通道均接收），作为系统消息追加到 ChatPanel。

位于 `src/tui/mod.rs` 的 `push_event`，在 `for_me` 判断中增加 `ToolCallStarted { .. }` 全局接收。

位于 `src/tui/app.rs` 事件处理，`ToolCallStarted` 转为 `ChatMessage::System`：

```text
[a1b2] 🔧 agent_name 调用 shell_exec: ls -la
```

格式：`[{task_short_id}] 🔧 {agent_name} 调用 {tool_name}: {tool_input_summary}`

- `task_short_id`：task_id 前 8 字符（与 IM 通道的 `task_short_id` 函数一致）
- 若 `tool_input_summary` 为空，省略 `: {summary}` 部分

理由：TUI 用于监控，工具调用是重要活动；放在 ChatPanel 作为调试信息不占用侧边栏空间，符合"对话仅用于调试"定位。

### IM 通道调整

#### TaskStatusChanged 渲染扩展

位于 `src/channels/frontend.rs` 行 183-225。

旧格式：

```text
[a1b2] 状态: 运行中 → 等待中
```

新格式：

```text
[a1b2] task_name: 运行中 → 等待中 @agent_name
```

- `task_name`：截断到 30 字符
- `@agent_name`：仅当 `agent_name` 存在时追加

#### ToolCallStarted 渲染

位于 `src/channels/frontend.rs`，新增 `EngineEvent::ToolCallStarted` match 分支。

格式：

```text
[a1b2] 🔧 agent_name 调用 tool_name: tool_input_summary
```

- `parse_mode: None`，纯文本发送
- `task_short_id`：task_id 前 8 字符
- 若 `tool_input_summary` 为空，省略 `: {summary}` 部分

路由过滤逻辑与现有 `TaskStatusChanged` 一致：`Broadcast` 忽略，`Directed` 按 `matches` 过滤。

### 错误处理与边界

- `index.get_agent` 返回 `None` 时，`agent_name` 填 `None`，TUI/IM 渲染时跳过 `@agent_name` 部分，不影响其他信息展示
- `summarize_tool_input` 对未知工具名或异常 JSON 返回空字符串，IM/TUI 渲染时省略 `: summary` 部分
- `ToolCallStarted` 推送失败不影响工具执行（推送是 fire-and-forget，与现有 `TaskStatusChanged` 一致）
- `ToolCallStarted` 仅在 `Allow` 路径推送，`Confirm` / `Approval` 路径不推送（已有 `ApprovalRequest` 事件携带 tool 信息）
- `ToolCallStarted` 在权限检查未通过（`ToolNotFound` / `AgentNotFound` / `ToolTagDenied`）时不推送

## 测试

### 单元测试

- `summarize_tool_input` 对各工具（shell_exec / channel_send / create_tasks / wait_tasks / 未知工具）的摘要正确性
- `WaitingReason` → `WaitingReasonKind` 映射覆盖所有变体
- `frontend_output_system` 构造 `TaskStatusChanged` 时正确填充 `agent_name` 与 `waiting_reason`
- `frontend_output_system` 在 `delegate` 为 `None` 时 `agent_name` 为 `None`
- `ChannelFrontend` 渲染 `ToolCallStarted` 输出格式正确
- `ChannelFrontend` 渲染扩展后的 `TaskStatusChanged`（含 task_name 与 agent_name）
- `TuiFrontend` 全局接收 `ToolCallStarted`（同 `TaskStatusChanged`，跨通道均接收）

### 现有测试更新

`TaskStatusChanged` 构造增加新字段后，所有构造该事件的测试 fixture 需补 `agent_name: None` 与 `waiting_reason: None` 字段。涉及文件：

- `src/systems/frontend_output.rs` 测试模块
- `src/tui/mod.rs` 测试模块
- `src/channels/frontend.rs` 测试模块
- 其他可能构造该事件的集成测试

## 影响范围

### 修改文件

- `src/domain/frontend.rs`：扩展 `TaskStatusChanged`，新增 `WaitingReasonKind`、`ToolCallStarted`、`summarize_tool_input`
- `src/systems/frontend_output.rs`：`frontend_output_system` 构造事件时填充新字段
- `src/systems/tools/dispatch.rs`：`tool_dispatch_system` 推送 `ToolCallStarted`
- `src/tui/mod.rs`：`push_event` 全局接收 `ToolCallStarted`
- `src/tui/app.rs`：`TaskState` 扩展，事件处理，侧边栏加宽，`ToolCallStarted` 转 `ChatMessage::System`
- `src/tui/status.rs`：去除 Agents 段，Task 渲染追加 `@agent_name` 与 `⏳reason`
- `src/channels/frontend.rs`：`TaskStatusChanged` 渲染扩展，新增 `ToolCallStarted` 渲染

### 不变项

- `App.agents` 字段与 `AgentStatusChanged` 事件处理保留
- `ChatPanel` 渲染逻辑不变（仅追加 `ToolCallStarted` 转的 System 消息）
- `ApprovalRequest` / `ApprovalResult` 事件不变
- 工具执行链路不变（仅新增事件推送，不影响执行流程）
