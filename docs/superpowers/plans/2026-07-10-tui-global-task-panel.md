# TUI 全局 Task 面板实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 将 TUI 的 Task 面板从只显示 TUI 通道任务扩展为显示所有通道任务的全局概览面板。

**Architecture:** 在事件层给 `TaskStatusChanged` 添加 `origin_channel` 字段；在 TuiFrontend 放宽过滤逻辑使其接收所有 TaskStatusChanged 事件；在 App 状态层记录来源通道和终态时间戳，并实现自动清理；在渲染层显示来源标签。

**Tech Stack:** Rust, ratatui, Bevy ECS, crossbeam-channel

## Global Constraints

- 语言：Rust，遵循官方风格指南
- 架构：Bevy ECS
- 测试：单元测试使用 `#[cfg(test)]`，与实现放在一起
- 提交信息：Conventional Commits
- 通过分支和 PR 合并代码，禁止直接推送到 `main`
- 使用中文撰写项目文档

---

## File Structure

| 文件 | 职责 | 变更类型 |
|---|---|---|
| `src/domain/frontend.rs` | `EngineEvent` 定义 | 修改 |
| `src/systems/frontend_output.rs` | 填充 `origin_channel` | 修改 |
| `src/tui/mod.rs` | `TuiFrontend.push_event()` 放宽过滤 | 修改 |
| `src/tui/app.rs` | `TaskState` 新增字段、自动清理、`handle_engine_event` 适配 | 修改 |
| `src/tui/status.rs` | 渲染来源标签 | 修改 |
| `src/channels/frontend.rs` | 适配 `TaskStatusChanged` 新字段 | 修改 |

---

### Task 1: EngineEvent::TaskStatusChanged 新增 origin_channel 字段

**Files:**
- Modify: `src/domain/frontend.rs:125-133`
- Modify: `src/channels/frontend.rs:183-225`（ChannelFrontend 解构处）

**Interfaces:**
- Produces: `EngineEvent::TaskStatusChanged { origin_channel: Option<ChannelId>, .. }` — 后续任务依赖此字段

- [ ] **Step 1: 在 `EngineEvent::TaskStatusChanged` 中添加 `origin_channel` 字段**

在 `src/domain/frontend.rs` 第 125-133 行，`TaskStatusChanged` 变体中添加字段：

```rust
    /// Task 状态变化
    TaskStatusChanged {
        target: EventTarget,
        task_id: TaskId,
        name: String,
        status: TaskStatusKind,
        old_status: Option<TaskStatusKind>,
        result: Option<String>,
        parent_id: Option<TaskId>,
        /// 任务来源的前端通道，事件任务为 None
        origin_channel: Option<ChannelId>,
    },
```

- [ ] **Step 2: 修复 `src/channels/frontend.rs` 中 `TaskStatusChanged` 解构**

在 `src/channels/frontend.rs` 第 183 行附近的 `EngineEvent::TaskStatusChanged` match arm，添加 `origin_channel: _,` 忽略该字段（Channel 前端不使用来源通道信息）：

```rust
            EngineEvent::TaskStatusChanged {
                target,
                task_id,
                status,
                old_status,
                origin_channel: _,
                ..
            } => {
```

- [ ] **Step 3: 修复所有测试中的 `TaskStatusChanged` 构造**

在以下文件中，所有构造 `EngineEvent::TaskStatusChanged { ... }` 的地方添加 `origin_channel: None,`：

- `src/channels/frontend.rs` 测试中所有 `EngineEvent::TaskStatusChanged { ... }` 构造（约第 328、456、480、501 行）
- `src/systems/frontend_output.rs` 测试中所有 `EngineEvent::TaskStatusChanged { ... }` 构造（约第 723、786、868 行）
- `src/tui/app.rs` 测试中所有 `EngineEvent::TaskStatusChanged { ... }` 构造（约第 602、626、637、648、864、875、886、908、920 行）

每个构造处添加 `origin_channel: None,`。

- [ ] **Step 4: 运行 `cargo check` 确认编译通过**

Run: `cargo check --all-features 2>&1 | head -30`
Expected: 无错误

- [ ] **Step 5: 运行全部测试确认通过**

Run: `cargo test --all-features 2>&1 | tail -20`
Expected: 全部 PASS

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat: add origin_channel field to EngineEvent::TaskStatusChanged"
```

---

### Task 2: frontend_output_system 填充 origin_channel

**Files:**
- Modify: `src/systems/frontend_output.rs:129-137`

**Interfaces:**
- Consumes: `EngineEvent::TaskStatusChanged { origin_channel: Option<ChannelId>, .. }` 来自 Task 1
- Produces: 发出的事件中 `origin_channel` 字段被正确填充

- [ ] **Step 1: 编写测试验证 origin_channel 被正确填充**

在 `src/systems/frontend_output.rs` 的 `#[cfg(test)] mod tests` 块中添加测试：

```rust
    #[test]
    fn task_status_changed_event_includes_origin_channel() {
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
            frontend: FrontendKind::QQ,
            user_id: "qq_user".to_string(),
            thread_id: None,
        };
        let task = Task::from_user_input("test", 3, origin_channel.clone());
        let task_id = task.id;
        app.world_mut().spawn(task);

        // Update task status to trigger event
        {
            let mut task = app
                .world_mut()
                .query::<&mut Task>()
                .iter_mut(app.world_mut())
                .find(|t| t.id == task_id)
                .unwrap();
            task.status = TaskStatus::Running;
        }
        app.update();

        let events = events.lock().unwrap();
        let origin = events
            .iter()
            .find_map(|e| match e {
                EngineEvent::TaskStatusChanged {
                    origin_channel, ..
                } => origin_channel.clone(),
                _ => None,
            })
            .expect("should emit TaskStatusChanged with origin_channel");
        assert_eq!(origin, Some(origin_channel));
    }
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test task_status_changed_event_includes_origin_channel --all-features 2>&1 | tail -10`
Expected: FAIL（编译错误或 assert 失败，因为 `origin_channel` 尚未被填充）

- [ ] **Step 3: 在 frontend_output_system 中填充 origin_channel**

在 `src/systems/frontend_output.rs` 第 129-137 行，`TaskStatusChanged` 事件构造处添加 `origin_channel`：

```rust
        let event = EngineEvent::TaskStatusChanged {
            target,
            task_id: task.id,
            name: task.input_summary.clone(),
            status,
            old_status,
            result,
            parent_id: task.parent_task_id,
            origin_channel: task.origin_channel.clone(),
        };
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test task_status_changed_event_includes_origin_channel --all-features 2>&1 | tail -10`
Expected: PASS

- [ ] **Step 5: 运行全部测试确认无回归**

Run: `cargo test --all-features 2>&1 | tail -20`
Expected: 全部 PASS

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat: populate origin_channel in frontend_output_system TaskStatusChanged events"
```

---

### Task 3: TuiFrontend 放宽 TaskStatusChanged 过滤

**Files:**
- Modify: `src/tui/mod.rs:44-63`

**Interfaces:**
- Consumes: `EngineEvent::TaskStatusChanged` 来自 Task 1
- Produces: TuiFrontend 将所有 `TaskStatusChanged` 事件转发给 App，无论 target 是否匹配 TUI 通道

- [ ] **Step 1: 编写测试验证 TuiFrontend 接收非 TUI 通道的 TaskStatusChanged**

在 `src/tui/mod.rs` 的 `#[cfg(test)]` 块中（如果不存在则创建）添加测试：

```rust
#[cfg(test)]
mod tests {
    use crossbeam_channel::unbounded;
    use crate::domain::{ChannelId, EngineEvent, EventTarget, FrontendKind, TaskStatusKind};
    use crate::tui::TuiFrontend;
    use crate::domain::Frontend;
    use uuid::Uuid;

    #[test]
    fn tui_accepts_task_status_from_other_channels() {
        let (event_tx, event_rx) = unbounded();
        let (action_tx, action_rx) = unbounded();
        let frontend = TuiFrontend::new(event_tx, action_rx);

        // QQ 通道的 TaskStatusChanged 事件
        let qq_channel = ChannelId {
            frontend: FrontendKind::QQ,
            user_id: "qq_user".to_string(),
            thread_id: None,
        };
        frontend.push_event(EngineEvent::TaskStatusChanged {
            target: EventTarget::Directed(vec![qq_channel]),
            task_id: Uuid::new_v4(),
            name: "qq task".to_string(),
            status: TaskStatusKind::Running,
            old_status: None,
            result: None,
            parent_id: None,
            origin_channel: Some(ChannelId {
                frontend: FrontendKind::QQ,
                user_id: "qq_user".to_string(),
                thread_id: None,
            }),
        });

        let received = event_rx.try_recv();
        assert!(received.is_ok(), "TUI should accept TaskStatusChanged from QQ channel");
    }

    #[test]
    fn tui_still_filters_text_for_other_channels() {
        let (event_tx, event_rx) = unbounded();
        let (action_tx, action_rx) = unbounded();
        let frontend = TuiFrontend::new(event_tx, action_rx);

        // QQ 通道的 Text 事件应被过滤
        let qq_channel = ChannelId {
            frontend: FrontendKind::QQ,
            user_id: "qq_user".to_string(),
            thread_id: None,
        };
        frontend.push_event(EngineEvent::Text {
            target: EventTarget::Directed(vec![qq_channel]),
            role: crate::domain::MessageRole::Agent,
            content: "hello".to_string(),
            task_id: None,
        });

        let received = event_rx.try_recv();
        assert!(received.is_err(), "TUI should filter Text events for other channels");
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test tui_accepts_task_status_from_other_channels --all-features 2>&1 | tail -10`
Expected: FAIL（TuiFrontend 当前过滤掉了非 TUI 通道的 TaskStatusChanged 事件）

- [ ] **Step 3: 修改 TuiFrontend.push_event() 放宽过滤**

在 `src/tui/mod.rs` 第 44-63 行，修改 `push_event` 方法：

```rust
    fn push_event(&self, event: EngineEvent) {
        // TaskStatusChanged 始终接收（全局任务概览）
        let for_me = matches!(event, EngineEvent::TaskStatusChanged { .. })
            || match event.target() {
                EventTarget::Broadcast => true,
                EventTarget::Directed(targets) => targets
                    .iter()
                    .any(|t| t.frontend == FrontendKind::Tui && t.user_id == self.user_id),
            };
        if for_me {
            debug!(
                event = "TuiFrontendPushEvent",
                event_kind = ?event,
                "pushing engine event to TUI channel"
            );
            let _ = self.event_tx.send(event);
        } else {
            trace!(
                event = "TuiFrontendEventSkipped",
                "engine event not for this frontend, skipping"
            );
        }
    }
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test tui_accepts_task_status --all-features 2>&1 | tail -10`
Expected: 两个测试均 PASS

- [ ] **Step 5: 运行全部测试确认无回归**

Run: `cargo test --all-features 2>&1 | tail -20`
Expected: 全部 PASS

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat: TuiFrontend accepts all TaskStatusChanged events for global task view"
```

---

### Task 4: TaskState 新增字段与 handle_engine_event 适配

**Files:**
- Modify: `src/tui/app.rs:39-49`（TaskState 结构体）
- Modify: `src/tui/app.rs:76-89`（App::new）
- Modify: `src/tui/app.rs:478-504`（handle_engine_event 中 TaskStatusChanged 分支）

**Interfaces:**
- Consumes: `EngineEvent::TaskStatusChanged { origin_channel, .. }` 来自 Task 1
- Produces: `TaskState { origin_channel, completed_at }` — Task 5 和 Task 6 依赖这两个字段

- [ ] **Step 1: 在 TaskState 中添加新字段**

在 `src/tui/app.rs` 第 39-49 行，修改 `TaskState` 结构体：

```rust
/// Task 前端状态
#[derive(Debug, Clone)]
pub struct TaskState {
    pub id: uuid::Uuid,
    pub name: String,
    pub status: TaskStatusKind,
    pub result: Option<String>,
    pub parent_id: Option<uuid::Uuid>,
    pub subtask_count: u32,
    pub completed_count: u32,
    /// 任务来源的前端通道
    pub origin_channel: Option<ChannelId>,
    /// 任务进入终态的时刻
    pub completed_at: Option<std::time::Instant>,
}
```

注意：需要在文件顶部添加 `use crate::domain::ChannelId;` 的 import（检查现有 imports 是否已包含）。

- [ ] **Step 2: 修改 handle_engine_event 中 TaskStatusChanged 分支**

在 `src/tui/app.rs` 第 478-504 行，修改 `TaskStatusChanged` match arm：

```rust
            EngineEvent::TaskStatusChanged {
                task_id,
                name,
                status,
                result,
                parent_id,
                origin_channel,
                ..
            } => {
                let completed_at = if matches!(status, TaskStatusKind::Done | TaskStatusKind::Failed)
                {
                    Some(std::time::Instant::now())
                } else {
                    None
                };
                if let Some(task) = self.tasks.iter_mut().find(|t| t.id == task_id) {
                    task.status = status;
                    task.result = result;
                    task.parent_id = parent_id;
                    task.origin_channel = origin_channel.or(task.origin_channel.take());
                    task.completed_at = completed_at.or(task.completed_at);
                } else {
                    self.tasks.push(TaskState {
                        id: task_id,
                        name,
                        status,
                        result,
                        parent_id,
                        subtask_count: 0,
                        completed_count: 0,
                        origin_channel,
                        completed_at,
                    });
                }

                // 更新子任务进度
                self.update_all_subtask_progress();
            }
```

- [ ] **Step 3: 修复所有测试中 TaskState 的构造**

在 `src/tui/app.rs` 测试中，所有涉及 `TaskState` 的断言需要兼容新字段。由于测试中都是通过 `handle_engine_event` 间接创建 `TaskState`，不需要直接修改断言，但要确保 `handle_engine_event` 调用处的 `EngineEvent::TaskStatusChanged` 构造已在 Task 1 中添加了 `origin_channel: None,`。

运行编译检查确认：

Run: `cargo check --all-features 2>&1 | head -30`
Expected: 无错误

- [ ] **Step 4: 编写测试验证新字段正确填充**

在 `src/tui/app.rs` 测试块中添加：

```rust
    #[test]
    fn task_state_origin_channel_from_event() {
        let mut app = test_app();
        let task_id = Uuid::new_v4();
        let qq_channel = ChannelId {
            frontend: FrontendKind::QQ,
            user_id: "qq_user".to_string(),
            thread_id: None,
        };
        app.handle_engine_event(EngineEvent::TaskStatusChanged {
            target: EventTarget::Broadcast,
            task_id,
            name: "qq task".to_string(),
            status: TaskStatusKind::Running,
            old_status: None,
            result: None,
            parent_id: None,
            origin_channel: Some(qq_channel.clone()),
        });
        assert_eq!(app.tasks[0].origin_channel, Some(qq_channel));
        assert_eq!(app.tasks[0].completed_at, None);
    }

    #[test]
    fn task_completed_at_set_on_terminal_status() {
        let mut app = test_app();
        let task_id = Uuid::new_v4();
        app.handle_engine_event(EngineEvent::TaskStatusChanged {
            target: EventTarget::Broadcast,
            task_id,
            name: "done task".to_string(),
            status: TaskStatusKind::Done,
            old_status: None,
            result: None,
            parent_id: None,
            origin_channel: None,
        });
        assert!(app.tasks[0].completed_at.is_some());
    }
```

注意：测试文件中需要已有 `use crate::domain::ChannelId;` import，如果没有需添加。

- [ ] **Step 5: 运行测试确认通过**

Run: `cargo test task_state_origin_channel_from_event task_completed_at_set_on_terminal_status --all-features 2>&1 | tail -10`
Expected: PASS

- [ ] **Step 6: 运行全部测试确认无回归**

Run: `cargo test --all-features 2>&1 | tail -20`
Expected: 全部 PASS

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "feat: add origin_channel and completed_at fields to TaskState"
```

---

### Task 5: 已完成任务自动清理

**Files:**
- Modify: `src/tui/app.rs`（添加 `cleanup_completed_tasks` 方法，在 `render` 中调用）

**Interfaces:**
- Consumes: `TaskState.completed_at` 来自 Task 4
- Produces: `App.cleanup_completed_tasks()` — 渲染前自动清理终态超时任务

- [ ] **Step 1: 编写测试验证自动清理逻辑**

在 `src/tui/app.rs` 测试块中添加：

```rust
    #[test]
    fn cleanup_removes_expired_completed_tasks() {
        let mut app = test_app();
        let main_id = Uuid::new_v4();
        let sub_id = Uuid::new_v4();

        // 添加已完成的主任务（completed_at 设为 6 秒前）
        app.handle_engine_event(EngineEvent::TaskStatusChanged {
            target: EventTarget::Broadcast,
            task_id: main_id,
            name: "old done".to_string(),
            status: TaskStatusKind::Done,
            old_status: None,
            result: None,
            parent_id: None,
            origin_channel: None,
        });
        // 添加已完成的子任务
        app.handle_engine_event(EngineEvent::TaskStatusChanged {
            target: EventTarget::Broadcast,
            task_id: sub_id,
            name: "sub done".to_string(),
            status: TaskStatusKind::Done,
            old_status: None,
            result: None,
            parent_id: Some(main_id),
            origin_channel: None,
        });

        // 手动将 completed_at 设为 6 秒前（超过 5 秒阈值）
        let six_secs_ago = std::time::Instant::now() - std::time::Duration::from_secs(6);
        for task in &mut app.tasks {
            task.completed_at = Some(six_secs_ago);
        }

        app.cleanup_completed_tasks();
        assert!(app.tasks.is_empty(), "expired completed tasks should be removed");
    }

    #[test]
    fn cleanup_keeps_recent_completed_tasks() {
        let mut app = test_app();
        let task_id = Uuid::new_v4();

        app.handle_engine_event(EngineEvent::TaskStatusChanged {
            target: EventTarget::Broadcast,
            task_id,
            name: "fresh done".to_string(),
            status: TaskStatusKind::Done,
            old_status: None,
            result: None,
            parent_id: None,
            origin_channel: None,
        });

        // completed_at 刚设置，不会超过 5 秒
        app.cleanup_completed_tasks();
        assert_eq!(app.tasks.len(), 1, "recently completed tasks should be kept");
    }

    #[test]
    fn cleanup_keeps_active_tasks() {
        let mut app = test_app();
        let task_id = Uuid::new_v4();

        app.handle_engine_event(EngineEvent::TaskStatusChanged {
            target: EventTarget::Broadcast,
            task_id,
            name: "running task".to_string(),
            status: TaskStatusKind::Running,
            old_status: None,
            result: None,
            parent_id: None,
            origin_channel: None,
        });

        app.cleanup_completed_tasks();
        assert_eq!(app.tasks.len(), 1, "active tasks should never be cleaned up");
    }
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test cleanup_ --all-features 2>&1 | tail -10`
Expected: FAIL（`cleanup_completed_tasks` 方法尚未存在）

- [ ] **Step 3: 实现 cleanup_completed_tasks 方法**

在 `src/tui/app.rs` 的 `impl App` 块中添加方法：

```rust
    /// 清理已超过 5 秒的终态任务及其子任务
    pub fn cleanup_completed_tasks(&mut self) {
        const CLEANUP_DELAY: std::time::Duration = std::time::Duration::from_secs(5);
        let now = std::time::Instant::now();

        // 找出需要清理的主任务 ID
        let expired_main_ids: Vec<Uuid> = self
            .tasks
            .iter()
            .filter(|t| {
                t.parent_id.is_none()
                    && t.completed_at.map_or(false, |at| now.duration_since(at) > CLEANUP_DELAY)
            })
            .map(|t| t.id)
            .collect();

        if expired_main_ids.is_empty() {
            return;
        }

        // 移除过期主任务及其子任务
        self.tasks.retain(|t| {
            !expired_main_ids.contains(&t.id)
                && !t.parent_id.map_or(false, |pid| expired_main_ids.contains(&pid))
        });
    }
```

- [ ] **Step 4: 在 render 方法中调用 cleanup**

在 `src/tui/app.rs` 的 `render` 方法（约第 510 行）开头添加清理调用：

```rust
    pub fn render(&mut self, frame: &mut Frame) {
        self.cleanup_completed_tasks();

        let area = frame.area();
        // ... 其余不变
```

- [ ] **Step 5: 运行测试确认通过**

Run: `cargo test cleanup_ --all-features 2>&1 | tail -10`
Expected: 三个测试均 PASS

- [ ] **Step 6: 运行全部测试确认无回归**

Run: `cargo test --all-features 2>&1 | tail -20`
Expected: 全部 PASS

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "feat: auto-cleanup completed tasks after 5 seconds"
```

---

### Task 6: 渲染来源通道标签

**Files:**
- Modify: `src/tui/status.rs:89-184`

**Interfaces:**
- Consumes: `TaskState.origin_channel` 来自 Task 4

- [ ] **Step 1: 在 status.rs 中添加来源标签辅助函数**

在 `src/tui/status.rs` 中添加辅助函数：

```rust
    fn channel_label(channel: &crate::domain::ChannelId) -> (&'static str, Color) {
        match channel.frontend {
            crate::domain::FrontendKind::Tui => ("TUI", Color::Green),
            crate::domain::FrontendKind::QQ => ("QQ", Color::Magenta),
            crate::domain::FrontendKind::Telegram => ("TG", Color::Blue),
            crate::domain::FrontendKind::Web => ("Web", Color::DarkGray),
            crate::domain::FrontendKind::Feishu => ("FS", Color::DarkGray),
        }
    }

    fn origin_label(origin_channel: &Option<crate::domain::ChannelId>) -> (&'static str, Color) {
        match origin_channel {
            Some(ch) => Self::channel_label(ch),
            None => ("EVT", Color::DarkGray),
        }
    }
```

- [ ] **Step 2: 修改主任务渲染行以包含来源标签**

在 `src/tui/status.rs` 约第 146-152 行，修改主任务渲染行：

将：
```rust
                lines.push(Line::from(vec![
                    Span::styled(format!("{icon} "), Style::default().fg(main_color)),
                    Span::styled(
                        format!("{}{}", main_task.name, progress_text),
                        Style::default().fg(main_color).add_modifier(Modifier::BOLD),
                    ),
                ]));
```

改为：
```rust
                let (label_text, label_color) = Self::origin_label(&main_task.origin_channel);
                lines.push(Line::from(vec![
                    Span::styled(format!("{icon} "), Style::default().fg(main_color)),
                    Span::styled(
                        format!("[{label_text}] "),
                        Style::default().fg(label_color),
                    ),
                    Span::styled(
                        format!("{}{}", main_task.name, progress_text),
                        Style::default().fg(main_color).add_modifier(Modifier::BOLD),
                    ),
                ]));
```

- [ ] **Step 3: 修改子任务渲染行以包含来源标签**

在 `src/tui/status.rs` 约第 174-181 行，修改子任务渲染行：

将：
```rust
                        lines.push(Line::from(vec![
                            Span::styled("  │ ", Style::default().fg(Color::DarkGray)),
                            Span::styled(
                                format!("{sub_icon} "),
                                Style::default().fg(sub_task_color),
                            ),
                            Span::styled(&subtask.name, Style::default().fg(sub_task_color)),
                        ]));
```

改为：
```rust
                        let (sub_label_text, sub_label_color) = Self::origin_label(&subtask.origin_channel);
                        lines.push(Line::from(vec![
                            Span::styled("  │ ", Style::default().fg(Color::DarkGray)),
                            Span::styled(
                                format!("{sub_icon} "),
                                Style::default().fg(sub_task_color),
                            ),
                            Span::styled(
                                format!("[{sub_label_text}] "),
                                Style::default().fg(sub_label_color),
                            ),
                            Span::styled(&subtask.name, Style::default().fg(sub_task_color)),
                        ]));
```

- [ ] **Step 4: 运行编译检查确认通过**

Run: `cargo check --all-features 2>&1 | head -30`
Expected: 无错误

- [ ] **Step 5: 运行全部测试确认无回归**

Run: `cargo test --all-features 2>&1 | tail -20`
Expected: 全部 PASS

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat: render origin channel labels in TUI task panel"
```

---

### Task 7: 端到端验证与收尾

**Files:**
- 无新文件修改

- [ ] **Step 1: 运行完整 CI 检查**

Run: `cargo fmt --all --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test --all-features`
Expected: 全部通过

- [ ] **Step 2: 检查 markdownlint**

Run: `markdownlint docs/superpowers/specs/2026-07-10-tui-global-task-panel-design.md 2>&1 || true`
Expected: 无错误（或安装 markdownlint 后无错误）

- [ ] **Step 3: 最终 Commit（如有 lint 修复）**

```bash
git add -A
git commit -m "chore: lint fixes for global task panel feature"
```
