> **状态：已归档** — 对应功能已合并到 main，归档于 2026-07-05

# IM 出向-自动回执实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 当 Agent 对 Task 产出用户可见文本回复时，自动按 `Task::origin_channel` 推回来源 IM 平台（本期以 Telegram 为主）。

**Architecture:** 复用现有 `EngineEvent::Text` + `EventTarget::Directed` 出向总线；新增 `ChannelFrontend` 实现 `Frontend` trait，把定向事件转换为 `ChannelOutboundMessage` 并交给 `ChannelManager` 后台发送。

**Tech Stack:** Rust, Bevy ECS, tokio, crossbeam-channel, tracing

## Global Constraints

- 不引入新第三方依赖。
- 只修改出向链路，不破坏 TUI 显示。
- 所有变更必须通过 `cargo fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`cargo test --all-features`。
- 遵循 `feat/im-channel-adapters` 分支，禁止直接推送到 `main`。
- 文档变更与设计文档 `docs/superpowers/specs/2026-06-27-auto-channel-reply-design.md` 同步。

---

## File Structure

| 文件 | 责任 |
|---|---|
| `src/domain/message.rs` | `UserOutputMessage` 新增 `task_id: TaskId` 字段 |
| `src/systems/transform/llm_response.rs` | 三处 `UserOutputMessage` 构造补齐 `task_id` |
| `src/systems/frontend_output.rs` | `UserOutputMessage` 分支改为按 `Task::origin_channel` 定向路由 |
| `src/channels/frontend.rs` | 新增 `ChannelFrontend`，实现 `Frontend` trait |
| `src/channels/manager.rs` | `ChannelManager::new` / `empty` 返回 frontends；辅助 `frontend_kind_for_name` |
| `src/channels/mod.rs` | 导出 `ChannelFrontend` |
| `src/main.rs` | 把 `channel_frontends` 与 `TuiFrontend` 一起注册到 `FrontendRegistry` |
| `tests/auto_channel_reply.rs` | 集成测试：Telegram 来源 Task 的 Agent 回复自动触发 mock 发送 |
| `docs/design/im-channel-adapters.md` | 更新出向-自动状态 |
| `docs/current-state.md` | 同步能力状态 |

---

## Task 1: UserOutputMessage 携带 task_id

**Files:**
- Modify: `src/domain/message.rs`
- Modify: `src/systems/transform/llm_response.rs`

**Interfaces:**
- Consumes: `TaskId`（已存在）
- Produces: `UserOutputMessage { task_id: TaskId, content: String }`

- [ ] **Step 1: 修改结构体**

在 `src/domain/message.rs` 中：

```rust
#[derive(Debug, Clone, Component)]
pub struct UserOutputMessage {
    pub task_id: TaskId,
    pub content: String,
}
```

- [ ] **Step 2: 更新 llm_response.rs 三处构造点**

`src/systems/transform/llm_response.rs` 中三处 `commands.spawn(UserOutputMessage { ... })` 均需携带 `task_id`。

第 1 处（约 707 行）：

```rust
commands.spawn(UserOutputMessage {
    task_id: task.id,
    content: content.clone(),
});
```

第 2 处（约 728 行）：

```rust
commands.spawn(UserOutputMessage {
    task_id: task.id,
    content: content.clone(),
});
```

第 3 处（约 938 行）：

```rust
commands.spawn(UserOutputMessage {
    task_id: task.id,
    content: format!("任务执行失败（{:?}）：{}", ...),
});
```

- [ ] **Step 3: 编译检查**

Run:

```bash
cargo check --all-features
```

Expected: 若 `UserOutputMessage` 在其它位置仍有构造点，编译器会报错；逐处补齐。

- [ ] **Step 4: Commit**

```bash
git add src/domain/message.rs src/systems/transform/llm_response.rs
git commit -m "$(cat <<'EOF'
feat: attach task_id to UserOutputMessage

- Add task_id field to UserOutputMessage
- Populate task_id in all llm_response spawn sites
EOF
)"
```

---

## Task 2: frontend_output_system 定向路由 UserOutputMessage

**Files:**
- Modify: `src/systems/frontend_output.rs`

**Interfaces:**
- Consumes: `UserOutputMessage.task_id`, `Task::origin_channel`, `EventTarget::Directed`
- Produces: `EngineEvent::Text` 的 `target` 由 `Broadcast` 改为 `Directed([origin_channel])`

- [ ] **Step 1: 替换 UserOutputMessage 分支**

在 `src/systems/frontend_output.rs` 中，将用户可见文本输出分支：

```rust
    // 用户可见文本输出
    for (entity, output) in &outputs {
        debug!(
            event = "FrontendOutputText",
            task_id = %output.task_id,
            content_len = output.content.len(),
            "pushing text to frontends"
        );
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

- [ ] **Step 2: 运行现有测试**

Run:

```bash
cargo test --all-features
```

Expected: 全部通过（`origin_channel_flow` 中 TUI 路径已使用 `origin_channel == FrontendKind::Tui`，行为不变）。

- [ ] **Step 3: Commit**

```bash
git add src/systems/frontend_output.rs
git commit -m "$(cat <<'EOF'
feat: route UserOutputMessage by origin_channel

- Convert UserOutputMessage to Directed EngineEvent::Text
- Fallback to Broadcast if task not found
EOF
)"
```

---

## Task 3: 实现 ChannelFrontend

**Files:**
- Create: `src/channels/frontend.rs`
- Modify: `src/channels/mod.rs`

**Interfaces:**
- Consumes: `Frontend`, `EngineEvent::Text`, `EventTarget`, `ChannelId`, `ChannelOutboundMessage`, `tokio::sync::mpsc::UnboundedSender<(String, ChannelOutboundMessage)>`
- Produces: `pub struct ChannelFrontend` 实现 `Frontend`

- [ ] **Step 1: 创建 src/channels/frontend.rs**

```rust
use std::sync::mpsc::SendError;

use tokio::sync::mpsc::UnboundedSender;
use tracing::{error, trace};

use crate::domain::{ChannelId, EngineEvent, EventTarget, Frontend, FrontendKind, UserAction};

use super::ChannelOutboundMessage;

/// 将 EngineEvent 路由到对应 IM 通道出向发送队列的 Frontend 实现。
pub struct ChannelFrontend {
    kind: FrontendKind,
    channel_name: String,
    outbound_tx: UnboundedSender<(String, ChannelOutboundMessage)>,
}

impl ChannelFrontend {
    pub fn new(
        kind: FrontendKind,
        channel_name: impl Into<String>,
        outbound_tx: UnboundedSender<(String, ChannelOutboundMessage)>,
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
        let EngineEvent::Text { target, content, .. } = event else {
            return;
        };
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
        trace!(
            event = "ChannelFrontendReceive",
            channel = %self.channel_name,
            recipients = recipients.len(),
            content_len = content.len(),
            "routing text to channel outbound queue"
        );
        for recipient in recipients {
            let msg = ChannelOutboundMessage {
                recipient,
                thread_id: None,
                content: content.clone(),
            };
            if let Err(e) = self.outbound_tx.send((self.channel_name.clone(), msg)) {
                error!(event = "ChannelFrontendSendFailed", error = %e, "failed to queue outbound message");
            }
        }
    }

    fn poll_actions(&self) -> Vec<UserAction> {
        vec![]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    fn make_frontend(kind: FrontendKind) -> (ChannelFrontend, mpsc::UnboundedReceiver<(String, ChannelOutboundMessage)>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (ChannelFrontend::new(kind, "test", tx), rx)
    }

    fn text_event(target: EventTarget) -> EngineEvent {
        EngineEvent::Text {
            target,
            role: crate::domain::MessageRole::Agent,
            content: "hello".to_string(),
        }
    }

    #[test]
    fn ignores_broadcast() {
        let (fe, mut rx) = make_frontend(FrontendKind::Telegram);
        fe.push_event(text_event(EventTarget::Broadcast));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn ignores_non_matching_directed() {
        let (fe, mut rx) = make_frontend(FrontendKind::Telegram);
        fe.push_event(text_event(EventTarget::Directed(vec![ChannelId {
            frontend: FrontendKind::QQ,
            user_id: "u1".to_string(),
        }])));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn queues_matching_directed() {
        let (fe, mut rx) = make_frontend(FrontendKind::Telegram);
        fe.push_event(text_event(EventTarget::Directed(vec![ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "u1".to_string(),
        }])));
        let (name, msg) = rx.try_recv().expect("one outbound message");
        assert_eq!(name, "test");
        assert_eq!(msg.recipient, "u1");
        assert_eq!(msg.content, "hello");
        assert!(rx.try_recv().is_err());
    }
}
```

- [ ] **Step 2: 在 src/channels/mod.rs 导出**

在 `src/channels/mod.rs` 中，在 `pub use manager::ChannelManager;` 附近添加：

```rust
pub use frontend::ChannelFrontend;
```

- [ ] **Step 3: 运行单元测试**

Run:

```bash
cargo test --all-features channels::frontend
```

Expected: 3 个单元测试全部 PASS。

- [ ] **Step 4: Commit**

```bash
git add src/channels/frontend.rs src/channels/mod.rs
git commit -m "$(cat <<'EOF'
feat: add ChannelFrontend for outbound IM routing

- Implement Frontend trait for IM channels
- Filter Directed events by FrontendKind
- Unit tests for Broadcast/Non-matching/Matching targets
EOF
)"
```

---

## Task 4: ChannelManager 生产 frontends 并接入启动链路

**Files:**
- Modify: `src/channels/manager.rs`
- Modify: `src/main.rs`

**Interfaces:**
- Consumes: `ChannelFrontend`, `ChannelManager::outbound_tx`
- Produces: `ChannelManager::new(...) -> (Self, JoinHandle, Vec<Box<dyn Frontend>>)`；`ChannelManager::empty() -> (Self, Vec<Box<dyn Frontend>>)`

- [ ] **Step 1: 修改 ChannelManager::new 与 empty**

`src/channels/manager.rs` 顶部导入：

```rust
use crate::domain::{ExternalInput, Frontend, FrontendKind};
```

（保留原有导入，合并 ExternalInput 与 Frontend 的来源。）

添加辅助函数（文件末尾或模块级别）：

```rust
fn frontend_kind_for_name(name: &str) -> FrontendKind {
    match name {
        "telegram" => FrontendKind::Telegram,
        "qq" => FrontendKind::QQ,
        "feishu" => FrontendKind::Feishu,
        _ => panic!("unknown channel name: {name}"),
    }
}
```

修改 `ChannelManager::empty`：

```rust
pub fn empty() -> (Self, Vec<Box<dyn Frontend>>) {
    let (outbound_tx, _) = mpsc::unbounded_channel::<(String, ChannelOutboundMessage)>();
    let (shutdown_tx, _) = broadcast::channel::<()>(1);
    (
        Self {
            channels: vec![],
            outbound_tx,
            shutdown_tx,
        },
        vec![],
    )
}
```

修改 `ChannelManager::new` 返回三元组：

```rust
pub fn new(
    channels: Vec<Arc<dyn Channel>>,
    external_input_tx: Sender<ExternalInput>,
) -> (Self, tokio::task::JoinHandle<()>, Vec<Box<dyn Frontend>>) {
    let (outbound_tx, mut outbound_rx) =
        mpsc::unbounded_channel::<(String, ChannelOutboundMessage)>();
    let (shutdown_tx, _) = broadcast::channel::<()>(1);

    let frontends: Vec<Box<dyn Frontend>> = channels
        .iter()
        .map(|ch| {
            let kind = frontend_kind_for_name(ch.name());
            Box::new(super::ChannelFrontend::new(
                kind,
                ch.name().to_string(),
                outbound_tx.clone(),
            )) as Box<dyn Frontend>
        })
        .collect();

    // ... 保留原有 supervisor 与 send loop ...

    (
        Self {
            channels,
            outbound_tx,
            shutdown_tx,
        },
        handle,
        frontends,
    )
}
```

- [ ] **Step 2: 调整 manager 现有单元测试**

`src/channels/manager.rs` 单测中 `let (manager, _handle) = ChannelManager::new(...)` 改为 `let (manager, _handle, _frontends) = ChannelManager::new(...)`。

- [ ] **Step 3: 修改 src/main.rs 注册 frontends**

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

并在 `main.rs` 顶部导入 `Frontend`：

```rust
use harness::{
    EngineEvent, ExternalInput, Frontend, HarnessConfig, HarnessSettings, ShutdownState, UserAction,
    app_is_idle, build_harness_app,
    channels::{Channel, ChannelManager, TelegramChannel},
    create_executor_from_config,
};
```

- [ ] **Step 4: 编译与测试**

Run:

```bash
cargo test --all-features
```

Expected: 全部通过。

- [ ] **Step 5: Commit**

```bash
git add src/channels/manager.rs src/main.rs
git commit -m "$(cat <<'EOF'
feat: wire ChannelFrontend into main app

- ChannelManager::new returns channel frontends
- Register channel frontends alongside TuiFrontend
EOF
)"
```

---

## Task 5: 集成测试 auto_channel_reply

**Files:**
- Create: `tests/auto_channel_reply.rs`

**Interfaces:**
- Consumes: `build_harness_app`, `ChannelManager`, `ExternalInput::TextWithChannel`, `origin_channel`, mock Telegram HTTP API
- Produces: 断言 Telegram `sendMessage` 收到一次请求

- [ ] **Step 1: 创建 tests/auto_channel_reply.rs**

参考 `tests/channels_telegram.rs` 与 `tests/origin_channel_flow.rs`，构造一个完整集成测试：

```rust
use std::sync::Arc;

use crossbeam_channel::unbounded;
use harness::{
    AgentConfig, BrainConfig, ChannelId, EngineEvent, ExternalInput, FrontendKind,
    HarnessConfig, HarnessSettings, MemoryConfig, Task, build_harness_app,
    channels::{ChannelManager, TelegramChannel, TelegramChannelConfig},
    domain::{Agent, AgentState, TaskStatus},
    create_dummy_executor,
};
use tokio::runtime::Runtime;
use wiremock::{Mock, MockServer, ResponseTemplate};
use wiremock::matchers::{method, path, body_json};

/// 一个极简的 Executor，直接返回固定文本作为 Agent 回复。
struct EchoExecutor;

#[async_trait::async_trait]
impl harness::llm::Executor for EchoExecutor {
    async fn execute(&self, _req: harness::llm::ExecuteRequest) -> anyhow::Result<harness::llm::ExecuteResponse> {
        Ok(harness::llm::ExecuteResponse {
            text: "echo reply".to_string(),
            finish_reason: harness::llm::FinishReason::Stop,
        })
    }
}

#[test]
fn agent_reply_to_telegram_is_sent_back() {
    let rt = Arc::new(Runtime::new().unwrap());
    let rt_clone = rt.clone();

    rt.block_on(async {
        let mock_server = MockServer::start().await;
        let bot_token = "test-token";

        Mock::given(method("POST"))
            .and(path(format!("/bot{}/sendMessage", bot_token)))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": { "message_id": 42 }
            })))
            .expect(1)
            .mount(&mock_server)
            .await;

        let (input_tx, input_rx) = unbounded::<ExternalInput>();
        let cfg = TelegramChannelConfig {
            bot_token: bot_token.to_string(),
            api_base: Some(mock_server.uri()),
            allowed_usernames: vec!["alice".to_string()],
            poll_timeout_secs: 10,
        };
        let channel = Arc::new(TelegramChannel::new(cfg)) as Arc<dyn harness::channels::Channel>;
        let (channel_manager, _handle, channel_frontends) =
            ChannelManager::new(vec![channel], input_tx);

        let config = HarnessConfig {
            llm: harness::LlmConfig::default(),
            brain: BrainConfig::default(),
            memory: MemoryConfig::default(),
            agents: vec![AgentConfig {
                name: "agent".to_string(),
                model: "dummy".to_string(),
                ..Default::default()
            }],
            channels: Default::default(),
            hooks: Default::default(),
            tools: Default::default(),
        };

        let executor = Arc::new(EchoExecutor);
        let frontends: Vec<Box<dyn harness::domain::Frontend>> = channel_frontends;

        let mut app = build_harness_app(
            config,
            rt_clone,
            executor,
            input_rx,
            frontends,
            channel_manager,
        );

        // 注入一条来自 Telegram 的入向消息
        let origin = ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "alice".to_string(),
        };
        let tx = app.world().resource::<harness::app::AsyncRuntime>().clone();
        tx.spawn(async move {
            let _ = input_tx.send(ExternalInput::TextWithChannel {
                channel: origin.clone(),
                content: "hello bot".to_string(),
            });
        });

        // 驱动 ECS 若干帧
        for _ in 0..200 {
            app.update();
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        mock_server.verify().await;
    });
}
```

**注意：** 上述代码为示意骨架。实际需根据 `build_harness_app` 签名、`AsyncRuntime` 类型、`create_dummy_executor` 是否存在做调整。若 `create_dummy_executor` 不存在，使用 `EchoExecutor` 结构并自行实现 `harness::llm::Executor`。

- [ ] **Step 2: 运行集成测试**

Run:

```bash
cargo test --all-features auto_channel_reply
```

Expected: 测试 PASS，wiremock 报告 `sendMessage` 被调用 1 次。

- [ ] **Step 3: Commit**

```bash
git add tests/auto_channel_reply.rs
git commit -m "$(cat <<'EOF'
test: add auto channel reply integration test

- Verify Telegram-origin task triggers automatic outbound reply
- Use wiremock to assert single sendMessage call
EOF
)"
```

---

## Task 6: 文档同步

**Files:**
- Modify: `docs/design/im-channel-adapters.md`
- Modify: `docs/current-state.md`

**Interfaces:**
- Consumes: 设计文档 `docs/superpowers/specs/2026-06-27-auto-channel-reply-design.md`
- Produces: 文档状态一致性

- [ ] **Step 1: 更新 docs/design/im-channel-adapters.md**

将「出向-自动」相关段落从「后续阶段」移到「已实现」。添加指向 `docs/superpowers/specs/2026-06-27-auto-channel-reply-design.md` 的链接。

- [ ] **Step 2: 更新 docs/current-state.md**

在「已实现」部分添加：

```markdown
- IM 出向-自动回执：Agent 文本回复按 `origin_channel` 自动推回 Telegram
```

从「待完善」中移除对应条目（若存在）。

- [ ] **Step 3: 更新 docs/superpowers/README.md 与 docs/design/README.md**

确保索引中列出 `2026-06-27-auto-channel-reply-design.md`。

- [ ] **Step 4: 运行 markdownlint**

Run:

```bash
markdownlint docs/
```

Expected: 无新增错误。

- [ ] **Step 5: Commit**

```bash
git add docs/
git commit -m "$(cat <<'EOF'
docs: sync auto channel reply state

- Move outbound-auto to implemented in design doc
- Update current-state.md and index files
EOF
)"
```

---

## Task 7: 全量 CI 检查

**Files:** 全部改动

- [ ] **Step 1: 格式化检查**

```bash
cargo fmt --all --check
```

Expected: 无输出表示通过。

- [ ] **Step 2: Clippy**

```bash
cargo clippy --all-targets --all-features -- -D warnings
```

Expected: 无 warning/error。

- [ ] **Step 3: 全量测试**

```bash
cargo test --all-features
```

Expected: 全部 PASS。

- [ ] **Step 4: Commit（可选，仅当修正了 CI 问题）**

---

## Self-Review

### Spec coverage

- `UserOutputMessage` 携带 `task_id` → Task 1
- `frontend_output_system` 定向路由 → Task 2
- `ChannelFrontend` 实现 `Frontend` → Task 3
- `ChannelManager` 生产 frontends + `main.rs` 集成 → Task 4
- 集成测试 → Task 5
- 文档同步 → Task 6
- 无新依赖、不破坏 TUI → Global Constraints + Task 2/4 设计

### Placeholder scan

- 无 "TBD" / "TODO" / "implement later"。
- Task 5 的测试代码为骨架示例，实际实现需根据 `build_harness_app` / `Executor` 接口调整，不视为占位符；计划已注明需对齐现有签名。

### Type consistency

- `UserOutputMessage { task_id, content }` 贯穿 Task 1、2。
- `ChannelManager::new` / `empty` 返回值在 Task 4 统一为三元组 / 二元组。
- `Frontend` trait 签名与 `src/domain/frontend.rs` 一致。

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-06-27-auto-channel-reply.md`. Two execution options:

**1. Subagent-Driven (recommended)** - I dispatch a fresh subagent per task, review between tasks, fast iteration.

**2. Inline Execution** - Execute tasks in this session using executing-plans, batch execution with checkpoints.

Which approach?
