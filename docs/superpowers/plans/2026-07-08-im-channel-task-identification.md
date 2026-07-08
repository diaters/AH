# IM 通道任务标识实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让 IM 通道中的 Agent 回复、系统通知、任务失败提示和任务状态变更都携带任务短 ID 前缀，解决多任务并行时消息归属不清的问题。

**Architecture:** 在 `EngineEvent::Text` 中增加 `task_id`，由 `frontend_output_system` 透传；`ChannelFrontend::push_event` 负责把任务 UUID 前 8 位短码与角色/状态标签拼入消息正文；任务状态变更通过本地状态映射获取旧状态并渲染为 `状态: 旧 → 新`。

**Tech Stack:** Rust, Bevy ECS, tokio, ratatui, tracing

## Global Constraints

- 语言：Rust，遵循官方风格指南
- 架构：Bevy ECS
- 前端：`ratatui` + `crossterm`
- 文档：Markdown，遵循 `markdownlint`
- 提交前通过 `cargo fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`cargo test --all-features`
- 同一变更涉及的代码与文档应尽量放在同一提交中
- 遵循 Conventional Commits
- 通过分支和 PR 合并代码，禁止直接推送到 `main`

---

## Task 1: 扩展 EngineEvent::Text 携带 task_id

**Files:**
- Modify: `src/domain/frontend.rs:94-99`

**Interfaces:**
- Consumes: N/A
- Produces: `EngineEvent::Text { target, role, content, task_id }`

- [ ] **Step 1: 修改 EngineEvent::Text 变体**

```rust
pub enum EngineEvent {
    /// 用户可见文本（Agent 回复、系统消息）
    Text {
        target: EventTarget,
        role: MessageRole,
        content: String,
        task_id: Option<TaskId>,
    },
    // ...
}
```

- [ ] **Step 2: 确认 target() 方法仍兼容**

`src/domain/frontend.rs:143-152` 的现有模式 `Self::Text { target, .. } => target` 已兼容新增字段，无需改动。编译检查确认即可。

- [ ] **Step 3: 更新测试代码中的 Text 事件构造**

修改 `src/channels/frontend.rs:170-176`：

```rust
fn text_event(target: EventTarget) -> EngineEvent {
    EngineEvent::Text {
        target,
        role: crate::domain::MessageRole::Agent,
        content: "hello".to_string(),
        task_id: None,
    }
}
```

修改 `src/tui/app.rs:550-554`：

```rust
app.handle_engine_event(EngineEvent::Text {
    target: EventTarget::Broadcast,
    role: MessageRole::Agent,
    content: "hello world".to_string(),
    task_id: None,
});
```

- [ ] **Step 4: 编译检查**

Run: `cargo check --all-features`
Expected: PASS（无新增字段导致的编译错误）

- [ ] **Step 5: Commit**

```bash
git add src/domain/frontend.rs src/channels/frontend.rs src/tui/app.rs
git commit -m "feat(domain): add task_id to EngineEvent::Text"
```

## Task 2: 更新 frontend_output_system 透传 task_id 并记录旧状态

**Files:**
- Modify: `src/domain/frontend.rs:124-131`（TaskStatusChanged 增加 old_status）
- Modify: `src/systems/frontend_output.rs:1-253`
- Test: `src/systems/frontend_output.rs:255-612`

**Interfaces:**
- Consumes: `EngineEvent::Text { task_id, .. }`（来自 Task 1）
- Produces: Text 事件携带 `task_id: Some(output.task_id)`；TaskStatusChanged 事件携带 `old_status: Option<TaskStatusKind>`

- [ ] **Step 1: 更新 UserOutputMessage 构造的 Text 事件**

修改 `src/systems/frontend_output.rs:48-52`：

```rust
let event = EngineEvent::Text {
    target,
    role: MessageRole::Agent,
    content: output.content.clone(),
    task_id: Some(output.task_id),
};
```

- [ ] **Step 2: 更新 SystemOutputMessage 构造的 Text 事件**

修改 `src/systems/frontend_output.rs:83-87`：

```rust
let event = EngineEvent::Text {
    target,
    role: MessageRole::System,
    content: output.content.clone(),
    task_id: Some(output.task_id),
};
```

- [ ] **Step 3: 修改 TaskStatusChanged 事件以携带旧状态**

修改 `src/domain/frontend.rs:124-131`：

```rust
TaskStatusChanged {
    target: EventTarget,
    task_id: TaskId,
    name: String,
    status: TaskStatusKind,
    old_status: Option<TaskStatusKind>,
    result: Option<String>,
    parent_id: Option<TaskId>,
},
```

- [ ] **Step 4: 在 frontend_output_system 中跟踪旧状态**

在 `src/systems/frontend_output.rs` 顶部增加：

```rust
use std::collections::HashMap;
```

在函数签名中增加 `Local` 参数：

```rust
pub(crate) fn frontend_output_system(
    registry: Res<FrontendRegistry>,
    mut commands: Commands,
    outputs: Query<(Entity, &UserOutputMessage)>,
    system_outputs: Query<(Entity, &SystemOutputMessage)>,
    all_tasks: Query<(Entity, &Task)>,
    tasks: Query<&Task, Changed<Task>>,
    agents: Query<&Agent, Changed<Agent>>,
    confirmations: Query<
        (Entity, &ToolConfirmationRequestMessage),
        Added<ToolConfirmationRequestMessage>,
    >,
    mut last_status: Local<HashMap<TaskId, TaskStatusKind>>,
) {
```

在 Task 状态变化段落（约 `src/systems/frontend_output.rs:109-126`）替换为：

```rust
let status = task_status_to_kind(&task.status);
let old_status = last_status.get(&task.id).copied();
let result = if task.status.is_terminal() {
    Some(task.result_summary.clone())
} else {
    None
};
let event = EngineEvent::TaskStatusChanged {
    target,
    task_id: task.id,
    name: task.input_summary.clone(),
    status,
    old_status,
    result,
    parent_id: task.parent_task_id,
};
last_status.insert(task.id, status);
```

- [ ] **Step 5: 更新 TUI 测试中的 TaskStatusChanged 构造**

在 `src/tui/app.rs` 测试模块中，所有 `EngineEvent::TaskStatusChanged { ... }` 构造添加 `old_status: None`。

涉及位置：约 `src/tui/app.rs:601, 624, 634, 644, 859, 869, 879, 900, 911`。

示例（其中一处）：

```rust
app.handle_engine_event(EngineEvent::TaskStatusChanged {
    target: EventTarget::Broadcast,
    task_id,
    name: "task".to_string(),
    status: TaskStatusKind::Running,
    old_status: None,
    result: None,
    parent_id: None,
});
```

- [ ] **Step 6: 运行测试**

Run: `cargo test --all-features frontend_output`
Expected: PASS

- [ ] **Step 7: Commit**

```bash
git add src/domain/frontend.rs src/systems/frontend_output.rs src/tui/app.rs
git commit -m "feat(frontend-output): propagate task_id and old_status"
```

## Task 3: 在 ChannelFrontend 渲染任务前缀与状态消息

**Files:**
- Modify: `src/channels/frontend.rs:1-153`
- Test: `src/channels/frontend.rs:155-260`

**Interfaces:**
- Consumes: `EngineEvent::Text { task_id, role, content, .. }` 和 `EngineEvent::TaskStatusChanged { task_id, status, old_status, .. }`
- Produces: `ChannelOutboundMessage` with prefixed content

- [ ] **Step 1: 添加辅助函数与导入**

修改 `src/channels/frontend.rs:4`：

```rust
use crate::domain::{ChannelId, EngineEvent, EventTarget, Frontend, FrontendKind, MessageRole, TaskId, TaskStatusKind, UserAction};
```

在 `fn html_escape` 之后（约 line 53）增加：

```rust
fn task_short_id(task_id: TaskId) -> String {
    task_id.to_string().split('-').next().unwrap_or("????").to_string()
}

fn role_label(role: MessageRole) -> &'static str {
    match role {
        MessageRole::Agent => "助手",
        MessageRole::System => "系统",
        MessageRole::User => "用户",
    }
}

fn status_label(status: TaskStatusKind) -> &'static str {
    match status {
        TaskStatusKind::Pending => "待处理",
        TaskStatusKind::Running => "运行中",
        TaskStatusKind::Waiting => "等待中",
        TaskStatusKind::Done => "已完成",
        TaskStatusKind::Failed => "已失败",
    }
}
```

- [ ] **Step 2: 修改 Text 事件处理**

替换 `src/channels/frontend.rs:62-93` 的 Text match arm：

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
    for channel_id in recipients {
        let msg = ChannelOutboundMessage {
            recipient: channel_id.user_id,
            thread_id: channel_id.thread_id,
            content: prefixed_content.clone(),
            parse_mode: None,
            reply_markup: None,
            attachments: vec![],
        };
        self.send_message(msg);
    }
}
```

- [ ] **Step 3: 添加 TaskStatusChanged 处理**

在 `EngineEvent::ApprovalRequest { .. }` match arm 之后、` _ => {}` 之前，增加：

```rust
EngineEvent::TaskStatusChanged {
    target,
    task_id,
    status,
    old_status,
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
    let status_text = match old_status {
        Some(old) => format!(
            "[{}] 状态: {} → {}",
            task_short_id(task_id),
            status_label(old),
            status_label(status)
        ),
        None => format!("[{}] 状态: {}", task_short_id(task_id), status_label(status)),
    };
    for channel_id in recipients {
        let msg = ChannelOutboundMessage {
            recipient: channel_id.user_id,
            thread_id: channel_id.thread_id,
            content: status_text.clone(),
            parse_mode: None,
            reply_markup: None,
            attachments: vec![],
        };
        self.send_message(msg);
    }
}
```

- [ ] **Step 4: 运行 ChannelFrontend 现有测试**

Run: `cargo test --all-features channels::frontend`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/channels/frontend.rs
git commit -m "feat(channels): render task prefix and status changes in IM"
```

## Task 4: 更新 TUI 事件处理以忽略 task_id

**Files:**
- Read-only verify: `src/tui/app.rs:384-396`

**Interfaces:**
- Consumes: `EngineEvent::Text { role, content, task_id, .. }`
- Produces: N/A（TUI 忽略 task_id）

- [ ] **Step 1: 确认 TUI match 模式兼容**

`src/tui/app.rs:386` 当前代码：

```rust
EngineEvent::Text { role, content, .. } => {
```

已使用 `..` 忽略额外字段，无需改动。

- [ ] **Step 2: 编译检查**

Run: `cargo check --all-features`
Expected: PASS

- [ ] **Step 3: 无需 commit**

TUI 代码无需改动，本任务不产生独立 commit。

## Task 5: 为 ChannelFrontend 添加单元测试

**依赖：** Task 3 完成后执行（验证 Task 3 的渲染逻辑）；测试代码中的 `EngineEvent::Text` 构造只需 Task 1 的字段定义即可编写。

**Files:**
- Modify: `src/channels/frontend.rs:155-260`

**Interfaces:**
- Consumes: `ChannelFrontend::push_event` 输出
- Produces: 通过测试

> 注：Task 5 与 Task 6 分别覆盖不同模块的测试（`ChannelFrontend` vs `frontend_output_system`），因此拆分为两个任务。

- [ ] **Step 1: 添加带 task_id 的 Text 事件测试辅助函数**

在 `src/channels/frontend.rs` 的 test 模块中，现有 `text_event` 函数（约 line 170）之后增加：

```rust
fn text_event_with_task(target: EventTarget, task_id: TaskId) -> EngineEvent {
    EngineEvent::Text {
        target,
        role: crate::domain::MessageRole::Agent,
        content: "hello".to_string(),
        task_id: Some(task_id),
    }
}
```

- [ ] **Step 2: 添加 Agent 角色前缀测试**

```rust
#[test]
fn prefixes_agent_text_with_task_short_id() {
    use uuid::Uuid;
    let (fe, mut rx) = make_frontend(FrontendKind::Telegram);
    let task_id = Uuid::parse_str("a1b2c3d4-1111-2222-3333-444444444444").unwrap();
    fe.push_event(text_event_with_task(
        EventTarget::Directed(vec![ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "u1".to_string(),
            thread_id: None,
        }]),
        task_id,
    ));
    let (_, msg) = rx.try_recv().expect("one outbound message");
    assert_eq!(msg.content, "[a1b2c3d4] 助手: hello");
    assert!(rx.try_recv().is_err());
}
```

- [ ] **Step 3: 添加 System 角色前缀测试**

```rust
#[test]
fn prefixes_system_text_with_task_short_id() {
    use uuid::Uuid;
    let (fe, mut rx) = make_frontend(FrontendKind::Telegram);
    let task_id = Uuid::parse_str("a1b2c3d4-1111-2222-3333-444444444444").unwrap();
    fe.push_event(EngineEvent::Text {
        target: EventTarget::Directed(vec![ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "u1".to_string(),
            thread_id: None,
        }]),
        role: crate::domain::MessageRole::System,
        content: "summary done".to_string(),
        task_id: Some(task_id),
    });
    let (_, msg) = rx.try_recv().expect("one outbound message");
    assert_eq!(msg.content, "[a1b2c3d4] 系统: summary done");
}
```

- [ ] **Step 4: 添加状态变更测试**

```rust
#[test]
fn renders_task_status_change_with_transition() {
    use uuid::Uuid;
    let (fe, mut rx) = make_frontend(FrontendKind::Telegram);
    let task_id = Uuid::parse_str("a1b2c3d4-1111-2222-3333-444444444444").unwrap();
    fe.push_event(EngineEvent::TaskStatusChanged {
        target: EventTarget::Directed(vec![ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "u1".to_string(),
            thread_id: None,
        }]),
        task_id,
        name: "test".to_string(),
        status: TaskStatusKind::Done,
        old_status: Some(TaskStatusKind::Running),
        result: None,
        parent_id: None,
    });
    let (_, msg) = rx.try_recv().expect("one outbound message");
    assert_eq!(msg.content, "[a1b2c3d4] 状态: 运行中 → 已完成");
    assert!(rx.try_recv().is_err());
}
```

- [ ] **Step 5: 运行新增测试**

Run: `cargo test --all-features channels::frontend`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add src/channels/frontend.rs
git commit -m "test(channels): add task prefix and status change tests"
```

## Task 6: 更新 frontend_output_system 测试以验证 task_id 透传

**Files:**
- Modify: `src/systems/frontend_output.rs:255-612`

**Interfaces:**
- Consumes: 更新后的 `EngineEvent::Text` 与 `TaskStatusChanged`
- Produces: 通过测试

- [ ] **Step 1: 添加 Text 事件携带 task_id 的测试**

在 `src/systems/frontend_output.rs` 的 test 模块中，现有 `event_task_user_output_is_dropped_when_output_channel_is_none` 测试（约 line 403-436）之后增加：

```rust
#[test]
fn user_output_text_event_includes_task_id() {
    let mut app = App::new();
    let events = Arc::new(Mutex::new(Vec::new()));
    let frontend = MockFrontend {
        kind: FrontendKind::Telegram,
        events: events.clone(),
    };
    app.insert_resource(FrontendRegistry {
        frontends: vec![Box::new(frontend)],
    });
    app.add_systems(Update, frontend_output_system);

    let origin_channel = ChannelId {
        frontend: FrontendKind::Telegram,
        user_id: "u1".to_string(),
        thread_id: None,
    };
    let task = Task::from_user_input("test", 3, origin_channel);
    let task_id = task.id;
    app.world_mut().spawn(task);
    app.world_mut().spawn(UserOutputMessage {
        task_id,
        content: "hello".to_string(),
    });

    app.update();

    let events = events.lock().unwrap();
    let text_task_id = events
        .iter()
        .find_map(|e| match e {
            EngineEvent::Text { task_id, .. } => *task_id,
            _ => None,
        })
        .expect("should emit Text event with task_id");
    assert_eq!(text_task_id, Some(task_id));
}
```

- [ ] **Step 2: 添加 TaskStatusChanged 携带 old_status 的测试**

```rust
#[test]
fn task_status_changed_event_includes_old_status() {
    let mut app = App::new();
    let events = Arc::new(Mutex::new(Vec::new()));
    let frontend = MockFrontend {
        kind: FrontendKind::Telegram,
        events: events.clone(),
    };
    app.insert_resource(FrontendRegistry {
        frontends: vec![Box::new(frontend)],
    });
    app.add_systems(Update, frontend_output_system);

    let origin_channel = ChannelId {
        frontend: FrontendKind::Telegram,
        user_id: "u1".to_string(),
        thread_id: None,
    };
    let task = Task::from_user_input("test", 3, origin_channel);
    let task_id = task.id;
    app.world_mut().spawn(task);

    // First update: task status change from Pending -> Running
    {
        let mut task = app
            .world_mut()
            .query::<&mut Task>()
            .iter_mut(app.world_mut())
            .find(|t| t.id == task_id)
            .unwrap();
        task.status = crate::domain::TaskStatus::Running;
    }
    app.update();

    // Second update: Running -> Done
    {
        let mut task = app
            .world_mut()
            .query::<&mut Task>()
            .iter_mut(app.world_mut())
            .find(|t| t.id == task_id)
            .unwrap();
        task.status = crate::domain::TaskStatus::Done;
    }
    app.update();

    let events = events.lock().unwrap();
    let status_events: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            EngineEvent::TaskStatusChanged { task_id: id, status, old_status, .. } if *id == task_id => {
                Some((*old_status, *status))
            }
            _ => None,
        })
        .collect();

    assert_eq!(status_events.len(), 2);
    assert_eq!(status_events[0], (None, crate::domain::TaskStatusKind::Running));
    assert_eq!(
        status_events[1],
        (
            Some(crate::domain::TaskStatusKind::Running),
            crate::domain::TaskStatusKind::Done,
        )
    );
}
```

- [ ] **Step 3: 运行测试**

Run: `cargo test --all-features frontend_output`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add src/systems/frontend_output.rs
git commit -m "test(frontend-output): verify task_id and old_status propagation"
```

## Task 7: 全量回归

**Files:**
- All modified files

- [ ] **Step 1: 格式化检查**

Run: `cargo fmt --all --check`
Expected: PASS

- [ ] **Step 2: Clippy**

Run: `cargo clippy --all-targets --all-features -- -D warnings`
Expected: PASS

- [ ] **Step 3: 测试**

Run: `cargo test --all-features`
Expected: PASS

- [ ] **Step 4: Commit（若产生修复）**

```bash
git add .
git commit -m "chore: fix fmt and clippy warnings"
```

## Task 8: 更新文档索引

**Files:**
- Modify: `docs/superpowers/README.md`（已在设计阶段更新，确认即可）
- Modify: `docs/current-state.md`（如其中描述了 IM  outbound 行为）

- [ ] **Step 1: 确认 README 索引**

确认 `docs/superpowers/README.md` 已包含：

```markdown
| `specs/2026-07-08-im-channel-task-identification-design.md` | IM 通道任务标识设计 | 活跃 |
```

- [ ] **Step 2: 检查 current-state.md**

搜索 `docs/current-state.md` 中关于 IM 自动回执、多任务并行的描述。如有"消息不带任务标识"等过时效描述，更新为反映新行为。

- [ ] **Step 3: Commit**

```bash
git add docs/
git commit -m "docs: update index and current state for IM task identification"
```

---

## Self-Review Checklist

- [x] **Spec coverage:** 设计文档中所有要点（Text 前缀、System 前缀、失败提示前缀、状态变更渲染、TUI 兼容）均已对应任务。
- [x] **Placeholder scan:** 无 TBD/TODO/"后续补充" 等占位符。
- [x] **Type consistency:** `task_id: Option<TaskId>`、`old_status: Option<TaskStatusKind>` 在各任务中一致使用。
- [x] **Scope check:** 本次计划仅覆盖 IM 通道任务标识与状态展示，不触及工具执行、LLM 调用、审批逻辑。

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-07-08-im-channel-task-identification.md`. Two execution options:

1. **Subagent-Driven (recommended)** - dispatch a fresh subagent per task, review between tasks, fast iteration
2. **Inline Execution** - execute tasks in this session using executing-plans, batch execution with checkpoints

Which approach?
