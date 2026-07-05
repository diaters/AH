> **状态：已归档** — 对应功能已合并到 main，归档于 2026-07-05

> **状态：当前有效**

# Telegram 通道体验优化设计

## 背景与目标

当前 Harness 的 Telegram 通道已实现基础文本收发，但存在以下体验问题：

1. Telegram 支持 Markdown/HTML 渲染，但当前输出原始字符。
2. 工具权限审批请求默认广播到 TUI，Telegram 对话中的用户看不到审批。
3. Telegram 通道暂不支持 Agent 发送/接收文件。
4. 缺少 ACK 反应、自助配对等基础交互反馈。

本设计参考 `/Users/diater/diahub/zeroclaw-dev/src/channels/telegram.rs` 的实现，对 Harness Telegram 通道进行体验优化，同时保持与其他通道（QQ、Feishu）的接口兼容。

## 设计原则

- **最小侵入**：不改动 `Channel` trait 的核心签名，其他通道不受影响。
- **统一抽象**：附件标记、通道能力声明在统一层定义，各通道按需实现。
- **失败降级**：HTML/Markdown 解析失败时自动降级为纯文本，避免消息丢失。
- **来源感知**：审批请求根据任务来源通道定向路由，而不是全局广播。
- **Thread 感知**：路由目标 `ChannelId` 携带 `thread_id`，保证 Telegram topic/QQ 频道 thread 等场景下审批和回复回到正确位置。

## 整体架构

```text
Agent 输出
    │
    ▼
EngineEvent::Text / EngineEvent::ApprovalRequest
    │
    ▼
frontend_output_system ──► ChannelFrontend ──► ChannelOutboundMessage
    │                                              │
    │ (origin_channel 定向)                        ▼
    │                                    TelegramChannel::send
    │                                              │
    │                                              ▼
    │                                    Telegram Bot API
    │
    ▼
TuiFrontend (TUI 仍保持原有行为)
```

Telegram 入向：

```text
Telegram Bot API (text / callback_query / document / photo / voice)
    │
    ▼
TelegramChannel::listen
    │
    ▼
ChannelInboundMessage
    │
    ▼
ExternalInput (TextWithChannel / Confirmation)
    │
    ▼
ECS Signal / ToolConfirmationResponseMessage
```

## 详细设计

### 0. 路由目标 `ChannelId` 扩展

为了让 Telegram topic/QQ 频道 thread 等场景下的审批和自动回执回到正确位置，扩展 `ChannelId`：

```rust
pub struct ChannelId {
    pub frontend: FrontendKind,
    pub user_id: String,
    pub thread_id: Option<String>, // 新增
}
```

影响与处理：

- `ExternalInput::TextWithChannel` 不额外扩展字段，因为 `channel: ChannelId` 已经包含 `thread_id`。
- `ChannelInboundMessage::to_external_input()` 将 `thread_id` 填入 `ChannelId`。
- `Signal`、`UserInputMessage`、`CreateTaskMessage` 的 `origin_channel` 类型为 `ChannelId`，自然携带 `thread_id`。
- `Task::origin_channel` 直接保存带 `thread_id` 的 `ChannelId`。
- `ChannelFrontend::push_event` 构造 `ChannelOutboundMessage` 时，从匹配到的 `ChannelId` 中取出 `thread_id`。
- 现有 TUI 等不区分 thread 的前端，`thread_id` 为 `None`，行为不变。

### 1. 出向消息格式扩展

扩展 `ChannelOutboundMessage`，新增可选字段：

```rust
pub struct ChannelOutboundMessage {
    pub recipient: String,
    pub thread_id: Option<String>,
    pub content: String,
    pub parse_mode: Option<ChannelParseMode>,   // 新增
    pub reply_markup: Option<ReplyMarkup>,      // 新增
    pub attachments: Vec<ChannelAttachment>,    // 新增
}

pub enum ChannelParseMode {
    Html,
    Markdown,
}

pub enum ReplyMarkup {
    InlineKeyboard(Vec<Vec<InlineKeyboardButton>>),
}

pub struct InlineKeyboardButton {
    pub text: String,
    pub callback_data: String,
}

pub enum AttachmentKind {
    Image,
    Document,
    Video,
    Audio,
    Voice,
}

pub struct ChannelAttachment {
    pub kind: AttachmentKind,
    pub target: String, // 本地路径、file:// 路径或 HTTP URL
}
```

说明：

- `parse_mode` 和 `reply_markup` 对不支持的平台无影响，平台实现可忽略。
- `attachments` 用于显式附件列表；同时保留文本内嵌 `[TYPE:path]` 标记的解析，以兼容 Agent 自然输出。
- 默认情况下 Agent 正常文本输出使用 `parse_mode: Html`。

### 2. Markdown 渲染与发送降级

#### 2.1 Markdown 转 Telegram HTML

在 `TelegramChannel` 中实现 `markdown_to_telegram_html`，支持以下转换：

- `**bold**` / `__bold__` → `<b>bold</b>`
- `*italic*` / `_italic_` → `<i>italic</i>`
- `` `code` `` → `<code>code</code>`
- ```` ```code``` ```` → `<pre><code>code</code></pre>`
- `[text](url)` → `<a href="url">text</a>`
- `~~strike~~` → `<s>strike</s>`
- `# Title` → `<b>Title</b>`

同时进行 HTML 转义，避免原始 `<`、`>`、`&` 破坏解析。

#### 2.2 发送策略

`TelegramChannel::send` 流程：

1. 解析内容中的内嵌附件标记 `[TYPE:target]`，生成 `ChannelAttachment` 列表。
2. 如果 `parse_mode == Some(Html)`（或默认）：
   - 先在 Markdown 阶段按语义块（段落、代码块、列表项等）切分。
   - 每个语义块单独渲染为 Telegram HTML 后发送；若单个语义块仍超过 4096 字符，再按字符边界安全切分。
   - 这样避免 `<pre>`、`<a>`、实体转义等在 HTML 边界被截断。
3. 如果 Telegram 返回 400 且是 parse mode 相关错误，自动降级为纯文本发送。
4. `parse_mode` 为 None 或 Markdown 时，内容超过 4096 字符按字符边界分片发送。
5. 最后按顺序发送附件。

### 3. 审批请求路由到 Telegram

#### 3.1 审批请求复用已有来源通道

`ToolConfirmationRequestMessage` 已经包含 `task_id`，而 `Task` 实体上已有 `origin_channel`。因此不复用新增字段，避免双真相源。

#### 3.2 审批事件定向

`frontend_output_system` 中，将 `EngineEvent::ApprovalRequest` 的目标从 `EventTarget::Broadcast` 改为通过 `task_id` 查找 `Task::origin_channel`：

```rust
let target = all_tasks
    .iter()
    .find(|t| t.id == confirmation.task_id)
    .map(|t| EventTarget::Directed(vec![t.origin_channel.clone()]))
    .unwrap_or(EventTarget::Broadcast);
```

这样 Telegram 发起的任务，审批请求会回到 Telegram（包括正确的 `thread_id`）；TUI 发起的任务仍回到 TUI。

#### 3.3 ChannelFrontend 处理 ApprovalRequest

`ChannelFrontend::push_event` 增加对 `EngineEvent::ApprovalRequest` 的处理：

1. 生成审批卡片文本（工具名、输入参数）。
2. 把审批选项映射为 Inline Keyboard 按钮，callback_data 格式为 `{request_id}:{option_id}`。
3. 通过 `ChannelOutboundMessage` 发送给匹配目标用户，同时从匹配到的 `ChannelId` 中取出 `thread_id` 填入 outbound 消息，保证 topic/thread 内回复位置正确。

#### 3.4 Telegram 处理 callback_query

当前 `Channel::listen()` 只能产出 `ChannelInboundMessage`，`ChannelManager` 的桥接代码也只负责把 `ChannelInboundMessage` 转成 `ExternalInput` 后送入 ECS。为了复用现有入向抽象，callback_query 不直接生成 `ToolConfirmationResponseMessage`，而是走 `ChannelInboundMessage` → `ExternalInput::Confirmation` 路径。

具体改动：

1. 扩展 `ChannelInboundMessage`，增加可选的确认信息字段：

```rust
pub struct ChannelInboundMessage {
    pub channel_name: String,
    pub sender_id: String,
    pub chat_id: String,
    pub thread_id: Option<String>,
    pub content: String,
    pub timestamp_secs: u64,
    pub confirmation: Option<InboundConfirmation>, // 新增
}

pub struct InboundConfirmation {
    pub request_id: Uuid,
    pub option: String,
}
```

2. 修改 `ChannelInboundMessage::to_external_input()`：
   - 当 `confirmation` 为 `Some` 时，返回 `ExternalInput::Confirmation { request_id, option }`。
   - 否则保持现有 `ExternalInput::TextWithChannel` 行为。

3. `TelegramChannel::listen` 处理 `update.callback_query`：
   - 从 `callback_query.data` 解析 `request_id` 和 `option_id`。
   - 调用 `answerCallbackQuery` 关闭客户端 loading 状态。
   - 构造 `ChannelInboundMessage`，`content` 可置空或填入选信息，`confirmation` 填入解析结果，通过 `tx` 发送。
   - 在原消息下追加一条“已选择：xxx”的文本反馈（可选，作为独立 `sendMessage`）。

4. `input_ingress_system` 已支持 `ExternalInput::Confirmation`，直接生成 `ToolConfirmationResponseMessage`，无需额外改动。

### 4. 文件收发能力

#### 4.1 统一附件标记规范

所有通道统一使用以下标记：

- `[IMAGE:path]`
- `[DOCUMENT:path]`
- `[VIDEO:path]`
- `[AUDIO:path]`
- `[VOICE:path]`

路径解析规则：

- 先按相对路径在当前工作目录查找；若存在则使用。
- 否则按绝对路径处理。
- 支持 `file://` 前缀。
- 支持 HTTP/HTTPS URL。

#### 4.2 通道能力声明

在 `Channel` trait 中增加能力查询：

```rust
fn supported_attachment_kinds(&self) -> Vec<AttachmentKind> {
    vec![]
}

fn supports_html(&self) -> bool {
    false
}

fn supports_inline_keyboard(&self) -> bool {
    false
}
```

Telegram 实现返回完整能力：

```rust
fn supported_attachment_kinds(&self) -> Vec<AttachmentKind> {
    vec![Image, Document, Video, Audio, Voice]
}

fn supports_html(&self) -> bool { true }
fn supports_inline_keyboard(&self) -> bool { true }
```

#### 4.3 发送附件

`TelegramChannel::send` 处理附件：

1. 扫描 content 中的 `[TYPE:target]` 标记，抽出附件列表。
2. 去掉标记后的剩余文本作为 caption。
3. 根据 TYPE 调用对应 API：
   - `IMAGE` → `sendPhoto`
   - `DOCUMENT` → `sendDocument`
   - `VIDEO` → `sendVideo`
   - `AUDIO` → `sendAudio`
   - `VOICE` → `sendVoice`
4. 本地文件使用 `multipart/form-data` 上传；URL 直接发送 JSON，失败降级为文本链接。
5. 若当前通道不支持某类附件，过滤后输出文本路径或提示。

#### 4.4 接收附件

`TelegramChannel::listen` 对每个 update 先尝试解析附件：

- `message.document` → 下载后 content = `[DOCUMENT:/absolute/path]`，文件名放入 caption 或说明文本。
- `message.photo` → 取最大尺寸下载后 content = `[IMAGE:/absolute/path]`。
- `message.voice` → 下载后 content = `[VOICE:/absolute/path]`。

所有接收到的附件统一使用 `[TYPE:/absolute/path]` 格式，与出向统一附件标记规范一致，Agent 可直接复用收到的路径。

下载流程：

1. 调用 `getFile` 获取 `file_path`。
2. 从 `https://api.telegram.org/file/bot<token>/<file_path>` 下载。
3. 保存到 `{current_dir}/telegram_files/`（当前工作目录，不存在则创建）。
4. 文件大小不超过 20 MB。

### 5. 将规范告知 Agent

采用工具描述 + 系统提示补充的方式：

- `channel_send` 工具描述中说明统一标记语法：`[IMAGE:path]`、`[DOCUMENT:path]` 等。
- 根据当前任务 `origin_channel` 的前端能力，在系统提示中补充说明支持的附件类型。
- Agent 不支持的附件类型会被通道过滤，避免发送失败。

### 6. 其他本次实现的优化

- **`/bind` 自助配对（显式开关 + 可写配置契约）**：为了不改变现有"空白名单 = 拒绝所有用户"的安全语义，`TelegramConfig` 中新增 `pairing_enabled: bool`（默认 `false`）。只有当 `pairing_enabled = true` 且 `allowed_users` 为空时，才进入自助配对模式。

  `/bind <code>` 成功后的授权行为：
  - `TelegramChannel` 内部维护一个**运行时白名单** `runtime_allowed_users: Arc<RwLock<HashSet<String>>>`，其中仅存放 **canonical identity = Telegram user_id 字符串**；`/bind` 成功后新增的授权统一写入这里。
  - 配置白名单 `TelegramConfig.allowed_users` 继续保持现有语义，支持三种写法：`username`、`user_id`、`"*"`。
  - 用户鉴权保持 `is_allowed(user: &TelegramUser)` 语义，而不是收窄为 `is_allowed(user_id)`。判定顺序：
    1. 若 `runtime_allowed_users` 包含当前用户的 `user_id` 字符串，则通过。
    2. 否则按现有规则检查 `TelegramConfig.allowed_users`：依次匹配 `"*"`、`username`（忽略大小写）和 `user_id`。
    3. 若都未命中，则拒绝。
  - 仅当 `HARNESS_CHANNELS_CONFIG` 指向一个**可写**的 TOML 文件时，才允许把新授权写回配置文件；写回时也统一使用 canonical identity，即 `user_id` 字符串，避免把运行时绑定结果写成不稳定的 `username`。
  - 如果配置来自默认值、环境变量直接注入、或文件不可写，则只把当前用户的 `user_id` 加入 `runtime_allowed_users`，进行**进程内临时放行**，并回复用户说明"本次运行期间已授权，重启后需运维手动添加"。
  - 写回前校验 `user_id` 格式，写回成功后同时更新 `runtime_allowed_users`，保证后续消息立即放行。
- **基础 ACK 反应**：收到已授权用户的消息后，随机发送一个表情反应（👍、👌 等）。
- **不支持的 message type 提示**：用户发送位置、联系人等未实现类型时，回复提示信息。

#### 配置变更

`TelegramConfig` 新增字段：

```rust
pub struct TelegramConfig {
    pub bot_token: String,
    pub allowed_users: Vec<String>,
    pub pairing_enabled: bool, // 新增，默认 false
    pub pairing_code: Option<String>, // 新增，自助配对码
}
```

需要同步更新：

- `docs/design/im-channel-adapters.md` 中 Telegram 配置说明。
- `.env.example` / channels 配置示例（如果存在）。

## 数据流示例

### 审批流程

```text
用户 (Telegram) 提问
    │
    ▼
Task 创建，origin_channel = Telegram
    │
    ▼
Agent 调用需要确认的工具
    │
    ▼
tool_dispatch_system 生成 ToolConfirmationRequestMessage
                         task_id → Task.origin_channel = Telegram
    │
    ▼
frontend_output_system 查找 Task.origin_channel
                         生成 EngineEvent::ApprovalRequest
                         target = Directed(Telegram)
    │
    ▼
ChannelFrontend 过滤到 Telegram，生成 Inline Keyboard 消息
    │
    ▼
TelegramChannel::send 调用 sendMessage + reply_markup
    │
    ▼
用户点击按钮 → callback_query
    │
    ▼
TelegramChannel::listen 生成 ChannelInboundMessage
                         confirmation = {request_id, option}
    │
    ▼
ChannelManager 桥接为 ExternalInput::Confirmation
    │
    ▼
input_ingress_system 生成 ToolConfirmationResponseMessage
    │
    ▼
tool_confirmation_result_system 继续执行工具
```

### 文件发送流程

```text
Agent 输出: "请看结果 [IMAGE:/tmp/chart.png]"
    │
    ▼
ChannelFrontend::push_event 路由到 Telegram
    │
    ▼
TelegramChannel::send
    - 解析出 IMAGE 附件
    - 先发送文本 "请看结果"
    - 再调用 sendPhoto 上传 /tmp/chart.png
```

## 错误处理

- **HTML 解析失败**：降级为纯文本发送，记录 warning。
- **附件路径不存在**：跳过该附件，记录 warning，文本部分正常发送。
- **附件大小超过 20 MB**：拒绝下载/发送，回复用户提示。
- **用户点击过期审批按钮**：`tool_confirmation_result_system` 报 `ToolConfirmationNoMatch`，Telegram 侧提示"该请求已过期"。
- **callback_query 解析失败**：忽略或提示用户。

## 测试策略

- 单元测试：
  - `markdown_to_telegram_html` 各种输入输出。
  - 附件标记解析（单附件、多附件、混合文本）。
  - `Channel::supported_attachment_kinds` 返回值。
  - callback_data 解析与反解析。
- 集成测试：
  - 使用 mock Telegram Bot API 服务器验证 sendMessage 带 `parse_mode: HTML`。
  - 验证审批请求根据 `origin_channel` 定向到 ChannelFrontend。
  - 验证审批消息和自动回执携带正确的 `thread_id`。
  - 验证文件下载保存路径和入向消息格式。

## 影响范围

- `src/domain/frontend.rs`：`ChannelId` 增加 `thread_id`。
- `src/domain/mod.rs` / `src/domain/message.rs`：`Signal`、`UserInputMessage`、`CreateTaskMessage` 的 `origin_channel` 自然携带 `thread_id`；`ExternalInput::TextWithChannel` 不新增字段，复用 `channel: ChannelId` 中的 `thread_id`。
- `src/channels/telegram.rs`：主要修改（Markdown 渲染、附件收发、callback_query、ACK 反应、`/bind` 配对、`thread_id` 透传）。
- `src/channels/traits.rs`：扩展 `ChannelOutboundMessage`、`ChannelInboundMessage` 和 `Channel` trait。
- `src/channels/frontend.rs`：处理 `EngineEvent::ApprovalRequest`，并从 `ChannelId` 取 `thread_id` 填入 outbound。
- `src/channels/manager.rs`：桥接逻辑需要处理带 `confirmation` 的 `ChannelInboundMessage`（若 `to_external_input()` 已处理，此处可能无需改动）。
- `src/systems/frontend_output.rs`：审批请求定向发送。
- `src/channels/send_tool.rs`：工具描述中补充附件标记语法说明。
- `src/channels/config.rs`：`TelegramConfig` 增加 `pairing_enabled`。
- `docs/design/im-channel-adapters.md`：更新 Telegram 配置说明与能力列表。
- QQ/Feishu 通道：仅需要实现新的 trait 方法（默认返回空能力），发送逻辑暂不受影响。

## TODO / 后续工作

以下优化建议后续分阶段实现：

- **流式 draft 输出**：使用 `sendMessage` 发送草稿，`editMessageText` 持续更新。需要 EngineEvent 支持流式文本事件。
- **语音消息接收与转录**：接收 `voice` 消息并转文字，需接入转录服务。
- **文本转语音发送**：将 Agent 回复转为语音消息发回。
- **mention-only 模式**：在群组中仅当用户 @bot 时才响应。
- **更丰富的 ACK 反应策略**：根据消息类型选择不同反应。

## 参考

- `/Users/diater/diahub/zeroclaw-dev/src/channels/telegram.rs`
- Harness `docs/logs.md`
- Harness `AGENTS.md`
