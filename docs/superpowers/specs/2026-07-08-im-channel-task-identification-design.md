# IM 通道任务标识设计

> **状态：当前有效**

## 背景

当前 Harness 已支持 Telegram、QQ 等 IM 通道的自动回执：Agent 文本回复、`SystemOutputMessage`、任务失败提示等都会按 `Task::routing_policy.output_channel` 自动推回来源会话。但当同一 IM 会话中存在多个并行任务时，用户收到多条消息却无法分辨每条消息来自哪个任务，体验较差。

## 目标

让 IM 通道中的每条系统/Agent 消息都携带清晰、紧凑的任务标识；同时把任务状态变更事件也展示在 IM 前端。

## 设计决策

- 任务标识：采用任务 UUID 前 8 位短码，例如 `[a1b2c3d4]`
- 前缀策略：所有目标为 IM 通道的文本消息统一加前缀
- 状态变更：作为独立状态消息发送到 IM
- 不影响 TUI：TUI 已有任务面板，文本事件中的 `task_id` 由 TUI 忽略

## 整体架构

改动集中在三层：

1. **事件层**：`EngineEvent::Text` 增加 `task_id: Option<TaskId>`
2. **输出路由层**：`frontend_output_system` 在构造 Text 事件时填入 `task_id`
3. **IM 前端层**：`ChannelFrontend::push_event` 为 Text 事件加前缀，并把 `TaskStatusChanged` 转成状态文本消息

```text
UserOutputMessage/SystemOutputMessage
            │
            ▼
   frontend_output_system
            │
            ▼
  EngineEvent::Text { task_id, role, content }
            │
            ▼
   ChannelFrontend::push_event
            │
            ▼
  ChannelOutboundMessage { content: "[a1b2c3d4] 助手: ..." }
            │
            ▼
       Telegram/QQ/飞书
```

## 消息格式

### 普通文本

```text
[a1b2c3d4] 助手: 已查到明天北京晴天。
[a1b2c3d4] 系统: 📝 摘要完成\n\n...
[a1b2c3d4] 失败: 任务执行失败（AgentError）：...
```

`role` 映射：

- `MessageRole::Agent` → `助手`
- `MessageRole::System` → `系统`
- `MessageRole::User` → `用户`（通常不通过自动回发走 IM）

### 状态变更

```text
[a1b2c3d4] 状态: 运行中 → 等待中
[a1b2c3d4] 状态: 等待中 → 已完成
```

`TaskStatusKind` 映射：

| 状态 | 文案 |
|---|---|
| Pending | 待处理 |
| Running | 运行中 |
| Waiting | 等待中 |
| Done | 已完成 |
| Failed | 已失败 |

## 实现要点

### 1. `EngineEvent::Text` 增加 `task_id`

```rust
pub enum EngineEvent {
    Text {
        target: EventTarget,
        role: MessageRole,
        content: String,
        task_id: Option<TaskId>,
    },
    // ...
}
```

### 2. `frontend_output_system` 透传 `task_id`

- 从 `UserOutputMessage` / `SystemOutputMessage` 构造 `EngineEvent::Text` 时，填入 `task_id: Some(output.task_id)`
- `TaskStatusChanged` 继续保留原事件（供 TUI 使用），但 `ChannelFrontend` 会额外将其渲染为状态文本消息

### 3. `ChannelFrontend` 渲染前缀

```rust
fn task_short_id(task_id: TaskId) -> String {
    task_id.to_string().split('-').next().unwrap_or("????").to_string()
}

fn prefix_content(task_id: TaskId, role_label: &str, content: &str) -> String {
    format!("[{}] {}: {}", task_short_id(task_id), role_label, content)
}
```

- Text 事件：根据 `role` 生成前缀；`ChannelFrontend::push_event` 的 Text match arm 需要从当前的 `EngineEvent::Text { target, content, .. }` 改为 `EngineEvent::Text { target, role, content, .. }`（或保留 `task_id` 字段）
- TaskStatusChanged 事件：生成 `状态: {旧状态} → {新状态}` 文本
- 广播事件：`ChannelFrontend` 对 `EventTarget::Broadcast` 原本就 `return` 不发送，改动后仍保持该行为

### 4. TUI 兼容

TUI 的 `handle_engine_event` 对 `EngineEvent::Text` 使用 `{ role, content, .. }` 模式，已兼容新增字段，忽略 `task_id`。

### 5. 其他事件

`EngineEvent::BatchProgress` 本次不处理，维持现有行为。

## 错误处理

- 找不到对应 Task 时：仍发送原内容（不填充 `task_id`），记录 `warn` 日志
- 短 ID 生成失败时：回退为 `[????]` 前缀
- `TaskStatusChanged` 目标 channel 与现有逻辑一致：若 `output_channel` 为 `None`，事件被丢弃

## 测试策略

### 单元测试

- `src/channels/frontend.rs`：
  - 带 `task_id` 的 Text 事件输出包含 `[短ID] 助手: ` 前缀
  - 无 `task_id` 的 Text 事件保持原内容
  - `TaskStatusChanged` 输出正确状态文本

### 集成测试

- 新增/扩展测试：多任务并行时，同一 channel 收到的多条回复分别带不同短 ID
- 验证系统通知和任务失败消息带前缀
- 验证 TUI 收到的 Text 事件不带前缀

### 回归

- `cargo fmt --all --check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test --all-features`
- 回归检查：`ChannelFrontend` 收到 `ApprovalRequest` 事件时不受 `task_id` 字段变化影响，编译与行为均正常

## 待实施计划

下一步调用 `writing-plans` 技能输出具体实施计划。
