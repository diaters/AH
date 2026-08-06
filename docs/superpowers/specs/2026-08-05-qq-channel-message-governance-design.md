# IM 通道状态消息治理设计

> **状态：当前有效**
>
> 关联计划：`docs/superpowers/plans/2026-08-05-qq-channel-apis-plan.md`（QQ 通道撤回/typing/交互回调能力已就绪，本设计为其调用方集成）

## 背景与动机

QQ 通道当前会向用户推送过多状态切换消息，掩盖 LLM 的核心回复。一个带工具确认的典型 Task 会产生 6-7 条消息，其中只有 1 条是 LLM 自然语言回复，其余为：

- 任务状态变更（Pending→Running、Running→Waiting、Waiting→Running、Running→Done），每次变更一条独立消息
- 入向 ACK（每次用户发消息都回 `收到：<预览>`）
- 审批按钮点击确认（`已选择：<label>`）

`docs/current-state.md` 已将"QQ 通道消息撤回的调用方集成尚未接入"列为待完善项。QQ 通道的 `recall_message`、`send_typing`、`QqMessageResponse.id` 三个能力已就绪但无生产调用方（标注 `#[allow(dead_code)]`）。

Telegram 通道同样存在状态消息噪音问题，且 Telegram Bot API 也支持 `deleteMessage`（撤回）、`sendChatAction`（typing）、`editMessageText`（编辑）。recall/typing 是跨通道通用能力，只是 API 不同。

## 目标

1. 减少状态消息对 LLM 回复的掩盖
2. 接入已就绪但未使用的 QQ 撤回/typing 能力
3. 在 Channel trait 层统一抽象 recall/typing，使治理策略可跨通道复用
4. 保持策略失败时不阻塞主流程（尽力而为）

## 非目标

- 不改动 LLM 回复本身的发送逻辑
- 不治理系统通知（摘要完成、任务失败提示等 `SystemOutputMessage`）
- 不新增 Telegram 集成测试基础设施（本次仅单元 + 通道层测试）
- 不改动飞书通道（`lark.rs` 仍为空占位）

## 治理策略

### 策略矩阵

| 场景 | 策略 | 实现层 |
|---|---|---|
| 入向 ACK | C2C 发 typing 替代文字 ACK；群聊静默不发 | 通道 `listen` 内部 |
| 任务状态变更（中间态） | 滚动撤回：发新状态消息前撤回上一条 | ChannelFrontend |
| 任务状态变更（最终态 Done） | 延迟决策：先保留，等 LLM 回复到达时撤回；若无 LLM 回复则保留作为 fallback | ChannelFrontend |
| 任务状态变更（最终态 Failed） | 保留（含错误信息） | ChannelFrontend |
| 审批请求（带按钮） | 用户点击后撤回审批请求消息 | 通道 `listen` 内部 |
| 审批确认（已选择） | 保留 | 不变 |
| LLM 回复 | 保留 | 不变 |
| 系统通知 | 保留 | 不变 |

### 滚动撤回时序

```
用户发消息 → 通道 listen 收到
  ↓
listen: send_typing()（C2C）或 静默（群聊）
  ↓
TaskStatusChanged (Pending→Running)
  → Frontend 查 last_status_msg[(task, recipient)] → 无
  → 发 TaskStatus 消息
  → on_sent(msg_id) → 更新 last_status_msg
  ↓
TaskStatusChanged (Running→Waiting)
  → Frontend 查 last_status_msg → Some(old_id)
  → 发 Recall{target=old_id}（撤回"运行中"）
  → 发 TaskStatus 消息（"等待中"）
  → on_sent(new_id) → 更新 last_status_msg
  ↓
（后续中间态同理滚动撤回）
  ↓
TaskStatusChanged (Waiting→Running→Done)
  → 滚动撤回至"已完成"
  → 延迟决策：不立即撤回，等 LLM 回复
  ↓
Text { role: Agent }（LLM 回复到达）
  → Frontend 查 last_status_msg → Some(final_status_id)
  → 发 Recall{target=final_status_id}（撤回"已完成"）
  → 发 LLMReply 消息
  → on_sent → 标记 task_finalized
  ↓
TaskCleared
  → 清理 last_status_msg / task_finalized 中该 task 条目
```

### 滚动撤回的必要性

QQ 撤回有 2 分钟硬限制（API 返回错误码 306011）。若任务执行超过 2 分钟，第一条状态消息将无法撤回。滚动撤回保证每次只撤回最近一条消息，最大概率在时间窗口内。

## 架构设计

### 1. Channel trait 扩展

```rust
#[async_trait]
pub trait Channel: Send + Sync 'static {
    fn name(&self) -> &str;

    /// 发送消息，返回 message_id（如通道支持事后引用）。
    /// 不支持撤回/编辑的通道返回 None。
    async fn send(&self, message: &ChannelOutboundMessage) -> Result<Option<String>, ChannelError>;

    async fn listen(&self, tx: Sender<ChannelInboundMessage>) -> Result<(), ChannelError>;

    /// 撤回消息。不支持撤回的通道返回 ChannelError::NotSupported。
    async fn recall_message(&self, _recipient: &str, _msg_id: &str) -> Result<(), ChannelError> {
        Err(ChannelError::NotSupported)
    }

    /// 发送输入状态指示器。不支持的通道静默跳过（默认 Ok(())）。
    async fn send_typing(&self, _recipient: &str) -> Result<(), ChannelError> {
        Ok(())
    }

    async fn health_check(&self) -> bool { true }
    fn supported_attachment_kinds(&self) -> Vec<AttachmentKind> { vec![] }
    fn supports_html(&self) -> bool { false }
    fn supports_inline_keyboard(&self) -> bool { false }
}
```

**关键点：**
- `send()` 签名从 `Result<(), ChannelError>` 改为 `Result<Option<String>, ChannelError>`，返回 `Option<message_id>`
- `recall_message` 默认 `NotSupported`（显式能力），`send_typing` 默认 `Ok(())`（尽力而为）——语义差异：撤回是显式能力，typing 是尽力而为
- 现有调用方 `let _ = channel.send(...).await?` 仍然有效（Ok 值自动丢弃）

### 2. MessageKind 枚举

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageKind {
    /// LLM 自然语言回复（UserOutputMessage role=Agent）
    LLMReply,
    /// 任务状态变更通知（如"运行中 → 等待中"）
    TaskStatus,
    /// 工具权限审批请求（带 InlineKeyboard）
    ApprovalRequest,
    /// 系统通知（SystemOutputMessage，如摘要完成、任务失败）
    System,
    /// 撤回目标消息。content 字段为目标 message_id。
    Recall,
    /// 其他用户可见文本（未分类）
    Other,
}
```

`ChannelOutboundMessage` 新增 `message_kind: MessageKind` 字段。不提供 `Default` 实现，强制调用方思考消息类型。

**EngineEvent → MessageKind 映射：**

| EngineEvent variant | MessageKind |
|---|---|
| `Text { role: Agent }` | `LLMReply` |
| `Text { role: System }` | `System` |
| `Text { role: User }` | `Other`（理论不会发生） |
| `ApprovalRequest` | `ApprovalRequest` |
| `TaskStatusChanged` | `TaskStatus` |

`channel_send` 工具构造的消息标记为 `LLMReply`（Agent 主动发起的回复）。

### 3. ChannelError 新增 NotSupported

```rust
#[derive(Debug, thiserror::Error)]
pub enum ChannelError {
    // ... 现有 variants ...
    #[error("channel does not support this operation")]
    NotSupported,
}
```

### 4. ChannelFrontend 有状态化

```rust
pub struct ChannelFrontend {
    kind: FrontendKind,
    channel_name: String,
    outbound_tx: UnboundedSender<OutboundEntry>,
    /// Per-task + per-recipient 的状态消息追踪。
    /// key = (task_id, recipient)，value = 最近一条状态消息的 msg_id。
    last_status_msg: Arc<RwLock<HashMap<(String, String), String>>>,
    /// Per-task 的最终态决策缓存。
    task_finalized: Arc<RwLock<HashSet<String>>>,
}

struct OutboundEntry {
    channel_name: String,
    message: ChannelOutboundMessage,
    /// 发送完成后的回调，传入通道返回的 message_id。
    on_sent: Option<Box<dyn FnOnce(Option<String>) + Send + Sync>>,
}
```

**为何用回调模式：** ChannelFrontend 通过 `outbound_tx` 队列与 ChannelManager supervisor 解耦，无法直接拿到 `send()` 返回的 message_id。`on_sent` 回调让 supervisor 在 `send()` 完成后回流 message_id，ChannelFrontend 用它更新 `last_status_msg` 或触发后续撤回。

**为何 Recall 也走队列：** 保持 ChannelFrontend 与通道解耦；RecallMessage 顺序自然进入出向队列，保证撤回在新消息发送前完成（ChannelManager supervisor 单线程顺序消费 `mpsc::UnboundedReceiver`）。

### 5. 通道 send 中处理 MessageKind::Recall

```rust
// QqChannel::send
match message.message_kind {
    MessageKind::Recall => {
        if let Err(e) = self.recall_message(&message.recipient, &message.content).await {
            warn!(event = "ChannelRecallFailed", error = %e, ...);
        }
        Ok(None)
    }
    _ => {
        let resp = self.send_text_markdown(...).await?;
        Ok(Some(resp.id))
    }
}
```

`content` 字段在 `MessageKind::Recall` 时承载目标 msg_id。

### 6. 通道实现

**QQ 通道：**
- 移除 `recall_message` / `send_typing` 的 `#[allow(dead_code)]`
- `send()` 返回 `Some(resp.id)`（从 `QqMessageResponse` 提取）
- `listen()` 中入向 ACK 改为 `send_typing()`（移除 `send_ack_text` 调用；该方法仅入向 ACK 使用，移除调用后成为死代码，应一并删除方法定义与相关测试）
- `handle_interaction_create` 中撤回审批请求消息
  - **msg_id 来源**：审批请求消息通过 `ChannelFrontend::push_event` → `outbound_tx` 队列发送，`on_sent` 回调将 msg_id 回流到 Frontend。但 `handle_interaction_create` 在通道 `listen` 内部，无法访问 Frontend 状态。
  - **解决方案**：通道维护自己的 `pending_approval_msg_ids: Arc<RwLock<HashMap<approval_id, msg_id>>>`。当 `send()` 处理 `MessageKind::ApprovalRequest` 时，从 `ChannelOutboundMessage` 中提取 approval 标识（如 `reply_markup` 中的 callback_data 或 content 中的 request_id），与返回的 msg_id 一起存入 map。`handle_interaction_create` 收到按钮点击时，从 map 中取出 msg_id 调 `recall_message`。
  - **替代方案**：在 `ChannelOutboundMessage` 新增 `approval_id: Option<Uuid>` 字段，通道据此索引。但会污染通用结构，**不采用**。
  - **简化方案**：审批请求消息的 `content` 中已包含 request_id（如 `🔒 需要你的确认 [request_id: xxx]`），通道可解析 content 提取。但脆弱，**不采用**。
  - **最终选择**：通道维护 `pending_approval_msg_ids` map，key 为 `reply_markup` 中按钮的 callback_data（已含 approval_id 编码），value 为 msg_id。

**Telegram 通道：**
- 新增 `recall_message`（调 `deleteMessage` API）
- 新增 `send_typing`（调 `sendChatAction` with `action=typing`）
- `send()` 返回 `Some(message_id)`（从 Telegram API 响应解析 `message_id` 字段）
- 处理 `MessageKind::Recall` 分支

## 并发安全

- `last_status_msg` / `task_finalized` 用 `Arc<RwLock<...>>`
- 同一 task 的状态变更事件由 ECS 单线程驱动，顺序到达，不会并发修改同一 task 的 `last_status_msg`
- 不同 task 之间可能并发，`RwLock` 足够
- `on_sent` 回调在 ChannelManager supervisor 单线程中顺序执行，不会出现"新消息已发但旧消息 on_sent 未执行"的竞态

## 错误处理与降级

**核心原则：治理策略失败不应阻塞主流程。**

| 场景 | 处理 |
|---|---|
| `recall_message` 失败（超 2 分钟、网络错误、权限不足） | `warn!` 日志，跳过；旧消息保留（降级体验，不阻塞） |
| `send_typing` 失败（网络错误、群聊 NotSupported） | `debug!` 日志，跳过 |
| `send()` 返回 `None` | `last_status_msg` 不更新；后续无撤回目标，跳过撤回 |
| `recall_message` 返回 `NotSupported` | 同 `send()` 返回 `None`，跳过撤回 |

**撤回失败不重试。** 理由：QQ 撤回有 2 分钟硬限制，重试大概率仍失败；滚动撤回的下一次循环会尝试撤回新消息；重试增加复杂度且收益有限。

## 测试策略

### 单元测试（`#[cfg(test)]`，与实现同文件）

**ChannelFrontend 治理逻辑：**
- `task_status_rolling_recall`：发新状态消息前撤回上一条
- `llm_reply_recalls_last_status`：LLM 回复到达时撤回最终态
- `task_done_without_llm_reply_preserves_final_status`：Done 后无 LLM 回复，最终态保留
- `task_failed_preserves_final_status`：Failed 时保留最终态
- `task_cleared_cleans_up_state`：TaskCleared 清理状态
- `no_recall_when_no_previous_status`：首次状态消息无撤回
- `recall_failure_does_not_block`：撤回失败后仍发新消息

**MessageKind 映射：**
- `engine_event_text_agent_maps_to_llm_reply`
- `engine_event_text_system_maps_to_system`
- `engine_event_approval_maps_to_approval_request`
- `engine_event_task_status_maps_to_task_status`

### 通道层测试（wiremock）

**QQ：**
- `send_returns_message_id`：send() 返回 Some(msg_id)
- `send_with_recall_kind_calls_recall_message`：MessageKind::Recall 时调 recall API
- `send_typing_on_inbound_message`：listen 收到入向消息时调 send_typing（C2C）
- `approval_button_click_recalls_approval_request`：handle_interaction_create 撤回审批请求

**Telegram：**
- `send_returns_message_id`：send() 返回 Some(msg_id)
- `recall_message_calls_delete_message`
- `send_typing_calls_send_chat_action`
- `send_with_recall_kind_calls_delete_message`

### 集成测试

**新增 `tests/qq_channel_recall_flow.rs`：**
- 端到端：Task 状态变更 → 滚动撤回 → LLM 回复到达 → 撤回最终态
- 用 wiremock 模拟 QQ API，验证 DELETE /messages/{id} 调用次数和顺序

## 变更文件清单

| 文件 | 变更类型 | 说明 |
|---|---|---|
| `src/channels/traits.rs` | 修改 | Channel trait 新增 `recall_message`/`send_typing`；`send()` 返回 `Option<String>`；新增 `MessageKind`；`ChannelOutboundMessage` 新增 `message_kind`；`ChannelError` 新增 `NotSupported` |
| `src/channels/frontend.rs` | 修改 | ChannelFrontend 有状态化；`push_event` 实现滚动撤回；`OutboundEntry` 回调模式；新增测试模块 |
| `src/channels/manager.rs` | 修改 | `outbound_tx` 改为 `OutboundEntry`；supervisor 执行 `on_sent` 回调 |
| `src/channels/qq.rs` | 修改 | 移除 dead_code；`send()` 返回 msg_id；处理 Recall；listen ACK 改 typing；handle_interaction_create 撤回审批请求 |
| `src/channels/telegram.rs` | 修改 | 新增 recall_message/send_typing；`send()` 返回 msg_id；处理 Recall |
| `src/channels/send_tool.rs` | 修改 | 构造消息补充 `message_kind: MessageKind::LLMReply` |
| `src/systems/tools/channel_send_dispatch.rs` | 修改 | 若构造 `ChannelOutboundMessage`，补充 `message_kind` |
| `docs/current-state.md` | 修改 | 更新能力状态 |
| `AGENTS.md` + `CLAUDE.md` | 修改 | 同步能力边界变化（如适用） |

## 实施顺序

1. Channel trait + MessageKind + ChannelError::NotSupported（基础设施）
2. QQ/Telegram 通道 send 返回 message_id + recall/typing 实现
3. ChannelFrontend 有状态化 + OutboundEntry 回调模式
4. ChannelManager supervisor 适配 OutboundEntry
5. QQ listen ACK 替换为 typing + 审批点击撤回
6. channel_send 工具 + 其他构造点补充 message_kind
7. 集成测试
8. 文档同步 + 全量 CI
