# IM 通道适配设计（Telegram / QQ / 飞书）

> **状态：当前有效**

## 目标

为 Harness 引入统一的即时通讯（IM）通道能力。本期交付：

1. **入向**：用户从 IM 发送消息，触发 Harness 的 Task。
2. **出向-主动**：Agent 通过 `channel_send` 工具向任意已配置平台主动发送消息。

下列能力**不在本期交付**，作为后续阶段推进：

3. **出向-自动**：Agent 对 Task 的文字回复按 `origin_channel` 自动推回来源平台。

> 拆分理由：出向-自动需要 `Channel` 持有出向发送句柄并作为 `Frontend` 真正发送消息，
> 涉及 `Frontend` trait 能力扩展与独立 companion system 设计，与本期“建立抽象 + Telegram
> 长轮询 + 主动工具”的最小闭环耦合度低，单独成阶段更易验证。

## 设计原则

- 不破坏现有 TUI 主链路。
- 通道作为可选功能：未配置时不启动后台任务。
- 优先建立通用抽象层，再逐个接入具体平台。
- 依赖按阶段引入：当前只引入长轮询所需的 `reqwest`，WebSocket/protobuf 推迟到 QQ/飞书阶段。
- 具体协议实现学习 zeroclaw 的实践，但适配 Harness 的 ECS 运行时。

## 总体架构

新增 `src/channels/` 模块，与 `src/tui/` 并列：

```text
src/
├── channels/
│   ├── mod.rs           # 注册、启动、ChannelManager
│   ├── traits.rs        # Channel trait + 统一消息类型
│   ├── config.rs        # 通道配置结构体（ChannelConfigs 等）
│   ├── manager.rs       # 生命周期、后台任务、入向消息分发
│   ├── telegram.rs      # Telegram Bot API 实现
│   ├── qq.rs            # QQ Bot API 占位
│   ├── lark.rs          # 飞书/Lark API 占位
│   └── send_tool.rs     # channel_send 内置工具实现
```

核心抽象：

```rust
#[async_trait]
pub trait Channel: Send + Sync + 'static {
    fn name(&self) -> &str;
    async fn send(&self, message: &ChannelOutboundMessage) -> anyhow::Result<()>;
    async fn listen(&self, tx: crossbeam_channel::Sender<ChannelInboundMessage>) -> anyhow::Result<()>;
    async fn health_check(&self) -> bool { true }
}
```

## 与现有代码的关系

实施前需明确当前代码已有能力，避免重复设计：

- `Task` 已有 `origin_channel: ChannelId` 字段（`src/domain/task.rs`），`Task::from_user_input`
  已接收 channel 参数。**本期缺口在消息类型未透传 origin_channel，不在 Task 本身。**
- `FrontendKind` 当前为 `Tui / Telegram / Web`（`src/domain/frontend.rs`）。本期**保留
  `Web` 变体不动**，新增 `QQ` 与 `Feishu`，最终为 `Tui / Telegram / Web / QQ / Feishu`。
- `SystemOutputMessage` 已实现“按 `task_id` 查 `Task::origin_channel` 生成 `Directed` 事件”
  的路由模式（`src/systems/frontend_output.rs`）。本期 `UserOutputMessage` 复用同一模式。
- `HarnessConfig` 位于 `src/app/mod.rs`，通过 `from_env()` 从环境变量加载；`agents.toml`
  是已有的 toml 文件加载先例（由 `HARNESS_AGENTS_CONFIG` 指定路径）。通道配置沿用此模式。

## 与 ECS 的集成

### 入向

`ChannelManager` 为每个已启用通道 spawn 一个 tokio 任务运行 `listen()`。收到消息后，转换为
`ExternalInput::TextWithChannel`，通过 `InputReceiver` 注入 ECS。

当前 `input_ingress_system` 会丢弃 `TextWithChannel` 中的 `channel` 字段（`src/systems/ingress.rs`）。
本设计要求：

- `Signal` 新增 `origin_channel: ChannelId` 字段。
- `UserInputMessage` 新增 `origin_channel`。
- `CreateTaskMessage` 新增 `origin_channel`。
- `user_message_to_task_system` 使用消息中的 `origin_channel`，不再写死 `FrontendKind::Tui`。

> 注意：`Signal` 的结构体字面量构造点（如 `retry_wakeup_system`）也需补字段。

### 出向-主动

新增 `channel_send` 工具，返回 `ToolAction::SendChannelMessage { channel, target, content }`。

为与现有工具执行链路一致，`SendChannelMessage` **不绕过** `handle_tool_action`。处理方式：

- 在 `handle_tool_action` 中新增 `SendChannelMessage` 分支。
- 由于 `handle_tool_action` 通过 `Commands` 写状态、无法直接读 `ChannelManager` Resource，
  本期采用**独立 companion system** 模式：`handle_tool_action` 把待发送消息写入一个
  `PendingChannelSend` 组件 entity，由 `channel_send_dispatch_system`（持有
  `Res<ChannelManager>`）消费并调用 `ChannelManager::send()`，再回写 `ToolExecutionResultMessage`。
- 该模式与项目中已有的 companion system（如 `on_message_dispatched_hook_system`）一致。

### 出向-自动（后续阶段，本期不交付）

后续阶段实现时：

- `UserOutputMessage` 新增 `task_id`，`frontend_output_system` 查找 `Task::origin_channel`
  生成 `EventTarget::Directed` 的 `EngineEvent::Text`（复用 `SystemOutputMessage` 已建立的模式）。
- 每个 `Channel` 实现 `Frontend` trait 并持有出向发送句柄，`push_event` 中真正调用 `send()`。
- 本期 `UserOutputMessage` 暂不新增 `task_id`，避免引入未使用字段。

## 平台实现策略

按 **Telegram → QQ → 飞书** 顺序接入。

### Telegram（本期）

- 长轮询 `getUpdates`。
- 文本按 4096 字符分块发送。
- 白名单支持 username / user_id；**空白名单表示拒绝所有用户**（语义：必须显式配置才放行）。

下列能力**不在本期交付**，作为后续阶段：

- `[IMAGE:path]`、`[VIDEO:path]`、`[DOCUMENT:path]`、`[VOICE:path]` 媒体标记。
- `stream_mode` 下的 `editMessageText` 草稿更新。
- `mention_only` 群组 @ 检测。

> 上述能力未在本期实现，故对应配置项**本期不暴露**，避免引入伪精细控制面。

### QQ（后续阶段）

- OAuth2 app token，WebSocket Gateway 接收事件。
- `msg_type=2` markdown 文本；`msg_type=7` 富媒体。
- 小文件 base64 上传 + 缓存；大文件分片上传。
- 被动回复限速（每 msg_id 每小时 4 条）。

### 飞书 / Lark（后续阶段）

- tenant_access_token，WebSocket 私有协议或 Webhook 接收事件。
- interactive card 发送 Markdown。
- 图片 base64 内联。
- 区分 Lark 与 Feishu endpoint。

## 配置

### 加载链路

通道配置沿用 `agents.toml` 的加载模式：

- 新增可选 toml 配置文件，路径由环境变量 `HARNESS_CHANNELS_CONFIG` 指定，默认不设置（不启动任何通道）。
- `HarnessConfig::from_env()` 在该变量存在时读取并解析为 `ChannelConfigs`，否则使用空默认值。
- 配置结构体定义在 `src/channels/config.rs`，由 `HarnessConfig` 持有 `channels: ChannelConfigs` 字段。

### 配置示例

```toml
[telegram]
bot_token = "${TELEGRAM_BOT_TOKEN}"
allowed_users = ["your_username"]
```

> 本期仅 `telegram` 段生效；`qq` 与 `feishu` 段在对应阶段接入前为占位，不解析。

## ChannelManager 生命周期

- `ChannelManager::new()` 接收通道列表与 `InputReceiver` 的 sender，返回 `(manager, shutdown)`。
  `shutdown` 句柄用于应用退出时优雅停止所有 listen 任务。
- 每个 `Channel::listen()` 任务在独立 tokio task 中运行；任务失败**不退出应用**，
  采用指数退避（起始 1s，上限 60s）后重启。
- `ChannelManager::send(channel_name, message)` 为同步入队（`mpsc::UnboundedSender`），
  实际网络发送在后台 task 中执行，避免阻塞 ECS。
- 发送失败通过 `tracing::error` 记录，并可生成 `SystemOutputMessage` 反馈给对应 Task。

## 错误处理

- 网络超时、token 过期、API 限流在通道内部重试。
- `listen()` 任务异常退出后由 `ChannelManager` 重启（指数退避）。
- `channel_send` 入队失败（如通道不存在）立即返回错误给工具调用方。

## 测试

- 单元测试：配置解析、白名单匹配（含空白名单拒绝）。
- 集成测试：使用 `wiremock` 模拟 Telegram Bot API（`sendMessage`、`getUpdates`）。
- 链路测试：`origin_channel` 从 `ExternalInput` 透传到 `Task` 的端到端断言（含 TUI 路径回归）。
- 手动测试：真实 bot 收发消息，验证与 TUI 共存。

## 依赖

本期新增 crate（符合项目依赖原则，按需引入）：

```toml
reqwest = { version = "0.12", features = ["json"] }
async-trait = "0.1"
```

后续阶段按需追加：

```toml
# QQ / 飞书阶段
tokio-tungstenite = { version = "0.24", features = ["native-tls"] }
prost = "0.13"
bytes = "1"
# Telegram 媒体阶段
reqwest = { version = "0.12", features = ["json", "multipart"] }
```

## 文档同步

- `docs/current-state.md`：新增通道能力描述（仅本期交付项）。
- `docs/configuration.md`：新增通道配置说明与加载链路。
- `.env.example`：新增 `HARNESS_CHANNELS_CONFIG` 与 `TELEGRAM_BOT_TOKEN` 示例。
- `docs/design/README.md` 与 `docs/README.md`：索引本设计文档。

## 后续阶段

- **阶段 2：出向-自动**：`UserOutputMessage` 携带 `task_id`，`Channel` 实现 `Frontend` 并真正发送。
- **阶段 3：QQ 通道**：OAuth2、WebSocket Gateway、markdown/富媒体发送。
- **阶段 4：飞书/Lark 通道**：tenant token、WebSocket/Webhook、interactive card。
- **阶段 5：媒体附件**：统一 `[IMAGE:path]` 等标记，支持三平台下载/上传。
- **阶段 6：Telegram 增强**：媒体标记、`stream_mode` 草稿更新、`mention_only` 群组检测。
