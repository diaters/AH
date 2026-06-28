# QQ 通道设计

> __状态：当前有效__

## 目标

为 Harness 引入 QQ 即时通讯通道，复用已建立的 [Channel trait 抽象](../../../src/channels/traits.rs)，实现与 Telegram 通道等价的能力：

1. __入向__：用户从 QQ（C2C 私聊 / 群 @ 消息）发送消息触发 Harness Task。
2. __出向-主动__：Agent 通过 `channel_send` 工具向 QQ 会话主动发送消息。
3. __出向-自动__：Agent 文字回复按 `origin_channel` 自动推回来源 QQ 会话（复用 [auto-channel-reply 设计](./2026-06-27-auto-channel-reply-design.md)）。
4. __Markdown 渲染__：通过 `msg_type=2` 让 QQ 客户端原生渲染 Markdown。
5. __媒体/文件传输__：出向支持本地文件、HTTP URL、base64 与分片上传；入向下载附件到工作目录。
6. __审批交互__：基于文本回复匹配实现 ToolConfirmationRequestMessage 的选项采集。
7. __运行时配对__：`/bind <code>` 在 `pairing_enabled=true` 时将用户加入运行时白名单。

## 设计原则

- 不破坏现有 TUI 主链路与 Telegram 通道实现。
- QQ 通道作为可选功能：未配置 `qq` 段时不启动后台任务。
- 优先复用 [Channel trait](../../../src/channels/traits.rs)、[ChannelOutboundMessage](../../../src/channels/traits.rs)、[extract_attachments](../../../src/channels/traits.rs)、[ChannelFrontend](../../../src/channels/frontend.rs)、[ChannelManager](../../../src/channels/manager.rs) 等已有抽象。
- 参考 [zeroclaw-dev/src/channels/qq.rs](file:///Users/diater/diahub/zeroclaw-dev/src/channels/qq.rs) 的官方 QQ Bot API 实战实现，但适配 Harness 的 `ChannelError`、`ChannelInboundMessage`、`ChannelOutboundMessage` 类型边界。

## 总体架构

QQ 通道作为 [src/channels/qq.rs](../../../src/channels/qq.rs) 的完整实现，与 `telegram.rs` 平级。所有逻辑集中在单文件，与 zeroclaw-dev 结构一致。

### 模块布局

```text
src/channels/
├── mod.rs           # 导出 QqChannel
├── traits.rs        # Channel trait + ChannelOutboundMessage（复用，不修改）
├── config.rs        # QqConfig + ChannelConfigs 扩展
├── manager.rs       # ChannelManager（复用，不修改）
├── frontend.rs      # ChannelFrontend（复用，不修改）
├── send_tool.rs     # channel_send 工具（复用，不修改）
├── telegram.rs      # Telegram 通道（不修改）
├── qq.rs            # QQ 通道（本期新增完整实现）
└── lark.rs          # 飞书通道占位（不修改）
```

### 与现有抽象的关系

| 抽象 | 复用方式 | 是否修改 |
|------|---------|---------|
| `Channel` trait | `QqChannel` 实现 trait | 否 |
| `ChannelOutboundMessage` | 含 `parse_mode` / `reply_markup` / `attachments` 字段，QQ 复用全部字段 | 否 |
| `ChannelInboundMessage` | QQ listen 填充 `channel_name="qq"`、`chat_id="user:<openid>"` / `"group:<openid>"`、`confirmation` | 否 |
| `extract_attachments` | 解析 `[IMAGE:path]` 等标记 | 否 |
| `ChannelManager` | `frontend_kind_for_name("qq")` 已支持 | 否 |
| `ChannelFrontend` | 通用 `EngineEvent` 路由 | 否 |
| `ChannelSendTool` | enum 已含 `"qq"` | 否 |
| `ChannelId` | `frontend=QQ`、`user_id` 携带前缀编码 | 否 |
| `FrontendKind::QQ` | 已存在 | 否 |
| `QqConfig` | 新增 | 是 |
| `ChannelConfigs.qq` | 新增字段 | 是 |
| `expand_env_vars()` | 扩展 QQ_APP_ID / QQ_APP_SECRET 回退 | 是 |

## 配置

### QqConfig 结构

新增于 [src/channels/config.rs](../../../src/channels/config.rs)：

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QqConfig {
    pub app_id: String,
    pub app_secret: String,
    #[serde(default)]
    pub allowed_users: Vec<String>,
    #[serde(default)]
    pub pairing_enabled: bool,
    pub pairing_code: Option<String>,
}
```

`ChannelConfigs` 扩展 `qq: Option<QqConfig>` 字段，`expand_env_vars()` 增加 `QQ_APP_ID` / `QQ_APP_SECRET` 环境变量回退（与 Telegram 模式一致）。

### 配置示例

```toml
[qq]
app_id = "${QQ_APP_ID}"
app_secret = "${QQ_APP_SECRET}"
allowed_users = []
pairing_enabled = false
pairing_code = ""
```

### 白名单匹配语义

与 Telegram 一致：

- `runtime_allowed_users`（/bind 运行时写入）优先于静态配置
- `allowed_users` 中包含 `"*"` 表示允许所有用户
- 空白名单表示拒绝所有用户（必须显式配置才放行）
- 静态匹配按 `user_openid` 字符串精确比较

## ChannelId 编码

完全沿用现有 `ChannelId { frontend, user_id, thread_id }` 结构，__不新增字段__。`user_id` 字符串携带前缀：

| 来源 | `ChannelId.user_id` | `resolve_recipient` 解析 |
|------|---------------------|--------------------------|
| C2C 消息 | `"user:<user_openid>"` | scope=`"users"`, id=`<user_openid>` |
| 群消息 | `"group:<group_openid>"` | scope=`"groups"`, id=`<group_openid>` |

`ChannelFrontend` 透传时原样保留，`QqChannel::send` 内部用 `resolve_recipient()` 解析前缀决定 API scope（`users` / `groups`）。[ChannelId::to_prompt_context()](../../../src/domain/frontend.rs) 已支持 `FrontendKind::QQ`，无需修改。

## OAuth2 与 WebSocket Gateway

QQ 官方 Bot API 使用 `appId` + `clientSecret` 换取 `access_token`，通过 Discord-like WebSocket Gateway 接收事件，与 Telegram 的 `bot_token` + 长轮询差异较大。

### Token 获取与缓存

```rust
const QQ_API_BASE: &str = "https://api.sgroup.qq.com";
const QQ_AUTH_URL: &str = "https://bots.qq.com/app/getAppAccessToken";

async fn fetch_access_token(&self) -> Result<(String, u64), ChannelError> {
    let body = json!({ "appId": self.config.app_id, "clientSecret": self.config.app_secret });
    let resp = self.client.post(QQ_AUTH_URL).json(&body).send().await?;
    // 解析 access_token + expires_in（默认 7200s），提前 60s 过期
    // 失败返回 ChannelError::Api / Auth
}

async fn get_token(&self) -> Result<String, ChannelError> {
    // 先读 token_cache，未过期直接返回；过期则 fetch_access_token 并写回缓存
}
```

`health_check()` 调用 `fetch_access_token().is_ok()` 判断通道健康。

### WebSocket Gateway 协议

完整握手流程：

```text
1. GET /gateway 获取 WebSocket URL（带 Authorization: QQBot <token>）
2. connect_async(url) 建立 WebSocket 连接
3. 接收 Hello 帧 (op=10)：{ "op": 10, "d": { "heartbeat_interval": 41250 } }
4. 发送 Identify 帧 (op=2)：
   { "op": 2, "d": {
       "token": "QQBot <token>",
       "intents": (1<<25) | (1<<30),  // C2C+GROUP_AT+PUBLIC_GUILD_MESSAGES
       "properties": { "os": "linux", "browser": "harness", "device": "harness" }
   }}
5. 启动心跳定时任务（按 heartbeat_interval 发送 op=1，d=sequence 或 null）
6. 主循环 tokio::select! { 心跳 / WebSocket 消息 }
```

### 事件分发

仅处理 `op=0` dispatch 事件：

| event_type | 处理 |
|------------|------|
| `C2C_MESSAGE_CREATE` | 私聊消息，author.user_openid → recipient=`user:<user_openid>` |
| `GROUP_AT_MESSAGE_CREATE` | 群 @ 消息，group_openid → recipient=`group:<group_openid>` |
| `op=1` | 服务器请求立即心跳 |
| `op=7` | Reconnect，break 让 ChannelManager 重启 |
| `op=9` | Invalid Session，break 重连 |

### 消息去重

`msg_id` 写入 `dedup` HashSet（容量 10000，满时淘汰一半），重复消息跳过。

## 消息流

### 入向 listen() 主流程

```text
WebSocket 消息 → 解析 event_type → 去重检查 → 白名单检查
  → compose_message_content(payload) 组装内容（含附件下载与 marker 生成）
  → 检查 /bind 命令
  → 检查审批回复匹配（try_match_approval_reply）
  → 发送 ACK 文本
  → tx.send(ChannelInboundMessage)
```

### 入向消息内容组装

`compose_message_content(payload) -> Option<String>` 处理 `attachments` 数组：

- 文本部分：`payload.content` trim
- 附件：遍历 `attachments`，按 `content_type` + `filename` 推断 marker 类型（`infer_attachment_marker`）
- 下载到工作目录：`{workspace_dir}/qq_files/<stem>_<8char-uuid>.<ext>`，文件名加 UUID 避免冲突
- URL 修复：`//cdn.example.com/...` → `https://cdn.example.com/...`（QQ CDN 常见）
- 语音附件优先用 `voice_wav_url`（WAV 格式，无需转码）
- 语音 ASR 转写：用 `<VOICE_TRANSCRIPTION>...</VOICE_TRANSCRIPTION>` 标签包裹，区别于 `[VOICE:path]` 媒体标记
- 最终组装：`text\n[IMAGE:path]\n[VOICE:path]` 或仅 `text` / 仅 markers

### 入向 ACK

发送文本确认（用户已确认策略）：

- 文本消息：发送 `"收到：{content 前 50 字符}..."` 确认
- 附件消息：发送 `"收到附件：{filename}"` 确认
- `/bind` 响应：不重复发 ACK（已有配对响应）
- 审批回复匹配成功：不重复发 ACK（已有 "已选择：xxx" 确认）

> __注意__：QQ 被动回复限速（4 条/小时）已取消，ACK 不再受配额约束。

### 出向 send() 主流程

```rust
async fn send(&self, message: &ChannelOutboundMessage) -> Result<(), ChannelError> {
    // 1. 审批请求（reply_markup.is_some()）不解析附件标记，直接走 markdown 文本
    let (text, attachments) = if message.reply_markup.is_some() {
        (message.content.clone(), vec![])
    } else {
        let (text, inline_atts) = extract_attachments(&message.content);
        let all = message.attachments.iter().chain(inline_atts.iter()).cloned().collect();
        (text, all)
    };

    // 2. 文本发送：根据 parse_mode 选择 API
    let final_text = if let Some(ref markup) = message.reply_markup {
        render_buttons_as_numbered_list(markup, &text)  // 审批请求转编号列表
    } else {
        text.clone()
    };

    match message.parse_mode {
        Some(ChannelParseMode::Html) | None => {
            // Html 模式（来自 ChannelFrontend 硬编码）转换为 QQ markdown
            // QQ 不支持原生 HTML 渲染，因此将 HTML 标签映射为 markdown 等价物
            let content = match message.parse_mode {
                Some(ChannelParseMode::Html) => html_to_markdown_for_qq(&final_text),
                _ => final_text.clone(),
            };
            send_text_markdown(recipient, &content).await?;
        }
        Some(ChannelParseMode::Markdown) => {
            send_text_markdown(recipient, &final_text).await?;
        }
    }

    // 3. 附件发送
    for att in &attachments { self.send_attachment(recipient, att).await?; }
}
```

### 主动消息路径

所有出向消息统一走主动消息路径：

- 端点：`POST /v2/{scope}/{openid}/messages`
- 请求体：`{ "msg_type": 2|7, "markdown": {...} | "media": {...}, "msg_seq": <next_msg_seq()> }`
- __不携带 `msg_id`__：出向时机与入向 msg_id 不同步（Agent 思考耗时可能数分钟，msg_id 已过期）
- `QqChannel` 不维护入向 msg_id 状态，不区分被动回复与主动消息

> __说明__：`{scope}/{openid}/messages` 中的 `id` 是 `user_openid` 或 `group_openid`，不是 QQ 号也不是会话 ID。QQ 官方 Bot API 不暴露用户 QQ 号，每个 Bot 看到的同一用户 openid 不同（平台隔离）。

### parse_mode 映射

QQ 不支持 HTML 渲染：

| `parse_mode` | 处理 | msg_type |
|--------------|------|----------|
| `Some(Html)` | 转义 HTML 特殊字符后通过 markdown 发送 | 2 |
| `Some(Markdown)` | 原文 markdown 发送 | 2 |
| `None` | 默认走 markdown（与 Telegram 的 None→HTML 模式方向一致，让 ChannelFrontend 的 `EngineEvent::Text` 路径零配置可用） | 2 |

### 附件上传策略

`send_attachment(recipient, attachment)` 分三条路径：

```text
target 以 http:// 或 https:// 开头：
    → upload_media(url=Some(target)) → send_media_message(file_info)

target 是本地路径且文件 <= 10MB：
    → 读取文件 → 计算 upload_cache_key (sha256+scope+target_id+file_type)
    → 缓存命中且未过期 → 直接 send_media_message(cached_file_info)
    → 缓存未命中 → base64 编码 → upload_media(file_data=Some(b64)) → 缓存 → send

target 是本地路径且文件 > 10MB：
    → upload_large_media() 分片流程：
       1. compute_local_file_digests() 计算 size/md5/sha1/md5_10m
       2. upload_prepare() 申请 upload_id + part_urls + block_size
       3. 循环 upload_part() 上传分片到 presigned URL
       4. 每片 upload_part_finish() 确认
       5. complete_multipart_upload() 换取 file_info
    → send_media_message(file_info)
```

### Voice 特殊处理

marker kind `AUDIO` / `VOICE` 时，按目标路径扩展名判断：

- `.wav` / `.mp3` / `.silk` → `QQMediaFileType::Voice`（QQ 原生支持）
- 其他扩展名（`.ogg` / `.flac` / `.m4a`）→ 降级为 `QQMediaFileType::File`（避免引入 silk-wasm + ffmpeg 重型依赖，与 zeroclaw-dev 一致）

### 失败降级

`send_attachment` 失败时 fallback 发送文本 `"<Image|Video|Voice|File>: <target>"`，warn 日志记录。

## 审批交互（文本回复匹配）

### 问题背景

Telegram 用 inline keyboard + `callback_query` 实现审批：服务端推送按钮，用户点击后异步回调。QQ 官方 Bot API 没有等价能力，本期采用文本回复匹配方案。

### 审批请求出向渲染

`ChannelFrontend::push_event(EngineEvent::ApprovalRequest)` 对 QQ 通道走与 Telegram __相同的代码路径__（[src/channels/frontend.rs](../../../src/channels/frontend.rs)）：

- 生成 HTML 内容：`"🔒 需要你的确认\n\n工具：{tool_name}\n输入：<pre>{escaped_input}</pre>\n\n请选择一个选项："`
- 生成 `ReplyMarkup::InlineKeyboard(buttons)`，每个 button 的 `callback_data = "<request_id>:<option_id>"`

QQ 差异化处理：`QqChannel::send()` 检测到 `reply_markup.is_some()` 时，__不发送 inline keyboard__（即不向 QQ API 传递任何 `reply_markup` 字段，QQ 也不支持该字段），而是将按钮转译为编号列表追加到文本末尾：

```text
🔒 需要你的确认

工具：channel_send
输入：<pre>{...}</pre>

请选择一个选项：
1. 允许
2. 拒绝

请回复数字 1-N 或选项名称。
```

### Pending Approval 记录

发送审批请求时，将 `request_id` 与 `recipient`（`user:<openid>` 或 `group:<openid>`）记录到 `pending_approvals`：

```rust
struct PendingApproval {
    request_id: Uuid,
    recipient: String,
    options: Vec<ApprovalOption>,
    created_at: u64,
}

pending_approvals: Arc<RwLock<HashMap<String, PendingApproval>>>,
```

key 为 `recipient` 字符串（同一会话同一时刻最多一个 pending approval；新审批请求到达时覆盖旧的，避免用户混淆）。

### 审批回复入向识别

`QqChannel::listen()` 在收到 C2C / GROUP_AT_MESSAGE_CREATE 事件后，__先检查是否为审批回复__，再走常规文本转发：

```rust
let content = self.compose_message_content(d).await?;
let user_openid = /* from payload */;

if let Some(confirmation) = self.try_match_approval_reply(&recipient, &content).await {
    let inbound = ChannelInboundMessage {
        channel_name: "qq".to_string(),
        sender_id: user_openid.to_string(),
        chat_id: recipient.clone(),
        thread_id: None,
        content: String::new(),
        timestamp_secs,
        confirmation: Some(confirmation),
    };
    let _ = tx.send(inbound);
    return;
}
// 常规文本消息路径
```

### 匹配优先级

`try_match_approval_reply` 三级匹配：

1. __数字匹配__：`"1"` → `options[0]`
2. __option id 精确匹配__：`"allow"` / `"deny"`
3. __option label 模糊匹配__：`"允许"` / `"拒绝"`

三种方式都失败时返回 `None`，消息走常规文本路径（用户可能在与 Bot 普通对话）。匹配成功后移除 `pending_approvals[recipient]`。

### Pending Approval TTL

`PendingApproval` 创建时记录 `created_at`，listen 中检查 `now - created_at > 300s`（5 分钟）时自动清除并丢弃。避免长时间挂起的审批请求阻塞后续消息。

### 审批回复确认

匹配成功后，发送确认文本给用户（与 Telegram 的 "已选择：允许" 一致）：

```rust
let note = format!("已选择：{}", matched_option.label);
self.send_text_markdown(&recipient, &note).await?;
```

与 Telegram 不同，QQ 不需要 `editMessageReplyMarkup` 移除 inline keyboard，因为审批请求本身就是普通文本消息。

### 与 Telegram 实现的对比

| 维度 | Telegram | QQ |
|------|----------|-----|
| 选项 UI | inline keyboard 按钮 | 编号列表文本 |
| 回采集方式 | `callback_query` 异步回调 | 后续文本消息匹配 |
| request_id 关联 | callback_data 中携带 | `pending_approvals` HashMap |
| 多审批并发 | 每条消息独立 keyboard | 同 recipient 覆盖（限制） |
| 重复确认防护 | `editMessageReplyMarkup` 移除键盘 | pending 移除 + TTL |
| 确认反馈 | "已选择：xxx" 文本 | "已选择：xxx" 文本（一致） |

### 边界情况

| 场景 | 处理 |
|------|------|
| 用户回复 "0" 或超出范围的数字 | 不匹配，走常规文本路径 |
| 用户回复 "允许" 但没有 pending approval | 不匹配，走常规文本路径 |
| 同一会话多个审批请求并发 | 后到的覆盖先到的（设计简化） |
| pending approval 超过 5 分钟 | listen 自动清除，后续回复不匹配 |
| `/bind` 命令在审批 pending 期间发送 | `/bind` 优先匹配，不触发审批回复识别 |
| 群消息审批回复 | `GROUP_AT_MESSAGE_CREATE` 中 `recipient = "group:<openid>"`，pending key 同样为 group，匹配逻辑一致 |

## /bind 运行时配对

与 Telegram 一致：

- 触发条件：`content.starts_with("/bind ")` && `pairing_enabled` && `allowed_users.is_empty()`
- 解析 `/bind ` 后的配对码
- 验证 `pairing_code`，通过则 `runtime_allow(user_openid)` 并可选回写 `channels.toml`
- 回复 `"已授权并已保存到配置。"` 或 `"已授权（本次运行有效）。"` 或 `"配对码错误。"`

`runtime_allowed_users` 优先于静态 `allowed_users`；`persist_allowed_user` 复用 Telegram 的 toml 回写模式。

## 错误处理

- `ChannelError::Auth` — token 获取失败
- `ChannelError::Api { code, message }` — HTTP 非 2xx，body 截断 600 字符用于日志
- `ChannelError::Network` — reqwest 网络错误
- `ensure_https(url)` — 拒绝向非 HTTPS URL 传输敏感数据
- WebSocket 断开返回 `Err`，由 ChannelManager 指数退避重启（1s → 60s 上限）
- `upload_prepare` 返回 `block_size=0` 时直接 bail，避免后续除零

## 与 ECS 集成的数据流

```text
入向：
  QQ WebSocket → QqChannel::listen() → ChannelInboundMessage {
      channel_name: "qq",
      sender_id: user_openid,
      chat_id: "user:<openid>" 或 "group:<openid>",
      thread_id: None,  // QQ 无 thread 概念
      content: "text" 或 "[IMAGE:path]" 或 "text\n[IMAGE:path]",
      timestamp_secs,
      confirmation: None,  // 文本审批回复在 listen 中识别并填充
  }
  → ChannelManager bridge → ExternalInput::TextWithChannel { channel, content }
  → input_ingress_system → Signal/UserInputMessage/CreateTaskMessage (origin_channel 透传)
  → Task::origin_channel = ChannelId { QQ, "user:<openid>", None }

出向（Agent 主动）：
  LLM 调用 channel_send tool → ToolAction::SendChannelMessage { channel: "qq", target, content, attachments }
  → channel_send_dispatch_system → ChannelManager::send("qq", ChannelOutboundMessage)
  → QqChannel::send() → QQ API

出向（自动回复）：
  Agent 文本回复 → UserOutputMessage → frontend_output_system
  → 查 Task::origin_channel → EngineEvent::Text { Directed([ChannelId::QQ]), content }
  → ChannelFrontend::push_event() → outbound_tx → ChannelManager → QqChannel::send()
```

## 依赖

新增 crate（[Cargo.toml](../../../Cargo.toml)），符合项目依赖原则（crates.io / MIT/Apache-2.0 兼容 / 纯 Rust 实现）：

```toml
tokio-tungstenite = { version = "0.24", features = ["native-tls"] }
base64 = "0.22"
sha2 = "0.10"
md5 = "0.7"
```

- `tokio-tungstenite`：WebSocket Gateway
- `base64`：小文件 base64 上传
- `sha2`：upload_cache_key 哈希 + upload_prepare 的 sha1 算法
- `md5`：分片上传的 md5 校验

## 测试

### 单元测试（src/channels/qq.rs `#[cfg(test)]`）

| 测试组 | 覆盖项 |
|--------|--------|
| 配置解析 | `QqConfig` toml 反序列化、环境变量展开、回退 |
| 白名单匹配 | `*` 通配、精确 openid、空白名单拒绝、runtime override |
| `/bind` 配对 | 配对码正确/错误、runtime_allow、persist_allowed_user 回写 toml |
| marker 解析 | 单 marker / 多 marker / 无 marker / 嵌套括号 / 大小写 / 无效 marker 保留 |
| `marker_kind_to_qq_file_type` | IMAGE/DOCUMENT/VIDEO/VOICE native / VOICE 非原生降级为 File |
| `infer_attachment_marker` | content_type 推断 + 扩展名回退 |
| `resolve_recipient` | `user:` / `group:` / 裸 ID 三种前缀 |
| `fix_qq_url` | `//` 前缀修复 |
| `html_to_markdown_for_qq` | `<pre>`/`<b>`/`<i>`/`<code>` 转 markdown、HTML 实体反转义、中文保留 |
| `render_buttons_as_numbered_list` | 单按钮 / 多按钮 / 含中英文 label |
| `try_match_approval_reply` | 数字匹配 / option id 匹配 / label 匹配 / 超范围数字 / 无 pending / TTL 过期 / 匹配后 pending 移除 |
| `compose_message_content` | 仅文本 / 仅附件 / 文本+附件 / 语音 WAV URL 优先 / ASR 转写 / URL `//` 修复 |
| `upload_cache_key` | 同内容不同 scope / 不同 file_type 区分 |
| `compute_local_file_digests` | 小文件 md5/sha1/md5_10m 一致性 |
| `parse_upload_prepare_response_body` | data 包裹 / parts 字段 / presigned_url 命名变体 |
| `parse_upload_response_body` | data 包裹 / ttl 字符串/数字兼容 |
| token 缓存 | 未过期复用 / 过期重新获取 |

### 集成测试（`tests/` 目录或同文件 `#[tokio::test]`）

使用 `wiremock` 模拟 QQ API（与 Telegram 集成测试一致）：

| 场景 | Mock 端点 |
|------|----------|
| OAuth2 token 获取 | `POST https://bots.qq.com/app/getAppAccessToken` |
| Gateway URL 获取 | `GET /gateway` |
| markdown 文本发送 | `POST /v2/users/{id}/messages` 验证 msg_type=2 |
| 富媒体发送 | `POST /v2/users/{id}/messages` 验证 msg_type=7 + media.file_info |
| URL 附件上传 | `POST /v2/users/{id}/files` 验证 file_type + url 字段 |
| base64 附件上传 | `POST /v2/users/{id}/files` 验证 file_data 字段 |
| 分片上传 prepare | `POST /v2/users/{id}/upload_prepare` |
| 分片上传 part | `PUT <presigned_url>` |
| 分片上传 finish | `POST /v2/users/{id}/upload_part_finish` |
| 分片上传 complete | `POST /v2/users/{id}/files` 验证 upload_id |
| 群消息路由 | recipient=`group:<id>` → scope=groups |
| 审批请求渲染 | 验证出向文本含编号列表 + 不含 inline keyboard JSON |
| 审批回复匹配 | 模拟用户回复 "1" → 验证 InboundConfirmation |
| 审批 TTL 过期 | pending 5 分钟后回复不匹配 |
| 附件发送失败降级 | upload 返回 500 → fallback 文本发送 |

### WebSocket 集成测试

由于 `tokio-tungstenite` 难以用 wiremock 模拟，采用两种策略：

1. __Mock WebSocket Server__：用 `tokio-tungstenite` 起本地 WebSocket server，模拟 Hello / Identify / Heartbeat / Dispatch 协议，验证 listen() 正确解析 C2C_MESSAGE_CREATE 和 GROUP_AT_MESSAGE_CREATE
2. __协议解析函数单元化__：把 `handle_dispatch_event(d, event_type)` 拆为纯函数，单测覆盖各种 payload 解析

## 文档同步

按 [AGENTS.md](../../../AGENTS.md) 要求同步更新：

| 文档 | 更新内容 |
|------|----------|
| `docs/current-state.md` | 已实现能力列表追加 "QQ 通道：C2C / 群消息、markdown、媒体出/入向、/bind 配对" |
| `docs/configuration.md` | 追加 QQ 配置段示例与环境变量说明 |
| `.env.example` | 追加 `QQ_APP_ID` / `QQ_APP_SECRET` 示例 |
| `docs/design/im-channel-adapters.md` | "QQ（后续阶段）" 段落状态标注从设计转为已实现 |
| `docs/design/README.md` 与 `docs/README.md` | 索引本设计文档 |
| `AGENTS.md` 与 `CLAUDE.md` | "已实现" 段追加 QQ 通道 |

## 实施范围边界

### 本期交付

- QQ 通道完整实现（[src/channels/qq.rs](../../../src/channels/qq.rs)）
- `QqConfig` 与配置加载扩展
- 4 个新依赖
- 单元测试 + 集成测试
- 文档同步

### 本期不交付

- Lark/Feishu 通道（[src/channels/lark.rs](../../../src/channels/lark.rs) 仍为占位）
- silk-wasm + ffmpeg 语音转码（VOICE 非原生格式降级为 File）
- QQ markdown template + button 申请与审核
- WebSocket 重连时的 session resume（op=6 RESUME），本期仅 break 后由 ChannelManager 重新 IDENTIFY
- ASR 文本提取的多模态处理（仅作为 `<VOICE_TRANSCRIPTION>` 标签透传给 LLM）
- __大文件分片上传（>10MB）__：`upload_prepare` / `upload_part` / `upload_part_finish` / `complete_multipart_upload` 四个 API 的完整流程。本期仅支持 URL 与 base64 两种路径，base64 路径隐含 ~10MB 上限。大文件场景由调用方降级为文本提示
- __WebSocket mock server 集成测试__：`tokio-tungstenite` 难以用 wiremock 模拟，本期 listen() 仅通过编译验证与单元测试覆盖协议解析函数，端到端 WebSocket 测试作为后续独立任务
