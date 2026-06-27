# IM 出向-自动回执设计

> **状态：当前有效**

## 目标

当 Agent 对某个 Task 产出用户可见文本回复时，Harness 自动将该回复推送到该 Task 的 `origin_channel` 所来源的 IM 平台。本期以 Telegram 为首个实现，QQ/飞书保留占位结构。

## 背景

前一阶段（`docs/design/im-channel-adapters.md`）已实现：

- 统一 `Channel` 抽象与 `ChannelManager`
- Telegram 入向长轮询 + 白名单
- `channel_send` 工具主动推送
- `origin_channel` 从入向消息透传到 `Task`

本期补齐「出向-自动」能力，使 Agent 回复无需显式调用 `channel_send` 即可回到用户来源平台。

## 设计原则

- 复用现有 `EngineEvent::Text` + `EventTarget::Directed` 出向事件总线。
- 不破坏 TUI 主链路：TUI 只接收 `FrontendKind::Tui` 的定向事件或广播事件。
- `Channel` 通过实现 `Frontend` trait 接入前端注册表，与 TUI 并列。
- 不引入新依赖。

## 总体架构

```text
AgentExecutionResult (Text)
  ↓
llm_response_system 产出 UserOutputMessage { task_id, content }
  ↓
frontend_output_system 查询 Task::origin_channel
  ↓
生成 EngineEvent::Text { target: Directed([origin_channel]), role: Agent, content }
  ↓
FrontendRegistry 遍历所有 Frontend
  ↓
TuiFrontend         处理 Tui target，本地显示
ChannelFrontend     处理 Telegram/QQ/Feishu target，入队 ChannelManager 出向发送
```

## 详细设计

### 1. UserOutputMessage 携带 task_id

`src/domain/message.rs`：

```rust
#[derive(Debug, Clone, Component)]
pub struct UserOutputMessage {
    pub task_id: TaskId,
    pub content: String,
}
```

所有构造点（`src/systems/transform/llm_response.rs`）补齐 `task_id`：

- 多轮回复分支
- 单轮完成分支
- 任务失败分支

### 2. frontend_output_system 定向路由

`src/systems/frontend_output.rs` 中 `UserOutputMessage` 分支改为：

```rust
for (entity, output) in &outputs {
    let target = all_tasks
        .iter()
        .find(|t| t.id == output.task_id)
        .map(|t| EventTarget::Directed(vec![t.origin_channel.clone()]))
        .unwrap_or(EventTarget::Broadcast);

    let event = EngineEvent::Text {
        target,
        role: MessageRole::Agent,
        content: output.content.clone(),
    };
    for frontend in &registry.frontends {
        frontend.push_event(event.clone());
    }
    commands.entity(entity).despawn();
}
```

### 3. ChannelFrontend 实现

新增 `src/channels/frontend.rs`：

```rust
use bevy::prelude::*;

use crate::channels::{ChannelManager, ChannelOutboundMessage};
use crate::domain::{ChannelId, EngineEvent, EventTarget, Frontend, FrontendKind, UserAction};

pub struct ChannelFrontend {
    kind: FrontendKind,
    channel_name: String,
    outbound_tx: tokio::sync::mpsc::UnboundedSender<(String, ChannelOutboundMessage)>,
}

impl ChannelFrontend {
    pub fn new(
        kind: FrontendKind,
        channel_name: impl Into<String>,
        outbound_tx: tokio::sync::mpsc::UnboundedSender<(String, ChannelOutboundMessage)>,
    ) -> Self {
        Self {
            kind,
            channel_name: channel_name.into(),
            outbound_tx,
        }
    }

    fn matches(&self, channel_id: &ChannelId) -> bool {
        channel_id.frontend == self.kind
    }
}

impl Frontend for ChannelFrontend {
    fn kind(&self) -> FrontendKind {
        self.kind
    }

    fn push_event(&self, event: EngineEvent) {
        let EngineEvent::Text { target, content, .. } = event else { return };
        let targets = match target {
            EventTarget::Broadcast => return,
            EventTarget::Directed(v) => v,
        };
        let recipients: Vec<String> = targets
            .iter()
            .filter(|cid| self.matches(cid))
            .map(|cid| cid.user_id.clone())
            .collect();
        if recipients.is_empty() {
            return;
        }
        for recipient in recipients {
            let msg = ChannelOutboundMessage {
                recipient,
                thread_id: None,
                content: content.clone(),
            };
            if let Err(e) = self.outbound_tx.send((self.channel_name.clone(), msg)) {
                tracing::error!(event = "ChannelFrontendSendFailed", error = %e, "failed to queue outbound message");
            }
        }
    }

    fn poll_actions(&self) -> Vec<UserAction> {
        vec![]
    }
}
```

### 4. ChannelManager 生产 frontends

`src/channels/manager.rs` 中 `ChannelManager::new` 返回三元组：

```rust
pub fn new(
    channels: Vec<Arc<dyn Channel>>,
    external_input_tx: Sender<ExternalInput>,
) -> (Self, tokio::task::JoinHandle<()>, Vec<Box<dyn Frontend>>) {
    // ... 现有初始化 ...
    let frontends: Vec<Box<dyn Frontend>> = channels
        .iter()
        .map(|ch| {
            let kind = frontend_kind_for_name(ch.name());
            Box::new(ChannelFrontend::new(
                kind,
                ch.name().to_string(),
                outbound_tx.clone(),
            )) as Box<dyn Frontend>
        })
        .collect();
    // ...
    (Self { ... }, handle, frontends)
}

pub fn empty() -> (Self, Vec<Box<dyn Frontend>>) {
    // ...
}
```

辅助函数：

```rust
fn frontend_kind_for_name(name: &str) -> FrontendKind {
    match name {
        "telegram" => FrontendKind::Telegram,
        "qq" => FrontendKind::QQ,
        "feishu" => FrontendKind::Feishu,
        _ => FrontendKind::Tui,
    }
}
```

### 5. 启动集成

`src/main.rs`：

```rust
let (channel_manager, channel_handle, channel_frontends) =
    ChannelManager::new(channel_list, input_tx);

let mut frontends: Vec<Box<dyn Frontend>> = vec![Box::new(tui_frontend)];
frontends.extend(channel_frontends);

let mut app = build_harness_app(
    config,
    runtime,
    executor,
    input_rx,
    frontends,
    channel_manager,
);
```

### 6. 出向字段映射

| EngineEvent::Text 字段 | ChannelOutboundMessage 字段 |
|---|---|
| `target` 中匹配 `ChannelId.user_id` | `recipient` |
| （本期无 thread 扩展） | `thread_id: None` |
| `content` | `content` |

## 与现有代码的关系

- `SystemOutputMessage` 已使用 `Task::origin_channel` 生成 `Directed`，本期 `UserOutputMessage` 复用同一模式。
- `FrontendRegistry` 已支持多个 `Frontend`，`TuiFrontend` 与 `ChannelFrontend` 并列注册即可。
- `ChannelManager` 的后台出向发送循环无需改动，复用既有 `outbound_tx`。

## 错误处理

- `frontend_output_system` 中若找不到对应 Task，回退到 `Broadcast`，保持当前行为不丢消息。
- `ChannelFrontend::push_event` 入队失败只记录 `tracing::error`，不阻塞 ECS 主循环。
- 实际网络发送失败由 `ChannelManager` 后台循环记录并丢弃，与 `channel_send` 一致。

## 测试

### 单元测试

`src/channels/frontend.rs` 内 `#[cfg(test)]`：

- `ChannelFrontend` 忽略 `Broadcast` 事件。
- `ChannelFrontend` 只处理匹配本通道 kind 的 `Directed` target。
- 不匹配 kind 的 target 不产生出向消息。

### 集成测试

新增 `tests/auto_channel_reply.rs`：

- 使用 `wiremock` 模拟 Telegram `sendMessage`。
- 构建 `build_harness_app`，`origin_channel` 为 Telegram。
- 注入 `ExternalInput::TextWithChannel` 触发 Task。
- 使用 `EchoExecutor` 模拟 Agent 文本回复。
- 断言 mock 收到一次 `POST /botTOKEN/sendMessage`。

### 回归测试

- `tests/origin_channel_flow.rs` 中 TUI 路径保持不触发 IM 发送。

## 文档同步

- `docs/design/im-channel-adapters.md`：将「出向-自动」从后续阶段移到已实现，并指向本设计文档。
- `docs/current-state.md`：将「IM 出向-自动」从待完善移到已实现。
- `docs/superpowers/README.md` 与 `docs/design/README.md`：索引本设计文档。

## 后续阶段

- **QQ 通道**：OAuth2 + WebSocket Gateway，复用 `ChannelFrontend` 自动回执。
- **飞书/Lark 通道**：tenant token + WebSocket/Webhook，复用 `ChannelFrontend` 自动回执。
- **媒体附件**：`EngineEvent::Text` 中解析 `[IMAGE:path]` 等标记，扩展 `ChannelOutboundMessage`。
