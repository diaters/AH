# 前端展示优化 实现计划

> **面向 AI 代理的工作者：** 必需子技能：使用 superpowers:subagent-driven-development（推荐）或 superpowers:executing-plans 逐任务实现此计划。步骤使用复选框（`- [ ]`）语法来跟踪进度。

**目标：** 优化 TUI 与 IM 通道的前端展示——去除 TUI Agent 列表、Task 附带 agent 名、侧边栏加宽强化监控、IM 通道推送工具调用具体情况。

**架构：** 扩展 `EngineEvent` 携带 agent_name 与 waiting_reason，新增 `ToolCallStarted` 事件；`frontend_output_system` 与 `tool_dispatch_system` 负责事件产生；TUI 与 IM 通道（`ChannelFrontend`）负责事件消费与渲染。

**技术栈：** Rust + Bevy ECS + ratatui

**规格：** `docs/superpowers/specs/2026-07-30-frontend-display-optimization-design.md`

---

## 文件结构

### 修改文件

- `src/domain/frontend.rs`：扩展 `TaskStatusChanged`，新增 `WaitingReasonKind`、`ToolCallStarted`、`summarize_tool_input`
- `src/domain/mod.rs`：导出新类型
- `src/systems/frontend_output.rs`：`frontend_output_system` 构造事件时填充 agent_name 与 waiting_reason
- `src/systems/tools/dispatch.rs`：`tool_dispatch_system` 在 Allow 路径推送 `ToolCallStarted`
- `src/tui/mod.rs`：`push_event` 全局接收 `ToolCallStarted`
- `src/tui/app.rs`：`TaskState` 扩展，事件处理，侧边栏加宽，`ToolCallStarted` 转 `ChatMessage::System`
- `src/tui/status.rs`：去除 Agents 段，Task 渲染追加 `@agent_name` 与 `⏳reason`
- `src/channels/frontend.rs`：`TaskStatusChanged` 渲染扩展，新增 `ToolCallStarted` 渲染

---

### 任务 1：扩展 EngineEvent 类型与辅助函数

**文件：**
- 修改：`src/domain/frontend.rs`
- 修改：`src/domain/mod.rs`

- [ ] **步骤 1：在 `src/domain/frontend.rs` 新增 `WaitingReasonKind` 枚举**

在 `TaskStatusKind` 枚举之后（约行 81 后）新增：

```rust
/// 等待原因（前端展示用，精简版）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitingReasonKind {
    Agent,
    Tool,
    User,
    Retry,
    Other,
}
```

- [ ] **步骤 2：在 `src/domain/frontend.rs` 新增 `summarize_tool_input` 函数**

在 `impl EngineEvent` 块之前（约行 145 前）新增：

```rust
/// 生成工具调用的输入摘要（用于前端展示，避免长参数刷屏）
pub fn summarize_tool_input(tool_name: &str, tool_input: &serde_json::Value) -> String {
    match tool_name {
        "shell_exec" | "shell_start" => tool_input
            .get("command")
            .and_then(|v| v.as_str())
            .map(|s| {
                if s.len() > 80 {
                    format!("{}…", &s[..80])
                } else {
                    s.to_string()
                }
            })
            .unwrap_or_default(),
        "channel_send" => {
            let channel = tool_input
                .get("channel")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let content = tool_input
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let content_preview = if content.len() > 50 {
                format!("{}…", &content[..50])
            } else {
                content.to_string()
            };
            format!("channel={channel} content={content_preview}")
        }
        "create_tasks" => tool_input
            .get("tasks")
            .and_then(|v| v.as_array())
            .map(|arr| format!("{} 个子任务", arr.len()))
            .unwrap_or_default(),
        "wait_tasks" => tool_input
            .get("task_ids")
            .and_then(|v| v.as_array())
            .map(|arr| format!("等待 {} 个任务", arr.len()))
            .unwrap_or_default(),
        _ => {
            let s = serde_json::to_string(tool_input).unwrap_or_default();
            if s.len() > 100 {
                format!("{}…", &s[..100])
            } else {
                s
            }
        }
    }
}
```

- [ ] **步骤 3：扩展 `TaskStatusChanged` 事件变体**

在 `EngineEvent` 枚举中，修改 `TaskStatusChanged` 变体（约行 125-135），新增 `agent_name` 与 `waiting_reason` 字段：

```rust
TaskStatusChanged {
    target: EventTarget,
    task_id: TaskId,
    name: String,
    status: TaskStatusKind,
    old_status: Option<TaskStatusKind>,
    result: Option<String>,
    parent_id: Option<TaskId>,
    origin_channel: Option<ChannelId>,
    agent_name: Option<String>,
    waiting_reason: Option<WaitingReasonKind>,
},
```

同步更新 `impl EngineEvent` 的 `target()` 方法（约行 147-156），match 分支无需改动（字段用 `..` 省略）。

- [ ] **步骤 4：新增 `ToolCallStarted` 事件变体**

在 `EngineEvent` 枚举中，`BatchProgress` 变体之后新增：

```rust
/// 工具调用开始（不含结果）
ToolCallStarted {
    target: EventTarget,
    task_id: TaskId,
    agent_name: String,
    tool_name: String,
    tool_input_summary: String,
},
```

同步更新 `impl EngineEvent` 的 `target()` 方法，新增 match 分支：

```rust
Self::ToolCallStarted { target, .. } => target,
```

- [ ] **步骤 5：在 `src/domain/mod.rs` 导出新类型**

在 `frontend` 模块的导出列表中（搜索 `WaitingReason` 或 `TaskStatusKind` 的导出行），追加 `WaitingReasonKind`。若 `frontend` 模块通过 `pub use frontend::{...}` 导出，确认 `WaitingReasonKind`、`ToolCallStarted`（事件变体无需单独导出，它在 `EngineEvent` 内）已在列表中。

- [ ] **步骤 6：运行编译验证**

运行：`export PATH="/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin:/usr/local/bin"; cargo build 2>&1 | tail -30`

预期：编译错误——所有构造 `TaskStatusChanged` 的地方缺少新字段。这是预期的，后续任务会修复。记录错误数量作为基线。

- [ ] **步骤 7：Commit**

```bash
git add src/domain/frontend.rs src/domain/mod.rs
git commit -m "feat: 扩展 EngineEvent 携带 agent_name/waiting_reason 与 ToolCallStarted 事件"
```

---

### 任务 2：frontend_output_system 填充新字段

**文件：**
- 修改：`src/systems/frontend_output.rs:105-152`（TaskStatusChanged 构造）
- 测试：`src/systems/frontend_output.rs` 测试模块

- [ ] **步骤 1：编写失败的测试——agent_name 与 waiting_reason 填充**

在 `src/systems/frontend_output.rs` 测试模块末尾新增测试：

```rust
#[test]
fn task_status_changed_includes_agent_name_and_waiting_reason() {
    let mut app = App::new();
    let events = Arc::new(Mutex::new(Vec::new()));
    let frontend = MockFrontend {
        kind: FrontendKind::Telegram,
        events: events.clone(),
    };
    app.insert_resource(FrontendRegistry {
        frontends: vec![Box::new(frontend)],
    });
    app.insert_resource(EntityIndex::default());
    app.add_systems(Update, frontend_output_system);

    let origin_channel = ChannelId {
        frontend: FrontendKind::Telegram,
        user_id: "u1".to_string(),
        thread_id: None,
    };
    let mut task = Task::from_user_input("test", 3, origin_channel);
    task.delegate = Some(Uuid::nil());
    task.status = TaskStatus::Waiting(crate::domain::WaitingReason::ToolExecution);
    let task_id = task.id;
    let task_entity = app.world_mut().spawn(task).id();
    app.world_mut()
        .resource_mut::<EntityIndex>()
        .tasks
        .insert(task_id, task_entity);

    // spawn agent with nil id and profile name "TestAgent"
    let agent = crate::domain::Agent {
        id: Uuid::nil(),
        profile: crate::domain::AgentProfile {
            name: "TestAgent".to_string(),
            ..Default::default()
        },
        ..Default::default()
    };
    let agent_entity = app.world_mut().spawn(agent).id();
    app.world_mut()
        .resource_mut::<EntityIndex>()
        .agents
        .insert(Uuid::nil(), agent_entity);

    app.update();

    let events = events.lock().unwrap();
    let (agent_name, waiting_reason) = events
        .iter()
        .find_map(|e| match e {
            EngineEvent::TaskStatusChanged {
                agent_name,
                waiting_reason,
                ..
            } => Some((agent_name.clone(), *waiting_reason)),
            _ => None,
        })
        .expect("should emit TaskStatusChanged");
    assert_eq!(agent_name.as_deref(), Some("TestAgent"));
    assert_eq!(
        waiting_reason,
        Some(crate::domain::WaitingReasonKind::Tool)
    );
}
```

- [ ] **步骤 2：运行测试验证失败**

运行：`export PATH="/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin:/usr/local/bin"; cargo test --lib frontend_output -- task_status_changed_includes_agent_name_and_waiting_reason 2>&1 | tail -20`

预期：FAIL（agent_name 为 None，waiting_reason 为 None）

- [ ] **步骤 3：实现 agent_name 与 waiting_reason 填充**

在 `src/systems/frontend_output.rs` 的 `frontend_output_system` 中，Task 状态变化处理段（约行 105-152），修改事件构造。

在构造事件前（约行 133 前），新增 agent_name 与 waiting_reason 的计算：

```rust
let agent_name = task
    .delegate
    .and_then(|agent_id| {
        index
            .get_agent(&agent_id)
            .and_then(|e| agents.get(e).ok())
            .map(|a| a.profile.name.clone())
    });

let waiting_reason = match &task.status {
    TaskStatus::Waiting(reason) => Some(waiting_reason_to_kind(reason)),
    _ => None,
};
```

然后在事件构造中填入新字段：

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
    agent_name,
    waiting_reason,
};
```

注意：`agents: Query<&Agent, Changed<Agent>>`（行 24）带 `Changed` 过滤器，不能用于按 entity 任意查询。需在函数签名新增 `all_agents: Query<&Agent>` 参数（不带 `Changed`），用于 `index.get_agent` 后按 entity 读取 agent name。在 `frontend_output_system` 签名中新增 `all_agents: Query<&Agent>`。

- [ ] **步骤 4：新增 `waiting_reason_to_kind` 辅助函数**

在 `src/systems/frontend_output.rs` 中（`task_status_to_kind` 函数附近，约行 272）新增：

```rust
fn waiting_reason_to_kind(reason: &crate::domain::WaitingReason) -> crate::domain::WaitingReasonKind {
    use crate::domain::{WaitingReason, WaitingReasonKind};
    match reason {
        WaitingReason::Agent => WaitingReasonKind::Agent,
        WaitingReason::ToolExecution
        | WaitingReason::Session { .. }
        | WaitingReason::SubTaskBatch { .. } => WaitingReasonKind::Tool,
        WaitingReason::User | WaitingReason::Approval => WaitingReasonKind::User,
        WaitingReason::RetryBackoff => WaitingReasonKind::Retry,
        WaitingReason::Evaluator
        | WaitingReason::Summarization
        | WaitingReason::ChatAgent => WaitingReasonKind::Other,
    }
}
```

- [ ] **步骤 5：运行测试验证通过**

运行：`export PATH="/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin:/usr/local/bin"; cargo test --lib frontend_output -- task_status_changed_includes_agent_name_and_waiting_reason 2>&1 | tail -20`

预期：PASS

- [ ] **步骤 6：Commit**

```bash
git add src/systems/frontend_output.rs
git commit -m "feat: frontend_output_system 填充 agent_name 与 waiting_reason"
```

---

### 任务 3：tool_dispatch_system 推送 ToolCallStarted

**文件：**
- 修改：`src/systems/tools/dispatch.rs:144-237`（Allow 路径）
- 测试：`src/systems/tools/dispatch.rs` 测试模块（若有）或 `tests/` 集成测试

- [ ] **步骤 1：编写失败的测试——ToolCallStarted 推送**

在 `tests/` 目录下新增 `tool_call_started_event.rs`（若不便构造集成测试，可在 `src/systems/tools/dispatch.rs` 测试模块中编写单元测试，覆盖事件推送逻辑）。

由于 `tool_dispatch_system` 参数复杂，推荐用单元测试验证 `summarize_tool_input` 与事件构造逻辑，用集成测试验证端到端推送。

先在 `src/domain/frontend.rs` 测试模块新增 `summarize_tool_input` 单元测试：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarize_shell_exec_command() {
        let input = serde_json::json!({"command": "ls -la"});
        assert_eq!(summarize_tool_input("shell_exec", &input), "ls -la");
    }

    #[test]
    fn summarize_shell_exec_long_command_truncated() {
        let long_cmd = "a".repeat(100);
        let input = serde_json::json!({"command": long_cmd});
        let result = summarize_tool_input("shell_exec", &input);
        assert!(result.ends_with('…'));
        assert_eq!(result.len(), 81); // 80 + ellipsis
    }

    #[test]
    fn summarize_channel_send() {
        let input = serde_json::json!({"channel": "qq", "content": "hello"});
        assert_eq!(summarize_tool_input("channel_send", &input), "channel=qq content=hello");
    }

    #[test]
    fn summarize_create_tasks() {
        let input = serde_json::json!({"tasks": [{"goal": "a"}, {"goal": "b"}]});
        assert_eq!(summarize_tool_input("create_tasks", &input), "2 个子任务");
    }

    #[test]
    fn summarize_wait_tasks() {
        let input = serde_json::json!({"task_ids": ["id1", "id2", "id3"]});
        assert_eq!(summarize_tool_input("wait_tasks", &input), "等待 3 个任务");
    }

    #[test]
    fn summarize_unknown_tool_fallback_json() {
        let input = serde_json::json!({"key": "value"});
        let result = summarize_tool_input("unknown_tool", &input);
        assert!(result.contains("key"));
    }

    #[test]
    fn summarize_missing_field_returns_empty() {
        let input = serde_json::json!({});
        assert_eq!(summarize_tool_input("shell_exec", &input), "");
    }
}
```

- [ ] **步骤 2：运行测试验证通过（summarize 函数已在任务 1 实现）**

运行：`export PATH="/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin:/usr/local/bin"; cargo test --lib domain::frontend -- summarize 2>&1 | tail -20`

预期：PASS（`summarize_tool_input` 已在任务 1 实现）

- [ ] **步骤 3：在 `tool_dispatch_system` 函数签名新增 FrontendRegistry 参数**

在 `src/systems/tools/dispatch.rs` 的 `tool_dispatch_system` 签名中新增参数 `frontend_registry: Res<crate::app::FrontendRegistry>`。

Allow 路径已有 `agent: &Agent`（行 88-105 通过 `index.get_agent` 获取），直接用 `agent.profile.name.clone()` 即可，无需额外 agent 查询参数。

- [ ] **步骤 4：在 Allow 路径推送 ToolCallStarted**

在 `src/systems/tools/dispatch.rs` 的 Allow 路径（约行 172-181 的 `info!` 之后，`executor.execute` 之前），新增事件推送：

```rust
// 推送 ToolCallStarted 事件到所有前端
let tool_input_summary =
    crate::domain::summarize_tool_input(&tool_name, &request.tool_input);
let target = index
    .get_task(&request.request.task_id)
    .and_then(|e| tasks.get(e).ok())
    .and_then(|(_, t)| t.routing_policy.output_channel.clone())
    .map(|channel| EventTarget::Directed(vec![channel]))
    .unwrap_or(EventTarget::Broadcast);
let event = crate::domain::EngineEvent::ToolCallStarted {
    target,
    task_id: request.request.task_id,
    agent_name: agent.profile.name.clone(),
    tool_name: tool_name.clone(),
    tool_input_summary,
};
for frontend in &frontend_registry.frontends {
    frontend.push_event(event.clone());
}
```

注：`EventTarget` 需确认已导入（`use crate::domain::EventTarget;` 或通过 prelude）。

- [ ] **步骤 5：运行编译验证**

运行：`export PATH="/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin:/usr/local/bin"; cargo build 2>&1 | tail -20`

预期：编译通过（或仅有其他任务的待修复错误）

- [ ] **步骤 6：运行现有测试确保无回归**

运行：`export PATH="/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin:/usr/local/bin"; cargo test --lib 2>&1 | tail -20`

预期：所有现有测试 PASS（或仅有 fixture 缺字段的编译错误，任务 6 修复）

- [ ] **步骤 7：Commit**

```bash
git add src/systems/tools/dispatch.rs
git commit -m "feat: tool_dispatch_system 在 Allow 路径推送 ToolCallStarted 事件"
```

---

### 任务 4：TUI 调整

**文件：**
- 修改：`src/tui/mod.rs:36-58`（push_event）
- 修改：`src/tui/app.rs:46-59`（TaskState）、`src/tui/app.rs:650-691`（事件处理）、`src/tui/app.rs:727-744`（布局）
- 修改：`src/tui/status.rs:71-205`（去除 Agents 段，Task 渲染扩展）

- [ ] **步骤 1：在 `push_event` 全局接收 ToolCallStarted**

在 `src/tui/mod.rs` 的 `push_event` 方法中（行 38），修改 `for_me` 判断：

```rust
let for_me = matches!(
    event,
    EngineEvent::TaskStatusChanged { .. } | EngineEvent::ToolCallStarted { .. }
) || match event.target() {
    EventTarget::Broadcast => true,
    EventTarget::Directed(targets) => targets
        .iter()
        .any(|t| t.frontend == FrontendKind::Tui && t.user_id == self.user_id),
};
```

- [ ] **步骤 2：扩展 TaskState 结构体**

在 `src/tui/app.rs` 的 `TaskState` 结构体（行 46-59）新增字段：

```rust
#[derive(Debug, Clone)]
pub struct TaskState {
    pub id: uuid::Uuid,
    pub name: String,
    pub status: TaskStatusKind,
    pub result: Option<String>,
    pub parent_id: Option<uuid::Uuid>,
    pub subtask_count: u32,
    pub completed_count: u32,
    pub origin_channel: Option<ChannelId>,
    pub completed_at: Option<std::time::Instant>,
    pub agent_name: Option<String>,
    pub waiting_reason: Option<crate::domain::WaitingReasonKind>,
}
```

- [ ] **步骤 3：更新 TaskStatusChanged 事件处理写入新字段**

在 `src/tui/app.rs` 的事件处理（行 650-691），`EngineEvent::TaskStatusChanged` 分支中，修改模式匹配以解构新字段，并在更新/创建 `TaskState` 时填入。

解构新增 `agent_name` 与 `waiting_reason`（保留现有所有字段解构）：

```rust
EngineEvent::TaskStatusChanged {
    task_id,
    name,
    status,
    result,
    parent_id,
    origin_channel,
    agent_name,
    waiting_reason,
    ..
} => {
```

在 `if let Some(task) = self.tasks.iter_mut().find(|t| t.id == task_id)` 分支中，现有字段更新之后追加：

```rust
task.agent_name = agent_name;
task.waiting_reason = waiting_reason;
```

在 `else` 分支的 `TaskState { ... }` 构造中，现有字段之后追加：

```rust
agent_name,
waiting_reason,
```

- [ ] **步骤 4：新增 ToolCallStarted 事件处理**

在 `src/tui/app.rs` 的事件处理中，新增 `EngineEvent::ToolCallStarted` 分支：

```rust
EngineEvent::ToolCallStarted {
    task_id,
    agent_name,
    tool_name,
    tool_input_summary,
    ..
} => {
    let short_id = task_id.to_string().split('-').next().unwrap_or("????").to_string();
    let content = if tool_input_summary.is_empty() {
        format!("[{}] 🔧 {} 调用 {}", short_id, agent_name, tool_name)
    } else {
        format!(
            "[{}] 🔧 {} 调用 {}: {}",
            short_id, agent_name, tool_name, tool_input_summary
        )
    };
    self.messages.push(ChatMessage::System(content));
}
```

- [ ] **步骤 5：侧边栏加宽**

在 `src/tui/app.rs` 的 `render` 方法（行 734-738），修改约束：

```rust
let content_layout = Layout::default()
    .direction(Direction::Horizontal)
    .constraints([Constraint::Min(1), Constraint::Length(48)])
    .split(main_layout[0]);
```

- [ ] **步骤 6：去除 Agents 段渲染**

在 `src/tui/status.rs` 中，删除行 69-105（空行 + Agents 标题 + 渲染循环）。即删除：

```rust
lines.push(Line::from(""));

// Agent 列表
lines.push(Line::from(Span::styled(
    "Agents",
    Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD),
)));

for agent in &app.agents {
    // ... 整个循环 ...
}
```

保留后面的空行分隔（在 Tasks 段之前）。

- [ ] **步骤 7：主任务行追加 agent_name 与 waiting_reason**

在 `src/tui/status.rs` 的主任务渲染（约行 165-172），修改行构造：

```rust
let mut spans = vec![
    Span::styled(format!("{icon} "), Style::default().fg(main_color)),
    Span::styled(format!("[{label_text}] "), Style::default().fg(label_color)),
    Span::styled(
        format!("{}{}", main_task.name, progress_text),
        Style::default().fg(main_color).add_modifier(Modifier::BOLD),
    ),
];
if let Some(ref agent) = main_task.agent_name {
    spans.push(Span::styled(
        format!(" @{agent}"),
        Style::default().fg(Color::White),
    ));
}
if let Some(reason) = main_task.waiting_reason {
    let reason_text = match reason {
        crate::domain::WaitingReasonKind::Agent => "⏳agent",
        crate::domain::WaitingReasonKind::Tool => "⏳tool",
        crate::domain::WaitingReasonKind::User => "⏳user",
        crate::domain::WaitingReasonKind::Retry => "⏳retry",
        crate::domain::WaitingReasonKind::Other => "⏳other",
    };
    spans.push(Span::styled(
        format!(" {reason_text}"),
        Style::default().fg(Color::Cyan),
    ));
}
lines.push(Line::from(spans));
```

- [ ] **步骤 8：子任务行追加 agent_name**

在 `src/tui/status.rs` 的子任务渲染（约行 194-201），修改行构造：

```rust
let mut sub_spans = vec![
    Span::styled("  │ ", Style::default().fg(Color::DarkGray)),
    Span::styled(format!("{sub_icon} "), Style::default().fg(sub_task_color)),
    Span::styled(&subtask.name, Style::default().fg(sub_task_color)),
];
if let Some(ref agent) = subtask.agent_name {
    sub_spans.push(Span::styled(
        format!(" @{agent}"),
        Style::default().fg(Color::White),
    ));
}
lines.push(Line::from(sub_spans));
```

- [ ] **步骤 9：运行编译验证**

运行：`export PATH="/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin:/usr/local/bin"; cargo build 2>&1 | tail -20`

预期：编译通过

- [ ] **步骤 10：Commit**

```bash
git add src/tui/mod.rs src/tui/app.rs src/tui/status.rs
git commit -m "feat: TUI 去除 Agent 列表、Task 附带 agent 名、侧边栏加宽、接收 ToolCallStarted"
```

---

### 任务 5：IM 通道调整

**文件：**
- 修改：`src/channels/frontend.rs:183-225`（TaskStatusChanged 渲染扩展）
- 修改：`src/channels/frontend.rs:90-228`（新增 ToolCallStarted 渲染分支）

- [ ] **步骤 1：编写失败的测试——ToolCallStarted 渲染**

在 `src/channels/frontend.rs` 测试模块新增测试：

```rust
#[test]
fn renders_tool_call_started() {
    use uuid::Uuid;
    use crate::domain::EngineEvent;

    let (fe, mut rx) = make_frontend(FrontendKind::Telegram);
    let task_id = Uuid::parse_str("a1b2c3d4-1111-2222-3333-444444444444").unwrap();
    fe.push_event(EngineEvent::ToolCallStarted {
        target: EventTarget::Directed(vec![ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "u1".to_string(),
            thread_id: None,
        }]),
        task_id,
        agent_name: "TestAgent".to_string(),
        tool_name: "shell_exec".to_string(),
        tool_input_summary: "ls -la".to_string(),
    });
    let (_, msg) = rx.try_recv().expect("one outbound message");
    assert_eq!(msg.content, "[a1b2c3d4] 🔧 TestAgent 调用 shell_exec: ls -la");
    assert!(rx.try_recv().is_err());
}

#[test]
fn renders_tool_call_started_without_summary() {
    use uuid::Uuid;
    use crate::domain::EngineEvent;

    let (fe, mut rx) = make_frontend(FrontendKind::Telegram);
    let task_id = Uuid::nil();
    fe.push_event(EngineEvent::ToolCallStarted {
        target: EventTarget::Directed(vec![ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "u1".to_string(),
            thread_id: None,
        }]),
        task_id,
        agent_name: "Agent".to_string(),
        tool_name: "unknown".to_string(),
        tool_input_summary: String::new(),
    });
    let (_, msg) = rx.try_recv().expect("one outbound message");
    assert_eq!(msg.content, "[00000000] 🔧 Agent 调用 unknown");
}
```

- [ ] **步骤 2：运行测试验证失败**

运行：`export PATH="/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin:/usr/local/bin"; cargo test --lib channels::frontend -- renders_tool_call_started 2>&1 | tail -20`

预期：FAIL（ToolCallStarted 未被处理，`_ => {}` 兜底）

- [ ] **步骤 3：新增 ToolCallStarted 渲染分支**

在 `src/channels/frontend.rs` 的 `push_event` 方法中，`TaskStatusChanged` 分支之后、`_ => {}` 之前，新增：

```rust
EngineEvent::ToolCallStarted {
    target,
    task_id,
    agent_name,
    tool_name,
    tool_input_summary,
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
    let content = if tool_input_summary.is_empty() {
        format!(
            "[{}] 🔧 {} 调用 {}",
            task_short_id(task_id),
            agent_name,
            tool_name
        )
    } else {
        format!(
            "[{}] 🔧 {} 调用 {}: {}",
            task_short_id(task_id),
            agent_name,
            tool_name,
            tool_input_summary
        )
    };
    for channel_id in recipients {
        let msg = ChannelOutboundMessage {
            recipient: channel_id.user_id,
            thread_id: channel_id.thread_id,
            content: content.clone(),
            parse_mode: None,
            reply_markup: None,
            attachments: vec![],
        };
        self.send_message(msg);
    }
}
```

- [ ] **步骤 4：运行测试验证通过**

运行：`export PATH="/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin:/usr/local/bin"; cargo test --lib channels::frontend -- renders_tool_call_started 2>&1 | tail -20`

预期：PASS

- [ ] **步骤 5：编写失败的测试——TaskStatusChanged 扩展渲染**

在 `src/channels/frontend.rs` 测试模块新增测试：

```rust
#[test]
fn renders_task_status_change_with_name_and_agent() {
    use uuid::Uuid;
    use crate::domain::EngineEvent;

    let (fe, mut rx) = make_frontend(FrontendKind::Telegram);
    let task_id = Uuid::parse_str("a1b2c3d4-1111-2222-3333-444444444444").unwrap();
    fe.push_event(EngineEvent::TaskStatusChanged {
        target: EventTarget::Directed(vec![ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "u1".to_string(),
            thread_id: None,
        }]),
        task_id,
        name: "build feature".to_string(),
        status: TaskStatusKind::Done,
        old_status: Some(TaskStatusKind::Running),
        result: None,
        parent_id: None,
        origin_channel: None,
        agent_name: Some("TestAgent".to_string()),
        waiting_reason: None,
    });
    let (_, msg) = rx.try_recv().expect("one outbound message");
    assert_eq!(msg.content, "[a1b2c3d4] build feature: 运行中 → 已完成 @TestAgent");
}
```

- [ ] **步骤 6：运行测试验证失败**

运行：`export PATH="/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin:/usr/local/bin"; cargo test --lib channels::frontend -- renders_task_status_change_with_name_and_agent 2>&1 | tail -20`

预期：FAIL（当前格式不含 task name 与 agent name）

- [ ] **步骤 7：修改 TaskStatusChanged 渲染**

在 `src/channels/frontend.rs` 的 `TaskStatusChanged` 分支（约行 183-225），修改 `status_text` 构造：

```rust
EngineEvent::TaskStatusChanged {
    target,
    task_id,
    name,
    status,
    old_status,
    agent_name,
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
    let task_name = if name.len() > 30 {
        format!("{}…", &name[..30])
    } else {
        name
    };
    let transition = match old_status {
        Some(old) => format!("{} → {}", status_label(old), status_label(status)),
        None => status_label(status).to_string(),
    };
    let mut content = format!("[{}] {}: {}", task_short_id(task_id), task_name, transition);
    if let Some(ref agent) = agent_name {
        content.push_str(&format!(" @{agent}"));
    }
    for channel_id in recipients {
        let msg = ChannelOutboundMessage {
            recipient: channel_id.user_id,
            thread_id: channel_id.thread_id,
            content: content.clone(),
            parse_mode: None,
            reply_markup: None,
            attachments: vec![],
        };
        self.send_message(msg);
    }
}
```

- [ ] **步骤 8：运行测试验证通过**

运行：`export PATH="/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin:/usr/local/bin"; cargo test --lib channels::frontend 2>&1 | tail -20`

预期：所有 channels::frontend 测试 PASS

- [ ] **步骤 9：Commit**

```bash
git add src/channels/frontend.rs
git commit -m "feat: IM 通道扩展 TaskStatusChanged 渲染并新增 ToolCallStarted 渲染"
```

---

### 任务 6：修复现有测试 fixture

**文件：**
- 修改：`src/systems/frontend_output.rs` 测试模块
- 修改：`src/tui/mod.rs` 测试模块
- 修改：`src/channels/frontend.rs` 测试模块
- 修改：其他构造 `TaskStatusChanged` 的测试文件

- [ ] **步骤 1：编译查找所有需修复的 fixture**

运行：`export PATH="/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin:/usr/local/bin"; cargo build --tests 2>&1 | grep "missing field" | head -30`

预期：列出所有构造 `TaskStatusChanged` 缺少 `agent_name` / `waiting_reason` 的位置。

- [ ] **步骤 2：逐个文件补充缺失字段**

对每个编译错误指出的位置，在 `TaskStatusChanged` 构造中补充：

```rust
agent_name: None,
waiting_reason: None,
```

涉及文件（基于探索报告，可能不完全）：
- `src/systems/frontend_output.rs` 测试模块（多个测试构造该事件）
- `src/tui/mod.rs` 测试模块（2 个测试构造该事件）
- `src/channels/frontend.rs` 测试模块（多个测试构造该事件）

- [ ] **步骤 3：运行编译验证**

运行：`export PATH="/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin:/usr/local/bin"; cargo build --tests 2>&1 | tail -10`

预期：编译通过

- [ ] **步骤 4：运行全部测试**

运行：`export PATH="/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin:/usr/local/bin"; cargo test 2>&1 | tail -30`

预期：所有测试 PASS

- [ ] **步骤 5：Commit**

```bash
git add -A
git commit -m "test: 修复 TaskStatusChanged 测试 fixture 补充新字段"
```

---

### 任务 7：格式化与 clippy 检查

**文件：** 无修改（仅验证）

- [ ] **步骤 1：cargo fmt**

运行：`export PATH="/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin:/usr/local/bin"; cargo fmt --all`

- [ ] **步骤 2：cargo fmt 检查**

运行：`export PATH="/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin:/usr/local/bin"; cargo fmt --all --check`

预期：无输出（格式正确）

- [ ] **步骤 3：cargo clippy**

运行：`export PATH="/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin:/usr/local/bin"; cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -20`

预期：无 warning

- [ ] **步骤 4：如有 fmt/clippy 修复，Commit**

```bash
git add -A
git commit -m "style: fmt 与 clippy 修复"
```

若无需修改则跳过此步骤。
