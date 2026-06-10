> **状态：已归档（2026-06-10）** — 本计划已执行完毕。
> 相关能力已记录在 [docs/current-state.md](../../current-state.md)。

# TUI 任务层级显示实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在 TUI StatusPanel 中以层级方式显示主任务和子任务，主任务显示子任务完成进度，已完成任务变暗。

**Architecture:** 扩展 TaskState 数据结构，新增子任务计数字段；扩展 EngineEvent::TaskStatusChanged 添加 parent_id；重构 StatusPanel 渲染逻辑实现层级分组显示。

**Tech Stack:** Rust, ratatui (TUI 框架), uuid

---

## 文件结构

| 文件 | 变更类型 | 职责 |
|------|----------|------|
| `src/domain/frontend.rs` | 修改 | EngineEvent::TaskStatusChanged 新增 parent_id 字段 |
| `src/tui/app.rs` | 修改 | TaskState 新增 subtask_count/completed_count 字段，更新 handle_engine_event |
| `src/tui/status.rs` | 修改 | 重构渲染逻辑，实现层级显示和样式 |
| `src/tui/app.rs` (tests) | 修改 | 更新单元测试适配新字段 |

---

## Task 1: 扩展 EngineEvent::TaskStatusChanged

**Files:**
- Modify: `src/domain/frontend.rs:97-103`
- Modify: `src/tui/app.rs:441-460`

- [ ] **Step 1: 更新 EngineEvent::TaskStatusChanged 结构**

在 `src/domain/frontend.rs` 中，为 `TaskStatusChanged` 变体添加 `parent_id` 字段：

```rust
/// Task 状态变化
TaskStatusChanged {
    target: EventTarget,
    task_id: TaskId,
    name: String,
    status: TaskStatusKind,
    result: Option<String>,
    parent_id: Option<TaskId>,  // 新增：父任务 ID
},
```

- [ ] **Step 2: 更新 target() 方法的 match 分支**

确认 `target()` 方法中 `TaskStatusChanged` 的 match 分支无需修改（已使用 `..` 忽略其他字段）。

- [ ] **Step 3: 搜索所有 TaskStatusChanged 的构造点**

运行: `grep -rn "TaskStatusChanged" src/ --include="*.rs"`

预期输出显示所有构造该事件的位置，需逐一更新。

- [ ] **Step 4: 更新所有 TaskStatusChanged 构造点**

为每个构造点添加 `parent_id` 字段，初始值为 `None`（后续由发送方填充实际值）。

- [ ] **Step 5: 更新 App::handle_engine_event 中的处理逻辑**

在 `src/tui/app.rs` 的 `handle_engine_event` 方法中，更新 `TaskStatusChanged` 分支：

```rust
EngineEvent::TaskStatusChanged {
    task_id,
    name,
    status,
    result,
    parent_id,
    ..
} => {
    if let Some(task) = self.tasks.iter_mut().find(|t| t.id == task_id) {
        task.status = status;
        task.result = result;
        task.parent_id = parent_id;
    } else {
        self.tasks.push(TaskState {
            id: task_id,
            name,
            status,
            result,
            parent_id,
        });
    }
}
```

- [ ] **Step 6: 运行测试确保编译通过**

运行: `cargo test --lib`

预期: 所有测试通过，无编译错误

- [ ] **Step 7: 提交变更**

```bash
git add src/domain/frontend.rs src/tui/app.rs
git commit -m "feat(domain): add parent_id to TaskStatusChanged event"
```

---

## Task 2: 扩展 TaskState 数据结构

**Files:**
- Modify: `src/tui/app.rs:39-47`

- [ ] **Step 1: 为 TaskState 添加子任务计数字段**

在 `src/tui/app.rs` 中更新 `TaskState` 结构：

```rust
/// Task 前端状态
#[derive(Debug, Clone)]
pub struct TaskState {
    pub id: uuid::Uuid,
    pub name: String,
    pub status: TaskStatusKind,
    pub result: Option<String>,
    pub parent_id: Option<uuid::Uuid>,
    pub subtask_count: u32,      // 新增：子任务总数
    pub completed_count: u32,    // 新增：已完成子任务数
}
```

- [ ] **Step 2: 更新 TaskState 构造点**

在 `handle_engine_event` 中更新 TaskState 创建：

```rust
self.tasks.push(TaskState {
    id: task_id,
    name,
    status,
    result,
    parent_id,
    subtask_count: 0,      // 新增
    completed_count: 0,    // 新增
});
```

- [ ] **Step 3: 运行测试确保编译通过**

运行: `cargo test --lib`

预期: 所有测试通过

- [ ] **Step 4: 提交变更**

```bash
git add src/tui/app.rs
git commit -m "feat(tui): add subtask count fields to TaskState"
```

---

## Task 3: 实现子任务进度计算

**Files:**
- Modify: `src/tui/app.rs`

- [ ] **Step 1: 在 App 中添加辅助方法计算子任务进度**

在 `impl App` 块中添加方法：

```rust
/// 计算指定主任务的子任务进度
fn calculate_subtask_progress(&self, parent_id: Uuid) -> (u32, u32) {
    let subtasks: Vec<_> = self.tasks.iter()
        .filter(|t| t.parent_id == Some(parent_id))
        .collect();

    let total = subtasks.len() as u32;
    let completed = subtasks.iter()
        .filter(|t| matches!(t.status, TaskStatusKind::Done | TaskStatusKind::Failed))
        .count() as u32;

    (total, completed)
}

/// 更新所有主任务的子任务进度
fn update_all_subtask_progress(&mut self) {
    // 收集主任务 ID
    let main_task_ids: Vec<Uuid> = self.tasks.iter()
        .filter(|t| t.parent_id.is_none())
        .map(|t| t.id)
        .collect();

    // 更新每个主任务的进度
    for id in main_task_ids {
        let (total, completed) = self.calculate_subtask_progress(id);
        if let Some(task) = self.tasks.iter_mut().find(|t| t.id == id) {
            task.subtask_count = total;
            task.completed_count = completed;
        }
    }
}
```

- [ ] **Step 2: 在 handle_engine_event 末尾调用进度更新**

在 `handle_engine_event` 方法的 `TaskStatusChanged` 分支末尾，添加进度更新调用：

```rust
EngineEvent::TaskStatusChanged { .. } => {
    // ... 现有处理逻辑 ...

    // 更新子任务进度
    self.update_all_subtask_progress();
}
```

- [ ] **Step 3: 运行测试确保编译通过**

运行: `cargo test --lib`

预期: 所有测试通过

- [ ] **Step 4: 提交变更**

```bash
git add src/tui/app.rs
git commit -m "feat(tui): implement subtask progress calculation"
```

---

## Task 4: 重构 StatusPanel 渲染逻辑

**Files:**
- Modify: `src/tui/status.rs`

- [ ] **Step 1: 添加辅助函数判断任务是否已完成**

在 `StatusPanel` 中添加：

```rust
fn is_task_completed(status: TaskStatusKind) -> bool {
    matches!(status, TaskStatusKind::Done | TaskStatusKind::Failed)
}
```

- [ ] **Step 2: 添加辅助函数获取已完成颜色**

```rust
fn get_dimmed_color_if_completed(status: TaskStatusKind, base_color: Color) -> Color {
    if is_task_completed(status) {
        Color::DarkGray  // #6272a4 的近似色
    } else {
        base_color
    }
}
```

- [ ] **Step 3: 重构 Task 渲染部分**

替换现有的 Task 渲染循环（第 85-97 行）为层级渲染逻辑：

```rust
// Task 列表（层级显示）
lines.push(Line::from(Span::styled(
    "Tasks",
    Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD),
)));

// 分离主任务和子任务
let main_tasks: Vec<_> = app.tasks.iter()
    .filter(|t| t.parent_id.is_none())
    .collect();

let subtasks_by_parent: std::collections::HashMap<Uuid, Vec<_>> = app.tasks.iter()
    .filter(|t| t.parent_id.is_some())
    .filter_map(|t| {
        t.parent_id.map(|pid| (pid, t))
    })
    .fold(std::collections::HashMap::new(), |mut acc, (pid, task)| {
        acc.entry(pid).or_default().push(task);
        acc
    });

// 渲染主任务及其子任务
for main_task in main_tasks {
    let (icon, color) = match main_task.status {
        TaskStatusKind::Pending => ("○", Color::DarkGray),
        TaskStatusKind::Running => ("●", Color::Yellow),
        TaskStatusKind::Waiting => ("○", Color::Cyan),
        TaskStatusKind::Done => ("✓", Color::Green),
        TaskStatusKind::Failed => ("✗", Color::Red),
    };

    // 主任务颜色（已完成则变暗）
    let main_color = get_dimmed_color_if_completed(main_task.status, color);

    // 构建主任务文本（包含进度）
    let progress_text = if main_task.subtask_count > 0 {
        format!(" ({}/{})", main_task.completed_count, main_task.subtask_count)
    } else {
        String::new()
    };

    lines.push(Line::from(vec![
        Span::styled(format!("{icon} "), Style::default().fg(main_color)),
        Span::styled(
            format!("{}{}", main_task.name, progress_text),
            Style::default().fg(main_color).add_modifier(Modifier::BOLD),
        ),
    ]));

    // 渲染子任务
    if let Some(subtasks) = subtasks_by_parent.get(&main_task.id) {
        for subtask in subtasks {
            let (sub_icon, sub_color) = match subtask.status {
                TaskStatusKind::Pending => ("○", Color::DarkGray),
                TaskStatusKind::Running => ("●", Color::Yellow),
                TaskStatusKind::Waiting => ("○", Color::Cyan),
                TaskStatusKind::Done => ("✓", Color::Green),
                TaskStatusKind::Failed => ("✗", Color::Red),
            };

            // 子任务颜色（已完成则变暗）
            let sub_task_color = get_dimmed_color_if_completed(subtask.status, sub_color);

            // 子任务行：缩进 + 虚线前缀
            lines.push(Line::from(vec![
                Span::styled("  │ ", Style::default().fg(Color::DarkGray)),  // 虚线效果
                Span::styled(format!("{sub_icon} "), Style::default().fg(sub_task_color)),
                Span::styled(&subtask.name, Style::default().fg(sub_task_color)),
            ]));
        }
    }
}
```

- [ ] **Step 4: 添加空状态提示**

在 Task 渲染前添加空状态检查：

```rust
if app.tasks.is_empty() {
    lines.push(Line::from(Span::styled(
        "  No active tasks",
        Style::default().fg(Color::DarkGray),
    )));
}
```

- [ ] **Step 5: 运行测试确保编译通过**

运行: `cargo test --lib`

预期: 所有测试通过

- [ ] **Step 6: 提交变更**

```bash
git add src/tui/status.rs
git commit -m "feat(tui): implement hierarchical task display with progress"
```

---

## Task 5: 更新单元测试

**Files:**
- Modify: `src/tui/app.rs` (tests module)

- [ ] **Step 1: 更新 handle_task_status_adds_task 测试**

在测试中添加新字段的断言：

```rust
#[test]
fn handle_task_status_adds_task() {
    let mut app = test_app();
    let task_id = Uuid::new_v4();
    app.handle_engine_event(EngineEvent::TaskStatusChanged {
        target: EventTarget::Broadcast,
        task_id,
        name: "test task".to_string(),
        status: TaskStatusKind::Running,
        result: None,
        parent_id: None,
    });
    assert_eq!(app.tasks.len(), 1);
    assert_eq!(app.tasks[0].name, "test task");
    assert_eq!(app.tasks[0].parent_id, None);
    assert_eq!(app.tasks[0].subtask_count, 0);
    assert_eq!(app.tasks[0].completed_count, 0);
}
```

- [ ] **Step 2: 添加子任务进度计算测试**

添加新测试：

```rust
#[test]
fn subtask_progress_calculated_correctly() {
    let mut app = test_app();
    let main_id = Uuid::new_v4();
    let sub1_id = Uuid::new_v4();
    let sub2_id = Uuid::new_v4();

    // 添加主任务
    app.handle_engine_event(EngineEvent::TaskStatusChanged {
        target: EventTarget::Broadcast,
        task_id: main_id,
        name: "main task".to_string(),
        status: TaskStatusKind::Running,
        result: None,
        parent_id: None,
    });

    // 添加子任务 1（已完成）
    app.handle_engine_event(EngineEvent::TaskStatusChanged {
        target: EventTarget::Broadcast,
        task_id: sub1_id,
        name: "subtask 1".to_string(),
        status: TaskStatusKind::Done,
        result: None,
        parent_id: Some(main_id),
    });

    // 添加子任务 2（运行中）
    app.handle_engine_event(EngineEvent::TaskStatusChanged {
        target: EventTarget::Broadcast,
        task_id: sub2_id,
        name: "subtask 2".to_string(),
        status: TaskStatusKind::Running,
        result: None,
        parent_id: Some(main_id),
    });

    // 验证主任务进度
    let main_task = app.tasks.iter().find(|t| t.id == main_id).unwrap();
    assert_eq!(main_task.subtask_count, 2);
    assert_eq!(main_task.completed_count, 1);
}
```

- [ ] **Step 3: 运行测试确保全部通过**

运行: `cargo test --lib`

预期: 所有测试通过

- [ ] **Step 4: 提交变更**

```bash
git add src/tui/app.rs
git commit -m "test(tui): add tests for subtask progress calculation"
```

---

## Task 6: 集成测试与验证

**Files:**
- 无新文件

- [ ] **Step 1: 运行完整测试套件**

运行: `cargo test`

预期: 所有测试通过

- [ ] **Step 2: 运行 clippy 检查**

运行: `cargo clippy -- -D warnings`

预期: 无警告

- [ ] **Step 3: 运行格式化检查**

运行: `cargo fmt --check`

预期: 无输出（已格式化）

- [ ] **Step 4: 手动运行 TUI 验证视觉效果**

运行: `cargo run`

验证点：
1. 任务列表按层级显示
2. 主任务显示进度 (n/m)
3. 子任务有缩进和虚线前缀
4. 已完成任务变暗

- [ ] **Step 5: 提交最终变更**

```bash
git add -A
git commit -m "feat(tui): complete hierarchical task display implementation"
```

---

## 验收检查清单

- [ ] 主任务和子任务层级关系清晰可辨
- [ ] 主任务显示正确的子任务进度 (n/m)
- [ ] 已完成任务视觉变暗
- [ ] 不影响现有功能（Agent 显示、审批流程等）
- [ ] 所有测试通过
- [ ] clippy 无警告
- [ ] 代码已格式化
