# 通道隔离修复 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 修复 Harness 中跨通道接管 bug，使不同通道的任务与命令作用域相互隔离，子任务正确继承父任务的 `origin_channel`。

**Architecture:** 在路由系统、命令解析系统、子任务编排系统的过滤条件中追加 `origin_channel` 等值比较；将硬编码的 `ChannelId { Tui, "default" }` 替换为输入或父任务的 `origin_channel`。不引入新抽象，仅复用已派生 `PartialEq`/`Eq` 的 `ChannelId`。

**Tech Stack:** Rust, Bevy ECS, tracing。

**Spec:** `docs/superpowers/specs/2026-06-29-channel-isolation-fix.md`

## Global Constraints

- 语言：Rust，遵循官方风格指南
- 架构：Bevy ECS
- 错误处理：库 crate 使用 `thiserror`，应用使用 `anyhow`
- 中文撰写项目文档，可夹杂必要英文术语
- 遵循 Conventional Commits：`feat`/`fix`/`refactor`/`test`/`docs`
- 通过分支和 PR 合并代码，禁止直接推送 `main`
- 单元测试与实现文件放在一起，使用 `#[cfg(test)]`
- 集成测试放在 `tests/` 目录
- CI 检查项：`markdownlint`、`cargo fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`cargo test --all-features`
- `ChannelId` 已派生 `PartialEq`/`Eq`/`Hash`，可直接 `==` 比较
- `Task::origin_channel: ChannelId` 字段已存在于 `src/domain/task.rs:55`
- `UserInputMessage.origin_channel: ChannelId` 字段已存在于 `src/domain/message.rs:114`
- 不在 `ContinueTaskMessage`、`ToolConfirmationResponseMessage` 上新增字段
- 不修改 `frontend_output_system`、`ToolConfirmationRequestMessage` 结构、Signal 事件触发路径

---

## File Structure

修改文件清单：

- **Modify**: `src/systems/routing.rs` — 在 `user_input_routing_system` 的 `waiting_tasks` 过滤条件追加 `origin_channel` 比较；新增内嵌 `#[cfg(test)]` 模块覆盖跨通道与同通道分支
- **Modify**: `src/systems/command.rs` — 在 `/btw`、`/finish`、`/summarize` 的任务查找条件追加 `origin_channel` 比较；将 `/btw` 子任务与回退 `CreateTaskMessage` 的硬编码 `ChannelId` 替换为 `input.origin_channel.clone()`
- **Modify**: `src/systems/tools/orchestrator.rs` — 给 `spawn_create_tasks_messages` 增加 `parent_origin_channel: ChannelId` 参数；在 `handle_tool_action` 的 `CreateBatch` 分支从父任务实体取出 `origin_channel` 传入
- **Modify**: `src/user_plugins/dispatcher.rs` — 仅在 `WorldCommand::CreateTask` 分支追加注释说明设计意图
- **Create**: `tests/cross_channel_isolation.rs` — 新增集成测试覆盖跨通道隔离场景与子任务继承场景
- **Modify**: `/Users/diater/.trae-cn/memory/projects/-Users-diater-workspace-Harness/project_memory.md` — 在 `Lessons Learned` 追加本次 bug 模式

每个文件职责单一：routing 负责路由决策，command 负责命令解析，orchestrator 负责子任务编排，dispatcher 负责插件 host API。修改面贴合现有结构，无文件拆分。

---

### Task 1: 路由系统加入通道过滤

**Files:**
- Modify: `src/systems/routing.rs:13-64`（`user_input_routing_system`）
- Test: `src/systems/routing.rs`（新增 `#[cfg(test)] mod tests`）

**Interfaces:**
- Consumes: `crate::domain::{ChannelId, UserInputMessage, Task, TaskStatus, WaitingReason}`（已存在）
- Produces: `user_input_routing_system` 行为变更 — `Waiting(User)` 任务过滤增加 `t.origin_channel == input.origin_channel` 条件；其余签名不变

- [ ] **Step 1: 在 `src/systems/routing.rs` 末尾追加测试模块与失败测试**

在文件末尾追加：

```rust
#[cfg(test)]
mod tests {
    use bevy::prelude::*;

    use super::user_input_routing_system;
    use crate::domain::{
        ChannelId, ContinueTaskMessage, CreateTaskMessage, FrontendKind, Task, TaskStatus,
        UserInputMessage, WaitingReason,
    };

    fn telegram_channel() -> ChannelId {
        ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "tg-user".to_string(),
            thread_id: None,
        }
    }

    fn qq_channel() -> ChannelId {
        ChannelId {
            frontend: FrontendKind::QQ,
            user_id: "qq-user".to_string(),
            thread_id: None,
        }
    }

    fn make_waiting_task(channel: ChannelId) -> Task {
        let now = chrono::Utc::now();
        Task {
            id: uuid::Uuid::new_v4(),
            content: "waiting".to_string(),
            creator: uuid::Uuid::nil(),
            delegate: None,
            status: TaskStatus::Waiting(WaitingReason::User),
            input_summary: String::new(),
            result_summary: String::new(),
            priority: 0,
            created_at: now,
            updated_at: now,
            retry_count: 0,
            max_retries: 3,
            next_retry_at: None,
            last_error: None,
            multi_turn: true,
            parent_task_id: None,
            batch_id: None,
            origin_channel: channel,
            last_evaluated_turn: None,
        }
    }

    #[test]
    fn cross_channel_input_not_routed_to_other_channel_waiting_task() {
        let mut app = App::new();
        app.add_systems(Update, user_input_routing_system);

        // Telegram 通道的 Waiting(User) 任务
        app.world_mut().spawn(make_waiting_task(telegram_channel()));

        // QQ 通道的纯文本输入
        app.world_mut().spawn(UserInputMessage {
            content: "hello from QQ".to_string(),
            origin_channel: qq_channel(),
        });

        app.update();

        // 断言：应生成 CreateTaskMessage（而非 ContinueTaskMessage）
        let create_count = app
            .world()
            .query::<&CreateTaskMessage>()
            .iter(app.world())
            .count();
        let continue_count = app
            .world()
            .query::<&ContinueTaskMessage>()
            .iter(app.world())
            .count();
        assert_eq!(create_count, 1, "QQ input should create new task, not continue Telegram task");
        assert_eq!(continue_count, 0, "no ContinueTaskMessage should be spawned");
    }

    #[test]
    fn same_channel_input_routed_to_waiting_task() {
        let mut app = App::new();
        app.add_systems(Update, user_input_routing_system);

        let task = make_waiting_task(telegram_channel());
        let task_id = task.id;
        app.world_mut().spawn(task);

        app.world_mut().spawn(UserInputMessage {
            content: "hello from Telegram".to_string(),
            origin_channel: telegram_channel(),
        });

        app.update();

        let continue_msgs: Vec<&ContinueTaskMessage> = app
            .world()
            .query::<&ContinueTaskMessage>()
            .iter(app.world())
            .collect();
        assert_eq!(continue_msgs.len(), 1);
        assert_eq!(continue_msgs[0].task_id, task_id);
    }
}
```

- [ ] **Step 2: 运行测试验证失败**

Run: `cargo test --lib --features= -- user_input_routing_system::tests --nocapture`

Expected: FAIL — `cross_channel_input_not_routed_to_other_channel_waiting_task` 断言失败（`create_count == 0, continue_count == 1`），因为当前过滤条件未比较通道。

- [ ] **Step 3: 修改 `user_input_routing_system` 的过滤条件**

在 `src/systems/routing.rs:25-28`，将：

```rust
        let waiting_tasks: Vec<_> = tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Waiting(WaitingReason::User))
            .collect();
```

替换为：

```rust
        let waiting_tasks: Vec<_> = tasks
            .iter()
            .filter(|t| {
                t.status == TaskStatus::Waiting(WaitingReason::User)
                    && t.origin_channel == input.origin_channel
            })
            .collect();
```

- [ ] **Step 4: 在 `continue_existing` 分支的 debug! 中追加通道字段**

在 `src/systems/routing.rs:31-40`，将：

```rust
            debug!(
                event = "RoutingDecision",
                decision = "continue_existing",
                input = %input.content,
                input_len = input.content.len(),
                selected_task_id = %task.id,
                waiting_tasks_count = waiting_tasks.len(),
                waiting_tasks = ?waiting_tasks.iter().map(|t| (t.id, t.status.clone())).collect::<Vec<_>>(),
                "routing input to existing Waiting(User) task"
            );
```

替换为：

```rust
            debug!(
                event = "RoutingDecision",
                decision = "continue_existing",
                input = %input.content,
                input_len = input.content.len(),
                selected_task_id = %task.id,
                waiting_tasks_count = waiting_tasks.len(),
                input_channel = ?input.origin_channel,
                task_channel = ?task.origin_channel,
                waiting_tasks = ?waiting_tasks.iter().map(|t| (t.id, t.status.clone())).collect::<Vec<_>>(),
                "routing input to existing Waiting(User) task"
            );
```

- [ ] **Step 5: 运行测试验证通过**

Run: `cargo test --lib -- user_input_routing_system::tests --nocapture`

Expected: PASS — 两个测试均通过。

- [ ] **Step 6: 运行回归测试**

Run: `cargo test --all-features --test multi_turn_routing --test origin_channel_flow --test frontend_routing`

Expected: PASS — 既有测试无回归（同通道内行为不变）。

- [ ] **Step 7: 提交**

```bash
git add src/systems/routing.rs
git commit -m "$(cat <<'EOF'
fix: route user input only to Waiting(User) tasks in the same channel

user_input_routing_system now compares task.origin_channel with
input.origin_channel before routing. Prevents cross-channel takeover
where a Telegram Waiting(User) task would absorb TUI/QQ inputs.
EOF
)"
```

---

### Task 2: `/btw` 命令加入通道过滤与 `origin_channel` 继承

**Files:**
- Modify: `src/systems/command.rs:34-80`（`UserCommand::NewTask` 分支）
- Test: `src/systems/command.rs`（追加到现有 `#[cfg(test)] mod tests`）

**Interfaces:**
- Consumes: `crate::domain::{ChannelId, FrontendKind, Task, TaskStatus, UserInputMessage}`（已存在）
- Produces: `/btw` 命令行为变更 — 父任务查找增加通道过滤；子任务与回退 `CreateTaskMessage` 使用 `input.origin_channel`

- [ ] **Step 1: 在 `src/systems/command.rs` 测试模块追加失败测试**

在文件末尾 `}` 之前的 `mod tests` 内追加：

```rust
    #[test]
    fn btw_picks_parent_only_in_same_channel() {
        use crate::domain::{FrontendKind, Task, TaskStatus};
        use bevy::ecs::world::CommandQueue;

        let mut app = App::new();
        app.insert_resource(MemoryConfig::default());
        app.insert_resource(SharedKnowledgeBase::default());
        app.insert_resource(PendingKnowledgeWriteHooks::default());
        app.add_systems(Update, command_parse_system);

        // QQ 通道的活跃任务
        let qq_channel = ChannelId {
            frontend: FrontendKind::QQ,
            user_id: "qq-user".to_string(),
            thread_id: None,
        };
        let now = chrono::Utc::now();
        app.world_mut().spawn((
            Task {
                id: uuid::Uuid::new_v4(),
                content: "qq active task".to_string(),
                creator: uuid::Uuid::nil(),
                delegate: None,
                status: TaskStatus::Ready,
                input_summary: "qq".to_string(),
                result_summary: String::new(),
                priority: 0,
                created_at: now,
                updated_at: now,
                retry_count: 0,
                max_retries: 3,
                next_retry_at: None,
                last_error: None,
                multi_turn: false,
                parent_task_id: None,
                batch_id: None,
                origin_channel: qq_channel.clone(),
                last_evaluated_turn: None,
            },
            ShortTermMemory::default(),
        ));

        // Telegram 通道发起 /btw
        let tg_channel = ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "tg-user".to_string(),
            thread_id: None,
        };
        app.world_mut().spawn(UserInputMessage {
            content: "/btw new topic".to_string(),
            origin_channel: tg_channel.clone(),
        });

        app.update();

        // 断言：无父任务，走 CreateTaskMessage 分支
        let create_msgs: Vec<&CreateTaskMessage> = app
            .world()
            .query::<&CreateTaskMessage>()
            .iter(app.world())
            .collect();
        assert_eq!(create_msgs.len(), 1, "Telegram /btw with no Telegram parent should fall back to CreateTaskMessage");
        assert_eq!(create_msgs[0].origin_channel, tg_channel);
    }

    #[test]
    fn btw_subtask_inherits_input_origin_channel() {
        use crate::domain::{FrontendKind, Task, TaskStatus};

        let mut app = App::new();
        app.insert_resource(MemoryConfig::default());
        app.insert_resource(SharedKnowledgeBase::default());
        app.insert_resource(PendingKnowledgeWriteHooks::default());
        app.add_systems(Update, command_parse_system);

        // QQ 通道的活跃父任务
        let qq_channel = ChannelId {
            frontend: FrontendKind::QQ,
            user_id: "qq-user".to_string(),
            thread_id: None,
        };
        let now = chrono::Utc::now();
        app.world_mut().spawn((
            Task {
                id: uuid::Uuid::new_v4(),
                content: "qq parent".to_string(),
                creator: uuid::Uuid::nil(),
                delegate: None,
                status: TaskStatus::Ready,
                input_summary: "qq".to_string(),
                result_summary: String::new(),
                priority: 0,
                created_at: now,
                updated_at: now,
                retry_count: 0,
                max_retries: 3,
                next_retry_at: None,
                last_error: None,
                multi_turn: false,
                parent_task_id: None,
                batch_id: None,
                origin_channel: qq_channel.clone(),
                last_evaluated_turn: None,
            },
            ShortTermMemory::default(),
        ));

        app.world_mut().spawn(UserInputMessage {
            content: "/btw child topic".to_string(),
            origin_channel: qq_channel.clone(),
        });

        app.update();

        // 断言：新创建的子任务（非 Pending 状态前会先 spawn 为 Task 实体）继承 QQ 通道
        let new_tasks: Vec<&Task> = app
            .world()
            .query::<&Task>()
            .iter(app.world())
            .filter(|t| t.content == "child topic" || t.input_summary == "child topic")
            .collect();
        // /btw 子任务使用 topic 作为 content（若 topic 为空则使用 input.content）
        let child_task = app
            .world()
            .query::<&Task>()
            .iter(app.world())
            .find(|t| t.content == "child topic");
        assert!(child_task.is_some(), "should spawn child task with topic as content");
        assert_eq!(
            child_task.unwrap().origin_channel, qq_channel,
            "child task should inherit input origin_channel"
        );
    }
```

- [ ] **Step 2: 运行测试验证失败**

Run: `cargo test --lib -- command::tests::btw_picks_parent_only_in_same_channel command::tests::btw_subtask_inherits_input_origin_channel --nocapture`

Expected: FAIL — `btw_picks_parent_only_in_same_channel` 断言失败（找到 QQ 父任务，未走 CreateTaskMessage）；`btw_subtask_inherits_input_origin_channel` 断言失败（子任务 origin_channel == Tui/default）。

- [ ] **Step 3: 修改 `/btw` 父任务查找条件**

在 `src/systems/command.rs:37-39`，将：

```rust
                let parent_task = tasks
                    .iter()
                    .find(|(t, _)| !t.status.is_terminal() && t.status != TaskStatus::Pending);
```

替换为：

```rust
                let parent_task = tasks
                    .iter()
                    .find(|(t, _)| {
                        !t.status.is_terminal()
                            && t.status != TaskStatus::Pending
                            && t.origin_channel == input.origin_channel
                    });
```

- [ ] **Step 4: 修改 `/btw` 子任务 `origin_channel` 使用 `input.origin_channel`**

在 `src/systems/command.rs:50-62`，将：

```rust
                    let child_task = Task::from_user_input(
                        if topic.is_empty() {
                            &input.content
                        } else {
                            &topic
                        },
                        parent.max_retries,
                        ChannelId {
                            frontend: FrontendKind::Tui,
                            user_id: "default".to_string(),
                            thread_id: None,
                        },
                    );
```

替换为：

```rust
                    let child_task = Task::from_user_input(
                        if topic.is_empty() {
                            &input.content
                        } else {
                            &topic
                        },
                        parent.max_retries,
                        input.origin_channel.clone(),
                    );
```

- [ ] **Step 5: 修改 `/btw` 回退 `CreateTaskMessage` 的 `origin_channel`**

在 `src/systems/command.rs:71-78`，将：

```rust
                    commands.spawn(CreateTaskMessage {
                        content: input.content.clone(),
                        origin_channel: crate::domain::ChannelId {
                            frontend: crate::domain::FrontendKind::Tui,
                            user_id: "default".to_string(),
                            thread_id: None,
                        },
                    });
```

替换为：

```rust
                    commands.spawn(CreateTaskMessage {
                        content: input.content.clone(),
                        origin_channel: input.origin_channel.clone(),
                    });
```

- [ ] **Step 6: 运行测试验证通过**

Run: `cargo test --lib -- command::tests::btw_picks_parent_only_in_same_channel command::tests::btw_subtask_inherits_input_origin_channel --nocapture`

Expected: PASS — 两个测试均通过。

- [ ] **Step 7: 运行 command 模块全部测试回归**

Run: `cargo test --lib -- command::tests --nocapture`

Expected: PASS — 既有命令解析测试无回归。

- [ ] **Step 8: 提交**

```bash
git add src/systems/command.rs
git commit -m "$(cat <<'EOF'
fix: scope /btw command to the issuing channel

- /btw parent task lookup now filters by origin_channel
- /btw child task and fallback CreateTaskMessage use input.origin_channel
  instead of hardcoded Tui/default
EOF
)"
```

---

### Task 3: `/finish` 与 `/summarize` 加入通道过滤

**Files:**
- Modify: `src/systems/command.rs:82-99`（`/finish`）
- Modify: `src/systems/command.rs:100-144`（`/summarize`）
- Test: `src/systems/command.rs`（追加到 `#[cfg(test)] mod tests`）

**Interfaces:**
- Consumes: `crate::domain::{Task, TaskStatus, UserInputMessage, ChannelId}`（已存在）
- Produces: `/finish` 与 `/summarize` 行为变更 — 任务查找增加 `origin_channel` 比较

- [ ] **Step 1: 在 `src/systems/command.rs` 测试模块追加失败测试**

在 `mod tests` 内追加：

```rust
    #[test]
    fn finish_does_not_finish_other_channel_task() {
        use crate::domain::{FinishTaskMessage, FrontendKind, Task, TaskStatus};

        let mut app = App::new();
        app.insert_resource(MemoryConfig::default());
        app.insert_resource(SharedKnowledgeBase::default());
        app.insert_resource(PendingKnowledgeWriteHooks::default());
        app.add_systems(Update, command_parse_system);

        let qq_channel = ChannelId {
            frontend: FrontendKind::QQ,
            user_id: "qq-user".to_string(),
            thread_id: None,
        };
        let now = chrono::Utc::now();
        app.world_mut().spawn((
            Task {
                id: uuid::Uuid::new_v4(),
                content: "qq active task".to_string(),
                creator: uuid::Uuid::nil(),
                delegate: None,
                status: TaskStatus::Ready,
                input_summary: "qq".to_string(),
                result_summary: String::new(),
                priority: 0,
                created_at: now,
                updated_at: now,
                retry_count: 0,
                max_retries: 3,
                next_retry_at: None,
                last_error: None,
                multi_turn: false,
                parent_task_id: None,
                batch_id: None,
                origin_channel: qq_channel,
                last_evaluated_turn: None,
            },
            ShortTermMemory::default(),
        ));

        let tg_channel = ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "tg-user".to_string(),
            thread_id: None,
        };
        app.world_mut().spawn(UserInputMessage {
            content: "/finish".to_string(),
            origin_channel: tg_channel,
        });

        app.update();

        // 断言：未生成 FinishTaskMessage
        let finish_count = app
            .world()
            .query::<&FinishTaskMessage>()
            .iter(app.world())
            .count();
        assert_eq!(finish_count, 0, "Telegram /finish should not touch QQ task");
    }

    #[test]
    fn summarize_does_not_summarize_other_channel_task() {
        use crate::domain::{FrontendKind, SummarizationRequestMessage, Task, TaskStatus};

        let mut app = App::new();
        app.insert_resource(MemoryConfig::default());
        app.insert_resource(SharedKnowledgeBase::default());
        app.insert_resource(PendingKnowledgeWriteHooks::default());
        app.add_systems(Update, command_parse_system);

        let qq_channel = ChannelId {
            frontend: FrontendKind::QQ,
            user_id: "qq-user".to_string(),
            thread_id: None,
        };
        let now = chrono::Utc::now();
        let mut stm = ShortTermMemory::default();
        stm.add_entry(
            crate::domain::EntryRole::User,
            "some content long enough",
            crate::domain::EntryMetadata::default(),
        );
        app.world_mut().spawn((
            Task {
                id: uuid::Uuid::new_v4(),
                content: "qq active task".to_string(),
                creator: uuid::Uuid::nil(),
                delegate: None,
                status: TaskStatus::Ready,
                input_summary: "qq".to_string(),
                result_summary: String::new(),
                priority: 0,
                created_at: now,
                updated_at: now,
                retry_count: 0,
                max_retries: 3,
                next_retry_at: None,
                last_error: None,
                multi_turn: false,
                parent_task_id: None,
                batch_id: None,
                origin_channel: qq_channel,
                last_evaluated_turn: None,
            },
            stm,
        ));

        let tg_channel = ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "tg-user".to_string(),
            thread_id: None,
        };
        app.world_mut().spawn(UserInputMessage {
            content: "/summarize".to_string(),
            origin_channel: tg_channel,
        });

        app.update();

        // 断言：未生成 SummarizationRequestMessage
        let summarize_count = app
            .world()
            .query::<&SummarizationRequestMessage>()
            .iter(app.world())
            .count();
        assert_eq!(summarize_count, 0, "Telegram /summarize should not touch QQ task");
    }
```

- [ ] **Step 2: 运行测试验证失败**

Run: `cargo test --lib -- command::tests::finish_does_not_finish_other_channel_task command::tests::summarize_does_not_summarize_other_channel_task --nocapture`

Expected: FAIL — 两个测试断言失败（`finish_count == 1`、`summarize_count == 1`），因为当前查找无通道过滤。

- [ ] **Step 3: 修改 `/finish` 任务查找条件**

在 `src/systems/command.rs:84`，将：

```rust
                let current_task = tasks.iter().find(|(t, _)| !t.status.is_terminal());
```

替换为：

```rust
                let current_task = tasks
                    .iter()
                    .find(|(t, _)| !t.status.is_terminal() && t.origin_channel == input.origin_channel);
```

- [ ] **Step 4: 修改 `/summarize` 任务查找条件**

在 `src/systems/command.rs:102`，将：

```rust
                let active_task = tasks.iter().find(|(t, _)| !t.status.is_terminal());
```

替换为：

```rust
                let active_task = tasks
                    .iter()
                    .find(|(t, _)| !t.status.is_terminal() && t.origin_channel == input.origin_channel);
```

- [ ] **Step 5: 运行测试验证通过**

Run: `cargo test --lib -- command::tests::finish_does_not_finish_other_channel_task command::tests::summarize_does_not_summarize_other_channel_task --nocapture`

Expected: PASS — 两个测试均通过。

- [ ] **Step 6: 运行 command 模块全部测试回归**

Run: `cargo test --lib -- command::tests --nocapture`

Expected: PASS — 所有命令解析测试无回归。

- [ ] **Step 7: 提交**

```bash
git add src/systems/command.rs
git commit -m "$(cat <<'EOF'
fix: scope /finish and /summarize to the issuing channel

Both commands now filter active tasks by origin_channel, preventing
cross-channel task termination or summarization.
EOF
)"
```

---

### Task 4: `create_tasks` 子任务继承父任务 `origin_channel`

**Files:**
- Modify: `src/systems/tools/orchestrator.rs:95-100`（`spawn_create_tasks_messages` 签名）
- Modify: `src/systems/tools/orchestrator.rs:141-145`（子任务 `origin_channel` 字段）
- Modify: `src/systems/tools/orchestrator.rs:451-461`（`handle_tool_action` 的 `CreateBatch` 分支调用方）
- Test: `tests/cross_channel_isolation.rs`（在 Task 6 中新增）

**Interfaces:**
- Consumes: `crate::domain::{ChannelId, FrontendKind, Task}`、`bevy::prelude::Commands`、`tasks: &mut Query<(Entity, &mut Task)>`（已存在于 `handle_tool_action`）
- Produces: `spawn_create_tasks_messages` 新签名 — 增加 `parent_origin_channel: ChannelId` 参数；`handle_tool_action` 的 `CreateBatch` 分支在调用前从父任务实体取出 `origin_channel` 传入

- [ ] **Step 1: 修改 `spawn_create_tasks_messages` 签名与子任务 `origin_channel`**

在 `src/systems/tools/orchestrator.rs:95-100`，将：

```rust
pub fn spawn_create_tasks_messages(
    commands: &mut Commands,
    request_entity: Entity,
    agent_id: AgentId,
    task_id: TaskId,
    request_kind: crate::domain::AgentRequestKind,
    definitions: Vec<SubTaskDefinition>,
    tool_call_id: Option<String>,
) {
```

替换为：

```rust
pub fn spawn_create_tasks_messages(
    commands: &mut Commands,
    request_entity: Entity,
    agent_id: AgentId,
    task_id: TaskId,
    request_kind: crate::domain::AgentRequestKind,
    definitions: Vec<SubTaskDefinition>,
    tool_call_id: Option<String>,
    parent_origin_channel: ChannelId,
) {
```

然后在 `src/systems/tools/orchestrator.rs:141-145`，将：

```rust
            origin_channel: ChannelId {
                frontend: FrontendKind::Tui,
                user_id: "default".to_string(),
                thread_id: None,
            },
```

替换为：

```rust
            origin_channel: parent_origin_channel.clone(),
```

- [ ] **Step 2: 修改 `handle_tool_action` 的 `CreateBatch` 分支调用方**

在 `src/systems/tools/orchestrator.rs:451-461`，将：

```rust
        Ok(ToolAction::CreateBatch(definitions)) => {
            spawn_create_tasks_messages(
                commands,
                request_entity,
                request.request.agent_id,
                request.request.task_id,
                request.request.request_kind.clone(),
                definitions,
                request.tool_call_id.clone(),
            );
        }
```

替换为：

```rust
        Ok(ToolAction::CreateBatch(definitions)) => {
            let parent_origin_channel = tasks
                .get(task_entity)
                .map(|(_, t)| t.origin_channel.clone())
                .unwrap_or_else(|_| {
                    warn!(
                        event = "ParentTaskNotFoundForSubTaskChannel",
                        task_entity = ?task_entity,
                        task_id = %request.request.task_id,
                        "parent task entity not found, falling back to Tui/default for sub-task origin_channel"
                    );
                    ChannelId {
                        frontend: FrontendKind::Tui,
                        user_id: "default".to_string(),
                        thread_id: None,
                    }
                });
            spawn_create_tasks_messages(
                commands,
                request_entity,
                request.request.agent_id,
                request.request.task_id,
                request.request.request_kind.clone(),
                definitions,
                request.tool_call_id.clone(),
                parent_origin_channel,
            );
        }
```

- [ ] **Step 3: 修改 `use tracing::debug;` 引入 `warn`**

在 `src/systems/tools/orchestrator.rs:7`，将：

```rust
use tracing::debug;
```

替换为：

```rust
use tracing::{debug, warn};
```

- [ ] **Step 4: 编译验证**

Run: `cargo build --all-features`

Expected: BUILD SUCCESS — 无编译错误。

- [ ] **Step 5: 运行 orchestrator 相关测试回归**

Run: `cargo test --all-features --test multi_agent_flow --test llm_tool_calling_flow --test brain_dispatch_flow`

Expected: PASS — 既有测试无回归。父任务的 `origin_channel` 在这些测试中是 `default_channel()`，子任务继承后也是 `default_channel()`，行为一致。

- [ ] **Step 6: 提交**

```bash
git add src/systems/tools/orchestrator.rs
git commit -m "$(cat <<'EOF'
fix: create_tasks subtasks inherit parent task origin_channel

spawn_create_tasks_messages now takes parent_origin_channel parameter.
handle_tool_action reads it from the parent task entity before spawning
subtasks, so child task output and approval requests route to the
correct channel.
EOF
)"
```

---

### Task 5: 插件 dispatcher 加注释说明

**Files:**
- Modify: `src/user_plugins/dispatcher.rs:206-216`（`WorldCommand::CreateTask` 分支）

**Interfaces:**
- Consumes: 无新依赖
- Produces: 无行为变更，仅追加注释

- [ ] **Step 1: 在 `apply_world_command` 的 `CreateTask` 分支追加注释**

在 `src/user_plugins/dispatcher.rs:206-216`，将：

```rust
        WorldCommand::CreateTask { title, parent: _ } => {
            // Task 无 metadata 字段，Task::new 也不存在，使用 from_user_input
            // 走与用户消息相同的多轮 Pending 路径，origin_channel 标记为 plugin 来源。
            let channel = ChannelId {
                frontend: FrontendKind::Tui,
                user_id: "plugin".to_string(),
                thread_id: None,
            };
            let task = Task::from_user_input(title, 0, channel);
            world.spawn((task, crate::domain::ShortTermMemory::default()));
        }
```

替换为：

```rust
        WorldCommand::CreateTask { title, parent: _ } => {
            // 插件创建的任务不属于任何 IM 通道，使用 Tui/plugin 标识其来源。
            // 这是有意为之：插件通过 host API 创建的任务不绑定到具体用户会话，
            // 因此不参与通道隔离过滤（与 Tui/default 通道也不冲突）。
            let channel = ChannelId {
                frontend: FrontendKind::Tui,
                user_id: "plugin".to_string(),
                thread_id: None,
            };
            let task = Task::from_user_input(title, 0, channel);
            world.spawn((task, crate::domain::ShortTermMemory::default()));
        }
```

- [ ] **Step 2: 编译验证**

Run: `cargo build --all-features`

Expected: BUILD SUCCESS — 仅注释变更。

- [ ] **Step 3: 提交**

```bash
git add src/user_plugins/dispatcher.rs
git commit -m "$(cat <<'EOF'
docs: clarify plugin CreateTask channel design intent

Add comment explaining that plugin-created tasks intentionally use
Tui/plugin channel since they don't bind to any IM user session.
EOF
)"
```

---

### Task 6: 新增跨通道隔离集成测试

**Files:**
- Create: `tests/cross_channel_isolation.rs`

**Interfaces:**
- Consumes: `harness::{build_harness_app, ChannelId, ExternalInput, FrontendKind, Task, TaskStatus, WaitingReason, ShortTermMemory, HarnessConfig, AgentExecutor, AgentExecutionRequest, AgentExecutionOutput, ExecutorFuture, OutputContent, channels::ChannelManager}`
- Produces: 新增集成测试文件 `tests/cross_channel_isolation.rs`

- [ ] **Step 1: 创建集成测试文件**

创建 `tests/cross_channel_isolation.rs`：

```rust
use std::sync::Arc;

use bevy::prelude::*;
use crossbeam_channel::unbounded;
use harness::{
    AgentExecutionOutput, AgentExecutionRequest, AgentExecutor, ChannelId, ChannelManager,
    ExecutorFuture, ExternalInput, FrontendKind, HarnessConfig, OutputContent, ShortTermMemory,
    Task, TaskStatus, WaitingReason, build_harness_app,
};
use tokio::runtime::Runtime;

fn telegram_channel() -> ChannelId {
    ChannelId {
        frontend: FrontendKind::Telegram,
        user_id: "tg-user".to_string(),
        thread_id: None,
    }
}

fn qq_channel() -> ChannelId {
    ChannelId {
        frontend: FrontendKind::QQ,
        user_id: "qq-user".to_string(),
        thread_id: None,
    }
}

struct EchoExecutor;

impl AgentExecutor for EchoExecutor {
    fn execute(&self, _request: AgentExecutionRequest) -> ExecutorFuture {
        Box::pin(async move {
            Ok(AgentExecutionOutput {
                content: OutputContent::Text("echo".to_string()),
                reasoning_content: None,
            })
        })
    }
}

fn test_config() -> HarnessConfig {
    HarnessConfig::default()
}

fn make_waiting_task(channel: ChannelId) -> Task {
    let now = chrono::Utc::now();
    Task {
        id: uuid::Uuid::new_v4(),
        content: "waiting".to_string(),
        creator: uuid::Uuid::nil(),
        delegate: None,
        status: TaskStatus::Waiting(WaitingReason::User),
        input_summary: String::new(),
        result_summary: String::new(),
        priority: 0,
        created_at: now,
        updated_at: now,
        retry_count: 0,
        max_retries: 3,
        next_retry_at: None,
        last_error: None,
        multi_turn: true,
        parent_task_id: None,
        batch_id: None,
        origin_channel: channel,
        last_evaluated_turn: None,
    }
}

#[test]
fn cross_channel_plain_text_does_not_takeover_waiting_task() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(EchoExecutor);
    let (input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(
        test_config(),
        runtime,
        executor,
        input_rx,
        vec![],
        ChannelManager::empty().0,
    );

    app.update();

    // Telegram 通道的 Waiting(User) 任务
    let tg_task_id = uuid::Uuid::new_v4();
    let mut tg_task = make_waiting_task(telegram_channel());
    tg_task.id = tg_task_id;
    app.world_mut().spawn((tg_task, ShortTermMemory::default()));

    // 从 QQ 通道发送纯文本
    input_tx
        .send(ExternalInput::TextWithChannel {
            channel: qq_channel(),
            content: "hello from QQ".to_string(),
        })
        .unwrap();

    for _ in 0..5 {
        app.update();
    }

    // 断言：QQ 输入创建了新任务，Telegram 任务仍处于 Waiting(User)
    let tasks: Vec<&Task> = app.world().query::<&Task>().iter(app.world()).collect();
    let tg_task = tasks
        .iter()
        .find(|t| t.id == tg_task_id)
        .expect("Telegram task should still exist");
    assert_eq!(
        tg_task.status,
        TaskStatus::Waiting(WaitingReason::User),
        "Telegram task should still be Waiting(User), not taken over by QQ input"
    );

    let qq_tasks: Vec<&Task> = tasks
        .iter()
        .filter(|t| t.origin_channel == qq_channel())
        .collect();
    assert!(
        !qq_tasks.is_empty(),
        "QQ input should create a new task in QQ channel"
    );
}

#[test]
fn cross_channel_btw_does_not_pick_other_channel_parent() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(EchoExecutor);
    let (input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(
        test_config(),
        runtime,
        executor,
        input_rx,
        vec![],
        ChannelManager::empty().0,
    );

    app.update();

    // QQ 通道的活跃任务
    let now = chrono::Utc::now();
    app.world_mut().spawn((
        Task {
            id: uuid::Uuid::new_v4(),
            content: "qq active".to_string(),
            creator: uuid::Uuid::nil(),
            delegate: None,
            status: TaskStatus::Ready,
            input_summary: "qq".to_string(),
            result_summary: String::new(),
            priority: 0,
            created_at: now,
            updated_at: now,
            retry_count: 0,
            max_retries: 3,
            next_retry_at: None,
            last_error: None,
            multi_turn: false,
            parent_task_id: None,
            batch_id: None,
            origin_channel: qq_channel(),
            last_evaluated_turn: None,
        },
        ShortTermMemory::default(),
    ));

    // 从 Telegram 通道发起 /btw
    input_tx
        .send(ExternalInput::TextWithChannel {
            channel: telegram_channel(),
            content: "/btw new topic".to_string(),
        })
        .unwrap();

    for _ in 0..5 {
        app.update();
    }

    // 断言：Telegram 通道无父任务，走 CreateTaskMessage 分支
    let tasks: Vec<&Task> = app.world().query::<&Task>().iter(app.world()).collect();
    let tg_tasks: Vec<&Task> = tasks
        .iter()
        .filter(|t| t.origin_channel == telegram_channel())
        .collect();
    assert!(
        !tg_tasks.is_empty(),
        "Telegram /btw should create a new task in Telegram channel"
    );
    // 新任务的 content 应该是 "new topic"（/btw topic）
    let tg_new_task = tg_tasks
        .iter()
        .find(|t| t.content == "new topic")
        .expect("Telegram task content should be the /btw topic");
    assert_eq!(tg_new_task.origin_channel, telegram_channel());
}

#[test]
fn cross_channel_finish_does_not_finish_other_channel_task() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(EchoExecutor);
    let (input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(
        test_config(),
        runtime,
        executor,
        input_rx,
        vec![],
        ChannelManager::empty().0,
    );

    app.update();

    // QQ 通道的活跃任务
    let qq_task_id = uuid::Uuid::new_v4();
    let now = chrono::Utc::now();
    app.world_mut().spawn((
        Task {
            id: qq_task_id,
            content: "qq active".to_string(),
            creator: uuid::Uuid::nil(),
            delegate: None,
            status: TaskStatus::Ready,
            input_summary: "qq".to_string(),
            result_summary: String::new(),
            priority: 0,
            created_at: now,
            updated_at: now,
            retry_count: 0,
            max_retries: 3,
            next_retry_at: None,
            last_error: None,
            multi_turn: false,
            parent_task_id: None,
            batch_id: None,
            origin_channel: qq_channel(),
            last_evaluated_turn: None,
        },
        ShortTermMemory::default(),
    ));

    // 从 Telegram 通道发起 /finish
    input_tx
        .send(ExternalInput::TextWithChannel {
            channel: telegram_channel(),
            content: "/finish".to_string(),
        })
        .unwrap();

    for _ in 0..5 {
        app.update();
    }

    // 断言：QQ 任务未终结
    let qq_task = app
        .world()
        .query::<&Task>()
        .iter(app.world())
        .find(|t| t.id == qq_task_id)
        .expect("QQ task should still exist");
    assert!(
        !qq_task.status.is_terminal(),
        "QQ task should not be terminated by Telegram /finish"
    );
}
```

- [ ] **Step 2: 运行集成测试验证通过**

Run: `cargo test --all-features --test cross_channel_isolation --nocapture`

Expected: PASS — 三个集成测试均通过。

- [ ] **Step 3: 提交**

```bash
git add tests/cross_channel_isolation.rs
git commit -m "$(cat <<'EOF'
test: add cross-channel isolation integration tests

Verify that:
- Cross-channel plain text does not take over Waiting(User) task
- Cross-channel /btw does not pick other channel's parent task
- Cross-channel /finish does not terminate other channel's task
EOF
)"
```

---

### Task 7: 全量回归测试与记忆同步

**Files:**
- Modify: `/Users/diater/.trae-cn/memory/projects/-Users-diater-workspace-Harness/project_memory.md`

**Interfaces:**
- Consumes: 所有前序任务的修改
- Produces: 通过 CI 全套检查；`project_memory.md` 的 `Lessons Learned` 追加本次 bug 模式

- [ ] **Step 1: 运行 fmt 检查**

Run: `cargo fmt --all --check`

Expected: 无 diff 输出。若有 diff，运行 `cargo fmt --all` 修正后重新检查。

- [ ] **Step 2: 运行 clippy 检查**

Run: `cargo clippy --all-targets --all-features -- -D warnings`

Expected: 无 warning。若有，修正后重新检查。

- [ ] **Step 3: 运行全量测试**

Run: `cargo test --all-features`

Expected: PASS — 所有单元测试、集成测试均通过。

- [ ] **Step 4: 运行 markdownlint 检查**

Run: `markdownlint docs/superpowers/specs/2026-06-29-channel-isolation-fix.md docs/superpowers/plans/2026-06-29-channel-isolation-fix.md`

Expected: 无 lint 错误。

- [ ] **Step 5: 在 `project_memory.md` 的 `Lessons Learned` 末尾追加**

打开 `/Users/diater/.trae-cn/memory/projects/-Users-diater-workspace-Harness/project_memory.md`，在 `## Lessons Learned` 章节末尾追加：

```markdown
- 路由/命令/子任务编排系统的任务查找必须按 `origin_channel` 过滤，否则会导致跨通道接管：来自通道 A 的输入会路由到通道 B 的 Waiting(User) 任务，造成回复通道错乱（2026-06-29 修复，见 docs/superpowers/specs/2026-06-29-channel-isolation-fix.md）
- 子任务的 `origin_channel` 必须从父任务继承，硬编码 `Tui/default` 会导致子任务输出与审批请求路由到错误通道
```

- [ ] **Step 6: 提交**

```bash
git add /Users/diater/.trae-cn/memory/projects/-Users-diater-workspace-Harness/project_memory.md
# 注意：project_memory.md 位于用户 home 目录，不属于本仓库，无需 git add/commit
# 仅作为记忆文件更新，无提交步骤
```

记忆文件无需提交到本仓库。若无其他代码变更，跳过 git commit。

- [ ] **Step 7: 推送分支并创建 PR**

```bash
git push -u origin feat/channel-isolation-fix
```

然后通过 GitHub 创建 PR，标题：`fix: channel isolation for routing, commands, and subtask orchestration`，描述引用 spec 与 plan。

---

## Self-Review

**1. Spec coverage**:
- 修改 1（routing.rs 过滤）→ Task 1 ✓
- 修改 2（/btw 父任务过滤）→ Task 2 ✓
- 修改 3（/btw 子任务与回退 origin_channel）→ Task 2 ✓
- 修改 4（/finish 过滤）→ Task 3 ✓
- 修改 5（/summarize 过滤）→ Task 3 ✓
- 修改 6（spawn_create_tasks_messages 继承）→ Task 4 ✓
- 修改 7（dispatcher 注释）→ Task 5 ✓
- 修改 8（project_memory.md 同步）→ Task 7 ✓
- 单元测试 → Task 1（routing.rs 内嵌）、Task 2（command.rs 内嵌）、Task 3（command.rs 内嵌）✓
- 集成测试 → Task 6 ✓
- CI 检查（fmt/clippy/test/markdownlint）→ Task 7 ✓
- Spec 中"日志增强 input_channel/task_channel"→ Task 1 Step 4 ✓

**2. Placeholder scan**: 无 TBD/TODO/省略代码。所有步骤含完整代码块。

**3. Type consistency**:
- `spawn_create_tasks_messages` 新参数名 `parent_origin_channel: ChannelId` 在 Task 4 Step 1（定义）与 Step 2（调用）一致 ✓
- `warn!` 在 Task 4 Step 2 使用，在 Step 3 通过 `use tracing::{debug, warn};` 引入 ✓
- `tasks.get(task_entity)` 返回 `Result<(Entity, &mut Task), QueryEntityError>`，`.map(|(_, t)| t.origin_channel.clone())` 取 `&mut Task` 的 `origin_channel` 字段 ✓（`&mut Task` 可读字段）
- 测试中 `Task` 字段顺序与 `src/domain/task.rs:33-58` 一致 ✓
- `ChannelId` 与 `FrontendKind` 在测试中通过 `use crate::domain::{...}` 引入 ✓

**4. 借用冲突检查**: `handle_tool_action` 在 `CreateBatch` 分支调用 `tasks.get(task_entity)` 后立即 `.map(...).clone()`，借用在 `spawn_create_tasks_messages` 调用前释放；`spawn_create_tasks_messages` 仅使用 `commands`，不使用 `tasks`，无借用冲突 ✓

**5. 既有测试兼容性**: `multi_turn_routing.rs`、`origin_channel_flow.rs`、`frontend_routing.rs`、`command.rs` 既有测试均使用 `default_channel()`（Tui/default），修改后同通道内行为不变 ✓

无 issues 需要修复。
