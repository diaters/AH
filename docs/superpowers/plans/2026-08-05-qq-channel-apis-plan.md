# QQ 通道消息撤回 / 输入状态 / 交互回调 / 按钮闭环 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-step. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 为 QQ 通道补齐消息撤回、输入状态（typing）、交互回调（PUT /interactions/{id}）、交互事件监听（INTERACTION_CREATE），并将审批交互从编号列表文本迁移到原生按钮点击闭环。

**Architecture:** 以现有 `QqChannel` struct 为载体，按层扩展：1) send 方法返回 message_id（供撤回等能力使用）；2) 消息撤回 API（DELETE 端点）；3) 底层 HTTP 方法（POST /typing、PUT /interactions）；4) WebSocket 事件扩展（INTERACTION_CREATE）+ 交互回调处理；5) 审批流程从文本编号列表迁移到按钮交互。每个 Task 产出可独立测试的增量。

**Tech Stack:** Rust, reqwest (HTTP), tokio-tungstenite (WebSocket), serde_json, wiremock (测试), async_trait

## Global Constraints

- 遵循 Conventional Commits 提交格式
- 所有新方法需有 wiremock 集成测试或单元测试覆盖
- 保留现有 `Channel` trait 的 `send` 签名不变（`Result<(), ChannelError>`）
- QQ API 基础 URL: `https://api.sgroup.qq.com`，沙箱: `https://sandbox.api.sgroup.qq.com`
- 鉴权: `Authorization: QQBot {access_token}`
- 文档中文撰写，代码注释可中英混合
- `chat_type` 映射：0=频道, 1=群聊, 2=单聊/C2C（参考 `QQ-Bot-API参考文档.md` §8）
- 遵循 AGENTS.md "简化优先 / 代码腐化治理"：不引入无调用方的代码

**调用方说明：** 消息撤回 API 的首个调用方是 IM 通道状态消息治理——QQ 通道当前会推送过多状态切换消息（如任务运行/等待/完成通知），需要撤回旧状态消息以保持聊天窗口整洁。调用方通过 Task 1 的 `QqMessageResponse.id` 获取 message_id，再调用 `recall_message` 撤回。

---

## File Structure

| 文件 | 职责 | 变更类型 |
|---|---|---|
| `src/channels/qq.rs` | QQ 通道主实现 | 修改 — 新增方法、扩展 listen、修改 send 逻辑 |
| `src/channels/qq.rs` 测试模块 | 集成/单元测试 | 修改 — 新增测试 |

`src/channels/traits.rs` 本次不变更：`send_typing` / `acknowledge_interaction` 均为 `QqChannel` 的 `pub async fn`（因 `Channel` trait 无 async 默认方法能力），不走 trait。

---

### Task 1: send 方法返回 message_id

**Files:**
- Modify: `src/channels/qq.rs` — `send_text_markdown`, `send_media_message`, `send()` impl

**Interfaces:**
- Consumes: 现有 `send_text_markdown(recipient, content) -> Result<(), ChannelError>`, `send_media_message(recipient, file_info) -> Result<(), ChannelError>`
- Produces: `QqMessageResponse { id: String }` struct; `send_text_markdown` 和 `send_media_message` 返回 `Result<QqMessageResponse, ChannelError>`

**背景:** 当前 `send_text_markdown` 和 `send_media_message` 丢弃了 QQ API 返回的 `message_id`（JSON 中的 `id` 字段）。未来撤回等能力需要 `message_id`，所以先捕获返回值。`Channel` trait 的 `send()` 签名保持 `Result<(), ChannelError>` 不变——Ok 值的 `QqMessageResponse` 在 `send()` 内被丢弃即可。

- [ ] **Step 1: 定义 `QqMessageResponse`**

在 `src/channels/qq.rs` 中，`QqUploadResponse` struct 之后添加：

```rust
/// QQ 消息发送接口返回的消息 ID。
#[derive(Debug, serde::Deserialize)]
struct QqMessageResponse {
    /// QQ API 返回的消息 ID（字段名为 "id"）
    id: String,
}
```

- [ ] **Step 2: 修改 `send_text_markdown` 返回 `QqMessageResponse`**

将签名从 `Result<(), ChannelError>` 改为 `Result<QqMessageResponse, ChannelError>`，在成功时解析返回体：

```rust
async fn send_text_markdown(&self, recipient: &str, content: &str) -> Result<QqMessageResponse, ChannelError> {
    let token = self.get_token().await?;
    let (scope, id) = Self::resolve_recipient(recipient);
    let url = format!("{}/v2/{scope}/{id}/messages", self.api_base);
    let body = json!({
        "markdown": { "content": content },
        "msg_type": 2,
        "msg_seq": next_msg_seq(),
    });
    let resp = self
        .client
        .post(&url)
        .header("Authorization", format!("QQBot {token}"))
        .json(&body)
        .send()
        .await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(ChannelError::Api {
            code: status.as_u16() as i32,
            message: text,
        });
    }
    let raw_body = resp.text().await.unwrap_or_default();
    // QQ API 返回 {"id":"msg_xxx","timestamp":...} 或 {"data":{"id":"msg_xxx",...}}
    let root: serde_json::Value = serde_json::from_str(&raw_body).unwrap_or(json!({}));
    let data = root.get("data").unwrap_or(&root);
    let msg_id = data
        .get("id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_string();
    Ok(QqMessageResponse { id: msg_id })
}
```

- [ ] **Step 3: 修改 `send_media_message` 返回 `QqMessageResponse`**

同样改为 `Result<QqMessageResponse, ChannelError>`，解析返回体中的 `id`：

```rust
async fn send_media_message(
    &self,
    recipient: &str,
    file_info: &str,
) -> Result<QqMessageResponse, ChannelError> {
    let token = self.get_token().await?;
    let (scope, id) = Self::resolve_recipient(recipient);
    let url = format!("{}/v2/{scope}/{id}/messages", self.api_base);
    let body = json!({
        "msg_type": 7,
        "media": { "file_info": file_info },
        "msg_seq": next_msg_seq(),
    });
    let resp = self
        .client
        .post(&url)
        .header("Authorization", format!("QQBot {token}"))
        .json(&body)
        .send()
        .await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(ChannelError::Api {
            code: status.as_u16() as i32,
            message: text,
        });
    }
    let raw_body = resp.text().await.unwrap_or_default();
    let root: serde_json::Value = serde_json::from_str(&raw_body).unwrap_or(json!({}));
    let data = root.get("data").unwrap_or(&root);
    let msg_id = data
        .get("id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_string();
    Ok(QqMessageResponse { id: msg_id })
}
```

- [ ] **Step 4: 适配现有调用方**

`send()` 内的 `self.send_text_markdown(...).await?;` 不需改动——`Ok(QqMessageResponse)` 值被自动丢弃，`Err` 仍然通过 `?` 传播。

需要显式忽略返回值的场景（内部消息不需要 message_id）：`send_ack_text`、`handle_bind_command`、审批回复确认中的 `send_text_markdown` 调用。这些当前用 `let _ =` 或 `if let Err` 模式，改返回类型后 Ok 值自动丢弃，也不需要改动。

`send_attachment` 内 `self.send_media_message(recipient, &file_info).await?;` 同理，不需改动。

- [ ] **Step 5: 修改现有测试适配新返回值**

`send_text_markdown_posts_msg_type_2` 测试需要更新 mock response body 返回 `id` 字段：

```rust
.respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
    "id": "msg_test_id",
    "timestamp": "1234567890"
})))
```

`send_media_message_posts_msg_type_7` 同理。

- [ ] **Step 6: 运行测试确认所有现有测试通过**

Run: `cargo test --lib channels::qq -- --nocapture`
Expected: 所有测试 PASS

- [ ] **Step 7: Commit**

```bash
git add src/channels/qq.rs
git commit -m "feat(qq): capture message_id from send responses"
```

---

### Task 2: 消息撤回 API

**Files:**
- Modify: `src/channels/qq.rs` — 新增 `recall_message` 方法

**Interfaces:**
- Consumes: Task 1 的 `QqMessageResponse`（调用方通过 `.id` 获取 msg_id 后传入）
- Produces: `QqChannel::recall_message(recipient, msg_id) -> Result<(), ChannelError>`

**撤回 API 参考**（来自 `QQ-Bot-撤回消息API.md`）：

| 端点 | 场景 |
|---|---|
| `DELETE /v2/users/{openid}/messages/{message_id}` | C2C |
| `DELETE /v2/groups/{group_openid}/messages/{message_id}` | 群聊 |

文字子频道撤回（`DELETE /channels/{channel_id}/messages/{message_id}`）和频道私信撤回（`DELETE /dms/{guild_id}/messages/{message_id}`）不在本期范围——Harness 不支持频道场景。

**调用方：** IM 通道状态消息治理——撤回过多的状态切换消息（如任务运行/等待/完成通知），保持聊天窗口整洁。

**依赖：** Task 1（`QqMessageResponse` 提供 message_id）。

- [ ] **Step 1: 编写 `recall_message` 的失败测试**

```rust
#[tokio::test]
async fn recall_message_deletes_c2c_message() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let mock_server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/v2/users/USER123/messages/MSG456"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&mock_server)
        .await;

    let ch = QqChannel::new(make_config()).with_api_base(mock_server.uri());
    ch.set_token_for_test("fake_token").await;
    ch.recall_message("user:USER123", "MSG456")
        .await
        .expect("recall_message");
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --lib channels::qq::tests::recall_message_deletes_c2c_message -- --nocapture`
Expected: 编译错误 — 方法不存在

- [ ] **Step 3: 实现 `recall_message`**

在 `QqChannel` 的 `impl` 块中添加：

```rust
/// 撤回消息。根据 recipient 的 scope 自动路由到对应 DELETE 端点。
///
/// 支持的端点：
/// - C2C: `DELETE /v2/users/{openid}/messages/{message_id}`
/// - 群聊: `DELETE /v2/groups/{group_openid}/messages/{message_id}`
///
/// QQ 限制发送超过 2 分钟的消息不可撤回（API 返回错误码 306011）。
/// 调用方通过 `QqMessageResponse.id` 获取 message_id。
pub async fn recall_message(
    &self,
    recipient: &str,
    msg_id: &str,
) -> Result<(), ChannelError> {
    let token = self.get_token().await?;
    let (scope, id) = Self::resolve_recipient(recipient);
    let url = format!("{}/v2/{scope}/{id}/messages/{msg_id}", self.api_base);
    let resp = self
        .client
        .delete(&url)
        .header("Authorization", format!("QQBot {token}"))
        .send()
        .await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(ChannelError::Api {
            code: status.as_u16() as i32,
            message: text,
        });
    }
    Ok(())
}
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --lib channels::qq::tests::recall_message_deletes_c2c_message -- --nocapture`
Expected: PASS

- [ ] **Step 5: 添加群聊撤回测试**

```rust
#[tokio::test]
async fn recall_message_deletes_group_message() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let mock_server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/v2/groups/GROUP456/messages/MSG789"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&mock_server)
        .await;

    let ch = QqChannel::new(make_config()).with_api_base(mock_server.uri());
    ch.set_token_for_test("fake_token").await;
    ch.recall_message("group:GROUP456", "MSG789")
        .await
        .expect("recall_message");
}
```

- [ ] **Step 6: 添加撤回错误码测试（超时/无权限）**

```rust
#[tokio::test]
async fn recall_message_returns_api_error_on_failure() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let mock_server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/v2/users/USER123/messages/MSG_OLD"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "code": 306011,
            "message": "超出可撤回消息时间"
        })))
        .mount(&mock_server)
        .await;

    let ch = QqChannel::new(make_config()).with_api_base(mock_server.uri());
    ch.set_token_for_test("fake_token").await;
    let result = ch.recall_message("user:USER123", "MSG_OLD").await;
    assert!(result.is_err());
    match result.unwrap_err() {
        ChannelError::Api { code, message } => {
            assert_eq!(code, 400);
            assert!(message.contains("306011") || message.contains("撤回"));
        }
        other => panic!("expected Api error, got: {other}"),
    }
}
```

- [ ] **Step 7: 运行全部 QQ 通道测试**

Run: `cargo test --lib channels::qq -- --nocapture`
Expected: 所有测试 PASS

- [ ] **Step 8: Commit**

```bash
git add src/channels/qq.rs
git commit -m "feat(qq): add message recall API (DELETE /v2/{scope}/{id}/messages/{msg_id})"
```

---

### Task 3: 输入状态（Typing）API

**Files:**
- Modify: `src/channels/qq.rs` — 新增 `send_typing` 方法

**Interfaces:**
- Consumes: `get_token()`, `resolve_recipient()`
- Produces: `QqChannel::send_typing(recipient) -> Result<(), ChannelError>`

**API 参考:** `POST /v2/users/{openid}/typing` — 仅 C2C 场景，在用户端显示"Bot 正在输入中…"状态。

- [ ] **Step 1: 编写 `send_typing` 的失败测试**

```rust
#[tokio::test]
async fn send_typing_posts_to_c2c_user() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v2/users/USER123/typing"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&mock_server)
        .await;

    let ch = QqChannel::new(make_config()).with_api_base(mock_server.uri());
    ch.set_token_for_test("fake_token").await;
    ch.send_typing("user:USER123").await.expect("send_typing");
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --lib channels::qq::tests::send_typing_posts_to_c2c_user -- --nocapture`
Expected: 编译错误 — 方法不存在

- [ ] **Step 3: 实现 `send_typing`**

在 `QqChannel` 的 `impl` 块中添加：

```rust
/// 发送输入状态（typing indicator），在用户端显示"Bot 正在输入中…"。
///
/// 仅 C2C 场景有效。群聊场景调用此方法静默跳过。
pub async fn send_typing(&self, recipient: &str) -> Result<(), ChannelError> {
    let token = self.get_token().await?;
    let (scope, id) = Self::resolve_recipient(recipient);
    // typing 仅支持 C2C (scope="users")，群聊静默跳过
    if scope != "users" {
        tracing::debug!(
            event = "QqTypingSkipped",
            recipient = %recipient,
            "typing indicator only supported in C2C, skipping"
        );
        return Ok(());
    }
    let url = format!("{}/v2/{scope}/{id}/typing", self.api_base);
    let resp = self
        .client
        .post(&url)
        .header("Authorization", format!("QQBot {token}"))
        .send()
        .await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(ChannelError::Api {
            code: status.as_u16() as i32,
            message: text,
        });
    }
    Ok(())
}
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --lib channels::qq::tests::send_typing_posts_to_c2c_user -- --nocapture`
Expected: PASS

- [ ] **Step 5: 添加群聊跳过测试**

```rust
#[tokio::test]
async fn send_typing_skips_group_recipient() {
    let ch = QqChannel::new(make_config());
    ch.set_token_for_test("fake_token").await;
    // 群聊 recipient 不发请求，直接返回 Ok
    ch.send_typing("group:GROUP456").await.expect("should skip with Ok");
}
```

- [ ] **Step 6: 运行全部 QQ 通道测试**

Run: `cargo test --lib channels::qq -- --nocapture`
Expected: 所有测试 PASS

- [ ] **Step 7: Commit**

```bash
git add src/channels/qq.rs
git commit -m "feat(qq): add typing indicator API (POST /v2/users/{openid}/typing)"
```

---

### Task 4: 交互回调 API（PUT /interactions/{id}）

**Files:**
- Modify: `src/channels/qq.rs` — 新增 `acknowledge_interaction` 方法

**Interfaces:**
- Consumes: `get_token()`, `api_base`
- Produces: `QqChannel::acknowledge_interaction(interaction_id, code) -> Result<(), ChannelError>`

**API 参考:** `PUT /interactions/{interaction_id}` — 回应交互事件，请求体 `{"code": 0}`。QQ API 要求在收到 INTERACTION_CREATE 事件后回调此接口，否则用户端按钮一直显示加载。

**依赖说明:** 此 Task 必须在 Task 5（INTERACTION_CREATE 监听）之前完成，因为 Task 5 的 INTERACTION_CREATE 处理会调用 `acknowledge_interaction`。

- [ ] **Step 1: 编写 `acknowledge_interaction` 的失败测试**

```rust
#[tokio::test]
async fn acknowledge_interaction_puts_to_interactions_endpoint() {
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let mock_server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/interactions/INTERACTION_001"))
        .and(body_partial_json(serde_json::json!({ "code": 0 })))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&mock_server)
        .await;

    let ch = QqChannel::new(make_config()).with_api_base(mock_server.uri());
    ch.set_token_for_test("fake_token").await;
    ch.acknowledge_interaction("INTERACTION_001", 0)
        .await
        .expect("acknowledge_interaction");
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --lib channels::qq::tests::acknowledge_interaction_puts_to_interactions_endpoint -- --nocapture`
Expected: 编译错误 — 方法不存在

- [ ] **Step 3: 实现 `acknowledge_interaction`**

在 `QqChannel` 的 `impl` 块中添加：

```rust
/// 回应交互回调（PUT /interactions/{interaction_id}）。
///
/// QQ API 要求在收到 INTERACTION_CREATE 事件后回调此接口，
/// 否则用户端按钮会一直显示加载状态。
pub async fn acknowledge_interaction(
    &self,
    interaction_id: &str,
    code: i32,
) -> Result<(), ChannelError> {
    let token = self.get_token().await?;
    let url = format!("{}/interactions/{interaction_id}", self.api_base);
    let body = json!({ "code": code });
    let resp = self
        .client
        .put(&url)
        .header("Authorization", format!("QQBot {token}"))
        .json(&body)
        .send()
        .await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(ChannelError::Api {
            code: status.as_u16() as i32,
            message: text,
        });
    }
    Ok(())
}
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --lib channels::qq::tests::acknowledge_interaction_puts_to_interactions_endpoint -- --nocapture`
Expected: PASS

- [ ] **Step 5: 添加错误场景测试**

```rust
#[tokio::test]
async fn acknowledge_interaction_returns_error_on_failure() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let mock_server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/interactions/INTERACTION_BAD"))
        .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({
            "code": 10016,
            "message": "interaction not found"
        })))
        .mount(&mock_server)
        .await;

    let ch = QqChannel::new(make_config()).with_api_base(mock_server.uri());
    ch.set_token_for_test("fake_token").await;
    let result = ch.acknowledge_interaction("INTERACTION_BAD", 0).await;
    assert!(result.is_err());
}
```

- [ ] **Step 6: 运行全部 QQ 通道测试**

Run: `cargo test --lib channels::qq -- --nocapture`
Expected: 所有测试 PASS

- [ ] **Step 7: Commit**

```bash
git add src/channels/qq.rs
git commit -m "feat(qq): add interaction callback API (PUT /interactions/{id})"
```

---

### Task 5: WebSocket 交互事件监听（INTERACTION_CREATE）

**Files:**
- Modify: `src/channels/qq.rs` — 扩展 intents 位掩码、`listen()` 事件分发

**Interfaces:**
- Consumes: 现有 `listen()` 的 WebSocket 事件循环；Task 4 的 `acknowledge_interaction`
- Produces: `handle_interaction_create(&self, &serde_json::Value) -> Option<ChannelInboundMessage>` 方法；`listen()` 处理 `INTERACTION_CREATE` 事件时分发到此方法

**背景:** 当前 intents = `(1 << 25) | (1 << 30)`（C2C + GROUP_AT）。需新增 `1 << 26`（INTERACTION_CREATE）以接收按钮点击事件。交互事件结构参见参考文档 §8 `InteractionEvent`。

**依赖说明:** 此 Task 依赖 Task 4（`acknowledge_interaction` 必须已实现）。

**`chat_type` 映射**（参考文档 §8）：`0=频道, 1=群聊, 2=单聊`。

- [ ] **Step 1: 扩展 intents 位掩码**

在 `listen()` 方法中修改：

```rust
// 旧: let intents: u64 = (1 << 25) | (1 << 30);
// 新: 新增 INTERACTION_CREATE (1 << 26)
let intents: u64 = (1 << 25) | (1 << 26) | (1 << 30);
```

- [ ] **Step 2: 编写交互事件解析的单元测试**

```rust
#[test]
fn parse_interaction_event_button_click() {
    let event_data = serde_json::json!({
        "id": "interaction_001",
        "type": 11,
        "chat_type": 2,
        "user_openid": "USER123",
        "group_openid": "GROUP456",
        "group_member_openid": "MEMBER789",
        "data": {
            "type": 2001,
            "resolved": {
                "button_data": "01912345-6789-7abc-8def-0123456789ab:allow",
                "button_id": "btn_allow",
                "message_id": "msg_001"
            }
        }
    });
    // 解析 button_data 中的 request_id:option_id
    let button_data = event_data["data"]["resolved"]["button_data"].as_str().unwrap();
    let (request_id_str, option_id) = button_data.split_once(':').unwrap();
    assert_eq!(request_id_str, "01912345-6789-7abc-8def-0123456789ab");
    assert_eq!(option_id, "allow");
}
```

- [ ] **Step 3: 运行测试确认通过**

Run: `cargo test --lib channels::qq::tests::parse_interaction_event_button_click -- --nocapture`
Expected: PASS（纯数据解析，不需要 mock）

- [ ] **Step 4: 将 INTERACTION_CREATE 处理逻辑抽为 `handle_interaction_create` 方法**

为了可测试性，将 INTERACTION_CREATE 的核心逻辑从 `listen()` 内联代码抽为一个独立方法。`listen()` 中只做事件分发和调用此方法。

```rust
/// 处理 INTERACTION_CREATE 事件。
///
/// 返回 `Some(ChannelInboundMessage)` 表示需要上报到引擎，
/// 返回 `None` 表示已内部消化（如 reject_with_feedback 进入两步流程）。
async fn handle_interaction_create(
    &self,
    event_data: &serde_json::Value,
) -> Option<ChannelInboundMessage> {
    let d = event_data;
    let interaction_id = d.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string();
    let button_data = d
        .get("data")
        .and_then(|data| data.get("resolved"))
        .and_then(|resolved| resolved.get("button_data"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");

    if button_data.is_empty() {
        tracing::warn!(
            event = "QqInteractionNoButtonData",
            interaction_id = %interaction_id,
            "INTERACTION_CREATE without button_data, skipping"
        );
        return None;
    }

    // button_data 格式: "<request_id>:<option_id>"
    let Some((request_id_str, option_id)) = button_data.split_once(':') else {
        tracing::warn!(
            event = "QqInteractionBadButtonData",
            button_data = %button_data,
            "button_data does not contain ':'"
        );
        return None;
    };

    let Ok(request_id) = Uuid::parse_str(request_id_str) else {
        tracing::warn!(
            event = "QqInteractionInvalidRequestId",
            request_id_str = %request_id_str,
            "request_id is not a valid UUID"
        );
        return None;
    };

    // 发送 ACK 回调（PUT /interactions/{interaction_id}）
    if let Err(e) = self.acknowledge_interaction(&interaction_id, 0).await {
        tracing::warn!(
            event = "QqInteractionAckFailed",
            interaction_id = %interaction_id,
            error = %e,
            "failed to acknowledge interaction"
        );
    }

    // 确定 sender_id 和 recipient
    // chat_type 映射：0=频道, 1=群聊, 2=单聊
    let chat_type = d.get("chat_type").and_then(serde_json::Value::as_u64);
    let (sender_id, recipient) = match chat_type {
        Some(1) => {
            // 群聊：sender 为点击按钮的群成员，recipient 为群
            let group_openid = d.get("group_openid").and_then(|g| g.as_str()).unwrap_or("unknown");
            let member_openid = d.get("group_member_openid").and_then(|m| m.as_str()).unwrap_or("unknown");
            (member_openid.to_string(), format!("group:{group_openid}"))
        }
        _ => {
            // C2C：sender 和 recipient 都基于 user_openid
            let user_openid = d.get("user_openid")
                .or_else(|| d.get("group_member_openid"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown");
            (user_openid.to_string(), format!("user:{user_openid}"))
        }
    };

    // 匹配 pending approval（遵循 try_match_approval_reply 的锁模式：
    // 读锁→clone→drop→写锁仅 remove→drop→无锁区做 HTTP/channel send）
    // 同时检查 TTL，与 try_match_approval_reply 行为一致
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let pending_opt = {
        let map = self.pending_approvals.read().await;
        map.get(&recipient)
            .filter(|p| p.request_id == request_id && now - p.created_at <= PENDING_APPROVAL_TTL_SECS)
            .cloned()
    };
    let Some(pending) = pending_opt else {
        // 过期或未匹配：写锁清理
        let mut map = self.pending_approvals.write().await;
        if let Some(p) = map.get(&recipient) {
            if p.request_id == request_id && now - p.created_at > PENDING_APPROVAL_TTL_SECS {
                map.remove(&recipient);
            }
        }
        return None;
    };
    let matched_option = pending.options.iter().find(|opt| opt.id == option_id);
    let Some(opt) = matched_option else { return None };

    // 写锁：仅 remove
    {
        let mut map = self.pending_approvals.write().await;
        map.remove(&recipient);
    }

    // 无锁区：reject_with_feedback 两步交互 或 普通 Confirmation
    if opt.id == "reject_with_feedback" {
        // 两步交互：插入 pending_feedback，提示输入，不发 Confirmation
        self.pending_feedback.write().await.insert(
            recipient.clone(),
            PendingFeedback {
                request_id,
                recipient: recipient.clone(),
            },
        );
        let _ = self
            .send_text_markdown(&recipient, "请输入评审建议（发送 /cancel 取消）：")
            .await;
        return None;
    }

    // 普通选项：发送确认提示 + InboundConfirmation
    let note = format!("已选择：{}", opt.label);
    let _ = self.send_text_markdown(&recipient, &note).await;
    Some(ChannelInboundMessage {
        channel_name: self.name().to_string(),
        sender_id,
        chat_id: recipient,
        thread_id: None,
        content: String::new(),
        timestamp_secs: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        confirmation: Some(InboundConfirmation {
            request_id,
            option: opt.id.clone(),
            label: Some(opt.label.clone()),
            feedback: None,
        }),
    })
}
```

在 `listen()` 的 `match event_type` 中只需调用此方法：

```rust
"INTERACTION_CREATE" => {
    if let Some(inbound) = self.handle_interaction_create(d).await {
        let _ = tx.send(inbound);
    }
}
```

- [ ] **Step 5: 编写 `handle_interaction_create` 的单元测试**

注意：`handle_interaction_create` 内部会调用 `acknowledge_interaction` 和 `send_text_markdown`（HTTP 请求），因此所有测试必须使用 `MockServer` + `with_api_base`，避免击打真实 QQ API 或在无网络 CI 中 hang。

```rust
#[tokio::test]
async fn interaction_create_allow_button_returns_confirmation() {
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let mock_server = MockServer::start().await;
    // mock acknowledge_interaction (PUT)
    Mock::given(method("PUT"))
        .and(path("/interactions/interaction_001"))
        .and(body_partial_json(json!({ "code": 0 })))
        .respond_with(ResponseTemplate::new(200))
        .mount(&mock_server)
        .await;
    // mock send_text_markdown (POST)
    Mock::given(method("POST"))
        .and(path("/v2/users/USER123/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "msg_ack"})))
        .mount(&mock_server)
        .await;

    let ch = QqChannel::new(make_config()).with_api_base(mock_server.uri());
    ch.set_token_for_test("fake_token").await;
    let request_id = Uuid::parse_str("01912345-6789-7abc-8def-0123456789ab").unwrap();
    ch.record_pending_approval("user:USER123", request_id, vec![
        crate::domain::ApprovalOption {
            id: "allow".to_string(),
            label: "允许".to_string(),
            description: String::new(),
        },
    ]).await;

    let event = serde_json::json!({
        "id": "interaction_001",
        "type": 11,
        "chat_type": 2,
        "user_openid": "USER123",
        "data": {
            "resolved": {
                "button_data": "01912345-6789-7abc-8def-0123456789ab:allow"
            }
        }
    });

    let result = ch.handle_interaction_create(&event).await;
    assert!(result.is_some());
    let inbound = result.unwrap();
    assert_eq!(inbound.sender_id, "USER123");
    assert_eq!(inbound.chat_id, "user:USER123");
    let confirmation = inbound.confirmation.unwrap();
    assert_eq!(confirmation.option, "allow");
    assert_eq!(confirmation.request_id, request_id);
}

#[tokio::test]
async fn interaction_create_group_routes_to_group_recipient() {
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let mock_server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/interactions/interaction_002"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&mock_server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v2/groups/GROUP456/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "msg_grp"})))
        .mount(&mock_server)
        .await;

    let ch = QqChannel::new(make_config()).with_api_base(mock_server.uri());
    ch.set_token_for_test("fake_token").await;
    let request_id = Uuid::parse_str("01912345-6789-7abc-8def-0123456789ab").unwrap();
    ch.record_pending_approval("group:GROUP456", request_id, vec![
        crate::domain::ApprovalOption {
            id: "allow".to_string(),
            label: "允许".to_string(),
            description: String::new(),
        },
    ]).await;

    let event = serde_json::json!({
        "id": "interaction_002",
        "type": 11,
        "chat_type": 1,
        "group_openid": "GROUP456",
        "group_member_openid": "MEMBER789",
        "data": {
            "resolved": {
                "button_data": "01912345-6789-7abc-8def-0123456789ab:allow"
            }
        }
    });

    let result = ch.handle_interaction_create(&event).await;
    assert!(result.is_some());
    let inbound = result.unwrap();
    assert_eq!(inbound.sender_id, "MEMBER789");
    assert_eq!(inbound.chat_id, "group:GROUP456");
}

#[tokio::test]
async fn interaction_create_reject_with_feedback_enters_two_step() {
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let mock_server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/interactions/interaction_003"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&mock_server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v2/users/USER123/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "msg_fb"})))
        .mount(&mock_server)
        .await;

    let ch = QqChannel::new(make_config()).with_api_base(mock_server.uri());
    ch.set_token_for_test("fake_token").await;
    let request_id = Uuid::parse_str("01912345-6789-7abc-8def-0123456789ab").unwrap();
    ch.record_pending_approval("user:USER123", request_id, vec![
        crate::domain::ApprovalOption {
            id: "reject_with_feedback".to_string(),
            label: "拒绝并反馈".to_string(),
            description: String::new(),
        },
    ]).await;

    let event = serde_json::json!({
        "id": "interaction_003",
        "type": 11,
        "chat_type": 2,
        "user_openid": "USER123",
        "data": {
            "resolved": {
                "button_data": "01912345-6789-7abc-8def-0123456789ab:reject_with_feedback"
            }
        }
    });

    let result = ch.handle_interaction_create(&event).await;
    // reject_with_feedback 不直接返回 Confirmation，进入两步流程
    assert!(result.is_none());
    // 验证 pending_feedback 已插入
    let feedback = ch.pending_feedback.read().await.get("user:USER123").cloned();
    assert!(feedback.is_some());
    assert_eq!(feedback.unwrap().request_id, request_id);
}

#[tokio::test]
async fn interaction_create_invalid_button_data_returns_none() {
    // 此测试不触发 HTTP 调用（提前返回 None），不需要 mock server
    let ch = QqChannel::new(make_config());
    ch.set_token_for_test("fake_token").await;

    let event = serde_json::json!({
        "id": "interaction_004",
        "type": 11,
        "chat_type": 2,
        "user_openid": "USER123",
        "data": {
            "resolved": {
                "button_data": "invalid_no_colon"
            }
        }
    });

    let result = ch.handle_interaction_create(&event).await;
    assert!(result.is_none());
}
```

- [ ] **Step 6: 运行编译确认无错误**

Run: `cargo check`
Expected: 编译通过，无错误

- [ ] **Step 7: Commit**

```bash
git add src/channels/qq.rs
git commit -m "feat(qq): add handle_interaction_create with proper lock handling and sender_id"
```

---

### Task 6: 审批交互从编号列表迁移到按钮交互

**Files:**
- Modify: `src/channels/qq.rs` — `send()` impl 改为发送 QQ 原生键盘；删除 `render_buttons_as_numbered_list`；覆写 `supports_inline_keyboard`

**Interfaces:**
- Consumes: Task 4 的 `acknowledge_interaction`，Task 5 的 INTERACTION_CREATE 事件处理
- Produces: `send()` 对 `reply_markup` 消息使用 QQ 原生 InlineKeyboard；`supports_inline_keyboard() -> bool { true }`

**背景:** 当前 QQ 通道将 `ReplyMarkup::InlineKeyboard` 渲染为编号列表文本（"1. 允许"），用户需输入数字匹配。QQ API 原生支持 InlineKeyboard 按钮（`keyboard` 字段），按钮点击会触发 `INTERACTION_CREATE` 事件。Task 4 已实现 INTERACTION_CREATE 监听，现在可以将审批消息从文本编号列表迁移到原生按钮。

**依赖:** Task 4 + Task 5（交互回调 + 事件监听必须已实现）。

- [ ] **Step 1: 编写发送带键盘消息的测试**

```rust
#[tokio::test]
async fn send_with_keyboard_posts_keyboard_field() {
    use crate::channels::traits::{ChannelOutboundMessage, ChannelParseMode, InlineKeyboardButton, ReplyMarkup};
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v2/users/USER123/messages"))
        .and(body_partial_json(serde_json::json!({
            "keyboard": {
                "content": {
                    "rows": [{
                        "buttons": [{
                            "id": "btn_allow",
                            "render_data": { "label": "允许", "visited_label": "已允许", "style": 1 },
                            "action": {
                                "type": 1,
                                "data": "01912345-6789-7abc-8def-0123456789ab:allow",
                                "permission": { "type": 0 },
                                "click_limit": 1
                            }
                        }]
                    }]
                }
            }
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "msg_keyboard_1"
        })))
        .expect(1)
        .mount(&mock_server)
        .await;

    let ch = QqChannel::new(make_config()).with_api_base(mock_server.uri());
    ch.set_token_for_test("fake_token").await;
    let message = ChannelOutboundMessage {
        recipient: "user:USER123".to_string(),
        thread_id: None,
        content: "🔒 需要你的确认".to_string(),
        parse_mode: Some(ChannelParseMode::Html),
        reply_markup: Some(ReplyMarkup::InlineKeyboard(vec![vec![InlineKeyboardButton {
            text: "允许".to_string(),
            callback_data: "01912345-6789-7abc-8def-0123456789ab:allow".to_string(),
        }]])),
        attachments: vec![],
    };
    ch.send(&message).await.expect("send with keyboard");
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --lib channels::qq::tests::send_with_keyboard_posts_keyboard_field -- --nocapture`
Expected: 测试失败 — 当前实现发送编号列表，不会在请求体中包含 `keyboard` 字段

- [ ] **Step 3: 新增 `send_text_with_keyboard` 方法**

在 `QqChannel` 的 `impl` 块中添加：

```rust
/// 发送带 InlineKeyboard 按钮的 markdown 消息。
async fn send_text_with_keyboard(
    &self,
    recipient: &str,
    content: &str,
    keyboard: &crate::channels::traits::ReplyMarkup,
) -> Result<QqMessageResponse, ChannelError> {
    use crate::channels::traits::ReplyMarkup;
    let token = self.get_token().await?;
    let (scope, id) = Self::resolve_recipient(recipient);
    let url = format!("{}/v2/{scope}/{id}/messages", self.api_base);

    let qq_keyboard = match keyboard {
        ReplyMarkup::InlineKeyboard(rows) => {
            let qq_rows: Vec<serde_json::Value> = rows
                .iter()
                .map(|row| {
                    let buttons: Vec<serde_json::Value> = row
                        .iter()
                        .map(|btn| {
                            // 从 callback_data 提取 option_id 作为按钮 id
                            let option_id = btn.callback_data
                                .split(':')
                                .nth(1)
                                .unwrap_or("unknown");
                            json!({
                                "id": format!("btn_{option_id}"),
                                "render_data": {
                                    "label": btn.text,
                                    "visited_label": format!("已{}", btn.text),
                                    "style": 1
                                },
                                "action": {
                                    "type": 1,
                                    "data": btn.callback_data,
                                    "permission": { "type": 0 },
                                    "click_limit": 1
                                }
                            })
                        })
                        .collect();
                    json!({ "buttons": buttons })
                })
                .collect();
            json!({
                "content": {
                    "rows": qq_rows
                }
            })
        }
    };

    let body = json!({
        "markdown": { "content": content },
        "msg_type": 2,
        "msg_seq": next_msg_seq(),
        "keyboard": qq_keyboard,
    });
    let resp = self
        .client
        .post(&url)
        .header("Authorization", format!("QQBot {token}"))
        .json(&body)
        .send()
        .await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(ChannelError::Api {
            code: status.as_u16() as i32,
            message: text,
        });
    }
    let raw_body = resp.text().await.unwrap_or_default();
    let root: serde_json::Value = serde_json::from_str(&raw_body).unwrap_or(json!({}));
    let data = root.get("data").unwrap_or(&root);
    let msg_id = data
        .get("id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_string();
    Ok(QqMessageResponse { id: msg_id })
}
```

- [ ] **Step 4: 修改 `send()` impl 中的 reply_markup 处理逻辑**

将 `send()` 中的主逻辑重构为有键盘 / 无键盘两条独立路径，消除冗余的外层提取：

```rust
async fn send(
    &self,
    message: &crate::channels::traits::ChannelOutboundMessage,
) -> Result<(), ChannelError> {
    use crate::channels::traits::{ChannelParseMode, extract_attachments};

    if let Some(ref markup) = message.reply_markup {
        // === 有键盘路径：发送 QQ 原生 InlineKeyboard ===
        if let Some((request_id, options)) = extract_approval_info(markup) {
            self.record_pending_approval(&message.recipient, request_id, options)
                .await;
        }
        let content_to_send = match message.parse_mode {
            Some(ChannelParseMode::Html) => html_to_markdown_for_qq(&message.content),
            Some(ChannelParseMode::Markdown) | None => message.content.clone(),
        };
        if !content_to_send.trim().is_empty() {
            self
                .send_text_with_keyboard(&message.recipient, &content_to_send, markup)
                .await?;
        }
    } else {
        // === 无键盘路径：普通消息 + 附件 ===
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

        for attachment in &all_attachments {
            if let Err(e) = self.send_attachment(&message.recipient, attachment).await {
                tracing::warn!(
                    event = "QqSendAttachmentFailed",
                    target = %attachment.target,
                    error = %e,
                    "QQ attachment send failed, degrading to text"
                );
                let fallback = format!(
                    "{}: {}",
                    match attachment.kind {
                        crate::channels::traits::AttachmentKind::Image => "Image",
                        crate::channels::traits::AttachmentKind::Document => "File",
                        crate::channels::traits::AttachmentKind::Video => "Video",
                        crate::channels::traits::AttachmentKind::Audio => "Audio",
                        crate::channels::traits::AttachmentKind::Voice => "Voice",
                    },
                    attachment.target
                );
                let _ = self.send_text_markdown(&message.recipient, &fallback).await;
            }
        }
    }
    Ok(())
}
```

- [ ] **Step 5: 删除 `render_buttons_as_numbered_list` 函数及其测试**

此函数不再被调用，且没有 fallback 路径使用它。按 AGENTS.md "代码腐化治理"原则删除。

删除以下函数和测试：
- `render_buttons_as_numbered_list` 函数
- `render_buttons_single_option` 测试
- `render_buttons_multiple_rows` 测试
- `render_buttons_empty_returns_base` 测试

- [ ] **Step 6: 覆写 `supports_inline_keyboard` 返回 `true`**

在 `impl Channel for QqChannel` 中添加：

```rust
fn supports_inline_keyboard(&self) -> bool {
    true
}
```

- [ ] **Step 7: 运行测试确认通过**

Run: `cargo test --lib channels::qq -- --nocapture`
Expected: 所有测试 PASS，包括新的 `send_with_keyboard_posts_keyboard_field`

- [ ] **Step 8: Commit**

```bash
git add src/channels/qq.rs
git commit -m "feat(qq): use native InlineKeyboard for approval messages instead of numbered list"
```

---

### Task 7: 端到端集成测试 + 清理 + 文档更新

**Files:**
- Modify: `src/channels/qq.rs` — 清理 `#[allow(dead_code)]`、补端到端测试
- Modify: `docs/current-state.md` — 更新 QQ 通道能力状态

**Interfaces:**
- Consumes: 所有前序 Task
- Produces: 端到端测试覆盖、干净的代码和更新的文档

- [ ] **Step 1: e2e 测试已在 Task 5 Step 5 完成**

Task 5 Step 5 已添加 4 个 `handle_interaction_create` 单元测试覆盖 INTERACTION_CREATE 核心路径：
- `interaction_create_allow_button_returns_confirmation` — 普通按钮 → InboundConfirmation
- `interaction_create_group_routes_to_group_recipient` — chat_type=1 → sender_id=member, chat_id=group
- `interaction_create_reject_with_feedback_enters_two_step` — reject_with_feedback → pending_feedback 插入，返回 None
- `interaction_create_invalid_button_data_returns_none` — 无效 button_data → 跳过

这些测试直接构造 INTERACTION_CREATE JSON payload 并调用 `handle_interaction_create`，验证了按钮闭环的核心路径。无需额外补充。

- [ ] **Step 2: 清理不再需要的 `#[allow(dead_code)]`**

检查以下方法在新流程中是否都已被使用，移除不必要的 `#[allow(dead_code)]`：
- `send_text_markdown` — 通过 `send()` 调用链使用 ✓
- `send_media_message` — 通过 `send_attachment` → `send()` 调用链使用 ✓
- `is_user_allowed` / `runtime_allow` — 由 listen 使用 ✓
- `is_duplicate` — 由 listen 使用 ✓
- `compose_message_content` — 由 listen 使用 ✓
- `get_token` — 由所有 API 方法使用 ✓
- `try_match_approval_reply` — 仍然由 listen 的文本消息路径使用（qq.rs:1457）✓。注意：此方法上的 `#[allow(dead_code)]` 是计划前就存在的误标（实际被调用），应在此步移除
- `send_attachment` — 通过 `send()` 调用链使用 ✓

- [ ] **Step 3: 更新 `docs/current-state.md` 中 QQ 通道的能力状态**

在"已实现"部分追加：

```markdown
- QQ 通道消息撤回 API（C2C / 群聊 DELETE 端点）
- QQ 通道输入状态指示器（typing indicator, POST /v2/users/{openid}/typing）
- QQ 通道交互回调（PUT /interactions/{id}）
- QQ 通道交互事件监听（INTERACTION_CREATE）与按钮点击闭环
- QQ 通道审批消息使用原生 InlineKeyboard 按钮交互（含 reject_with_feedback 两步流程）
- QQ 通道 send 方法返回 message_id（QqMessageResponse）
```

在"待继续完善"中追加：

```markdown
- QQ 通道消息撤回的调用方集成（状态消息治理：撤回过多状态切换消息）尚未接入，recall_message 方法已就绪
```

- [ ] **Step 4: 运行全量 CI 检查**

Run: `cargo fmt --all --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test --all-features`
Expected: 全部通过

- [ ] **Step 5: Commit**

```bash
git add src/channels/qq.rs docs/current-state.md
git commit -m "chore(qq): clean up dead_code annotations and update capability docs"
```

---

## Self-Review

### 1. Spec Coverage

| 需求 | 对应 Task |
|---|---|
| 消息撤回 API（C2C / 群聊 DELETE 端点） | Task 2 |
| 输入状态 typing | Task 3 |
| 交互回调 PUT /interactions/{id} | Task 4 |
| 交互事件监听 INTERACTION_CREATE | Task 5 |
| 按钮交互闭环（审批迁移） | Task 6 |
| reject_with_feedback 两步流程 | Task 5（handle_interaction_create 内） |
| message_id 返回 | Task 1 |
| supports_inline_keyboard 覆写 | Task 6 |
| 锁不跨 HTTP | Task 5（读锁→clone→drop→写锁仅 remove→drop→无锁区 HTTP） |
| sender_id 语义正确 | Task 5（C2C: sender_id=裸 openid；群聊: sender_id=member_openid） |
| TTL 检查一致性 | Task 5（handle_interaction_create 与 try_match_approval_reply 均检查 PENDING_APPROVAL_TTL_SECS） |
| 测试 hermeticity | Task 5 Step 5（3 个触发 HTTP 的测试使用 MockServer + with_api_base） |
| INTERACTION_CREATE 单元测试 | Task 5 Step 5（4 个测试覆盖 allow/group/reject_with_feedback/invalid） |
| `#[allow(dead_code)]` 误标清理 | Task 7 Step 2（显式列出 try_match_approval_reply 误标移除） |

**本期不包含：**
- 文字子频道 / 频道私信撤回 — Harness 不支持频道场景，`resolve_recipient` 仅处理 C2C 和群聊
- `sent_messages` 存储 — 调用方通过 `QqMessageResponse.id` 直接传 msg_id，无需内部存储

### 2. Placeholder Scan

无 TBD / TODO / "implement later" / "add appropriate error handling" 等占位符。

### 3. Type Consistency

- `QqMessageResponse { id: String }` 在 Task 1 定义，Task 2/3/4/5/6 均引用（或忽略 Ok 值）
- `recall_message` 在 Task 2 定义，签名：`pub async fn (&self, &str, &str) -> Result<(), ChannelError>`，调用方通过 `QqMessageResponse.id` 获取 msg_id 后传入
- `acknowledge_interaction` 在 Task 4 定义，Task 5 中调用，签名一致：`pub async fn (&self, &str, i32) -> Result<(), ChannelError>`
- `handle_interaction_create` 在 Task 5 定义，`listen()` 中调用，签名：`async fn (&self, &serde_json::Value) -> Option<ChannelInboundMessage>`
- `send_text_with_keyboard` 在 Task 5 定义，返回 `Result<QqMessageResponse, ChannelError>`，与 `send_text_markdown` 一致
- 按钮 `id` 生成逻辑：`format!("btn_{option_id}")`，其中 `option_id = callback_data.split(':').nth(1)`，与测试断言 `"btn_allow"` 一致
- `chat_type` 映射：`Some(1) => 群聊`，`Some(2) | _ => C2C`，与参考文档 §8 一致
- `sender_id`：C2C 为裸 `user_openid`，群聊为 `member_openid`，与现有 C2C_MESSAGE_CREATE/GROUP_AT_MESSAGE_CREATE 处理一致
