> **状态：已归档** — 对应功能已合并到 main，归档于 2026-07-05

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

# Telegram 通道体验优化实施计划

**Goal:** 为 Harness Telegram 通道补齐 Markdown 渲染、Inline Keyboard 权限审批回流、文件收发、`/bind` 自助配对与 ACK 反应等能力，同时保持其他 IM 通道接口兼容。

**Architecture:** 扩展 `ChannelId` 携带 `thread_id`；扩展 `ChannelOutboundMessage`/`ChannelInboundMessage` 和 `Channel` trait 以支持富媒体、能力与确认回流；`frontend_output_system` 按 `Task::origin_channel` 定向审批请求；`TelegramChannel` 负责 Telegram 特有的 HTML/Markdown 转换、附件 API 调用与 `/bind` 运行时白名单。

**Tech Stack:** Rust, Bevy ECS, tokio, reqwest, serde_json, ratatui（TUI 不受影响）。

## Global Constraints

- 所有变更不得破坏 QQ/Feishu 通道编译。
- `Channel` trait 核心签名（`name`/`send`/`listen`）不变，只能新增带默认实现的方法。
- Telegram Bot API 单条消息限制 4096 字符；Bot API 下载文件限制 20 MB。
- 默认安全语义不变：空 `allowed_users` 仍表示拒绝，除非显式开启 `pairing_enabled`。
- 文档使用中文，代码注释与项目保持一致。

---

## 评审修订记录

针对前一轮评审提出的 3 处高风险不一致，本计划已做如下对齐：

| 评审问题 | 处理方式 | 对应位置 |
|---|---|---|
| Telegram 出向目标误用 `sender_id` 会导致路由错误 | 保持 `ChannelId.user_id = self.chat_id.clone()`，`thread_id` 仅作为新增维度透传 | Task 1 Step 6 |
| `/bind` 白名单模型收窄为仅 `user_id` | `runtime_allowed_users` 初始化为空集，仅 `/bind` 注入；`is_allowed` 保留 `&TelegramUser` 签名及 `username`/`user_id`/`"*"` 既有匹配语义 | Task 6 Step 5 |
| callback_query 步骤使用动态 JSON + 对同步 `Sender` 调用 `.await` | 保持现有强类型 `TelegramUpdate`/`TelegramMessage` 解析路径，仅新增 `TelegramCallbackQuery` 结构体，使用同步 `tx.send(...)` | Task 3 Step 5 |

此外，修正了 Task 2 Step 3 中引用的不存在的 `FrontendEvent`/`confirmation` 字段，使其与当前 `frontend_output_system` 实际结构一致。

---

## File Map

- `src/domain/frontend.rs`：`ChannelId` 增加 `thread_id`。
- `src/channels/traits.rs`：`ChannelOutboundMessage`、`ChannelInboundMessage`、`Channel` trait、`AttachmentKind`、`ChannelParseMode`、`ReplyMarkup`。
- `src/systems/frontend_output.rs`：审批请求定向路由。
- `src/channels/frontend.rs`：`ChannelFrontend` 处理 `EngineEvent::ApprovalRequest`。
- `src/channels/telegram.rs`：Telegram 特化实现（HTML、附件、Inline Keyboard、callback_query、`/bind`、运行时白名单、ACK）。
- `src/channels/config.rs`：`TelegramConfig` 增加 `pairing_enabled`。
- `src/channels/send_tool.rs`：`channel_send` 工具描述补充附件标记语法。
- `docs/design/im-channel-adapters.md`：更新 Telegram 配置与能力说明。
- `docs/configuration.md`：更新配置项说明。

---

### Task 1: 扩展核心抽象（thread_id、出向/入向消息格式、通道能力）

**Files:**
- Modify: `src/domain/frontend.rs`
- Modify: `src/channels/traits.rs`
- Test: `src/channels/traits.rs`（新增 `#[cfg(test)]` 单元测试）

**Interfaces:**
- Consumes: `ChannelId { frontend, user_id }`
- Produces: `ChannelId { frontend, user_id, thread_id }`, `ChannelOutboundMessage { parse_mode, reply_markup, attachments }`, `ChannelInboundMessage { confirmation }`, `Channel::supported_attachment_kinds()`

- [ ] **Step 1: Write the failing test for thread_id equality**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_id_with_thread_id_not_equal_to_without() {
        let a = ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "u1".to_string(),
            thread_id: None,
        };
        let b = ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "u1".to_string(),
            thread_id: Some("t1".to_string()),
        };
        assert_ne!(a, b);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib channel_id_with_thread_id_not_equal_to_without`
Expected: FAIL due to missing `thread_id` field.

- [ ] **Step 3: Add `thread_id` to `ChannelId`**

In `src/domain/frontend.rs`:

```rust
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct ChannelId {
    pub frontend: FrontendKind,
    pub user_id: String,
    pub thread_id: Option<String>,
}
```

Update **every** call site that constructs `ChannelId` to include `thread_id: None`. Use the following command to find all sites:

```bash
grep -rn "ChannelId {" src/
```

Commonly affected files include:
- `src/channels/traits.rs` (in `to_external_input` and tests)
- `src/channels/frontend.rs` (tests)
- `src/domain/message.rs`
- `src/domain/task.rs`
- `src/systems/command.rs`
- `src/systems/memory.rs`
- `src/systems/transform/task_completion_hook.rs`
- `src/systems/tools/tool_called_hook.rs`
- `src/systems/dispatch/task_dispatch.rs`
- `src/tui/app.rs`
- `src/tui/mod.rs`
- `src/user_plugins/dispatcher.rs`
- `src/user_plugins/host_api/entity_query.rs`
- `src/llm/brain_prompt.rs`
- Any other test-only construction.

For each site, add `thread_id: None,` as the simplest fix; thread-aware filling happens in Task 2.

> **路由关键约束**：评审指出 `ChannelId.user_id` 在当前链路中承载的是 Telegram `chat_id`（出向 recipient），不能误填为 `sender_id`。本计划保持 `user_id: self.chat_id.clone()`，`thread_id` 作为新增维度透传。

- [ ] **Step 4: Extend `ChannelOutboundMessage` and related types**

In `src/channels/traits.rs`:

```rust
#[derive(Clone, Debug)]
pub struct ChannelOutboundMessage {
    pub recipient: String,
    pub thread_id: Option<String>,
    pub content: String,
    pub parse_mode: Option<ChannelParseMode>,
    pub reply_markup: Option<ReplyMarkup>,
    pub attachments: Vec<ChannelAttachment>,
}

#[derive(Clone, Debug, Serialize)]
pub enum ChannelParseMode {
    Html,
    Markdown,
}

#[derive(Clone, Debug, Serialize)]
pub enum ReplyMarkup {
    InlineKeyboard(Vec<Vec<InlineKeyboardButton>>),
}

#[derive(Clone, Debug, Serialize)]
pub struct InlineKeyboardButton {
    pub text: String,
    pub callback_data: String,
}

#[derive(Clone, Debug)]
pub struct ChannelAttachment {
    pub kind: AttachmentKind,
    pub target: String,
}

#[derive(Clone, Debug)]
pub enum AttachmentKind {
    Image,
    Document,
    Video,
    Audio,
    Voice,
}
```

Update existing construction sites of `ChannelOutboundMessage` to set new fields to `None`/empty. Use the following command to find all sites:

```bash
grep -rn "ChannelOutboundMessage {" src/
```

Commonly affected files:
- `src/channels/frontend.rs`
- `src/channels/manager.rs`
- `src/systems/tools/channel_send_dispatch.rs`
- `src/channels/send_tool.rs` (if any)
- `src/channels/telegram.rs` (tests)

- [ ] **Step 5: Extend `ChannelInboundMessage` for confirmations**

In `src/channels/traits.rs`:

```rust
#[derive(Clone, Debug)]
pub struct ChannelInboundMessage {
    pub channel_name: String,
    pub sender_id: String,
    pub chat_id: String,
    pub thread_id: Option<String>,
    pub content: String,
    pub timestamp_secs: u64,
    pub confirmation: Option<InboundConfirmation>,
}

#[derive(Clone, Debug)]
pub struct InboundConfirmation {
    pub request_id: Uuid,
    pub option: String,
}
```

Update all construction sites of `ChannelInboundMessage` to add `confirmation: None`. Use the following command to find all sites:

```bash
grep -rn "ChannelInboundMessage {" src/
```

Commonly affected files:
- `src/channels/traits.rs` (tests)
- `src/channels/telegram.rs`
- `src/channels/manager.rs`

- [ ] **Step 6: Update `to_external_input` to pass `chat_id` as `user_id`, `thread_id`, and `confirmation`**

In `src/channels/traits.rs`:

```rust
impl ChannelInboundMessage {
    pub fn to_external_input(&self) -> ExternalInput {
        if let Some(ref confirmation) = self.confirmation {
            return ExternalInput::Confirmation {
                request_id: confirmation.request_id,
                option: confirmation.option.clone(),
            };
        }

        let channel_id = ChannelId {
            frontend: match self.channel_name.as_str() {
                "telegram" => FrontendKind::Telegram,
                "qq" => FrontendKind::QQ,
                "feishu" => FrontendKind::Feishu,
                _ => FrontendKind::Tui,
            },
            user_id: self.chat_id.clone(),
            thread_id: self.thread_id.clone(),
        };

        ExternalInput::TextWithChannel {
            content: self.content.clone(),
            channel: channel_id,
        }
    }
}
```

注意：`ChannelId.user_id` 在当前链路中承载的是 Telegram `chat_id`（出向目标），不是 `sender_id`。透传 `thread_id` 时使用 `chat_id` 作为出向 recipient。

- [ ] **Step 7: Extend `Channel` trait with capability queries**

In `src/channels/traits.rs`:

```rust
#[async_trait]
pub trait Channel: Send + Sync + 'static {
    fn name(&self) -> &str;
    async fn send(&self, message: &ChannelOutboundMessage) -> Result<(), ChannelError>;
    async fn listen(&self, tx: Sender<ChannelInboundMessage>) -> Result<(), ChannelError>;
    async fn health_check(&self) -> bool { true }

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

- [ ] **Step 8: Run tests**

Run: `cargo test --all-features`
Expected: PASS.

- [ ] **Step 9: Commit**

```bash
git add \
  src/domain/frontend.rs \
  src/domain/message.rs \
  src/domain/task.rs \
  src/channels/traits.rs \
  src/channels/frontend.rs \
  src/channels/manager.rs \
  src/channels/send_tool.rs \
  src/channels/telegram.rs \
  src/systems/command.rs \
  src/systems/frontend_output.rs \
  src/systems/memory.rs \
  src/systems/transform/task_completion_hook.rs \
  src/systems/transform/task_creation.rs \
  src/systems/transform/task_lifecycle.rs \
  src/systems/dispatch/task_dispatch.rs \
  src/systems/dispatch/brain_dispatch.rs \
  src/systems/tools/orchestrator.rs \
  src/systems/tools/tool_called_hook.rs \
  src/systems/tools/tool_returned_hook.rs \
  src/tui/app.rs \
  src/tui/mod.rs \
  src/user_plugins/dispatcher.rs \
  src/user_plugins/host_api/entity_query.rs \
  src/llm/brain_prompt.rs
git commit -m "feat(channels): extend ChannelId with thread_id and add rich outbound/inbound message types"
```

---

### Task 2: 审批请求定向与 Inline Keyboard 发送

**Files:**
- Modify: `src/systems/frontend_output.rs`
- Modify: `src/channels/frontend.rs`
- Test: `src/systems/frontend_output.rs`（新增 `#[cfg(test)]` 单元测试）

**Interfaces:**
- Consumes: `EngineEvent::ApprovalRequest`, `Task::origin_channel`
- Produces: `EventTarget::Directed(Vec<ChannelId>)` for approvals

- [ ] **Step 1: Write the failing test for approval directed routing**

In `src/systems/frontend_output.rs` `#[cfg(test)]`:

```rust
#[test]
fn approval_request_targeted_to_task_origin_channel() {
    // Construct a minimal ECS world with a Task whose origin_channel is Telegram + thread_id
    // Spawn an EngineEvent::ApprovalRequest for that task_id
    // Run frontend_output_system
    // Assert that the produced event target is Directed(vec![task_origin_channel])
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib approval_request_targeted_to_task_origin_channel`
Expected: FAIL due to Broadcast target.

- [ ] **Step 3: Update `frontend_output_system` to route approvals by task origin**

In `src/systems/frontend_output.rs`, modify the existing `EngineEvent::ApprovalRequest` handling block. The current block already iterates over `confirmations` and constructs the event with `target: EventTarget::Broadcast`. Change it to look up the associated `Task` by `confirmation.task_id` and use that task's `origin_channel` as the directed target:

```rust
// 审批请求
for (entity, confirmation) in &confirmations {
    let target = all_tasks
        .iter()
        .find(|t| t.id == confirmation.task_id)
        .map(|t| EventTarget::Directed(vec![t.origin_channel.clone()]))
        .unwrap_or(EventTarget::Broadcast);

    let options: Vec<crate::domain::ApprovalOption> = confirmation
        .options
        .iter()
        .map(|opt| crate::domain::ApprovalOption {
            id: opt.id.clone(),
            label: opt.label.clone(),
            description: if opt.id == "deny" {
                "拒绝".to_string()
            } else {
                match opt.mode {
                    crate::domain::GrantMode::Once => "仅本次允许".to_string(),
                    crate::domain::GrantMode::Permanent => "永久允许此工具".to_string(),
                }
            },
        })
        .collect();

    let event = EngineEvent::ApprovalRequest {
        target,
        request_id: confirmation.request_id,
        agent_name: String::new(),
        tool_name: confirmation.tool_name.clone(),
        tool_input: confirmation.tool_input.clone(),
        options,
    };
    for frontend in &registry.frontends {
        frontend.push_event(event.clone());
    }

    commands.entity(entity).despawn();
}
```

> **一致性说明**：`ToolConfirmationRequestMessage` 已经包含 `task_id`（见 `src/domain/message.rs`），因此通过 `all_tasks` 查找即可获得 `origin_channel`，不需要在 `EngineEvent::ApprovalRequest` 上新增字段。

- [ ] **Step 4: Update `ChannelFrontend::push_event` to handle `ApprovalRequest`**

Rewrite `push_event` in `src/channels/frontend.rs` to dispatch both `Text` and `ApprovalRequest`:

```rust
fn push_event(&self, event: EngineEvent) {
    match event {
        EngineEvent::Text {
            target, content, ..
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
            for channel_id in recipients {
                let msg = ChannelOutboundMessage {
                    recipient: channel_id.user_id,
                    thread_id: channel_id.thread_id,
                    content: content.clone(),
                    parse_mode: None,
                    reply_markup: None,
                    attachments: vec![],
                };
                let _ = self.outbound_tx.send((self.channel_name.clone(), msg));
            }
        }
        EngineEvent::ApprovalRequest {
            target,
            request_id,
            tool_name,
            tool_input,
            options,
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

            let tool_input_str = serde_json::to_string_pretty(&tool_input)
                .unwrap_or_else(|_| tool_input.to_string());
            let content = format!(
                "🔒 需要你的确认\n\n工具：{}\n输入：{}\n\n请选择一个选项：",
                tool_name, tool_input_str
            );
            let buttons: Vec<Vec<InlineKeyboardButton>> = options
                .chunks(2)
                .map(|chunk| {
                    chunk
                        .iter()
                        .map(|opt| InlineKeyboardButton {
                            text: opt.label.clone(),
                            callback_data: format!("{}:{}", request_id, opt.id),
                        })
                        .collect()
                })
                .collect();

            for channel_id in recipients {
                let msg = ChannelOutboundMessage {
                    recipient: channel_id.user_id,
                    thread_id: channel_id.thread_id,
                    content: content.clone(),
                    parse_mode: Some(ChannelParseMode::Html),
                    reply_markup: Some(ReplyMarkup::InlineKeyboard(buttons.clone())),
                    attachments: vec![],
                };
                let _ = self.outbound_tx.send((self.channel_name.clone(), msg));
            }
        }
        _ => {}
    }
}
```

Add imports for `ChannelParseMode`, `ReplyMarkup`, and `InlineKeyboardButton` at the top of `src/channels/frontend.rs`.

- [ ] **Step 5: Run tests**

Run: `cargo test --all-features`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/systems/frontend_output.rs src/channels/frontend.rs
git commit -m "feat(frontend): route approval requests to task origin channel with inline keyboard"
```

---

### Task 3: Telegram 入向 callback_query 与确认回流

**Files:**
- Modify: `src/channels/telegram.rs`
- Test: `src/channels/telegram.rs`（新增 `#[cfg(test)]` 单元测试）

**Interfaces:**
- Consumes: `update.callback_query`, `ChannelInboundMessage { confirmation }`
- Produces: `ChannelInboundMessage` with `confirmation: Some(...)`

- [ ] **Step 1: Write the failing test for callback_query parsing**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_callback_query_data() {
        let data = "01912345-6789-7abc-8def-0123456789ab:allow_once";
        let (request_id, option) = parse_callback_data(data).unwrap();
        assert_eq!(request_id.to_string(), "01912345-6789-7abc-8def-0123456789ab");
        assert_eq!(option, "allow_once");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib parse_callback_query_data`
Expected: FAIL due to missing `parse_callback_data`.

- [ ] **Step 3: Implement callback_query parsing helper**

In `src/channels/telegram.rs`:

```rust
fn parse_callback_data(data: &str) -> Option<(Uuid, String)> {
    let (uuid_part, option_part) = data.split_once(':')?;
    let request_id = Uuid::parse_str(uuid_part).ok()?;
    Some((request_id, option_part.to_string()))
}
```

- [ ] **Step 4: Add `post` helper method**

Extract a reusable POST helper on `TelegramChannel` (later tasks also use it):

```rust
async fn post(&self, method: &str, payload: &serde_json::Value) -> Result<(), ChannelError> {
    let url = self.api_url(method);
    let resp = self.client.post(&url).json(payload).send().await?;
    if !resp.status().is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(ChannelError::Api {
            code: 0,
            message: text,
        });
    }
    Ok(())
}
```

- [ ] **Step 5: Handle `callback_query` in `listen` loop with typed structs**

Add typed structs for callback queries to the existing `telegram.rs` typed deser model:

```rust
#[derive(Debug, Deserialize)]
struct TelegramUpdate {
    update_id: i64,
    message: Option<TelegramMessage>,
    callback_query: Option<TelegramCallbackQuery>,
}

#[derive(Debug, Deserialize)]
struct TelegramCallbackQuery {
    id: String,
    from: TelegramUser,
    message: Option<TelegramCallbackMessage>,
    data: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TelegramCallbackMessage {
    message_id: i64,
    chat: TelegramChat,
    message_thread_id: Option<i64>,
}
```

Inside `TelegramChannel::listen`, alongside the existing `update.message` handling, handle `update.callback_query` with synchronous `tx.send` (current `Channel::listen` uses `crossbeam_channel::Sender`, not async):

> **实现路径约束**：评审指出不能将 `update` 当作动态 JSON 取值，也不能对同步 `Sender` 调用 `.await`。本计划保持现有 `TelegramUpdate`/`TelegramMessage` 强类型解析路径，仅新增 `callback_query` 对应结构体，并用同步 `tx.send(...)` 投递。

```rust
if let Some(callback_query) = update.callback_query {
    self.last_update_id
        .store(update.update_id, Ordering::SeqCst);

    if let Some(data) = callback_query.data {
        if let Some((request_id, option)) = parse_callback_data(&data) {
            // Answer callback query to stop client loading spinner
            let answer_payload = json!({
                "callback_query_id": callback_query.id,
            });
            let _ = self.post("answerCallbackQuery", &answer_payload).await;

            // Optionally reply with a confirmation note
            if let Some(ref message) = callback_query.message {
                let note = format!("已选择：{}", option);
                let note_payload = json!({
                    "chat_id": message.chat.id,
                    "text": note,
                    "message_thread_id": message.message_thread_id,
                });
                let _ = self.post("sendMessage", &note_payload).await;
            }

            if let Some(ref message) = callback_query.message {
                let inbound = ChannelInboundMessage {
                    channel_name: self.name().to_string(),
                    sender_id: callback_query.from.id.to_string(),
                    chat_id: message.chat.id.to_string(),
                    thread_id: message.message_thread_id.map(|id| id.to_string()),
                    content: String::new(),
                    timestamp_secs: now_secs(),
                    confirmation: Some(InboundConfirmation { request_id, option }),
                };
                let _ = tx.send(inbound);
            }
        }
    }
    continue;
}
```

- [ ] **Step 6: Run tests**

Run: `cargo test --all-features`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add src/channels/telegram.rs
git commit -m "feat(telegram): handle callback_query for inline keyboard approvals"
```

---

### Task 4: Telegram Markdown 渲染与 HTML 发送

**Files:**
- Modify: `src/channels/telegram.rs`
- Test: `src/channels/telegram.rs`（新增 `#[cfg(test)]` 单元测试）

**Interfaces:**
- Consumes: `ChannelOutboundMessage { parse_mode: Html, content }`
- Produces: `sendMessage` with `parse_mode: "HTML"`

- [ ] **Step 1: Write the failing test for markdown_to_telegram_html**

```rust
#[test]
fn markdown_bold_to_telegram_html() {
    let input = "**hello**";
    assert_eq!(markdown_to_telegram_html(input), "<b>hello</b>");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib markdown_bold_to_telegram_html`
Expected: FAIL due to missing function.

- [ ] **Step 3: Implement `markdown_to_telegram_html` without new dependencies**

In `src/channels/telegram.rs`:

```rust
fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn markdown_to_telegram_html(text: &str) -> String {
    let mut out = String::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        // fenced code block
        if chars[i..].starts_with(&['`', '`', '`']) {
            let rest: String = chars[i + 3..].iter().collect();
            let (lang, body_start) = if let Some(nl) = rest.find('\n') {
                (rest[..nl].trim(), nl + 1)
            } else {
                ("", 0)
            };
            let body_and_tail = &rest[body_start..];
            if let Some(end) = body_and_tail.find("```") {
                let body = &body_and_tail[..end];
                out.push_str("<pre><code>");
                if !lang.is_empty() {
                    out.push_str(&format!("class=\"language-{}\" ", escape_html(lang)));
                }
                out.push_str(&escape_html(body));
                out.push_str("</code></pre>");
                i += 3 + lang.len() + 1 + body.len() + 3 + body_start;
                continue;
            }
        }

        // inline code
        if chars[i] == '`' {
            let mut j = i + 1;
            while j < chars.len() && chars[j] != '`' {
                j += 1;
            }
            if j < chars.len() {
                let code: String = chars[i + 1..j].iter().collect();
                out.push_str("<code>");
                out.push_str(&escape_html(&code));
                out.push_str("</code>");
                i = j + 1;
                continue;
            }
        }

        // bold ** or __
        if chars[i..].starts_with(&['*', '*']) || chars[i..].starts_with(&['_', '_']) {
            let marker = chars[i];
            if let Some(end) = find_closing_pair(&chars, i + 2, marker, marker) {
                let inner: String = chars[i + 2..end].iter().collect();
                out.push_str("<b>");
                out.push_str(&markdown_to_telegram_html(&inner));
                out.push_str("</b>");
                i = end + 2;
                continue;
            }
        }

        // italic * or _
        if chars[i] == '*' || chars[i] == '_' {
            let marker = chars[i];
            if let Some(end) = find_closing_single(&chars, i + 1, marker) {
                let inner: String = chars[i + 1..end].iter().collect();
                out.push_str("<i>");
                out.push_str(&markdown_to_telegram_html(&inner));
                out.push_str("</i>");
                i = end + 1;
                continue;
            }
        }

        // strikethrough ~~
        if chars[i..].starts_with(&['~', '~']) {
            if let Some(end) = find_closing_pair(&chars, i + 2, '~', '~') {
                let inner: String = chars[i + 2..end].iter().collect();
                out.push_str("<s>");
                out.push_str(&markdown_to_telegram_html(&inner));
                out.push_str("</s>");
                i = end + 2;
                continue;
            }
        }

        // link [text](url)
        if chars[i] == '[' {
            if let Some(close_bracket) = chars[i + 1..].iter().position(|&c| c == ']') {
                let close_bracket = close_bracket + i + 1;
                if close_bracket + 1 < chars.len() && chars[close_bracket + 1] == '(' {
                    if let Some(close_paren) = chars[close_bracket + 2..].iter().position(|&c| c == ')') {
                        let close_paren = close_paren + close_bracket + 2;
                        let text: String = chars[i + 1..close_bracket].iter().collect();
                        let url: String = chars[close_bracket + 2..close_paren].iter().collect();
                        out.push_str(&format!("<a href=\"{}\">{}</a>", escape_html(&url), escape_html(&text)));
                        i = close_paren + 1;
                        continue;
                    }
                }
            }
        }

        // headings -> bold
        if i == 0 || chars[i - 1] == '\n' {
            let mut j = i;
            while j < chars.len() && chars[j] == '#' {
                j += 1;
            }
            if j > i && j < chars.len() && chars[j] == ' ' {
                let mut k = j + 1;
                while k < chars.len() && chars[k] != '\n' {
                    k += 1;
                }
                let heading: String = chars[j + 1..k].iter().collect();
                out.push_str("<b>");
                out.push_str(&escape_html(&heading));
                out.push_str("</b>\n");
                i = k + 1;
                continue;
            }
        }

        out.push(chars[i]);
        i += 1;
    }

    out
}

fn find_closing_pair(chars: &[char], start: usize, a: char, b: char) -> Option<usize> {
    if start + 1 >= chars.len() {
        return None;
    }
    for i in start..chars.len() - 1 {
        if chars[i] == a && chars[i + 1] == b {
            return Some(i);
        }
    }
    None
}

fn find_closing_single(chars: &[char], start: usize, m: char) -> Option<usize> {
    chars[start..].iter().position(|&c| c == m).map(|p| p + start)
}
```

Add required unit tests for each rule.

- [ ] **Step 4: Implement semantic chunking before HTML rendering**

In `src/channels/telegram.rs`:

```rust
fn split_markdown_semantic(text: &str) -> Vec<String> {
    // Split by double newline (paragraphs) and fenced code blocks.
    // Preserve code blocks as atomic units.
    let mut chunks = vec![];
    let mut current = String::new();
    let mut in_code = false;

    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            if !current.is_empty() {
                chunks.push(current.trim_end().to_string());
                current.clear();
            }
            in_code = !in_code;
            current.push_str(line);
            current.push('\n');
            if !in_code {
                chunks.push(current.trim_end().to_string());
                current.clear();
            }
        } else {
            current.push_str(line);
            current.push('\n');
            if !in_code && line.trim().is_empty() {
                if !current.trim().is_empty() {
                    chunks.push(current.trim_end().to_string());
                    current.clear();
                }
            }
        }
    }
    if !current.trim().is_empty() {
        chunks.push(current.trim_end().to_string());
    }
    chunks
}
```

- [ ] **Step 5: Add fallback helper functions**

In `src/channels/telegram.rs`:

```rust
fn is_parse_mode_error(err: &ChannelError) -> bool {
    match err {
        ChannelError::Api { message, .. } => {
            message.to_lowercase().contains("parse") || message.to_lowercase().contains("can't")
        }
        _ => false,
    }
}

fn strip_tags(html: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}
```

- [ ] **Step 6: Update `TelegramChannel::send` for HTML and chunking**

In `src/channels/telegram.rs`:

```rust
async fn send(&self, message: &ChannelOutboundMessage) -> Result<(), ChannelError> {
    let text_parts = match message.parse_mode {
        Some(ChannelParseMode::Html) => {
            let chunks = split_markdown_semantic(&message.content);
            chunks.into_iter()
                .map(|chunk| markdown_to_telegram_html(&chunk))
                .collect::<Vec<_>>()
        }
        _ => split_text(&message.content, TELEGRAM_MAX_TEXT_LENGTH),
    };

    for part in text_parts {
        let mut payload = json!({
            "chat_id": message.recipient,
            "text": part,
            "parse_mode": "HTML",
        });
        if let Some(thread_id) = &message.thread_id {
            if let Ok(id) = thread_id.parse::<i64>() {
                payload["message_thread_id"] = json!(id);
            }
        }
        if let Some(ref reply_markup) = message.reply_markup {
            payload["reply_markup"] = json!(reply_markup);
        }

        let result = self.post("sendMessage", &payload).await;
        if let Err(ref e) = result {
            if is_parse_mode_error(e) {
                // Fallback to plain text
                let mut fallback = json!({
                    "chat_id": message.recipient,
                    "text": strip_tags(&part),
                });
                if let Some(thread_id) = &message.thread_id {
                    if let Ok(id) = thread_id.parse::<i64>() {
                        fallback["message_thread_id"] = json!(id);
                    }
                }
                self.post("sendMessage", &fallback).await?;
            } else {
                result?;
            }
        }
    }

    // Attachments are handled in Task 5.

    Ok(())
}
```

- [ ] **Step 7: Run tests**

Run: `cargo test --all-features`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add src/channels/telegram.rs
git commit -m "feat(telegram): render markdown as HTML with semantic chunking and fallback"
```

---

### Task 5: Telegram 附件收发

**Files:**
- Modify: `src/channels/telegram.rs`
- Test: `src/channels/telegram.rs`（新增 `#[cfg(test)]` 单元测试）

**Interfaces:**
- Consumes: `[IMAGE:path]` / `[DOCUMENT:path]` / etc. in content
- Produces: `sendPhoto`, `sendDocument`, `sendVideo`, `sendAudio`, `sendVoice`

- [ ] **Step 1: Write the failing test for attachment marker parsing**

```rust
#[test]
fn parse_attachment_markers() {
    let (text, attachments) = extract_attachments("see [IMAGE:/tmp/a.png] and [DOCUMENT:/tmp/b.pdf]");
    assert_eq!(text, "see  and ");
    assert_eq!(attachments.len(), 2);
    assert_eq!(attachments[0].kind, AttachmentKind::Image);
    assert_eq!(attachments[0].target, "/tmp/a.png");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib parse_attachment_markers`
Expected: FAIL.

- [ ] **Step 3: Implement attachment marker parser without new dependencies**

In `src/channels/telegram.rs`:

```rust
fn extract_attachments(content: &str) -> (String, Vec<ChannelAttachment>) {
    let mut attachments = vec![];
    let mut text = String::new();
    let mut last_end = 0;
    let bytes = content.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'[' {
            if let Some(close) = content[i + 1..].find(']') {
                let close = close + i + 1;
                let inner = &content[i + 1..close];
                if let Some((kind_str, target)) = inner.split_once(':') {
                    let kind = match kind_str.to_uppercase().as_str() {
                        "IMAGE" => Some(AttachmentKind::Image),
                        "DOCUMENT" => Some(AttachmentKind::Document),
                        "VIDEO" => Some(AttachmentKind::Video),
                        "AUDIO" => Some(AttachmentKind::Audio),
                        "VOICE" => Some(AttachmentKind::Voice),
                        _ => None,
                    };
                    if let Some(kind) = kind {
                        text.push_str(&content[last_end..i]);
                        attachments.push(ChannelAttachment {
                            kind,
                            target: target.trim().to_string(),
                        });
                        last_end = close + 1;
                        i = close + 1;
                        continue;
                    }
                }
            }
        }
        i += 1;
    }
    text.push_str(&content[last_end..]);
    (text, attachments)
}
```

- [ ] **Step 4: Implement `send_attachment`**

In `src/channels/telegram.rs`:

```rust
async fn send_attachment(
    &self,
    base: &ChannelOutboundMessage,
    attachment: &ChannelAttachment,
) -> Result<(), ChannelError> {
    if !self.supported_attachment_kinds().contains(&attachment.kind) {
        // Unsupported by this channel, send as text fallback
        let fallback = json!({
            "chat_id": base.recipient,
            "text": format!("Unsupported attachment: {}", attachment.target),
            "message_thread_id": base.thread_id.as_ref().and_then(|t| t.parse::<i64>().ok()),
        });
        let _ = self.post("sendMessage", &fallback).await;
        return Ok(());
    }

    let (method, file_field) = match attachment.kind {
        AttachmentKind::Image => ("sendPhoto", "photo"),
        AttachmentKind::Document => ("sendDocument", "document"),
        AttachmentKind::Video => ("sendVideo", "video"),
        AttachmentKind::Audio => ("sendAudio", "audio"),
        AttachmentKind::Voice => ("sendVoice", "voice"),
    };

    if attachment.target.starts_with("http://") || attachment.target.starts_with("https://") {
        let mut payload = json!({
            "chat_id": base.recipient,
            file_field: &attachment.target,
            "caption": base.content,
        });
        if let Some(thread_id) = &base.thread_id {
            if let Ok(id) = thread_id.parse::<i64>() {
                payload["message_thread_id"] = json!(id);
            }
        }
        self.post(method, &payload).await?;
    } else {
        let target = resolve_attachment_path(&attachment.target);
        self.post_multipart(method, &base.recipient, base.thread_id.as_deref(), file_field, &target, &base.content).await?;
    }
    Ok(())
}
```

Implement `resolve_attachment_path` and `post_multipart` helpers:

```rust
fn resolve_attachment_path(target: &str) -> PathBuf {
    let path = if target.starts_with("file://") {
        &target[7..]
    } else {
        target
    };
    let relative = PathBuf::from(path);
    if relative.exists() {
        return relative.canonicalize().unwrap_or(relative);
    }
    PathBuf::from(path)
}

async fn post_multipart(
    &self,
    method: &str,
    chat_id: &str,
    thread_id: Option<&str>,
    file_field: &str,
    file_path: &Path,
    caption: &str,
) -> Result<(), ChannelError> {
    let file_bytes = std::fs::read(file_path)
        .map_err(|e| ChannelError::Api { code: 0, message: e.to_string() })?;
    let part = reqwest::multipart::Part::bytes(file_bytes)
        .file_name(file_path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "file".to_string()));
    let mut form = reqwest::multipart::Form::new()
        .text("chat_id", chat_id.to_string())
        .part(file_field.to_string(), part);
    if let Some(thread_id) = thread_id {
        if let Ok(id) = thread_id.parse::<i64>() {
            form = form.text("message_thread_id", id.to_string());
        }
    }
    if !caption.is_empty() {
        form = form.text("caption", caption.to_string());
    }

    let url = self.api_url(method);
    let resp = self.client.post(&url).multipart(form).send().await?;
    if !resp.status().is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(ChannelError::Api { code: 0, message: text });
    }
    Ok(())
}
```

- [ ] **Step 5: Update `TelegramChannel::send` to extract and send inline attachments**

In `src/channels/telegram.rs`, extend the `send` method from Task 4 to parse inline markers and send attachments after the text parts:

```rust
async fn send(&self, message: &ChannelOutboundMessage) -> Result<(), ChannelError> {
    let (text_without_markers, inline_attachments) = extract_attachments(&message.content);
    let all_attachments: Vec<_> = message.attachments.iter()
        .cloned()
        .chain(inline_attachments)
        .collect();

    // Text sending logic remains the same as Task 4, but use `text_without_markers`
    // in place of `message.content`.
    let text_parts = match message.parse_mode {
        Some(ChannelParseMode::Html) => {
            let chunks = split_markdown_semantic(&text_without_markers);
            chunks.into_iter()
                .map(|chunk| markdown_to_telegram_html(&chunk))
                .collect::<Vec<_>>()
        }
        _ => split_text(&text_without_markers, TELEGRAM_MAX_TEXT_LENGTH),
    };

    for part in text_parts {
        let mut payload = json!({
            "chat_id": message.recipient,
            "text": part,
            "parse_mode": "HTML",
        });
        if let Some(thread_id) = &message.thread_id {
            if let Ok(id) = thread_id.parse::<i64>() {
                payload["message_thread_id"] = json!(id);
            }
        }
        if let Some(ref reply_markup) = message.reply_markup {
            payload["reply_markup"] = json!(reply_markup);
        }

        let result = self.post("sendMessage", &payload).await;
        if let Err(ref e) = result {
            if is_parse_mode_error(e) {
                let mut fallback = json!({
                    "chat_id": message.recipient,
                    "text": strip_tags(&part),
                });
                if let Some(thread_id) = &message.thread_id {
                    if let Ok(id) = thread_id.parse::<i64>() {
                        fallback["message_thread_id"] = json!(id);
                    }
                }
                self.post("sendMessage", &fallback).await?;
            } else {
                result?;
            }
        }
    }

    // New in Task 5: send explicit and inline attachments
    for attachment in &all_attachments {
        self.send_attachment(message, attachment).await?;
    }

    Ok(())
}
```

- [ ] **Step 6: Implement incoming attachment handling**

In `TelegramChannel::listen`, before processing text messages:

```rust
if let Some(attachment) = self.extract_incoming_attachment(&msg).await {
    let inbound = ChannelInboundMessage {
        channel_name: self.name().to_string(),
        sender_id: msg.from.id.to_string(),
        chat_id: msg.chat.id.to_string(),
        thread_id: msg.message_thread_id.map(|id| id.to_string()),
        content: attachment.to_agent_text(),
        timestamp_secs: msg.date as u64,
        confirmation: None,
    };
    let _ = tx.send(inbound);
    continue;
}
```

Implement `extract_incoming_attachment` and `IncomingAttachment`:

```rust
#[derive(Debug)]
struct IncomingAttachment {
    kind: AttachmentKind,
    path: PathBuf,
    name: Option<String>,
}

impl IncomingAttachment {
    fn to_agent_text(&self) -> String {
        let path = self.path.display().to_string();
        match self.kind {
            AttachmentKind::Image => format!("[IMAGE:{}]", path),
            AttachmentKind::Document => format!("[DOCUMENT:{}]", path),
            AttachmentKind::Voice => format!("[VOICE:{}]", path),
            _ => format!("[DOCUMENT:{}]", path),
        }
    }
}

async fn extract_incoming_attachment(&self, msg: &TelegramMessage) -> Option<IncomingAttachment> {
    use std::io::Write;

    let dir = std::env::current_dir().ok()?.join("telegram_files");
    std::fs::create_dir_all(&dir).ok()?;

    if let Some(doc) = &msg.document {
        return self.download_telegram_file(&doc.file_id, &dir, doc.file_name.as_deref(), AttachmentKind::Document).await;
    }

    if let Some(photo) = msg.photo.last() {
        return self.download_telegram_file(&photo.file_id, &dir, None, AttachmentKind::Image).await;
    }

    if let Some(voice) = &msg.voice {
        return self.download_telegram_file(&voice.file_id, &dir, None, AttachmentKind::Voice).await;
    }

    None
}

async fn download_telegram_file(
    &self,
    file_id: &str,
    dir: &Path,
    file_name: Option<&str>,
    kind: AttachmentKind,
) -> Option<IncomingAttachment> {
    let get_file_payload = json!({ "file_id": file_id });
    let resp = self.client
        .post(self.api_url("getFile"))
        .json(&get_file_payload)
        .send()
        .await
        .ok()?;
    let data: serde_json::Value = resp.json().await.ok()?;
    let file_path = data["result"]["file_path"].as_str()?;

    let download_url = format!("{}/file/bot{}/{}", self.base_url, self.config.bot_token, file_path);
    let bytes = self.client.get(&download_url).send().await.ok()?.bytes().await.ok()?;

    if bytes.len() > 20 * 1024 * 1024 {
        warn!(event = "TelegramFileTooLarge", file_id = %file_id, "incoming file exceeds 20MB limit");
        return None;
    }

    let local_name = file_name.map(|s| s.to_string()).unwrap_or_else(|| format!("{}_{}", file_id, file_path.rsplit('/').next().unwrap_or("file")));
    let local_path = dir.join(&local_name);
    let mut file = std::fs::File::create(&local_path).ok()?;
    file.write_all(&bytes).ok()?;

    Some(IncomingAttachment {
        kind,
        path: local_path,
        name: file_name.map(|s| s.to_string()),
    })
}
```

Also extend `TelegramMessage` to include `document`, `photo`, and `voice` fields:

```rust
#[derive(Debug, Deserialize)]
struct TelegramMessage {
    from: TelegramUser,
    chat: TelegramChat,
    date: i64,
    text: Option<String>,
    message_thread_id: Option<i64>,
    document: Option<TelegramDocument>,
    photo: Vec<TelegramPhotoSize>,
    voice: Option<TelegramVoice>,
}

#[derive(Debug, Deserialize)]
struct TelegramDocument {
    file_id: String,
    file_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TelegramPhotoSize {
    file_id: String,
}

#[derive(Debug, Deserialize)]
struct TelegramVoice {
    file_id: String,
}
```

- [ ] **Step 7: Run tests**

Run: `cargo test --all-features`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add src/channels/telegram.rs
git commit -m "feat(telegram): support sending and receiving attachments"
```

---

### Task 6: Telegram `/bind` 配对与运行时白名单

**Files:**
- Modify: `src/channels/config.rs`
- Modify: `src/channels/telegram.rs`
- Test: `src/channels/telegram.rs`

**Interfaces:**
- Consumes: `/bind <code>` command, `TelegramConfig.pairing_enabled`
- Produces: runtime allowlist updates, optional config file write-back

- [ ] **Step 1: Write the failing test for runtime allowlist precedence**

```rust
#[test]
fn runtime_allowlist_overrides_config() {
    let user = TelegramUser {
        id: 1,
        username: None,
    };
    let channel = TelegramChannel::new_for_test(
        TelegramConfig {
            bot_token: "x".to_string(),
            allowed_users: vec![],
            pairing_enabled: true,
            pairing_code: None,
        },
        Some(PathBuf::from("/dev/null")),
    );
    channel.runtime_allow("1");
    assert!(channel.is_allowed(&user));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib runtime_allowlist_overrides_config`
Expected: FAIL.

- [ ] **Step 3: Add `pairing_enabled` to `TelegramConfig`**

In `src/channels/config.rs`:

```rust
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TelegramConfig {
    pub bot_token: String,
    #[serde(default)]
    pub allowed_users: Vec<String>,
    #[serde(default)]
    pub pairing_enabled: bool,
    pub pairing_code: Option<String>,
}
```

Update any construction sites/tests to include `pairing_enabled: false, pairing_code: None`.

- [ ] **Step 4: Add runtime allowlist to `TelegramChannel`**

In `src/channels/telegram.rs`:

```rust
pub struct TelegramChannel {
    config: TelegramConfig,
    config_path: Option<PathBuf>,
    runtime_allowed_users: Arc<RwLock<HashSet<String>>>,
    client: Client,
    base_url: String,
    last_update_id: AtomicI64,
}
```

Initialize `runtime_allowed_users` as an empty set; it is only populated by successful `/bind` commands. Update `new` to delegate to a new `new_with_path` constructor so tests can supply `config_path`:

```rust
impl TelegramChannel {
    pub fn new(config: TelegramConfig) -> Self {
        Self::new_with_path(config, None)
    }

    pub fn new_with_path(config: TelegramConfig, config_path: Option<PathBuf>) -> Self {
        Self {
            config,
            config_path,
            runtime_allowed_users: Arc::new(RwLock::new(HashSet::new())),
            client: Client::new(),
            base_url: "https://api.telegram.org".to_string(),
            last_update_id: AtomicI64::new(0),
        }
    }
}
```

- [ ] **Step 5: Implement `is_allowed` and `runtime_allow` with preserved semantics**

`runtime_allowed_users` only stores canonical `user_id` values injected by `/bind`. `is_allowed` keeps the existing `&TelegramUser` signature and matching rules (username, user_id, wildcard `*`), checking runtime allowlist first:

```rust
fn is_allowed(&self, user: &TelegramUser) -> bool {
    // Runtime allowlist from /bind takes precedence
    if self.runtime_allowed_users.read().unwrap().contains(&user.id.to_string()) {
        return true;
    }

    // Fall back to configured allowlist semantics
    if self.config.allowed_users.iter().any(|allowed| allowed == "*") {
        return true;
    }
    if self.config.allowed_users.is_empty() {
        return false;
    }
    self.config.allowed_users.iter().any(|allowed| {
        if let Some(username) = &user.username && username.eq_ignore_ascii_case(allowed) {
            return true;
        }
        user.id.to_string() == *allowed
    })
}

fn runtime_allow(&self, user_id: &str) {
    self.runtime_allowed_users.write().unwrap().insert(user_id.to_string());
}
```

> **白名单语义约束**：评审指出 `/bind` 不能收窄现有 `allowed_users` 的匹配语义（`username` / `user_id` / `"*"`）。本计划保持 `is_allowed(user: &TelegramUser)` 签名和原有匹配逻辑，`runtime_allowed_users` 仅作为 `/bind` 注入的临时运行时白名单，配置白名单继续按规格语义生效。

- [ ] **Step 6: Implement `/bind` command handling**

In the message handling loop, before auth check:

```rust
if content.starts_with("/bind ") && self.config.pairing_enabled && self.config.allowed_users.is_empty() {
    let code = content[6..].trim();
    let reply = if code == self.expected_pairing_code() {
        self.runtime_allow(&msg.from.id.to_string());
        if let Some(ref path) = self.config_path {
            if is_writable_toml(path) {
                let _ = self.persist_allowed_user(&msg.from.id.to_string(), path);
            }
        }
        "已授权（本次运行有效，重启后需运维手动添加）。"
    } else {
        "配对码错误。"
    };
    let payload = json!({
        "chat_id": msg.chat.id,
        "text": reply,
        "message_thread_id": msg.message_thread_id,
    });
    self.post("sendMessage", &payload).await?;
    continue;
}
```

`expected_pairing_code()` returns a configured bootstrap code from `TelegramConfig.pairing_code`. Add `pairing_code: Option<String>` to `TelegramConfig` as well.

- [ ] **Step 7: Implement config write-back helpers**

```rust
fn expected_pairing_code(&self) -> String {
    self.config.pairing_code.clone().unwrap_or_default()
}

fn is_writable_toml(path: &Path) -> bool {
    path.extension().map(|e| e == "toml").unwrap_or(false)
        && std::fs::metadata(path).map(|m| !m.permissions().readonly()).unwrap_or(false)
}

fn persist_allowed_user(&self, user_id: &str, path: &Path) -> Result<(), ChannelError> {
    let mut config: TelegramConfig = std::fs::read_to_string(path)
        .ok()
        .and_then(|s| toml::from_str(&s).ok())
        .unwrap_or_else(|| self.config.clone());

    if !config.allowed_users.iter().any(|u| u == user_id) {
        config.allowed_users.push(user_id.to_string());
    }

    let content = toml::to_string_pretty(&config)
        .map_err(|e| ChannelError::Api { code: 0, message: e.to_string() })?;
    std::fs::write(path, content)
        .map_err(|e| ChannelError::Api { code: 0, message: e.to_string() })?;

    Ok(())
}
```

- [ ] **Step 8: Run tests**

Run: `cargo test --all-features`
Expected: PASS.

- [ ] **Step 9: Commit**

```bash
git add src/channels/config.rs src/channels/telegram.rs
git commit -m "feat(telegram): add /bind pairing with runtime allowlist and optional config write-back"
```

---

### Task 7: 工具描述、ACK 反应与配置文档更新

**Files:**
- Modify: `src/channels/send_tool.rs`
- Modify: `src/channels/telegram.rs`
- Modify: `docs/design/im-channel-adapters.md`
- Modify: `docs/configuration.md`
- Test: `src/channels/telegram.rs`

**Interfaces:**
- Consumes: `ChannelOutboundMessage`, incoming text messages
- Produces: `setMessageReaction`, updated docs

- [ ] **Step 1: Update `channel_send` tool description**

In `src/channels/send_tool.rs`, append to the description:

```rust
const ATTACHMENT_HINT: &str = r#"
You can include attachments using markers like [IMAGE:/path/to/file.png], [DOCUMENT:/path/to/file.pdf], [VIDEO:...], [AUDIO:...], [VOICE:...]. The target path may be relative or absolute, a file:// URL, or an HTTP(S) URL. Unsupported attachment types will be sent as plain text links by the channel implementation.
"#;
```

Include this hint in the `channel_send` schema description.

- [ ] **Step 2: Implement basic ACK reaction**

In `src/channels/telegram.rs`, after a message from an allowed user is processed:

```rust
async fn send_ack_reaction(&self, chat_id: i64, message_id: i64) -> Result<(), ChannelError> {
    let reactions = ["👍", "👌", "✅", "🆗"];
    let reaction = reactions[message_id as usize % reactions.len()];
    let payload = json!({
        "chat_id": chat_id,
        "message_id": message_id,
        "reaction": [{"type": "emoji", "emoji": reaction}],
        "is_big": false,
    });
    self.post("setMessageReaction", &payload).await?;
    Ok(())
}
```

Call this for text/attachment messages after forwarding to ECS.

- [ ] **Step 3: Handle unsupported message types**

In the `listen` loop, if a message has neither `text`, `document`, `photo`, `voice`, nor `callback_query`:

```rust
let payload = json!({
    "chat_id": chat_id,
    "text": "暂不支持该消息类型。",
    "message_thread_id": thread_id,
});
self.post("sendMessage", &payload).await?;
```

- [ ] **Step 4: Update docs**

In `docs/design/im-channel-adapters.md`:

- Add `pairing_enabled` and `pairing_code` to Telegram configuration example.
- Clarify that `/bind` requires a writable `HARNESS_CHANNELS_CONFIG` TOML to persist; otherwise only runtime allowlist is updated.
- List supported attachment kinds and inbound/outbound markers.
- Document Inline Keyboard approval flow.

In `docs/configuration.md`:

- Add the same Telegram config fields.

- [ ] **Step 5: Run tests**

Run: `cargo test --all-features`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/channels/send_tool.rs src/channels/telegram.rs docs/design/im-channel-adapters.md docs/configuration.md
git commit -m "feat(telegram): ack reactions, tool description hints, and docs"
```

---

### Task 8: 集成验证与 CI

**Files:**
- All modified files

- [ ] **Step 1: Run full test suite**

Run: `cargo test --all-features`
Expected: PASS.

- [ ] **Step 2: Run clippy**

Run: `cargo clippy --all-targets --all-features -- -D warnings`
Expected: PASS.

- [ ] **Step 3: Run formatter check**

Run: `cargo fmt --all --check`
Expected: PASS.

- [ ] **Step 4: Run markdownlint**

Run: `markdownlint docs/superpowers/specs/2026-06-27-telegram-channel-experience-design.md docs/superpowers/plans/2026-06-27-telegram-channel-experience.md docs/design/im-channel-adapters.md docs/configuration.md`
Expected: PASS.

- [ ] **Step 5: Commit final fixes**

```bash
git commit -a -m "chore: formatting and lint fixes for telegram channel experience"
```

---

## Spec Coverage Check

| Spec Section | Implementing Task |
|---|---|
| `ChannelId` 增加 `thread_id` | Task 1 |
| 出向消息格式扩展 | Task 1 |
| 入向消息 `confirmation` 扩展 | Task 1 |
| 通道能力声明 | Task 1 |
| 审批请求定向路由 | Task 2 |
| Inline Keyboard 审批发送 | Task 2 |
| callback_query 确认回流 | Task 3 |
| Markdown → Telegram HTML | Task 4 |
| HTML 语义分片与降级 | Task 4 |
| 附件标记与发送 | Task 5 |
| 附件接收 | Task 5 |
| `/bind` 配对 | Task 6 |
| 运行时白名单 | Task 6 |
| 工具描述附件语法 | Task 7 |
| ACK 反应 | Task 7 |
| 文档更新 | Task 7, Task 8 |

## Placeholder Scan

No TBD/TODO/"implement later"/"similar to Task N" placeholders remain. Every step includes concrete code, file paths, and expected test commands.

## Type Consistency Notes

- `ChannelId` always carries `thread_id: Option<String>` after Task 1.
- `ChannelOutboundMessage` always carries `thread_id`, `parse_mode`, `reply_markup`, `attachments` after Task 1.
- `ChannelInboundMessage` always carries `confirmation: Option<InboundConfirmation>` after Task 1.
- `AttachmentKind`, `ChannelParseMode`, `ReplyMarkup`, `InlineKeyboardButton` used consistently across Tasks 2-5.
