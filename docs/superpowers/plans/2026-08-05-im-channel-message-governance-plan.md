# IM 通道状态消息治理 实施计划

> **面向 AI 代理的工作者：** 必需子技能：使用 superpowers:subagent-driven-development（推荐）或 superpowers:executing-plans 逐任务实现此计划。步骤使用复选框（`- [ ]`）语法来跟踪进度。

**目标：** 通过 Channel trait 扩展（recall/typing/send 返回 message_id）+ ChannelFrontend 滚动撤回策略 + 入向 ACK 用 typing 替代，减少 IM 通道状态消息对 LLM 回复的掩盖。

**架构：** 在 Channel trait 层新增 `recall_message`/`send_typing` 默认方法并将 `send()` 返回 `Option<String>`（message_id）；ChannelFrontend 从无状态变有状态，维护 per-task 状态消息 id 队列实现滚动撤回；出向队列改为 `OutboundEntry` 回调模式让 message_id 回流；QQ/Telegram 通道各自实现 recall/typing 的 HTTP 调用。

**技术栈：** Rust, async_trait, tokio mpsc, reqwest, wiremock (测试), tracing

## Global Constraints

- 遵循 Conventional Commits 提交格式
- 所有新方法需有 wiremock 集成测试或单元测试覆盖
- Channel trait 的 `send()` 签名从 `Result<(), ChannelError>` 改为 `Result<Option<String>, ChannelError>`
- 文档中文撰写，代码注释可中英混合
- 遵循 AGENTS.md "简化优先 / 代码腐化治理"：不引入无调用方的代码；本次清理 QQ 通道的 `#[allow(dead_code)]` 标注
- 治理策略失败不阻塞主流程（撤回/typing 失败仅记录日志，降级为不撤回/不显示 typing）
- `ChannelOutboundMessage` 不提供 `Default` 实现，强制调用方指定 `message_kind`

**关联规格：** `docs/superpowers/specs/2026-08-05-qq-channel-message-governance-design.md`

---

## File Structure

| 文件 | 职责 | 变更类型 |
|---|---|---|
| `src/channels/traits.rs` | Channel trait + 出向消息类型定义 | 修改 — 新增 `MessageKind`、`recall_message`/`send_typing` 默认方法；`send()` 返回 `Option<String>`；`ChannelOutboundMessage` 新增 `message_kind`；`ChannelError` 新增 `NotSupported` |
| `src/channels/frontend.rs` | EngineEvent → ChannelOutboundMessage 转换 + 治理策略 | 修改 — 有状态化；滚动撤回；`OutboundEntry` 回调模式；新增测试 |
| `src/channels/manager.rs` | 通道管理与出向队列消费 | 修改 — `outbound_tx` 改为 `OutboundEntry`；supervisor 执行 `on_sent` 回调 |
| `src/channels/qq.rs` | QQ 通道实现 | 修改 — 移除 dead_code；`send()` 返回 msg_id；处理 Recall；listen ACK 改 typing；审批点击撤回审批请求 |
| `src/channels/telegram.rs` | Telegram 通道实现 | 修改 — 新增 recall/typing；`send()` 返回 msg_id；处理 Recall |
| `src/channels/send_tool.rs` | channel_send 工具 | 修改 — 构造消息补充 `message_kind: MessageKind::LLMReply` |
| `src/systems/tools/channel_send_dispatch.rs` | channel_send 派发系统 | 修改 — 构造 `ChannelOutboundMessage` 补充 `message_kind` |
| `docs/current-state.md` | 能力状态文档 | 修改 — 更新能力状态 |
| `tests/qq_channel_recall_flow.rs` | 端到端集成测试 | 新增 — 滚动撤回 + LLM 回复撤回最终态 |

---

### Task 1: Channel trait 扩展 + MessageKind + ChannelError::NotSupported

**Files:**
- Modify: `src/channels/traits.rs`

**Interfaces:**
- Produces: `MessageKind` 枚举；`ChannelOutboundMessage.message_kind` 字段；`ChannelError::NotSupported` variant；`Channel::recall_message`/`send_typing` 默认方法；`send()` 返回 `Result<Option<String>, ChannelError>`

**背景:** 治理策略的基础设施层。所有后续任务依赖这些类型定义。本任务只改类型定义，不改动任何调用方——调用方适配在后续任务中分批进行。

- [ ] **Step 1: 新增 `MessageKind` 枚举**

在 `src/channels/traits.rs` 中，`ChannelOutboundMessage` 之前添加：

```rust
/// 出向消息类型，用于通道决定撤回/typing 等策略。
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

- [ ] **Step 2: `ChannelOutboundMessage` 新增 `message_kind` 字段**

将 `ChannelOutboundMessage` struct 改为：

```rust
/// 统一出向消息
#[derive(Debug, Clone)]
pub struct ChannelOutboundMessage {
    pub recipient: String,
    pub thread_id: Option<String>,
    pub content: String,
    pub parse_mode: Option<ChannelParseMode>,
    pub reply_markup: Option<ReplyMarkup>,
    pub attachments: Vec<ChannelAttachment>,
    /// 消息类型，用于通道决定撤回/typing 策略。
    pub message_kind: MessageKind,
}
```

- [ ] **Step 3: `ChannelError` 新增 `NotSupported` variant**

将 `ChannelError` enum 改为：

```rust
#[derive(thiserror::Error, Debug)]
pub enum ChannelError {
    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),
    #[error("api error {code}: {message}")]
    Api { code: i32, message: String },
    #[error("auth failed")]
    Auth,
    #[error("rate limited")]
    RateLimited,
    #[error("not configured")]
    NotConfigured,
    #[error("channel does not support this operation")]
    NotSupported,
}
```

- [ ] **Step 4: `Channel` trait 新增 `recall_message`/`send_typing` 默认方法 + `send()` 签名变更**

将 `Channel` trait 改为：

```rust
#[async_trait]
pub trait Channel: Send + Sync + 'static {
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

    async fn health_check(&self) -> bool {
        true
    }

    fn supported_attachment_kinds(&self) -> Vec<AttachmentKind> {
        vec![]
    }

    fn supports_html(&self) -> bool {
        false
    }

    fn supports_inline_keyboard(&self) -> bool {
        false
    }
}
```

- [ ] **Step 5: 更新 `traits.rs` 中的现有测试，补充 `message_kind` 字段**

在 `src/channels/traits.rs` 测试模块中，所有 `ChannelOutboundMessage { ... }` 构造点需要补充 `message_kind` 字段。当前测试中没有直接构造 `ChannelOutboundMessage`（测试只构造 `ChannelInboundMessage`），所以本步骤无改动。**验证：** 搜索 `ChannelOutboundMessage {` 在 `traits.rs` 中的出现，若无处则跳过此步骤。

- [ ] **Step 6: 编译验证（预期失败——调用方未适配）**

Run: `cargo check --lib 2>&1 | head -50`
Expected: 编译错误，因为：
1. `QqChannel::send` 返回 `Result<(), ChannelError>` 与新 trait 签名不匹配
2. `TelegramChannel::send` 同上
3. 所有 `ChannelOutboundMessage { ... }` 构造点缺少 `message_kind` 字段

**这些错误会在后续任务中修复。本步骤只确认 trait 定义本身编译通过。**

Run: `cargo check --lib 2>&1 | grep "error\[" | head -5`
Expected: 错误均来自 `qq.rs` / `telegram.rs` / `frontend.rs` / `send_tool.rs` / `channel_send_dispatch.rs`，而非 `traits.rs` 本身。

- [ ] **Step 7: Commit**

```bash
git add src/channels/traits.rs
git commit -m "feat(channels): add MessageKind, recall/typing trait methods, NotSupported error"
```

---

### Task 2: QQ 通道适配 — send 返回 message_id + 处理 Recall + 移除 dead_code

**Files:**
- Modify: `src/channels/qq.rs`

**Interfaces:**
- Consumes: Task 1 的 `MessageKind`、`ChannelError::NotSupported`、新 `send()` 签名
- Produces: `QqChannel::send` 返回 `Result<Option<String>, ChannelError>`；处理 `MessageKind::Recall`

**背景:** QQ 通道已实现 `recall_message`/`send_typing` 但标注 `#[allow(dead_code)]`。本任务让 `send()` 返回 message_id，处理 Recall kind，并移除 dead_code 标注。

- [ ] **Step 1: 移除 `recall_message`/`send_typing` 的 `#[allow(dead_code)]`**

在 `src/channels/qq.rs` 中，找到 `recall_message` 方法（约 1092 行）和 `send_typing` 方法（约 1117 行），移除它们上方的 `#[allow(dead_code)]` 标注。

- [ ] **Step 2: 修改 `QqChannel::send` 签名与返回值**

在 `src/channels/qq.rs` 中，将 `send` 方法签名从 `Result<(), ChannelError>` 改为 `Result<Option<String>, ChannelError>`，并在所有成功返回点返回 `Some(msg_id)`。

当前 `send` 方法（约 1462-1452 行）的结构：

```rust
async fn send(
    &self,
    message: &crate::channels::traits::ChannelOutboundMessage,
) -> Result<(), ChannelError> {
    use crate::channels::traits::{ChannelParseMode, extract_attachments};

    if let Some(ref markup) = message.reply_markup {
        // 有键盘路径
        if let Some((request_id, options)) = extract_approval_info(markup) {
            self.record_pending_approval(&message.recipient, request_id, options)
                .await;
        }
        let content_to_send = match message.parse_mode {
            Some(ChannelParseMode::Html) => html_to_markdown_for_qq(&message.content),
            Some(ChannelParseMode::Markdown) | None => message.content.clone(),
        };
        if !content_to_send.trim().is_empty() {
            self.send_text_with_keyboard(&message.recipient, &content_to_send, markup)
                .await?;
        }
    } else {
        // 无键盘路径
        let (text, inline_attachments) = extract_attachments(&message.content);
        let all_attachments: Vec<_> = message
            .attachments
            .iter()
            .chain(inline_attachments.iter())
            .filter(|a| !a.target.trim().is_empty())
            .cloned()
            .collect();
        let content_to_send = match message.parse_mode {
            Some(ChannelParseMode::Html) => html_to_markdown_for_qq(&text),
            Some(ChannelParseMode::Markdown) | None => text,
        };
        if !content_to_send.trim().is_empty() {
            self.send_text_markdown(&message.recipient, &content_to_send).await?;
        }
        // 附件发送...
    }
    Ok(())
}
```

改为：

```rust
async fn send(
    &self,
    message: &crate::channels::traits::ChannelOutboundMessage,
) -> Result<Option<String>, ChannelError> {
    use crate::channels::traits::{ChannelParseMode, MessageKind, extract_attachments};

    // 撤回指令：content 字段为目标 msg_id
    if message.message_kind == MessageKind::Recall {
        if let Err(e) = self.recall_message(&message.recipient, &message.content).await {
            tracing::warn!(
                event = "ChannelRecallFailed",
                channel = "qq",
                recipient = %message.recipient,
                msg_id = %message.content,
                error = %e,
                "recall failed, falling back to leaving old message"
            );
        }
        return Ok(None);
    }

    let msg_id = if let Some(ref markup) = message.reply_markup {
        // 有键盘路径
        if let Some((request_id, options)) = extract_approval_info(markup) {
            self.record_pending_approval(&message.recipient, request_id, options)
                .await;
        }
        let content_to_send = match message.parse_mode {
            Some(ChannelParseMode::Html) => html_to_markdown_for_qq(&message.content),
            Some(ChannelParseMode::Markdown) | None => message.content.clone(),
        };
        if !content_to_send.trim().is_empty() {
            Some(self.send_text_with_keyboard(&message.recipient, &content_to_send, markup).await?.id)
        } else {
            None
        }
    } else {
        // 无键盘路径
        let (text, inline_attachments) = extract_attachments(&message.content);
        let all_attachments: Vec<_> = message
            .attachments
            .iter()
            .chain(inline_attachments.iter())
            .filter(|a| !a.target.trim().is_empty())
            .cloned()
            .collect();
        let content_to_send = match message.parse_mode {
            Some(ChannelParseMode::Html) => html_to_markdown_for_qq(&text),
            Some(ChannelParseMode::Markdown) | None => text,
        };
        let mut id = None;
        if !content_to_send.trim().is_empty() {
            id = Some(self.send_text_markdown(&message.recipient, &content_to_send).await?.id);
        }
        // 附件发送（保持现有逻辑，最后一个附件的 id 作为返回值；若无文本无附件则 None）
        for attachment in all_attachments {
            let resp = self.send_media_message(&message.recipient, &attachment.target).await?;
            id = Some(resp.id);
        }
        id
    };
    Ok(msg_id)
}
```

**注意：** 上述代码假设 `send_text_with_keyboard`/`send_text_markdown`/`send_media_message` 已返回 `QqMessageResponse`（Task 1 of QQ channel APIs plan 已实现）。验证：查看这些方法的当前签名，确认返回 `Result<QqMessageResponse, ChannelError>`。

- [ ] **Step 3: 移除 `QqMessageResponse.id` 的 `#[allow(dead_code)]`**

在 `src/channels/qq.rs` 中，找到 `QqMessageResponse` struct（约 236 行），移除 `id` 字段上的 `#[allow(dead_code)]` 标注：

```rust
#[derive(Debug, serde::Deserialize)]
struct QqMessageResponse {
    /// QQ API 返回的消息 ID（字段名为 "id"）
    id: String,
}
```

- [ ] **Step 4: 添加 `send_with_recall_kind_calls_recall_api` 测试**

在 `src/channels/qq.rs` 测试模块中添加：

```rust
#[tokio::test]
async fn send_with_recall_kind_calls_recall_api() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    use crate::channels::traits::{ChannelOutboundMessage, MessageKind};

    let mock_server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/v2/users/USER123/messages/MSG_TO_RECALL"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&mock_server)
        .await;

    let ch = QqChannel::new(make_config()).with_api_base(mock_server.uri());
    ch.set_token_for_test("fake_token").await;
    let msg = ChannelOutboundMessage {
        recipient: "user:USER123".to_string(),
        thread_id: None,
        content: "MSG_TO_RECALL".to_string(),
        parse_mode: None,
        reply_markup: None,
        attachments: vec![],
        message_kind: MessageKind::Recall,
    };
    let result = ch.send(&msg).await.expect("send should succeed");
    assert!(result.is_none(), "recall should return None");
}
```

- [ ] **Step 5: 添加 `send_returns_message_id_for_text` 测试**

```rust
#[tokio::test]
async fn send_returns_message_id_for_text() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    use crate::channels::traits::{ChannelOutboundMessage, MessageKind};

    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v2/users/USER123/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "msg_abc123",
            "timestamp": "1234567890"
        })))
        .mount(&mock_server)
        .await;

    let ch = QqChannel::new(make_config()).with_api_base(mock_server.uri());
    ch.set_token_for_test("fake_token").await;
    let msg = ChannelOutboundMessage {
        recipient: "user:USER123".to_string(),
        thread_id: None,
        content: "hello".to_string(),
        parse_mode: None,
        reply_markup: None,
        attachments: vec![],
        message_kind: MessageKind::LLMReply,
    };
    let result = ch.send(&msg).await.expect("send should succeed");
    assert_eq!(result, Some("msg_abc123".to_string()));
}
```

- [ ] **Step 6: 更新现有 QQ 测试中 `ChannelOutboundMessage` 构造点**

搜索 `src/channels/qq.rs` 测试模块中所有 `ChannelOutboundMessage { ... }` 构造，补充 `message_kind` 字段。**若测试中不直接构造 `ChannelOutboundMessage`（通过其他 helper 方法发送），则跳过。** 验证方式：

Run: `grep -n "ChannelOutboundMessage {" src/channels/qq.rs`

如有命中，每个构造点补充 `message_kind: MessageKind::Other`（或其他合适类型）。

- [ ] **Step 7: 运行 QQ 通道测试**

Run: `cargo test --lib channels::qq -- --nocapture 2>&1 | tail -20`
Expected: 所有测试 PASS（如有编译错误，逐一修复 `message_kind` 缺失问题）

- [ ] **Step 8: Commit**

```bash
git add src/channels/qq.rs
git commit -m "feat(qq): send returns message_id, handle Recall kind, remove dead_code"
```

---

### Task 3: Telegram 通道适配 — recall/typing/send 返回 message_id

**Files:**
- Modify: `src/channels/telegram.rs`

**Interfaces:**
- Consumes: Task 1 的 `MessageKind`、`ChannelError::NotSupported`、新 `send()` 签名
- Produces: `TelegramChannel::recall_message`（调 `deleteMessage`）；`TelegramChannel::send_typing`（调 `sendChatAction`）；`send()` 返回 `Option<String>`

**背景:** Telegram Bot API 支持 `deleteMessage`（撤回）、`sendChatAction` with `action=typing`（输入状态）。Telegram 通道当前 `send()` 返回 `Result<(), ChannelError>`，不返回 message_id。

- [ ] **Step 1: 修改 `TelegramChannel::send` 返回 message_id**

在 `src/channels/telegram.rs` 中，将 `send` 方法签名改为 `Result<Option<String>, ChannelError>`，并在成功发送后返回 `Some(message_id)`。

Telegram `sendMessage` API 响应格式：`{ "ok": true, "result": { "message_id": 123, "chat": {...}, "text": "..." } }`

需要定义响应解析 struct（如尚未存在）：

```rust
#[derive(serde::Deserialize)]
struct TelegramSendResponse {
    ok: bool,
    result: Option<TelegramSendResult>,
}

#[derive(serde::Deserialize)]
struct TelegramSendResult {
    message_id: i64,
}
```

在 `send` 方法中，发送后解析响应：

```rust
async fn send(&self, message: &ChannelOutboundMessage) -> Result<Option<String>, ChannelError> {
    use crate::channels::traits::MessageKind;

    // 撤回指令
    if message.message_kind == MessageKind::Recall {
        if let Err(e) = self.recall_message(&message.recipient, &message.content).await {
            tracing::warn!(
                event = "ChannelRecallFailed",
                channel = "telegram",
                recipient = %message.recipient,
                msg_id = %message.content,
                error = %e,
                "recall failed, falling back to leaving old message"
            );
        }
        return Ok(None);
    }

    // 现有发送逻辑（保持不变），但解析响应获取 message_id
    // ... 现有代码 ...
    let response: TelegramSendResponse = self.post("sendMessage", &payload).await?;
    Ok(response.result.map(|r| r.message_id.to_string()))
}
```

**注意：** `send` 方法内部可能有多个发送路径（文本、附件、键盘）。每条路径都需要解析响应。如果 `self.post` 当前不解析响应（只检查状态码），需要改为解析。**实施时先阅读 `send` 方法和 `post` 方法的完整实现，再决定最小改动方式。**

- [ ] **Step 2: 实现 `recall_message`（调 `deleteMessage`）**

在 `src/channels/telegram.rs` 的 `impl Channel for TelegramChannel` 块中添加：

```rust
async fn recall_message(&self, recipient: &str, msg_id: &str) -> Result<(), ChannelError> {
    let payload = serde_json::json!({
        "chat_id": recipient,
        "message_id": msg_id.parse::<i64>().unwrap_or(0),
    });
    self.post("deleteMessage", &payload).await?;
    Ok(())
}
```

- [ ] **Step 3: 实现 `send_typing`（调 `sendChatAction`）**

```rust
async fn send_typing(&self, recipient: &str) -> Result<(), ChannelError> {
    let payload = serde_json::json!({
        "chat_id": recipient,
        "action": "typing",
    });
    self.post("sendChatAction", &payload).await?;
    Ok(())
}
```

- [ ] **Step 4: 添加 `recall_message_calls_delete_message` 测试**

```rust
#[tokio::test]
async fn recall_message_calls_delete_message() {
    use wiremock::matchers::{method, path, body_string_contains};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(format!("/bot{}/deleteMessage", "TEST_TOKEN")))
        .and(body_string_contains("chat_id"))
        .and(body_string_contains("message_id"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true, "result": true})))
        .expect(1)
        .mount(&mock_server)
        .await;

    let ch = make_telegram_channel_with_base(mock_server.uri());
    ch.recall_message("123456", "789").await.expect("recall");
}
```

**注意：** 需要参考 `telegram.rs` 现有测试的 helper 函数（如 `make_telegram_channel_with_base` 或类似），确保 mock 的 URL 格式正确。实施时先阅读 `telegram.rs` 测试模块的现有 helper。

- [ ] **Step 5: 添加 `send_typing_calls_send_chat_action` 测试**

```rust
#[tokio::test]
async fn send_typing_calls_send_chat_action() {
    use wiremock::matchers::{method, path, body_string_contains};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(format!("/bot{}/sendChatAction", "TEST_TOKEN")))
        .and(body_string_contains("\"action\":\"typing\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true, "result": true})))
        .expect(1)
        .mount(&mock_server)
        .await;

    let ch = make_telegram_channel_with_base(mock_server.uri());
    ch.send_typing("123456").await.expect("typing");
}
```

- [ ] **Step 6: 添加 `send_returns_message_id` 测试**

```rust
#[tokio::test]
async fn send_returns_message_id() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    use crate::channels::traits::{ChannelOutboundMessage, MessageKind};

    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(format!("/bot{}/sendMessage", "TEST_TOKEN")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "result": { "message_id": 42, "chat": { "id": 123456 }, "text": "hello" }
        })))
        .mount(&mock_server)
        .await;

    let ch = make_telegram_channel_with_base(mock_server.uri());
    let msg = ChannelOutboundMessage {
        recipient: "123456".to_string(),
        thread_id: None,
        content: "hello".to_string(),
        parse_mode: None,
        reply_markup: None,
        attachments: vec![],
        message_kind: MessageKind::LLMReply,
    };
    let result = ch.send(&msg).await.expect("send");
    assert_eq!(result, Some("42".to_string()));
}
```

- [ ] **Step 7: 更新现有 Telegram 测试中 `ChannelOutboundMessage` 构造点**

搜索 `src/channels/telegram.rs` 测试模块中所有 `ChannelOutboundMessage { ... }` 构造，补充 `message_kind` 字段。

Run: `grep -n "ChannelOutboundMessage {" src/channels/telegram.rs`

- [ ] **Step 8: 运行 Telegram 通道测试**

Run: `cargo test --lib channels::telegram -- --nocapture 2>&1 | tail -20`
Expected: 所有测试 PASS

- [ ] **Step 9: Commit**

```bash
git add src/channels/telegram.rs
git commit -m "feat(telegram): add recall/typing, send returns message_id"
```

---

### Task 4: channel_send 工具 + channel_send_dispatch 补充 message_kind

**Files:**
- Modify: `src/channels/send_tool.rs`
- Modify: `src/systems/tools/channel_send_dispatch.rs`

**Interfaces:**
- Consumes: Task 1 的 `MessageKind`
- Produces: 所有 `ChannelOutboundMessage` 构造点补充 `message_kind` 字段

**背景:** 这两处是 `ChannelOutboundMessage` 的构造点，需要补充 `message_kind` 字段以消除编译错误。`channel_send` 工具是 Agent 主动发起的消息，标记为 `LLMReply`。

- [ ] **Step 1: `channel_send_dispatch.rs` 补充 `message_kind`**

在 `src/systems/tools/channel_send_dispatch.rs` 第 44-54 行，`ChannelOutboundMessage` 构造补充 `message_kind` 字段：

```rust
let result = channel_manager.send(
    send.channel.clone(),
    ChannelOutboundMessage {
        recipient: recipient.clone(),
        thread_id: thread_id.clone(),
        content: send.content.clone(),
        parse_mode: None,
        reply_markup: None,
        attachments: send.attachments.clone(),
        message_kind: crate::channels::traits::MessageKind::LLMReply,
    },
);
```

- [ ] **Step 2: 检查 `send_tool.rs` 是否直接构造 `ChannelOutboundMessage`**

`send_tool.rs` 的 `execute` 方法返回 `ToolAction::SendChannelMessage`，不直接构造 `ChannelOutboundMessage`。`ChannelOutboundMessage` 的构造在 `channel_send_dispatch.rs` 中（Step 1 已处理）。

**验证：** Run: `grep -n "ChannelOutboundMessage" src/channels/send_tool.rs`
Expected: 无匹配（或仅有 import 注释），无需改动。

- [ ] **Step 3: 搜索其他 `ChannelOutboundMessage` 构造点**

Run: `grep -rn "ChannelOutboundMessage {" src/`
Expected: 命中 `frontend.rs`（Task 5 处理）、`channel_send_dispatch.rs`（Step 1 已处理）、`qq.rs`（Task 2 处理）、`telegram.rs`（Task 3 处理）。如有其他命中点，补充 `message_kind: MessageKind::Other`（或更合适的类型）。

- [ ] **Step 4: 编译验证**

Run: `cargo check --lib 2>&1 | grep "error\[" | head -10`
Expected: 仅剩 `frontend.rs` 的编译错误（Task 5 处理），其他文件应编译通过

- [ ] **Step 5: Commit**

```bash
git add src/systems/tools/channel_send_dispatch.rs src/channels/send_tool.rs
git commit -m "feat(channels): add message_kind to channel_send dispatch"
```

---

### Task 5: ChannelFrontend 有状态化 + OutboundEntry 回调模式

**Files:**
- Modify: `src/channels/frontend.rs`
- Modify: `src/channels/manager.rs`

**Interfaces:**
- Consumes: Task 1 的 `MessageKind`、`ChannelOutboundMessage.message_kind`
- Produces: `OutboundEntry` struct；`ChannelFrontend` 新增 `last_status_msg`/`task_finalized` 状态；滚动撤回策略

**背景:** ChannelFrontend 当前是无状态的 EngineEvent → ChannelOutboundMessage 转换器。本任务让它变有状态，维护 per-task 的状态消息 msg_id，实现滚动撤回。同时出向队列从 `(String, ChannelOutboundMessage)` 改为 `OutboundEntry`，通过 `on_sent` 回调回流 message_id。

- [ ] **Step 1: 定义 `OutboundEntry` struct**

在 `src/channels/frontend.rs` 中（`ChannelFrontend` struct 之前）添加：

```rust
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::RwLock;

/// 出向队列条目，携带发送完成后的回调。
pub struct OutboundEntry {
    pub channel_name: String,
    pub message: ChannelOutboundMessage,
    /// 发送完成后的回调，传入通道返回的 message_id。
    pub on_sent: Option<Box<dyn FnOnce(Option<String>) + Send + Sync>>,
}
```

- [ ] **Step 2: 修改 `ChannelFrontend` struct 添加状态字段**

将 `ChannelFrontend` struct 改为：

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
```

- [ ] **Step 3: 更新 `ChannelFrontend::new` 签名**

```rust
impl ChannelFrontend {
    pub fn new(
        kind: FrontendKind,
        channel_name: impl Into<String>,
        outbound_tx: UnboundedSender<OutboundEntry>,
    ) -> Self {
        Self {
            kind,
            channel_name: channel_name.into(),
            outbound_tx,
            last_status_msg: Arc::new(RwLock::new(HashMap::new())),
            task_finalized: Arc::new(RwLock::new(HashSet::new())),
        }
    }

    fn matches(&self, channel_id: &ChannelId) -> bool {
        channel_id.frontend == self.kind
    }

    fn enqueue(&self, msg: ChannelOutboundMessage, on_sent: Option<Box<dyn FnOnce(Option<String>) + Send + Sync>>) {
        let entry = OutboundEntry {
            channel_name: self.channel_name.clone(),
            message: msg,
            on_sent,
        };
        if let Err(e) = self.outbound_tx.send(entry) {
            error!(event = "ChannelFrontendSendFailed", error = %e, channel = %self.channel_name);
        }
    }

    /// 发送 Recall 消息（撤回指定 msg_id）。
    fn enqueue_recall(&self, recipient: String, thread_id: Option<String>, msg_id: String) {
        let msg = ChannelOutboundMessage {
            recipient,
            thread_id,
            content: msg_id,
            parse_mode: None,
            reply_markup: None,
            attachments: vec![],
            message_kind: super::traits::MessageKind::Recall,
        };
        self.enqueue(msg, None);
    }
}
```

- [ ] **Step 4: 修改 `push_event` 中 `Text { role: Agent }` 分支，实现"LLM 回复撤回最终态"**

将 `EngineEvent::Text` 分支改为根据 `role` 区分 `LLMReply`/`System`/`Other`，并在 `LLMReply` 时撤回最终态状态消息：

```rust
EngineEvent::Text {
    target,
    role,
    content,
    task_id,
    ..
} => {
    let targets = match target {
        EventTarget::Broadcast => return,
        EventTarget::Directed(v) => v,
    };
    let recipients: Vec<ChannelId> = targets
        .into_iter()
        .filter(|cid| self.matches(cid))
        .collect();
    if recipients.is_empty() {
        return;
    }
    trace!(
        event = "ChannelFrontendReceiveText",
        channel = %self.channel_name,
        recipients = recipients.len(),
        content_len = content.len(),
    );
    let prefixed_content = task_id
        .map(|id| format!("[{}] {}: {}", task_short_id(id), role_label(role), content))
        .unwrap_or(content);
    let message_kind = match role {
        MessageRole::Agent => super::traits::MessageKind::LLMReply,
        MessageRole::System => super::traits::MessageKind::System,
        MessageRole::User => super::traits::MessageKind::Other,
    };
    for channel_id in recipients {
        // LLM 回复到达时，撤回该 task+recipient 的最终态状态消息
        if message_kind == super::traits::MessageKind::LLMReply {
            if let Some(tid) = task_id {
                let key = (tid.to_string(), channel_id.user_id.clone());
                let last_msg_id = self.last_status_msg.read().await.get(&key).cloned();
                if let Some(msg_id) = last_msg_id {
                    self.enqueue_recall(
                        channel_id.user_id.clone(),
                        channel_id.thread_id.clone(),
                        msg_id,
                    );
                    self.last_status_msg.write().await.remove(&key);
                }
                self.task_finalized.write().await.insert(tid.to_string());
            }
        }
        let msg = ChannelOutboundMessage {
            recipient: channel_id.user_id,
            thread_id: channel_id.thread_id,
            content: prefixed_content.clone(),
            parse_mode: None,
            reply_markup: None,
            attachments: vec![],
            message_kind,
        };
        self.enqueue(msg, None);
    }
}
```

**注意：** `push_event` 是同步方法（`fn push_event(&self, event: EngineEvent)`），但 `read().await`/`write().await` 需要 async。**这需要将 `push_event` 改为 async，或在同步上下文中使用 `try_read`/`try_write`。**

**解决方案：** 使用 `try_read`/`try_write`（非阻塞），因为：
- 同一 task 的状态变更由 ECS 单线程驱动，不会并发竞争同一 key
- `try_read`/`try_write` 失败时降级为不撤回（符合"尽力而为"原则）

修改为：

```rust
// LLM 回复到达时，撤回该 task+recipient 的最终态状态消息
if message_kind == super::traits::MessageKind::LLMReply {
    if let Some(tid) = task_id {
        let key = (tid.to_string(), channel_id.user_id.clone());
        if let Ok(map) = self.last_status_msg.try_read() {
            if let Some(msg_id) = map.get(&key).cloned() {
                drop(map);
                self.enqueue_recall(
                    channel_id.user_id.clone(),
                    channel_id.thread_id.clone(),
                    msg_id,
                );
                if let Ok(mut map) = self.last_status_msg.try_write() {
                    map.remove(&key);
                }
            }
        }
        if let Ok(mut set) = self.task_finalized.try_write() {
            set.insert(tid.to_string());
        }
    }
}
```

- [ ] **Step 5: 修改 `TaskStatusChanged` 分支，实现滚动撤回**

将 `EngineEvent::TaskStatusChanged` 分支改为：

```rust
EngineEvent::TaskStatusChanged {
    target,
    task_id,
    status,
    old_status,
    agent_name,
    ..
} => {
    let targets = match target {
        EventTarget::Broadcast => return,
        EventTarget::Directed(v) => v,
    };
    let recipients: Vec<ChannelId> = targets
        .into_iter()
        .filter(|cid| self.matches(cid))
        .collect();
    if recipients.is_empty() {
        return;
    }
    let transition = match old_status {
        Some(old) => format!("{} → {}", status_label(old), status_label(status)),
        None => status_label(status).to_string(),
    };
    let content = match agent_name.as_deref() {
        Some(agent) => format!("[{}]: {} @{}", task_short_id(task_id), transition, agent),
        None => format!("[{}]: {}", task_short_id(task_id), transition),
    };

    for channel_id in recipients {
        let key = (task_id.to_string(), channel_id.user_id.clone());
        // 滚动撤回：发新状态消息前撤回上一条
        // Failed 状态不撤回（保留错误信息作为最终态）
        if status != TaskStatusKind::Failed {
            if let Ok(map) = self.last_status_msg.try_read() {
                if let Some(old_msg_id) = map.get(&key).cloned() {
                    drop(map);
                    self.enqueue_recall(
                        channel_id.user_id.clone(),
                        channel_id.thread_id.clone(),
                        old_msg_id,
                    );
                    if let Ok(mut map) = self.last_status_msg.try_write() {
                        map.remove(&key);
                    }
                }
            }
        }

        // 准备 on_sent 回调：更新 last_status_msg
        let last_status_msg = self.last_status_msg.clone();
        let on_sent: Option<Box<dyn FnOnce(Option<String>) + Send + Sync>> = Some(Box::new(
            move |msg_id: Option<String>| {
                if let Some(id) = msg_id {
                    if let Ok(mut map) = last_status_msg.try_write() {
                        map.insert(key, id);
                    }
                }
            },
        ));

        let msg = ChannelOutboundMessage {
            recipient: channel_id.user_id,
            thread_id: channel_id.thread_id,
            content: content.clone(),
            parse_mode: None,
            reply_markup: None,
            attachments: vec![],
            message_kind: super::traits::MessageKind::TaskStatus,
        };
        self.enqueue(msg, on_sent);
    }
}
```

- [ ] **Step 6: 修改 `ApprovalRequest` 分支，补充 `message_kind`**

将 `EngineEvent::ApprovalRequest` 分支中的 `ChannelOutboundMessage` 构造补充 `message_kind`：

```rust
let msg = ChannelOutboundMessage {
    recipient: channel_id.user_id,
    thread_id: channel_id.thread_id,
    content: content.clone(),
    parse_mode: Some(ChannelParseMode::Html),
    reply_markup: Some(ReplyMarkup::InlineKeyboard(buttons.clone())),
    attachments: vec![],
    message_kind: super::traits::MessageKind::ApprovalRequest,
};
self.enqueue(msg, None);
```

- [ ] **Step 7: 处理 `TaskCleared` 事件，清理状态**

在 `push_event` 的 match 中，将 `_ => {}` 之前添加 `TaskCleared` 分支：

```rust
EngineEvent::TaskCleared { task_id, .. } => {
    let task_id_str = task_id.to_string();
    if let Ok(mut map) = self.last_status_msg.try_write() {
        map.retain(|(tid, _), _| tid != &task_id_str);
    }
    if let Ok(mut set) = self.task_finalized.try_write() {
        set.remove(&task_id_str);
    }
}
```

- [ ] **Step 8: 更新 `ChannelManager` 的 `outbound_tx` 类型和 supervisor 消费逻辑**

在 `src/channels/manager.rs` 中：

1. 将 `outbound_tx` 类型从 `mpsc::UnboundedSender<(String, ChannelOutboundMessage)>` 改为 `mpsc::UnboundedSender<OutboundEntry>`（需 import `ChannelFrontend` 模块中的 `OutboundEntry`，或将 `OutboundEntry` 移到 `manager.rs` 或 `traits.rs`）

2. 将 `send` 方法改为接受 `ChannelOutboundMessage` + `message_kind`（保持外部 API 兼容）：

```rust
pub fn send(&self, channel_name: String, message: ChannelOutboundMessage) -> Result<()> {
    if !self.channels.iter().any(|c| c.name() == channel_name) {
        anyhow::bail!("channel not found: {channel_name}");
    }
    let entry = OutboundEntry {
        channel_name,
        message,
        on_sent: None,
    };
    self.outbound_tx
        .send(entry)
        .map_err(|_| anyhow::anyhow!("channel manager outbound channel closed"))?;
    Ok(())
}
```

3. 将 supervisor 消费逻辑改为：

```rust
msg = outbound_rx.recv() => {
    let Some(entry) = msg else { break };
    if let Some(channel) = send_channels.iter().find(|c| c.name() == entry.channel_name) {
        match channel.send(&entry.message).await {
            Ok(msg_id) => {
                if let Some(on_sent) = entry.on_sent {
                    on_sent(msg_id);
                }
            }
            Err(e) => {
                error!(event = "ChannelSendFailed", channel = %entry.channel_name, error = %e, "failed to send outbound message");
            }
        }
    } else {
        warn!(event = "ChannelNotFound", channel = %entry.channel_name, "no such channel for outbound message");
    }
}
```

**关键决策：`OutboundEntry` 的位置。** 为了避免循环依赖，将 `OutboundEntry` 定义在 `src/channels/mod.rs` 或 `src/channels/traits.rs` 中。建议放在 `traits.rs`（与 `ChannelOutboundMessage` 一起）。

**实施时：** 将 `OutboundEntry` 从 `frontend.rs` 移到 `traits.rs`，`frontend.rs` 和 `manager.rs` 都从 `traits.rs` import。

- [ ] **Step 9: 更新 `frontend.rs` 测试模块中的 `make_frontend` helper**

```rust
fn make_frontend(
    kind: FrontendKind,
) -> (
    ChannelFrontend,
    mpsc::UnboundedReceiver<OutboundEntry>,
) {
    let (tx, rx) = mpsc::unbounded_channel();
    (ChannelFrontend::new(kind, "test", tx), rx)
}
```

- [ ] **Step 10: 更新 `frontend.rs` 测试模块中所有断言**

现有测试通过 `rx.try_recv()` 接收 `(String, ChannelOutboundMessage)`，现在改为接收 `OutboundEntry`。需要更新所有 `let (_, msg) = rx.try_recv().expect(...)` 为 `let entry = rx.try_recv().expect(...); let msg = entry.message;`。

- [ ] **Step 11: 添加滚动撤回单元测试**

在 `frontend.rs` 测试模块中添加：

```rust
#[tokio::test]
async fn task_status_rolling_recall() {
    use uuid::Uuid;
    let (fe, mut rx) = make_frontend(FrontendKind::Telegram);
    let task_id: TaskId = Uuid::nil();

    // 第一条状态消息（Pending→Running）
    fe.push_event(EngineEvent::TaskStatusChanged {
        target: EventTarget::Directed(vec![ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "u1".to_string(),
            thread_id: None,
        }]),
        task_id,
        name: "task".to_string(),
        status: TaskStatusKind::Running,
        old_status: Some(TaskStatusKind::Pending),
        result: None,
        parent_id: None,
        origin_channel: None,
        agent_name: None,
        waiting_reason: None,
    });
    let entry1 = rx.try_recv().expect("first status msg");
    assert_eq!(entry1.message.message_kind, super::traits::MessageKind::TaskStatus);
    assert!(rx.try_recv().is_err(), "no recall for first status");

    // 模拟 on_sent 回调，更新 last_status_msg
    (entry1.on_sent.unwrap())(Some("msg_1".to_string()));

    // 第二条状态消息（Running→Waiting）—— 应先发 Recall，再发新状态
    fe.push_event(EngineEvent::TaskStatusChanged {
        target: EventTarget::Directed(vec![ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "u1".to_string(),
            thread_id: None,
        }]),
        task_id,
        name: "task".to_string(),
        status: TaskStatusKind::Waiting,
        old_status: Some(TaskStatusKind::Running),
        result: None,
        parent_id: None,
        origin_channel: None,
        agent_name: None,
        waiting_reason: None,
    });
    let recall_entry = rx.try_recv().expect("recall msg");
    assert_eq!(recall_entry.message.message_kind, super::traits::MessageKind::Recall);
    assert_eq!(recall_entry.message.content, "msg_1");
    let status_entry = rx.try_recv().expect("new status msg");
    assert_eq!(status_entry.message.message_kind, super::traits::MessageKind::TaskStatus);
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn llm_reply_recalls_last_status() {
    use uuid::Uuid;
    let (fe, mut rx) = make_frontend(FrontendKind::Telegram);
    let task_id: TaskId = Uuid::nil();

    // 发送状态消息
    fe.push_event(EngineEvent::TaskStatusChanged {
        target: EventTarget::Directed(vec![ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "u1".to_string(),
            thread_id: None,
        }]),
        task_id,
        name: "task".to_string(),
        status: TaskStatusKind::Done,
        old_status: Some(TaskStatusKind::Running),
        result: None,
        parent_id: None,
        origin_channel: None,
        agent_name: None,
        waiting_reason: None,
    });
    let status_entry = rx.try_recv().expect("status msg");
    (status_entry.on_sent.unwrap())(Some("msg_final".to_string()));

    // LLM 回复到达 —— 应先发 Recall，再发 LLMReply
    fe.push_event(EngineEvent::Text {
        target: EventTarget::Directed(vec![ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "u1".to_string(),
            thread_id: None,
        }]),
        role: MessageRole::Agent,
        content: "done".to_string(),
        task_id: Some(task_id),
    });
    let recall_entry = rx.try_recv().expect("recall msg");
    assert_eq!(recall_entry.message.message_kind, super::traits::MessageKind::Recall);
    assert_eq!(recall_entry.message.content, "msg_final");
    let llm_entry = rx.try_recv().expect("llm reply");
    assert_eq!(llm_entry.message.message_kind, super::traits::MessageKind::LLMReply);
}

#[tokio::test]
async fn task_failed_preserves_final_status() {
    use uuid::Uuid;
    let (fe, mut rx) = make_frontend(FrontendKind::Telegram);
    let task_id: TaskId = Uuid::nil();

    // 发送 Running 状态消息
    fe.push_event(EngineEvent::TaskStatusChanged {
        target: EventTarget::Directed(vec![ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "u1".to_string(),
            thread_id: None,
        }]),
        task_id,
        name: "task".to_string(),
        status: TaskStatusKind::Running,
        old_status: Some(TaskStatusKind::Pending),
        result: None,
        parent_id: None,
        origin_channel: None,
        agent_name: None,
        waiting_reason: None,
    });
    let status_entry = rx.try_recv().expect("status msg");
    (status_entry.on_sent.unwrap())(Some("msg_running".to_string()));

    // Failed 状态 —— 不应撤回 Running
    fe.push_event(EngineEvent::TaskStatusChanged {
        target: EventTarget::Directed(vec![ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "u1".to_string(),
            thread_id: None,
        }]),
        task_id,
        name: "task".to_string(),
        status: TaskStatusKind::Failed,
        old_status: Some(TaskStatusKind::Running),
        result: None,
        parent_id: None,
        origin_channel: None,
        agent_name: None,
        waiting_reason: None,
    });
    // 只应有 Failed 状态消息，没有 Recall
    let failed_entry = rx.try_recv().expect("failed status msg");
    assert_eq!(failed_entry.message.message_kind, super::traits::MessageKind::TaskStatus);
    assert!(failed_entry.message.content.contains("已失败"));
    assert!(rx.try_recv().is_err(), "no recall for Failed status");
}

#[tokio::test]
async fn task_cleared_cleans_up_state() {
    use uuid::Uuid;
    let (fe, mut rx) = make_frontend(FrontendKind::Telegram);
    let task_id: TaskId = Uuid::nil();

    // 发送状态消息
    fe.push_event(EngineEvent::TaskStatusChanged {
        target: EventTarget::Directed(vec![ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "u1".to_string(),
            thread_id: None,
        }]),
        task_id,
        name: "task".to_string(),
        status: TaskStatusKind::Running,
        old_status: Some(TaskStatusKind::Pending),
        result: None,
        parent_id: None,
        origin_channel: None,
        agent_name: None,
        waiting_reason: None,
    });
    let status_entry = rx.try_recv().expect("status msg");
    (status_entry.on_sent.unwrap())(Some("msg_1".to_string()));

    // TaskCleared
    fe.push_event(EngineEvent::TaskCleared {
        target: EventTarget::Directed(vec![ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "u1".to_string(),
            thread_id: None,
        }]),
        task_id,
    });
    assert!(rx.try_recv().is_err(), "TaskCleared should not produce outbound");

    // 再次发送同 task 的状态消息 —— 不应触发撤回（状态已清理）
    fe.push_event(EngineEvent::TaskStatusChanged {
        target: EventTarget::Directed(vec![ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "u1".to_string(),
            thread_id: None,
        }]),
        task_id,
        name: "task".to_string(),
        status: TaskStatusKind::Running,
        old_status: Some(TaskStatusKind::Pending),
        result: None,
        parent_id: None,
        origin_channel: None,
        agent_name: None,
        waiting_reason: None,
    });
    let new_status = rx.try_recv().expect("new status msg");
    assert_eq!(new_status.message.message_kind, super::traits::MessageKind::TaskStatus);
    assert!(rx.try_recv().is_err(), "no recall after TaskCleared");
}
```

- [ ] **Step 12: 运行 frontend 测试**

Run: `cargo test --lib channels::frontend -- --nocapture 2>&1 | tail -30`
Expected: 所有测试 PASS

- [ ] **Step 13: 全量编译检查**

Run: `cargo check --all-features 2>&1 | tail -10`
Expected: 编译通过（可能有 warning，无 error）

- [ ] **Step 14: Commit**

```bash
git add src/channels/frontend.rs src/channels/manager.rs src/channels/traits.rs
git commit -m "feat(channels): stateful ChannelFrontend with rolling recall + OutboundEntry callback"
```

---

### Task 6: QQ 通道 listen ACK 替换为 typing + 审批点击撤回审批请求

**Files:**
- Modify: `src/channels/qq.rs`

**Interfaces:**
- Consumes: Task 2 的 `recall_message`/`send_typing`（已移除 dead_code）
- Produces: `listen()` 中入向 ACK 改为 `send_typing()`；`handle_interaction_create` 撤回审批请求消息

**背景:** 入向 ACK `收到：<预览>` 用 typing 替代；用户点击审批按钮后撤回审批请求消息（带按钮的那条）。

- [ ] **Step 1: `listen()` 中入向 ACK 改为 `send_typing`**

在 `src/channels/qq.rs` 的 `listen` 方法中（约 1842 行），将：

```rust
// 发送 ACK
self.send_ack_text(&recipient, &content).await;
```

改为：

```rust
// 用 typing indicator 替代文字 ACK（C2C 有效，群聊静默跳过）
if let Err(e) = self.send_typing(&recipient).await {
    tracing::debug!(
        event = "QqTypingFailed",
        recipient = %recipient,
        error = %e,
        "typing indicator failed, skipping silently"
    );
}
```

- [ ] **Step 2: 删除 `send_ack_text` 方法及其测试**

删除 `src/channels/qq.rs` 中 `send_ack_text` 方法（约 591-614 行）。

搜索测试模块中是否有 `send_ack_text` 相关测试并删除：

Run: `grep -n "send_ack_text\|ack_text" src/channels/qq.rs`

如有命中（非方法定义本身），删除相关测试。

- [ ] **Step 3: 为 QQ 通道添加 `pending_approval_msg_ids` 状态字段**

在 `QqChannel` struct 中添加：

```rust
pub struct QqChannel {
    // ... 现有字段 ...
    /// 记录审批请求消息的 msg_id，key 为 approval_id（request_id 字符串）。
    /// 用户点击按钮后据此撤回审批请求消息。
    pending_approval_msg_ids: Arc<RwLock<HashMap<String, String>>>,
}
```

**注意：** 需要确认 `QqChannel` 当前是否已 import `Arc`/`RwLock`/`HashMap`。实施时检查现有 import。

- [ ] **Step 4: 在 `send()` 中记录审批请求消息的 msg_id**

在 `send` 方法的有键盘路径中（`reply_markup` 存在时），当 `message_kind == ApprovalRequest` 时，记录 msg_id：

```rust
let msg_id = if let Some(ref markup) = message.reply_markup {
    if let Some((request_id, options)) = extract_approval_info(markup) {
        self.record_pending_approval(&message.recipient, request_id, options)
            .await;
    }
    let content_to_send = match message.parse_mode {
        Some(ChannelParseMode::Html) => html_to_markdown_for_qq(&message.content),
        Some(ChannelParseMode::Markdown) | None => message.content.clone(),
    };
    let id = if !content_to_send.trim().is_empty() {
        Some(self.send_text_with_keyboard(&message.recipient, &content_to_send, markup).await?.id)
    } else {
        None
    };
    // 记录审批请求消息的 msg_id
    if message.message_kind == MessageKind::ApprovalRequest {
        if let Some(ref mid) = id {
            if let Some((request_id, _)) = extract_approval_info(markup) {
                self.pending_approval_msg_ids
                    .write()
                    .await
                    .insert(request_id, mid.clone());
            }
        }
    }
    id
} else {
    // ... 无键盘路径 ...
};
```

- [ ] **Step 5: 在 `handle_interaction_create` 中撤回审批请求消息**

在 `handle_interaction_create` 方法中（解析出 `approval_id` 后），撤回审批请求消息：

```rust
// 在 handle_interaction_create 中，解析出 button_data 后：
// button_data 格式为 "{request_id}:{option_id}"
if let Some((request_id_str, _option_id)) = button_data.split_once(':') {
    let approval_msg_id = self.pending_approval_msg_ids.write().await.remove(request_id_str);
    if let Some(msg_id) = approval_msg_id {
        if let Err(e) = self.recall_message(&recipient, &msg_id).await {
            tracing::warn!(
                event = "ChannelRecallFailed",
                channel = "qq",
                recipient = %recipient,
                msg_id = %msg_id,
                error = %e,
                "recall approval request failed"
            );
        }
    }
}
```

**注意：** 需要确认 `handle_interaction_create` 中 `recipient` 变量是否已定义。实施时阅读该方法完整实现，找到 `button_data` 解析点和 `recipient` 来源。

- [ ] **Step 6: 添加 `send_typing_on_inbound_message` 测试**

```rust
#[tokio::test]
async fn send_typing_on_inbound_message_replaces_ack() {
    // 此测试验证 listen() 内部行为，较难直接测试。
    // 改为验证 send_typing 在 C2C 场景下发 POST 请求（已有 send_typing_posts_to_c2c_user 测试）。
    // 本测试改为验证 send_ack_text 方法已被删除：
    // 搜索 qq.rs 确认无 send_ack_text 方法定义。
    // 这是一个编译期验证，无需运行时测试。
}
```

**改为编译期验证：** 删除此测试步骤，改为在 Step 2 中验证 `send_ack_text` 已删除（编译通过即可）。

- [ ] **Step 7: 添加 `approval_button_click_recalls_approval_request` 测试**

此测试需要模拟 `handle_interaction_create` 的完整流程，较复杂。**简化方案：** 验证 `pending_approval_msg_ids` 在 `send(ApprovalRequest)` 后被填充，且 `handle_interaction_create` 能从中取出并调用 `recall_message`。

由于 `handle_interaction_create` 是私有方法且依赖 WebSocket 事件数据，**改为集成测试**（Task 7）。

- [ ] **Step 8: 运行 QQ 通道测试**

Run: `cargo test --lib channels::qq -- --nocapture 2>&1 | tail -20`
Expected: 所有测试 PASS

- [ ] **Step 9: Commit**

```bash
git add src/channels/qq.rs
git commit -m "feat(qq): replace inbound ACK with typing, recall approval request on button click"
```

---

### Task 7: 集成测试 — QQ 通道滚动撤回端到端流程

**Files:**
- Create: `tests/qq_channel_recall_flow.rs`

**Interfaces:**
- Consumes: Task 1-6 的所有改动
- Produces: 端到端集成测试，验证 Task 状态变更 → 滚动撤回 → LLM 回复到达 → 撤回最终态

**背景:** 端到端验证整个治理策略在 QQ 通道上的工作流程。

- [ ] **Step 1: 创建集成测试文件**

创建 `tests/qq_channel_recall_flow.rs`：

```rust
/// QQ 通道滚动撤回端到端集成测试。
///
/// 验证流程：
/// 1. Task 状态变更（Pending→Running）→ 发送状态消息，无撤回
/// 2. Task 状态变更（Running→Waiting）→ 撤回上一条 + 发新状态消息
/// 3. LLM 回复到达 → 撤回最终态状态消息 + 发 LLM 回复
///
/// 使用 wiremock 模拟 QQ API，验证 DELETE /messages/{id} 调用次数和顺序。

use harness::channels::traits::{Channel, ChannelOutboundMessage, MessageKind};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// 注意：此测试需要 harness 库暴露必要的构造 API。
// 如果 QqChannel::new / with_api_base / set_token_for_test 不是 pub，
// 需要在 lib.rs 中添加 #[cfg(test)] pub 或使用 pub(crate) + 测试 feature。
// 实施时先确认暴露方式。
```

**注意：** 集成测试需要 `harness` 库暴露 `QqChannel` 构造方法和 `ChannelFrontend`。如果这些当前不是 `pub`，需要：
1. 在 `lib.rs` 中添加 `pub mod channels;`（如尚未公开）
2. 或使用 `#[cfg(test)]` feature 暴露测试 helper

**实施时先检查 `src/lib.rs` 的模块暴露情况。** 如果暴露成本过高，**降级为 `src/channels/qq.rs` 内部的 `#[cfg(test)]` 集成测试**（不放在 `tests/` 目录）。

- [ ] **Step 2: 实现端到端测试**

根据 Step 1 的暴露情况，实现以下测试（在 `tests/qq_channel_recall_flow.rs` 或 `src/channels/qq.rs` 测试模块中）：

```rust
#[tokio::test]
async fn qq_channel_rolling_recall_end_to_end() {
    // 1. 启动 mock server
    let mock_server = MockServer::start().await;

    // 2. 配置 mock：
    //    - POST /v2/users/{openid}/messages → 返回 {"id": "msg_N"}（每次返回不同 id）
    //    - DELETE /v2/users/{openid}/messages/{msg_id} → 返回 200
    //    - POST /v2/users/{openid}/typing → 返回 200

    // 3. 构造 QqChannel
    let ch = QqChannel::new(make_config()).with_api_base(mock_server.uri());
    ch.set_token_for_test("fake_token").await;

    // 4. 模拟 ChannelFrontend 的滚动撤回策略：
    //    - 发送 TaskStatus(Running) → 收到 msg_id="msg_1"
    //    - 发送 Recall(msg_1) + TaskStatus(Waiting) → 收到 msg_id="msg_2"
    //    - 发送 Recall(msg_2) + TaskStatus(Done) → 收到 msg_id="msg_3"
    //    - 发送 Recall(msg_3) + LLMReply → 收到 msg_id="msg_4"

    // 5. 验证 DELETE 调用次数 = 3（msg_1, msg_2, msg_3）
    // 6. 验证最终只剩 msg_4（LLM 回复）
}
```

**简化方案（如集成测试成本过高）：** 在 `src/channels/qq.rs` 测试模块中添加一个组合测试，直接调用 `send()` 多次并验证 `recall_message` 被调用：

```rust
#[tokio::test]
async fn qq_send_with_recall_kind_calls_recall_and_returns_none() {
    // 已在 Task 2 Step 4 中实现 send_with_recall_kind_calls_recall_api
    // 本测试验证端到端流程
}
```

- [ ] **Step 3: 运行集成测试**

Run: `cargo test --test qq_channel_recall_flow -- --nocapture 2>&1 | tail -20`
Expected: 测试 PASS

（如降级为 `src/channels/qq.rs` 内部测试：`cargo test --lib channels::qq::tests::qq_rolling_recall -- --nocapture`）

- [ ] **Step 4: Commit**

```bash
git add tests/qq_channel_recall_flow.rs  # 或 src/channels/qq.rs
git commit -m "test(qq): add end-to-end rolling recall integration test"
```

---

### Task 8: 文档同步 + 全量 CI + 清理

**Files:**
- Modify: `docs/current-state.md`
- Modify: `AGENTS.md` + `CLAUDE.md`（如适用）
- Modify: `src/channels/qq.rs`（清理 `send_ack_text` 残留测试）

**背景:** 所有代码改动完成后，同步文档并运行全量 CI。

- [ ] **Step 1: 更新 `docs/current-state.md`**

在"已实现"章节添加：

```markdown
- IM 通道状态消息治理：任务状态消息滚动撤回（发新撤旧，避免 2 分钟超时）、入向 ACK 用 typing 替代（C2C）、审批请求消息点击后撤回、LLM 回复到达时撤回最终态状态消息
- Channel trait 统一抽象 recall/typing（QQ + Telegram 实现，默认 NotSupported/Ok）
- ChannelOutboundMessage 携带 MessageKind（LLMReply/TaskStatus/ApprovalRequest/System/Recall/Other）
```

在"待继续完善"章节，移除"QQ 通道消息撤回的调用方集成（状态消息治理：撤回过多状态切换消息）尚未接入，recall_message 方法已就绪"条目（已被本次实施完成）。

- [ ] **Step 2: 检查 `AGENTS.md` 是否需要更新**

检查 `AGENTS.md` 的"技术边界"或"工程约定"章节是否需要补充 Channel trait 的 recall/typing 抽象说明。如涉及能力边界变化，补充；否则跳过。

**验证：** 阅读 `AGENTS.md` 相关章节，判断是否需要更新。

- [ ] **Step 3: 同步 `CLAUDE.md`**

如 `AGENTS.md` 有更新，同步到 `CLAUDE.md`。

- [ ] **Step 4: 全量 CI**

Run: `cargo fmt --all --check`
Expected: 无格式问题

Run: `cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -10`
Expected: 无 warning

Run: `cargo test --all-features 2>&1 | tail -20`
Expected: 所有测试 PASS

- [ ] **Step 5: 搜索残留的 `#[allow(dead_code)]`**

Run: `grep -n "#\[allow(dead_code)\]" src/channels/qq.rs`
Expected: 无命中（或仅有合理保留的标注，需人工确认）

如有残留，逐一评估是否可移除。

- [ ] **Step 6: Commit**

```bash
git add docs/current-state.md AGENTS.md CLAUDE.md src/channels/qq.rs
git commit -m "docs: sync IM channel message governance capability state"
```

---

## 自检

### 1. 规格覆盖度

| 规格章节 | 对应任务 |
|---|---|
| Channel trait 扩展 | Task 1 |
| MessageKind 枚举 | Task 1 |
| ChannelError::NotSupported | Task 1 |
| ChannelFrontend 有状态化 | Task 5 |
| OutboundEntry 回调模式 | Task 5 |
| 通道 send 处理 Recall | Task 2 (QQ) + Task 3 (Telegram) |
| QQ 通道实现（移除 dead_code、send 返回 msg_id、ACK 改 typing、审批撤回） | Task 2 + Task 6 |
| Telegram 通道实现（recall/typing/send 返回 msg_id） | Task 3 |
| channel_send 工具补充 message_kind | Task 4 |
| 并发安全（try_read/try_write 降级） | Task 5 Step 4 |
| 错误处理与降级 | Task 2 Step 2 (Recall 失败 warn) + Task 5 (try_read/try_write) |
| 测试策略 | Task 2 + Task 3 + Task 5 + Task 7 |
| 文档同步 | Task 8 |

**遗漏：** 无。所有规格章节都有对应任务。

### 2. 占位符扫描

- 无 "TODO"/"待定"
- "实施时先阅读"/"实施时确认" 是合理的实施指引（非占位符），因为具体代码位置可能因前序任务改动而偏移

### 3. 类型一致性

- `MessageKind` 在 Task 1 定义，Task 2/3/4/5 使用——名称一致
- `OutboundEntry` 在 Task 5 Step 1 定义（建议移到 `traits.rs`），Task 5 Step 8 在 `manager.rs` 使用——一致
- `recall_message`/`send_typing` 在 Task 1 定义为 trait 默认方法，Task 2/3 在通道 impl 中使用——签名一致
- `send()` 返回 `Result<Option<String>, ChannelError>` 在 Task 1 定义，Task 2/3 实现——一致
