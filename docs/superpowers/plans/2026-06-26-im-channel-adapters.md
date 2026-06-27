# IM 通道适配（Telegram / QQ / 飞书）实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在 Harness 中新增统一的 IM 通道抽象层，并以 Telegram 为首个实现，支持用户从 IM 触发 Task，以及 Agent 通过 `channel_send` 工具主动推送。**本期不交付“出向-自动”**（按 `origin_channel` 自动回执），该能力作为后续阶段。

**Architecture:** 新增 `src/channels/` 模块，定义 `Channel` trait 与 `ChannelManager`；入向消息通过 `ExternalInput::TextWithChannel` 注入 ECS 并透传 `origin_channel` 到 `Task`；`channel_send` 工具产生 `ToolAction::SendChannelMessage`，由 `handle_tool_action` 写入 `PendingChannelSend` 组件，再由 companion system `channel_send_dispatch_system` 消费并调用 `ChannelManager::send()`。

**Tech Stack:** Rust, Bevy ECS, tokio, reqwest, crossbeam-channel, async-trait

## Global Constraints

- 所有代码变更必须遵循 `AGENTS.md` 项目规范。
- 新增依赖必须来自 crates.io，MIT 或 Apache-2.0 兼容，且**按当前阶段真实需求引入**（WebSocket/protobuf 推迟到 QQ/飞书阶段）。
- 不破坏现有 TUI 主链路。
- 同一变更涉及的代码与文档应尽量放在同一提交中。
- 关键路径必须有单元测试或集成测试覆盖。
- 文档使用中文，可夹杂必要英文术语。
- 本期保留 `FrontendKind::Web` 变体不动，仅新增 `QQ` 与 `Feishu`。
- 本期不修改 `UserOutputMessage`（`task_id` 字段属后续“出向-自动”阶段）。

---

## File Structure

```text
src/
├── channels/
│   ├── mod.rs           # 模块导出、启动入口
│   ├── traits.rs        # Channel trait + 统一消息类型
│   ├── config.rs        # ChannelConfigs / TelegramConfig 等配置结构体
│   ├── manager.rs       # ChannelManager：生命周期、后台任务、出向发送
│   ├── telegram.rs      # Telegram Bot API 实现
│   ├── qq.rs            # QQ Bot API 占位
│   ├── lark.rs          # 飞书/Lark API 占位
│   └── send_tool.rs     # channel_send 工具实现
├── domain/
│   ├── frontend.rs      # FrontendKind 新增 QQ / Feishu（保留 Web）
│   ├── message.rs       # Signal / UserInputMessage / CreateTaskMessage 扩展 origin_channel
│   └── space.rs         # ToolAction 新增 SendChannelMessage 变体
├── systems/
│   ├── ingress.rs               # input_ingress_system 保留 origin_channel；修复 Signal 字面量
│   ├── transform/
│   │   ├── signal_ingest.rs     # 透传 origin_channel
│   │   └── task_creation.rs     # 使用消息中的 origin_channel
│   ├── routing.rs               # 透传 origin_channel
│   ├── frontend_input.rs        # TUI UserAction::Text 透传 channel
│   └── tools/
│       ├── orchestrator.rs      # handle_tool_action 新增 SendChannelMessage 分支（spawn PendingChannelSend）
│       ├── channel_send_dispatch.rs  # companion system：消费 PendingChannelSend
│       └── builtin/
│           └── mod.rs           # 注册 channel_send 工具
├── app/
│   └── mod.rs           # HarnessConfig 新增 channels 字段；from_env 加载 toml
└── main.rs              # 启动 ChannelManager 并插入 Resource

tests/
├── channels_telegram.rs # Telegram 集成测试（wiremock）
└── channel_send_tool.rs # channel_send 工具集成测试
```

> 注：配置结构体放在 `src/channels/config.rs`，不新建 `src/config/` 目录（仓库当前无此目录，`HarnessConfig` 位于 `src/app/mod.rs`）。

---

## Task 1: 新增依赖与基础类型

**Files:**
- Modify: `Cargo.toml`
- Create: `src/channels/mod.rs`
- Create: `src/channels/traits.rs`
- Create: `src/channels/qq.rs`
- Create: `src/channels/lark.rs`
- Test: `src/channels/traits.rs` 内 `#[cfg(test)]`

**Interfaces:**
- Produces: `Channel` trait, `ChannelInboundMessage`, `ChannelOutboundMessage`, `ChannelError`

- [ ] **Step 1: 在 Cargo.toml 添加依赖（仅本期所需）**

```toml
[dependencies]
# ... existing deps ...
reqwest = { version = "0.12", features = ["json"] }
async-trait = "0.1"
```

> 不引入 `tokio-tungstenite` / `prost` / `bytes` / `tokio-util`，它们属于 QQ/飞书阶段。

Run: `cargo check`
Expected: passes (downloads crates).

- [ ] **Step 2: 创建 src/channels/traits.rs**

```rust
use anyhow::Result;
use async_trait::async_trait;
use crossbeam_channel::Sender;
use serde::{Deserialize, Serialize};

use crate::domain::{ChannelId, FrontendKind};

/// 统一入向消息
#[derive(Debug, Clone)]
pub struct ChannelInboundMessage {
    pub channel_name: String,
    pub sender_id: String,
    pub chat_id: String,
    pub thread_id: Option<String>,
    pub content: String,
    pub timestamp_secs: u64,
}

impl ChannelInboundMessage {
    pub fn to_external_input(&self) -> crate::domain::ExternalInput {
        crate::domain::ExternalInput::TextWithChannel {
            channel: ChannelId {
                frontend: match self.channel_name.as_str() {
                    "telegram" => FrontendKind::Telegram,
                    "qq" => FrontendKind::QQ,
                    "feishu" => FrontendKind::Feishu,
                    _ => FrontendKind::Tui,
                },
                user_id: self.chat_id.clone(),
            },
            content: self.content.clone(),
        }
    }
}

/// 统一出向消息
#[derive(Debug, Clone)]
pub struct ChannelOutboundMessage {
    pub recipient: String,
    pub thread_id: Option<String>,
    pub content: String,
}

/// 通道错误
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
}

#[async_trait]
pub trait Channel: Send + Sync + 'static {
    fn name(&self) -> &str;

    async fn send(&self, message: &ChannelOutboundMessage) -> Result<(), ChannelError>;

    async fn listen(&self, tx: Sender<ChannelInboundMessage>) -> Result<(), ChannelError>;

    async fn health_check(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_inbound_to_external_input() {
        let msg = ChannelInboundMessage {
            channel_name: "telegram".to_string(),
            sender_id: "123".to_string(),
            chat_id: "456".to_string(),
            thread_id: None,
            content: "hello".to_string(),
            timestamp_secs: 0,
        };
        let input = msg.to_external_input();
        match input {
            crate::domain::ExternalInput::TextWithChannel { channel, content } => {
                assert_eq!(channel.frontend, FrontendKind::Telegram);
                assert_eq!(channel.user_id, "123");
                assert_eq!(content, "hello");
            }
            _ => panic!("unexpected variant"),
        }
    }
}
```

Run: `cargo test -p harness channels::traits`
Expected: passes.

- [ ] **Step 3: 创建 src/channels/mod.rs 初始版本**

```rust
pub mod lark;
pub mod manager;
pub mod qq;
pub mod traits;

pub use traits::{Channel, ChannelError, ChannelInboundMessage, ChannelOutboundMessage};
```

> `config.rs`、`send_tool.rs`、`telegram.rs` 在后续 Task 创建；此处先不导出，避免编译错误。
> `manager` 模块在 Task 5 创建前可先放空文件占位，或本 Task 暂不声明 `pub mod manager`。
> 推荐做法：本 Task 只声明已创建的模块，随后续 Task 增量添加 `pub mod` 行。

- [ ] **Step 4: 创建 qq.rs 与 lark.rs 占位**

Create `src/channels/qq.rs`:

```rust
//! QQ Bot API 通道实现（后续阶段接入）
```

Create `src/channels/lark.rs`:

```rust
//! 飞书/Lark 通道实现（后续阶段接入）
```

Run: `cargo check`
Expected: passes.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml src/channels/
git commit -m "feat(channels): add Channel trait and scaffolding for IM adapters"
```

---

## Task 2: 扩展 FrontendKind 与 domain 消息类型

**Files:**
- Modify: `src/domain/frontend.rs`
- Modify: `src/domain/message.rs`
- Modify: `src/domain/mod.rs`（如需导出 ChannelId 已存在则不动）
- Test: `src/domain/message.rs` 内 `#[cfg(test)]`

**Interfaces:**
- Produces: `FrontendKind::QQ/Feishu`（保留 `Web`），`Signal` 含 `origin_channel`, `UserInputMessage` 含 `origin_channel`, `CreateTaskMessage` 含 `origin_channel`

> 本期**不**修改 `UserOutputMessage`（`task_id` 属后续“出向-自动”阶段）。

- [ ] **Step 1: 修改 src/domain/frontend.rs，新增 QQ / Feishu 并保留 Web**

```rust
#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub enum FrontendKind {
    Tui,
    Telegram,
    Web,
    QQ,
    Feishu,
}
```

更新 `Display` 实现（如存在）：为 `Web` / `QQ` / `Feishu` 增加分支。

Run: `cargo check`
Expected: passes.

- [ ] **Step 2: 修改 src/domain/message.rs，为 Signal 新增 origin_channel 字段**

```rust
#[derive(Debug, Clone, Component)]
pub struct Signal {
    pub kind: SignalType,
    pub payload: SignalPayload,
    pub origin_channel: ChannelId,
}
```

- [ ] **Step 3: 更新 Signal 便捷构造函数**

```rust
impl Signal {
    /// 旧调用点兼容：默认 Tui 通道。
    pub fn user_input(content: impl Into<String>) -> Self {
        Self::user_input_with_channel(
            content,
            ChannelId {
                frontend: FrontendKind::Tui,
                user_id: "default".to_string(),
            },
        )
    }

    pub fn user_input_with_channel(content: impl Into<String>, origin_channel: ChannelId) -> Self {
        Self {
            kind: SignalType::UserInput,
            payload: SignalPayload::UserInput(content.into()),
            origin_channel,
        }
    }

    /// 重试唤醒信号：origin_channel 对当前 task 无意义，使用 Tui 默认。
    pub fn retry_wakeup(task_id: TaskId) -> Self {
        Self {
            kind: SignalType::RetryWakeup,
            payload: SignalPayload::RetryWakeup(task_id),
            origin_channel: ChannelId {
                frontend: FrontendKind::Tui,
                user_id: "default".to_string(),
            },
        }
    }
}
```

- [ ] **Step 4: 为 UserInputMessage / CreateTaskMessage 新增 origin_channel**

```rust
#[derive(Debug, Clone, Component)]
pub struct UserInputMessage {
    pub content: String,
    pub origin_channel: ChannelId,
}

#[derive(Debug, Clone, Component)]
pub struct CreateTaskMessage {
    pub content: String,
    pub origin_channel: ChannelId,
}
```

- [ ] **Step 5: 修复所有构造点（含结构体字面量）**

需覆盖的构造点：

1. `Signal::user_input(...)` 调用点：保持不变（走默认 Tui）。
2. **`src/systems/ingress.rs` 中 `retry_wakeup_system` 的 `Signal { kind, payload }` 字面量构造**：改为 `Signal::retry_wakeup(task.id)`，避免手写字面量遗漏字段。
3. `UserInputMessage { content }` 字面量：补充 `origin_channel`。
4. `CreateTaskMessage { content }` 字面量：补充 `origin_channel`。

搜索全仓库 `Signal {`、`UserInputMessage {`、`CreateTaskMessage {` 字面量，按编译错误逐个修复。

Run: `cargo check`
Expected: passes.

- [ ] **Step 6: 添加单元测试**

在 `src/domain/message.rs` 末尾 `#[cfg(test)]` 中新增：

```rust
#[test]
fn signal_user_input_carries_default_channel() {
    let signal = Signal::user_input("hi");
    assert_eq!(signal.origin_channel.frontend, FrontendKind::Tui);
}

#[test]
fn signal_user_input_with_channel_preserves_channel() {
    let channel = ChannelId {
        frontend: FrontendKind::Telegram,
        user_id: "u1".to_string(),
    };
    let signal = Signal::user_input_with_channel("hi", channel.clone());
    assert_eq!(signal.origin_channel, channel);
}
```

Run: `cargo test -p harness domain::message`
Expected: passes.

- [ ] **Step 7: Commit**

```bash
git add src/domain/
git commit -m "feat(domain): carry origin_channel through Signal/UserInput/CreateTask"
```

---

## Task 3: 更新入向链路以透传 origin_channel

**Files:**
- Modify: `src/systems/ingress.rs`
- Modify: `src/systems/transform/signal_ingest.rs`
- Modify: `src/systems/frontend_input.rs`
- Test: 相关系统测试

**Interfaces:**
- Consumes: `ExternalInput::TextWithChannel { channel, content }`
- Produces: `UserInputMessage { content, origin_channel }`

- [ ] **Step 1: 修改 src/systems/ingress.rs**

当前 `ExternalInput::TextWithChannel` 分支用 `channel: _` 丢弃了 channel。改为捕获并透传：

```rust
ExternalInput::TextWithChannel { channel, content } => {
    debug!(
        event = "ExternalInputReceived",
        kind = "TextWithChannel",
        content_len = content.len(),
        "received external text input"
    );
    commands.spawn((
        Signal::user_input_with_channel(content, channel),
        MessageReceivedHookPending,
    ));
}
```

同时确认 Step 5 of Task 2 已把 `retry_wakeup_system` 的 `Signal { ... }` 字面量改为 `Signal::retry_wakeup(task.id)`。

- [ ] **Step 2: 修改 src/systems/transform/signal_ingest.rs**

把 `Signal` 转为 `UserInputMessage` 时透传 `origin_channel`：

```rust
SignalType::UserInput => {
    if let SignalPayload::UserInput(content) = &signal.payload {
        commands.spawn(UserInputMessage {
            content: content.clone(),
            origin_channel: signal.origin_channel.clone(),
        });
    }
}
```

- [ ] **Step 3: 修改 src/systems/frontend_input.rs**

TUI 前端 `UserAction::Text { channel, content }` 当前可能忽略 `channel`。改为透传：

```rust
UserAction::Text { channel, content } => {
    commands.spawn(Signal::user_input_with_channel(content, channel));
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p harness systems`
Expected: passes.

- [ ] **Step 5: Commit**

```bash
git add src/systems/ingress.rs src/systems/transform/signal_ingest.rs src/systems/frontend_input.rs
git commit -m "feat(systems): preserve origin_channel through input ingress and signal ingest"
```

---

## Task 4: 更新路由与任务创建以透传 origin_channel

**Files:**
- Modify: `src/systems/routing.rs`
- Modify: `src/systems/transform/task_creation.rs`
- Modify: `src/systems/command.rs`（如存在 CreateTaskMessage 构造）
- Test: `src/systems/transform/task_creation.rs` 内 `#[cfg(test)]`

- [ ] **Step 1: 修改 src/systems/routing.rs**

`UserInputMessage` 转 `CreateTaskMessage` 时携带 `origin_channel`：

```rust
for (entity, msg) in user_inputs.iter() {
    commands.spawn(CreateTaskMessage {
        content: msg.content.clone(),
        origin_channel: msg.origin_channel.clone(),
    });
    commands.entity(entity).despawn();
}
```

- [ ] **Step 2: 修改 src/systems/transform/task_creation.rs**

`user_message_to_task_system` 把写死的 `FrontendKind::Tui` 替换为消息中的 `origin_channel`：

```rust
let task = Task::from_user_input(
    msg.content.clone(),
    settings.0.max_retries,
    msg.origin_channel.clone(),
);
```

- [ ] **Step 3: 修复 command.rs 中的 CreateTaskMessage 构造点**

搜索 `CreateTaskMessage {` 与 `Task::from_user_input` 全仓库，确保子任务创建处使用 `input.origin_channel.clone()` 而非写死 Tui。

`ContinueTaskMessage` 不需要新增 `origin_channel`（通过 `task_id` 引用已有 Task，Task 已保存 `origin_channel`）。

- [ ] **Step 4: Run tests**

Run: `cargo test -p harness task_creation`
Expected: passes.

- [ ] **Step 5: Commit**

```bash
git add src/systems/routing.rs src/systems/transform/task_creation.rs src/systems/command.rs
git commit -m "feat(systems): route origin_channel into Task creation"
```

---

## Task 5: 实现 ChannelManager（含重启退避与关闭语义）

**Files:**
- Create: `src/channels/manager.rs`
- Modify: `src/channels/mod.rs`
- Test: `src/channels/manager.rs` 内 `#[cfg(test)]`

**Interfaces:**
- Consumes: `Arc<dyn Channel>`, `crossbeam_channel::Sender<ExternalInput>`
- Produces: `ChannelManager::new()`, `ChannelManager::send()`, `ChannelManager::shutdown()`

- [ ] **Step 1: 创建 src/channels/manager.rs**

关键设计：

- listen 任务在 supervisor 中循环重启，指数退避（1s → 60s 上限）。
- shutdown 通过 `tokio::sync::broadcast` 信号通知所有 supervisor 退出。
- `send()` 同步入队 `mpsc::UnboundedSender`，网络发送在后台 task 执行。

```rust
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use crossbeam_channel::Sender;
use tokio::sync::{broadcast, mpsc};
use tracing::{error, info, warn};

use crate::domain::ExternalInput;

use super::traits::{Channel, ChannelOutboundMessage};

#[derive(Clone)]
pub struct ChannelManager {
    channels: Vec<Arc<dyn Channel>>,
    outbound_tx: mpsc::UnboundedSender<(String, ChannelOutboundMessage)>,
    shutdown_tx: broadcast::Sender<()>,
}

impl ChannelManager {
    pub fn new(
        channels: Vec<Arc<dyn Channel>>,
        external_input_tx: Sender<ExternalInput>,
    ) -> (Self, tokio::task::JoinHandle<()>) {
        let (outbound_tx, mut outbound_rx) =
            mpsc::unbounded_channel::<(String, ChannelOutboundMessage)>();
        let (shutdown_tx, _) = broadcast::channel::<()>(1);

        let supervisor_channels = channels.clone();
        let supervisor_shutdown = shutdown_tx.clone();
        let supervisor_input_tx = external_input_tx.clone();

        let handle = tokio::spawn(async move {
            // 启动每个通道的 listen supervisor
            for channel in &supervisor_channels {
                let ch = channel.clone();
                let tx = supervisor_input_tx.clone();
                let mut shutdown_rx = supervisor_shutdown.subscribe();
                let name = ch.name().to_string();
                tokio::spawn(async move {
                    let mut backoff = Duration::from_secs(1);
                    info!(event = "ChannelListenStart", channel = %name, "starting channel listener");
                    loop {
                        tokio::select! {
                            _ = shutdown_rx.recv() => {
                                info!(event = "ChannelListenStopped", channel = %name, "shutdown signal received");
                                break;
                            }
                            res = ch.listen(tx.clone()) => {
                                match res {
                                    Ok(()) => {
                                        info!(event = "ChannelListenEnd", channel = %name, "listener exited cleanly");
                                        break;
                                    }
                                    Err(e) => {
                                        warn!(event = "ChannelListenExit", channel = %name, error = %e, "listener failed, will restart");
                                        tokio::select! {
                                            _ = shutdown_rx.recv() => break,
                                            _ = tokio::time::sleep(backoff) => {}
                                        }
                                        backoff = (backoff * 2).min(Duration::from_secs(60));
                                    }
                                }
                            }
                        }
                    }
                });
            }

            // 出向发送循环
            let send_channels = supervisor_channels.clone();
            let mut send_shutdown = supervisor_shutdown.subscribe();
            loop {
                tokio::select! {
                    _ = send_shutdown.recv() => break,
                    msg = outbound_rx.recv() => {
                        let Some((name, message)) = msg else { break };
                        if let Some(channel) = send_channels.iter().find(|c| c.name() == name) {
                            if let Err(e) = channel.send(&message).await {
                                error!(event = "ChannelSendFailed", channel = %name, error = %e, "failed to send outbound message");
                            }
                        } else {
                            warn!(event = "ChannelNotFound", channel = %name, "no such channel for outbound message");
                        }
                    }
                }
            }
        });

        (
            Self {
                channels,
                outbound_tx,
                shutdown_tx,
            },
            handle,
        )
    }

    /// 同步入队出向消息，立即返回。网络发送在后台执行。
    pub fn send(
        &self,
        channel_name: String,
        message: ChannelOutboundMessage,
    ) -> Result<()> {
        if !self.channels.iter().any(|c| c.name() == channel_name) {
            anyhow::bail!("channel not found: {channel_name}");
        }
        self.outbound_tx
            .send((channel_name, message))
            .map_err(|_| anyhow::anyhow!("channel manager outbound channel closed"))?;
        Ok(())
    }

    /// 通知所有 listen / send 任务退出。
    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use crossbeam_channel::unbounded;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    struct DummyChannel {
        name: String,
        send_count: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Channel for DummyChannel {
        fn name(&self) -> &str {
            &self.name
        }
        async fn send(&self, _msg: &ChannelOutboundMessage) -> Result<(), super::super::traits::ChannelError> {
            self.send_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        async fn listen(
            &self,
            tx: Sender<super::super::traits::ChannelInboundMessage>,
        ) -> Result<(), super::super::traits::ChannelError> {
            let _ = tx.send(super::super::traits::ChannelInboundMessage {
                channel_name: self.name.clone(),
                sender_id: "u1".to_string(),
                chat_id: "c1".to_string(),
                thread_id: None,
                content: "ping".to_string(),
                timestamp_secs: 0,
            });
            // 立即返回，触发 supervisor 退避重启逻辑（测试中 shutdown 会停止它）
            Err(super::super::traits::ChannelError::NotConfigured)
        }
    }

    #[tokio::test]
    async fn manager_receives_inbound_and_sends_outbound() {
        let (input_tx, input_rx) = unbounded::<ExternalInput>();
        let send_count = Arc::new(AtomicUsize::new(0));
        let channel = Arc::new(DummyChannel {
            name: "dummy".to_string(),
            send_count: send_count.clone(),
        }) as Arc<dyn Channel>;
        let (manager, _handle) = ChannelManager::new(vec![channel], input_tx);

        // 入向：listen 会立即发一条消息
        let input = tokio::time::timeout(Duration::from_secs(2), input_rx.recv())
            .await
            .expect("timeout")
            .expect("receive inbound");
        match input {
            ExternalInput::TextWithChannel { content, .. } => assert_eq!(content, "ping"),
            _ => panic!("unexpected"),
        }

        // 出向：send 入队后后台 task 异步发送
        manager
            .send(
                "dummy".to_string(),
                ChannelOutboundMessage {
                    recipient: "c1".to_string(),
                    thread_id: None,
                    content: "pong".to_string(),
                },
            )
            .expect("queue outbound");

        // 等待后台发送完成
        tokio::time::timeout(Duration::from_secs(2), async {
            while send_count.load(Ordering::SeqCst) == 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("send timeout");
        assert!(send_count.load(Ordering::SeqCst) >= 1);

        manager.shutdown();
    }

    #[tokio::test]
    async fn send_unknown_channel_errors() {
        let (_input_tx, input_rx) = unbounded::<ExternalInput>();
        let (manager, _handle) =
            ChannelManager::new(vec![], input_rx);
        let result = manager.send(
            "nope".to_string(),
            ChannelOutboundMessage {
                recipient: "x".to_string(),
                thread_id: None,
                content: "x".to_string(),
            },
        );
        assert!(result.is_err());
    }
}
```

- [ ] **Step 2: 更新 src/channels/mod.rs**

```rust
pub mod manager;
// ... existing mods

pub use manager::ChannelManager;
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p harness channels::manager`
Expected: passes.

- [ ] **Step 4: Commit**

```bash
git add src/channels/manager.rs src/channels/mod.rs
git commit -m "feat(channels): implement ChannelManager with restart backoff and shutdown"
```

---

## Task 6: 通道配置结构体与加载链路

**Files:**
- Create: `src/channels/config.rs`
- Modify: `src/channels/mod.rs`
- Modify: `src/app/mod.rs`（HarnessConfig 新增 channels 字段，from_env 加载）
- Test: `src/channels/config.rs` 内 `#[cfg(test)]`

**Interfaces:**
- Produces: `ChannelConfigs`, `TelegramConfig`

> 配置结构体放 `src/channels/config.rs`，不新建 `src/config/` 目录。

- [ ] **Step 1: 创建 src/channels/config.rs**

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChannelConfigs {
    #[serde(default)]
    pub telegram: Option<TelegramConfig>,
    // qq / feishu 在后续阶段接入前不解析
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelegramConfig {
    pub bot_token: String,
    #[serde(default)]
    pub allowed_users: Vec<String>,
}
```

> 本期不暴露 `stream_mode` / `mention_only` / `proxy_url`，避免引入未实现的控制面。

- [ ] **Step 2: 在 HarnessConfig 新增 channels 字段**

在 `src/app/mod.rs` 的 `HarnessConfig` 结构体中新增：

```rust
pub channels: crate::channels::config::ChannelConfigs,
```

在 `from_env()` 中加载：

```rust
let channels = {
    let path = std::env::var("HARNESS_CHANNELS_CONFIG").ok();
    match path {
        Some(p) if !p.is_empty() => {
            let text = std::fs::read_to_string(&p)
                .with_context(|| format!("read channels config: {p}"))?;
            toml::from_str(&text).context("parse channels config")?
        }
        _ => crate::channels::config::ChannelConfigs::default(),
    }
};
```

> 复用现有 `toml` 依赖（`toml = "0.8"`）。环境变量 `HARNESS_CHANNELS_CONFIG` 指向 toml 文件，默认不设置（不启动通道）。

- [ ] **Step 3: 更新 src/channels/mod.rs 导出**

```rust
pub mod config;
pub use config::{ChannelConfigs, TelegramConfig};
```

- [ ] **Step 4: 添加配置解析测试**

在 `src/channels/config.rs` 的 `#[cfg(test)]` 中新增：

```rust
#[test]
fn parse_telegram_config() {
    let toml = r#"
[telegram]
bot_token = "xxx"
allowed_users = ["alice"]
"#;
    let cfg: ChannelConfigs = toml::from_str(toml).expect("parse");
    let tg = cfg.telegram.expect("telegram present");
    assert_eq!(tg.bot_token, "xxx");
    assert_eq!(tg.allowed_users, vec!["alice".to_string()]);
}

#[test]
fn empty_config_is_default() {
    let cfg: ChannelConfigs = toml::from_str("").expect("parse empty");
    assert!(cfg.telegram.is_none());
}
```

Run: `cargo test -p harness channels::config`
Expected: passes.

- [ ] **Step 5: Commit**

```bash
git add src/channels/config.rs src/channels/mod.rs src/app/mod.rs
git commit -m "feat(config): add ChannelConfigs loaded from HARNESS_CHANNELS_CONFIG"
```

---

## Task 7: 实现 TelegramChannel

**Files:**
- Create: `src/channels/telegram.rs`
- Modify: `src/channels/mod.rs`
- Test: `src/channels/telegram.rs` 内 `#[cfg(test)]`

- [ ] **Step 1: 创建 src/channels/telegram.rs**

```rust
use std::sync::atomic::{AtomicI64, Ordering};

use async_trait::async_trait;
use crossbeam_channel::Sender;
use reqwest::Client;
use serde::Deserialize;
use serde_json::json;
use tracing::{debug, warn};

use crate::channels::config::TelegramConfig;

use super::traits::{Channel, ChannelError, ChannelInboundMessage, ChannelOutboundMessage};

pub struct TelegramChannel {
    config: TelegramConfig,
    client: Client,
    base_url: String,
    last_update_id: AtomicI64,
}

impl TelegramChannel {
    pub fn new(config: TelegramConfig) -> Self {
        Self {
            config,
            client: Client::new(),
            base_url: "https://api.telegram.org".to_string(),
            last_update_id: AtomicI64::new(0),
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    fn api_url(&self, method: &str) -> String {
        format!("{}/bot{}/{}", self.base_url, self.config.bot_token, method)
    }

    /// 白名单匹配：username（忽略大小写）或 user_id。
    /// 空白名单表示拒绝所有用户（必须显式配置才放行）。
    fn is_allowed(&self, user: &TelegramUser) -> bool {
        if self.config.allowed_users.is_empty() {
            return false;
        }
        self.config.allowed_users.iter().any(|allowed| {
            if let Some(username) = &user.username {
                if username.eq_ignore_ascii_case(allowed) {
                    return true;
                }
            }
            if let Ok(id) = allowed.parse::<i64>() {
                if user.id == id {
                    return true;
                }
            }
            false
        })
    }
}

#[async_trait]
impl Channel for TelegramChannel {
    fn name(&self) -> &str {
        "telegram"
    }

    async fn send(&self, message: &ChannelOutboundMessage) -> Result<(), ChannelError> {
        // 文本分块（4096 字符上限）
        for chunk in split_text(&message.content, 4096) {
            let url = self.api_url("sendMessage");
            let mut payload = json!({
                "chat_id": message.recipient,
                "text": chunk,
            });
            if let Some(thread_id) = &message.thread_id {
                if let Ok(id) = thread_id.parse::<i64>() {
                    payload["message_thread_id"] = json!(id);
                }
            }
            let resp = self.client.post(&url).json(&payload).send().await?;
            if !resp.status().is_success() {
                let text = resp.text().await.unwrap_or_default();
                return Err(ChannelError::Api { code: 0, message: text });
            }
        }
        Ok(())
    }

    async fn listen(&self, tx: Sender<ChannelInboundMessage>) -> Result<(), ChannelError> {
        loop {
            let url = self.api_url("getUpdates");
            let offset = self.last_update_id.load(Ordering::SeqCst) + 1;
            let resp = self
                .client
                .get(&url)
                .query(&[("offset", offset.to_string()), ("limit", "100".to_string())])
                .send()
                .await?;

            if !resp.status().is_success() {
                let text = resp.text().await.unwrap_or_default();
                return Err(ChannelError::Api { code: 0, message: text });
            }

            let data: TelegramGetUpdatesResponse = resp.json().await?;
            for update in data.result {
                if let Some(msg) = update.message {
                    self.last_update_id.store(update.update_id, Ordering::SeqCst);

                    if !self.is_allowed(&msg.from) {
                        warn!(
                            event = "TelegramUserDenied",
                            user_id = %msg.from.id,
                            "user not in allowed list"
                        );
                        continue;
                    }

                    let _ = tx.send(ChannelInboundMessage {
                        channel_name: self.name().to_string(),
                        sender_id: msg.from.id.to_string(),
                        chat_id: msg.chat.id.to_string(),
                        thread_id: msg.message_thread_id.map(|id| id.to_string()),
                        content: msg.text.unwrap_or_default(),
                        timestamp_secs: msg.date as u64,
                    });
                }
            }

            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
        }
    }
}

fn split_text(text: &str, max_len: usize) -> Vec<String> {
    if text.len() <= max_len {
        return vec![text.to_string()];
    }
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let end = (start + max_len).min(text.len());
        // 不切断 UTF-8 字符边界
        let end = text.floor_char_boundary(end);
        chunks.push(text[start..end].to_string());
        start = end;
    }
    chunks
}

#[derive(Debug, Deserialize)]
struct TelegramGetUpdatesResponse {
    result: Vec<TelegramUpdate>,
}

#[derive(Debug, Deserialize)]
struct TelegramUpdate {
    update_id: i64,
    message: Option<TelegramMessage>,
}

#[derive(Debug, Deserialize)]
struct TelegramMessage {
    from: TelegramUser,
    chat: TelegramChat,
    date: i64,
    text: Option<String>,
    message_thread_id: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct TelegramUser {
    id: i64,
    username: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TelegramChat {
    id: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(users: Vec<String>) -> TelegramConfig {
        TelegramConfig {
            bot_token: "x".to_string(),
            allowed_users: users,
        }
    }

    #[test]
    fn allowed_user_by_username() {
        let ch = TelegramChannel::new(cfg(vec!["alice".to_string()]));
        let user = TelegramUser { id: 1, username: Some("Alice".to_string()) };
        assert!(ch.is_allowed(&user));
    }

    #[test]
    fn allowed_user_by_id() {
        let ch = TelegramChannel::new(cfg(vec!["123".to_string()]));
        let user = TelegramUser { id: 123, username: None };
        assert!(ch.is_allowed(&user));
    }

    #[test]
    fn empty_allowlist_denies_all() {
        let ch = TelegramChannel::new(cfg(vec![]));
        let user = TelegramUser { id: 1, username: Some("anyone".to_string()) };
        assert!(!ch.is_allowed(&user));
    }

    #[test]
    fn split_text_respects_char_boundary() {
        let s = "a".repeat(4097);
        let chunks = split_text(&s, 4096);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].len(), 4096);
    }
}
```

> 注：`str::floor_char_boundary` 在 Rust 1.80+ 稳定。若工具链较旧，改用手动 UTF-8 边界回退实现。

- [ ] **Step 2: 更新 src/channels/mod.rs**

```rust
pub mod telegram;
pub use telegram::TelegramChannel;
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p harness channels::telegram`
Expected: passes.

- [ ] **Step 4: Commit**

```bash
git add src/channels/telegram.rs src/channels/mod.rs
git commit -m "feat(channels): implement TelegramChannel with long-polling and allowlist"
```

---

## Task 8: 在 main.rs 启动 ChannelManager

**Files:**
- Modify: `src/main.rs`
- Modify: `src/app/mod.rs`（build_harness_app 注册 ChannelManager Resource 与 channel_send_dispatch_system）

> 本期**不**为 TelegramChannel 实现 `Frontend` trait（“出向-自动”属后续阶段）。

- [ ] **Step 1: 修改 src/main.rs**

```rust
use std::sync::Arc;

use crossbeam_channel::unbounded;
use harness::channels::{ChannelManager, TelegramChannel};
use harness::domain::ExternalInput;

let (input_tx, input_rx) = unbounded::<ExternalInput>();

let mut channels: Vec<Arc<dyn harness::channels::Channel>> = vec![];
if let Some(tg_cfg) = config.channels.telegram.clone() {
    channels.push(Arc::new(TelegramChannel::new(tg_cfg)));
}

let (channel_manager, channel_handle) = ChannelManager::new(channels, input_tx);
```

把 `input_rx` 与 `channel_manager` 传入 `build_harness_app`，`channel_handle` 在主循环退出后 await 并调用 `channel_manager.shutdown()`。

- [ ] **Step 2: 在 build_harness_app 注册 Resource 与 companion system**

```rust
app.insert_resource(channel_manager);
app.add_systems(
    Update,
    crate::systems::tools::channel_send_dispatch::channel_send_dispatch_system
        .in_set(HarnessSet::Dispatch)
        .after(crate::systems::tools::dispatch::tool_dispatch_system),
);
```

> `channel_send_dispatch_system` 在 Task 9 实现。

- [ ] **Step 3: Compile check**

Run: `cargo check`
Expected: passes.

- [ ] **Step 4: Commit**

```bash
git add src/main.rs src/app/mod.rs
git commit -m "feat(main): wire ChannelManager into startup and ECS resources"
```

---

## Task 9: 实现 channel_send 工具与 companion system

**Files:**
- Create: `src/channels/send_tool.rs`
- Create: `src/systems/tools/channel_send_dispatch.rs`
- Modify: `src/channels/mod.rs`
- Modify: `src/domain/space.rs`（ToolAction 新增变体 + PendingChannelSend 组件）
- Modify: `src/domain/message.rs`（如需导出 PendingChannelSend）
- Modify: `src/systems/tools/orchestrator.rs`（handle_tool_action 新增分支）
- Modify: `src/systems/tools/mod.rs`（注册工具与系统）
- Test: `src/channels/send_tool.rs` 内 `#[cfg(test)]`

> 关键设计：`SendChannelMessage` **不绕过** `handle_tool_action`。`handle_tool_action` spawn
> `PendingChannelSend` 组件 entity，由 `channel_send_dispatch_system` 消费。

- [ ] **Step 1: 在 src/domain/space.rs 新增 ToolAction 变体与 PendingChannelSend 组件**

```rust
pub enum ToolAction {
    // ... existing variants ...
    /// 向 IM 通道发送消息
    SendChannelMessage {
        channel: String,
        target: String,
        content: String,
    },
}
```

在 `src/domain/message.rs`（或 space.rs）新增组件：

```rust
/// 待发送的通道消息，由 channel_send_dispatch_system 消费。
#[derive(Debug, Clone, Component)]
pub struct PendingChannelSend {
    pub channel: String,
    pub recipient: String,
    pub content: String,
    /// 关联的工具请求，用于回写结果
    pub tool_call_id: Option<String>,
    pub task_id: TaskId,
    pub agent_id: AgentId,
    pub request_entity: Entity,
}
```

- [ ] **Step 2: 创建 src/channels/send_tool.rs**

```rust
use anyhow::Result;
use serde_json::{json, Value};

use crate::domain::{ToolAction, ToolError, ToolPermission, ToolSchema};

pub struct ChannelSendTool;

impl ChannelSendTool {
    pub fn definition() -> crate::domain::ToolDefinition {
        crate::domain::ToolDefinition {
            name: "channel_send".to_string(),
            description: "向指定 IM 通道（telegram/qq/feishu）发送消息".to_string(),
            parameters: ToolSchema {
                schema: json!({
                    "type": "object",
                    "properties": {
                        "channel": {
                            "type": "string",
                            "enum": ["telegram", "qq", "feishu"],
                            "description": "通道名称"
                        },
                        "target": {
                            "type": "string",
                            "description": "目标 chat_id / open_id / user_id"
                        },
                        "content": {
                            "type": "string",
                            "description": "要发送的内容"
                        }
                    },
                    "required": ["channel", "target", "content"]
                }),
            },
            default_permission: ToolPermission::Confirm,
            executor: crate::domain::ToolExecutorKind::Builtin("channel_send".to_string()),
            required_tag: None,
        }
    }
}

impl crate::domain::BuiltinTool for ChannelSendTool {
    fn name(&self) -> &str {
        "channel_send"
    }

    fn execute(
        &self,
        input: &Value,
        _ctx: &crate::domain::ToolContext,
    ) -> Result<ToolAction, ToolError> {
        let channel = input
            .get("channel")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing channel".into()))?;
        let target = input
            .get("target")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing target".into()))?;
        let content = input
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing content".into()))?;

        Ok(ToolAction::SendChannelMessage {
            channel: channel.to_string(),
            target: target.to_string(),
            content: content.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_send_tool_parses_input() {
        let tool = ChannelSendTool;
        let input = json!({
            "channel": "telegram",
            "target": "12345",
            "content": "hello"
        });
        // ToolContext 构造按现有测试模式，此处省略具体字段填充
        // 见 src/systems/tools/ 现有测试获取 ToolContext 构造方式
        let ctx = test_tool_context();
        let action = tool.execute(&input, &ctx).unwrap();
        match action {
            ToolAction::SendChannelMessage { channel, target, content } => {
                assert_eq!(channel, "telegram");
                assert_eq!(target, "12345");
                assert_eq!(content, "hello");
            }
            _ => panic!("unexpected action"),
        }
    }

    fn test_tool_context() -> crate::domain::ToolContext<'static> {
        // 实施时参考 src/systems/tools/ 现有测试的 ToolContext 构造方式填充
        unimplemented!("参考现有测试构造")
    }
}
```

> 实施时把 `test_tool_context` 替换为现有测试中已使用的真实构造方式。

- [ ] **Step 3: 在 handle_tool_action 新增 SendChannelMessage 分支**

在 `src/systems/tools/orchestrator.rs` 的 `handle_tool_action` 中新增：

```rust
ToolAction::SendChannelMessage { channel, target, content } => {
    commands.spawn(PendingChannelSend {
        channel,
        recipient: target,
        content,
        tool_call_id: tool_call_id.cloned(),
        task_id: request.task_id,
        agent_id: request.agent_id,
        request_entity,
    });
    // 不在此处生成 ToolExecutionResultMessage，由 channel_send_dispatch_system
    // 完成实际发送后回写结果。
}
```

> 注意 `handle_tool_action` 现有签名是否提供 `request_entity` 与 `tool_call_id`；
> 若不可得，把它们经由调用方传入或存在 PendingChannelSend 上后由 dispatch system 查询。
> 实施时按现有 `handle_tool_action` 签名对齐。

- [ ] **Step 4: 创建 src/systems/tools/channel_send_dispatch.rs**

```rust
use bevy::prelude::*;

use crate::channels::{ChannelManager, ChannelOutboundMessage};
use crate::domain::{
    AgentExecutionOutput, AgentExecutionResult, OutputContent, PendingChannelSend,
    ToolExecutionResultMessage, ToolReturnedHookPending,
};

/// 消费 PendingChannelSend，调用 ChannelManager 发送并回写工具结果。
pub fn channel_send_dispatch_system(
    mut commands: Commands,
    channel_manager: Res<ChannelManager>,
    pending: Query<(Entity, &PendingChannelSend)>,
) {
    for (entity, send) in &pending {
        let result = channel_manager.send(
            send.channel.clone(),
            ChannelOutboundMessage {
                recipient: send.recipient.clone(),
                thread_id: None,
                content: send.content.clone(),
            },
        );

        let (output_text, tool_output) = match result {
            Ok(()) => (
                format!("channel_send queued: {}", send.channel),
                serde_json::json!({ "status": "queued", "channel": send.channel }),
            ),
            Err(e) => (
                format!("channel_send failed: {e}"),
                serde_json::json!({ "status": "error", "error": e.to_string() }),
            ),
        };

        commands.entity(entity).despawn();

        commands.spawn((
            ToolExecutionResultMessage {
                result: AgentExecutionResult {
                    task_id: send.task_id,
                    agent_id: send.agent_id,
                    request_kind: Default::default(),
                    result: Ok(AgentExecutionOutput {
                        content: OutputContent::Text(output_text),
                        reasoning_content: None,
                    }),
                    prompt: String::new(),
                    system_prompt: None,
                    tools: vec![],
                    reasoning_content: None,
                    work_item_id: None,
                },
                tool_name: "channel_send".to_string(),
                tool_output: Ok(tool_output),
                tool_call_id: send.tool_call_id.clone(),
                original_tool_output: None,
                processed: false,
            },
            ToolReturnedHookPending,
        ));
    }
}
```

- [ ] **Step 5: 注册工具与系统**

在 `src/systems/tools/mod.rs` 的 `register_builtin_tools` 末尾新增：

```rust
use crate::channels::send_tool::ChannelSendTool;
registry.register(ChannelSendTool::definition());
executors.register(Box::new(ChannelSendTool));
```

在 `src/systems/tools/mod.rs` 导出：

```rust
pub mod channel_send_dispatch;
pub use channel_send_dispatch::channel_send_dispatch_system;
```

在 `src/channels/mod.rs` 导出：

```rust
pub mod send_tool;
```

- [ ] **Step 6: Run tests**

Run: `cargo test -p harness channels::send_tool`
Run: `cargo test -p harness tools`
Expected: passes.

- [ ] **Step 7: Commit**

```bash
git add src/channels/send_tool.rs src/systems/tools/ src/domain/space.rs src/domain/message.rs
git commit -m "feat(channels): add channel_send tool with companion dispatch system"
```

---

## Task 10: 链路测试与单元测试补全

**Files:**
- Modify: `src/systems/` 相关测试文件
- Test: `cargo test`

- [ ] **Step 1: origin_channel 端到端链路测试**

在 `src/systems/` 下新增或扩展 Bevy app 测试，覆盖：

1. **TUI 路径回归**：`UserAction::Text { channel: ChannelId { Tui, "default" }, content }` → 经 `frontend_input_system` → `signal_ingest_system` → `user_message_to_task_system` → 断言 `Task.origin_channel.frontend == Tui`。
2. **IM 路径**：`ExternalInput::TextWithChannel { channel: ChannelId { Telegram, "u1" }, content }` → 经 `input_ingress_system` → `signal_ingest_system` → `routing` → `task_creation` → 断言 `Task.origin_channel.frontend == Telegram` 且 `user_id == "u1"`。
3. **白名单拒绝**：`TelegramChannel::is_allowed` 在空白名单下返回 false（已在 Task 7 覆盖，确认在测试套件中可见）。

- [ ] **Step 2: 运行完整测试**

Run: `cargo test -p harness`
Expected: all passes.

- [ ] **Step 3: Commit**

```bash
git add src/systems/
git commit -m "test(systems): add origin_channel end-to-end coverage with TUI regression"
```

---

## Task 11: Telegram 集成测试（wiremock）

**Files:**
- Create: `tests/channels_telegram.rs`
- Modify: `Cargo.toml` dev-dependencies 添加 `wiremock`

- [ ] **Step 1: 添加 wiremock 依赖**

```toml
[dev-dependencies]
wiremock = "0.6"
```

- [ ] **Step 2: 创建 tests/channels_telegram.rs**

```rust
use crossbeam_channel::unbounded;
use harness::channels::{Channel, ChannelInboundMessage, ChannelOutboundMessage, TelegramChannel};
use harness::channels::config::TelegramConfig;
use std::time::Duration;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn telegram_send_message() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/botTOKEN/sendMessage"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"ok": true, "result": {"message_id": 1}})),
        )
        .mount(&mock_server)
        .await;

    let cfg = TelegramConfig {
        bot_token: "TOKEN".to_string(),
        allowed_users: vec!["u".to_string()],
    };
    let channel = TelegramChannel::new(cfg).with_base_url(mock_server.uri());

    channel
        .send(&ChannelOutboundMessage {
            recipient: "123".to_string(),
            thread_id: None,
            content: "hello".to_string(),
        })
        .await
        .expect("send");
}

#[tokio::test]
async fn telegram_listen_receives_update() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/botTOKEN/getUpdates"))
        .and(query_param("offset", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "result": [{
                "update_id": 42,
                "message": {
                    "from": {"id": 123, "username": "alice"},
                    "chat": {"id": 456, "type": "private"},
                    "date": 0,
                    "text": "hi"
                }
            }]
        })))
        .mount(&mock_server)
        .await;

    let cfg = TelegramConfig {
        bot_token: "TOKEN".to_string(),
        allowed_users: vec!["alice".to_string()],
    };
    let channel = TelegramChannel::new(cfg).with_base_url(mock_server.uri());

    let (tx, rx) = unbounded::<ChannelInboundMessage>();
    let listen_handle = tokio::spawn(async move {
        let _ = channel.listen(tx).await;
    });

    let msg = rx
        .recv_timeout(Duration::from_secs(2))
        .expect("receive");
    assert_eq!(msg.sender_id, "123");
    assert_eq!(msg.content, "hi");

    listen_handle.abort();
}
```

- [ ] **Step 3: Run integration tests**

Run: `cargo test --test channels_telegram`
Expected: passes.

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml tests/channels_telegram.rs
git commit -m "test(channels): add Telegram wiremock integration tests"
```

---

## Task 12: 文档同步

**Files:**
- Modify: `docs/current-state.md`
- Modify: `docs/configuration.md`
- Modify: `.env.example`
- Modify: `docs/design/README.md`
- Modify: `docs/README.md`

- [ ] **Step 1: 更新 docs/current-state.md**

在“已实现”下新增：

```markdown
- 统一 `Channel` 抽象与 `ChannelManager`（含 listen 重启退避与 shutdown）
- Telegram 通道接入（长轮询、白名单、文本分块发送）
- `channel_send` 工具主动推送
- `origin_channel` 从入向消息透传到 `Task`
```

在“待继续完善”下新增：

```markdown
- 出向-自动：Agent 回复按 `origin_channel` 自动回执
- QQ 与飞书通道具体实现
- 通道媒体附件发送与下载
- Telegram 媒体标记、stream_mode、mention_only
```

> 注意：**不写“自动路由”**，该能力本期未交付。

- [ ] **Step 2: 更新 docs/configuration.md**

新增一节“IM 通道配置”，说明加载链路与示例：

```markdown
## IM 通道配置

通道配置通过 toml 文件加载，路径由环境变量 `HARNESS_CHANNELS_CONFIG` 指定。
未设置时不启动任何通道。

### Telegram

```toml
[telegram]
bot_token = "your_bot_token"
allowed_users = ["your_telegram_username"]
```

`allowed_users` 留空表示拒绝所有用户。同时支持 username 与数字 user_id。

### QQ（待实现）

### 飞书（待实现）
```

- [ ] **Step 3: 更新 .env.example**

新增：

```bash
# IM 通道配置文件路径（toml，可选）
# HARNESS_CHANNELS_CONFIG=channels.toml
# Telegram Bot Token（写在 channels.toml 中，此处仅作提示）
# TELEGRAM_BOT_TOKEN=your_bot_token_here
```

- [ ] **Step 4: 更新 docs/design/README.md 与 docs/README.md 索引**

在 `docs/design/README.md` 设计文档列表中新增：

```markdown
- [IM 通道适配设计](im-channel-adapters.md) — Telegram/QQ/飞书通道抽象与 ECS 集成
```

在 `docs/README.md` 对应章节同步索引。

- [ ] **Step 5: Commit**

```bash
git add docs/ .env.example
git commit -m "docs: update current-state, configuration and env example for IM channels"
```

---

## Task 13: CI 与最终检查

**Files:** 所有新增/修改文件

- [ ] **Step 1: 运行格式化**

Run: `cargo fmt --all --check`
Expected: passes.

- [ ] **Step 2: 运行 clippy**

Run: `cargo clippy --all-targets --all-features -- -D warnings`
Expected: passes.

- [ ] **Step 3: 运行完整测试**

Run: `cargo test --all-features`
Expected: passes.

- [ ] **Step 4: markdownlint**

Run: `markdownlint docs/design/im-channel-adapters.md docs/configuration.md docs/current-state.md`
Expected: passes.

- [ ] **Step 5: 创建 PR**

```bash
git push origin feature/im-channel-adapters
```

在 GitHub 创建 PR，标题：`feat: add IM channel abstraction and Telegram adapter`。

---

## 后续阶段（不在本次计划内）

- **阶段 2：出向-自动**：`UserOutputMessage` 携带 `task_id`，`Channel` 实现 `Frontend` 并真正发送；`frontend_output_system` 按 `origin_channel` 生成 Directed 事件。
- **阶段 3：QQ 通道**：OAuth2、WebSocket Gateway、markdown/富媒体发送；引入 `tokio-tungstenite` / `prost` / `bytes`。
- **阶段 4：飞书/Lark 通道**：tenant token、WebSocket/Webhook、interactive card。
- **阶段 5：媒体附件**：统一 `[IMAGE:path]` 等标记，三平台下载/上传；reqwest 启用 `multipart` feature。
- **阶段 6：Telegram 增强**：媒体标记、`stream_mode` 草稿更新、`mention_only` 群组检测。
