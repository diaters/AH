# /clear 命令实现计划

> **面向 AI 代理的工作者：** 必需子技能：使用 superpowers:subagent-driven-development（推荐）或 superpowers:executing-plans 逐任务实现此计划。步骤使用复选框（`- [ ]`）语法来跟踪进度。

**目标：** 新增 `/clear` slash command，直接 despawn 当前 task 而不触发任何终态下游操作（摘要、经验收集、hook 派发等）。

**架构：** 复用 `/finish` → `FinishTaskMessage` → `finish_task_system` 的消息模式，新增 `ClearCurrentTask` → `ClearTaskMessage` → `clear_task_system`。despawn 不触发 `Changed<Task>`，自然绕开下游链路。清理逻辑（shell sessions、ToolCallingState）集中在 `clear_task_system` 中，使用 `despawn_task` 中心封装维护 `EntityIndex` 一致性。

**技术栈：** Rust, Bevy ECS

---

## 文件结构

| 文件 | 职责 |
|---|---|
| `src/domain/command.rs` | `UserCommand` 枚举 + `parse` 方法，新增 `ClearCurrentTask` 变体 |
| `src/domain/message.rs` | 新增 `ClearTaskMessage` 消息类型 |
| `src/domain/mod.rs` | 导出 `ClearTaskMessage` |
| `src/systems/command.rs` | `command_parse_system` 处理 `ClearCurrentTask` 分支 |
| `src/systems/transform/task_lifecycle.rs` | 新增 `clear_task_system` |
| `src/systems/transform/mod.rs` | 导出 `clear_task_system` |
| `src/systems/mod.rs` | 再导出 `clear_task_system` |
| `src/plugins/frontend.rs` | 注册 `clear_task_system` 到 Transform 集 |

---

### 任务 1：命令解析 — 新增 `ClearCurrentTask`

**文件：**
- 修改：`src/domain/command.rs`

- [ ] **步骤 1：编写失败的测试**

在 `src/domain/command.rs` 的 `#[cfg(test)] mod tests` 中新增测试：

```rust
#[test]
fn parse_clear() {
    let cmd = UserCommand::parse("/clear");
    assert_eq!(cmd, UserCommand::ClearCurrentTask);
    assert!(cmd.is_command());
}
```

- [ ] **步骤 2：运行测试验证失败**

运行：`cargo test -p harness parse_clear -- --nocapture`
预期：编译失败，`ClearCurrentTask` 变体不存在

- [ ] **步骤 3：编写最少实现代码**

1. 在 `UserCommand` 枚举中新增变体（在 `FinishCurrentTask` 之后）：

```rust
/// /clear - 删除当前任务（不触发终态处理链路）
ClearCurrentTask,
```

2. 在 `parse` 方法中新增 `/clear` 分支（在 `trimmed == "/finish"` 分支之后）：

```rust
} else if trimmed == "/clear" {
    Self::ClearCurrentTask
}
```

- [ ] **步骤 4：运行测试验证通过**

运行：`cargo test -p harness parse_clear -- --nocapture`
预期：PASS

- [ ] **步骤 5：Commit**

```bash
git add src/domain/command.rs
git commit -m "feat: 新增 UserCommand::ClearCurrentTask 变体及 /clear 解析"
```

---

### 任务 2：消息层 — 新增 `ClearTaskMessage`

**文件：**
- 修改：`src/domain/message.rs`
- 修改：`src/domain/mod.rs`

- [ ] **步骤 1：新增 `ClearTaskMessage`**

在 `src/domain/message.rs` 中 `FinishTaskMessage` 定义之后新增：

```rust
/// /clear 命令触发的任务清除消息
#[derive(Debug, Clone, Component)]
pub struct ClearTaskMessage {
    pub task_id: TaskId,
}
```

- [ ] **步骤 2：导出 `ClearTaskMessage`**

在 `src/domain/mod.rs` 的 `pub use message::{` 块中，在 `FinishTaskMessage,` 之后添加 `ClearTaskMessage,`：

```rust
    CreateTaskMessage, ClearTaskMessage, ExperienceCollectionCompletedMessage, ExternalInput, FinishTaskMessage,
```

- [ ] **步骤 3：运行编译验证**

运行：`cargo check`
预期：编译通过（`ClearTaskMessage` 已定义并导出，但尚无使用方）

- [ ] **步骤 4：Commit**

```bash
git add src/domain/message.rs src/domain/mod.rs
git commit -m "feat: 新增 ClearTaskMessage 消息类型"
```

---

### 任务 3：命令处理 — `command_parse_system` 处理 `ClearCurrentTask`

**文件：**
- 修改：`src/systems/command.rs`

- [ ] **步骤 1：编写失败的测试**

在 `src/systems/command.rs` 的 `#[cfg(test)] mod tests` 中新增测试：

```rust
#[test]
fn clear_command_spawns_clear_task_message() {
    use crate::domain::{ClearTaskMessage, FrontendKind, Task, TaskStatus};

    let mut app = App::new();
    app.insert_resource(MemoryConfig::default());
    app.insert_resource(SharedKnowledgeBase::default());
    app.insert_resource(PendingKnowledgeWriteHooks::default());
    app.add_systems(Update, command_parse_system);

    let channel = ChannelId {
        frontend: FrontendKind::Tui,
        user_id: "test".to_string(),
        thread_id: None,
    };
    let now = chrono::Utc::now();
    let task_id = uuid::Uuid::new_v4();
    app.world_mut().spawn((
        Task {
            id: task_id,
            content: "active task".to_string(),
            creator: uuid::Uuid::nil(),
            delegate: None,
            status: TaskStatus::Running,
            pending_confirmation_id: None,
            input_summary: "test".to_string(),
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
            origin_channel: Some(channel.clone()),
            routing_policy: crate::domain::TaskRoutingPolicy::conversational(channel.clone()),
            last_evaluated_turn: None,
        },
        ShortTermMemory::default(),
    ));

    app.world_mut().spawn(UserInputMessage {
        content: "/clear".to_string(),
        origin_channel: channel,
    });

    app.update();

    let clear_msgs: Vec<&ClearTaskMessage> = app
        .world_mut()
        .query::<&ClearTaskMessage>()
        .iter(app.world())
        .collect();
    assert_eq!(clear_msgs.len(), 1);
    assert_eq!(clear_msgs[0].task_id, task_id);
}
```

- [ ] **步骤 2：运行测试验证失败**

运行：`cargo test -p harness clear_command_spawns_clear_task_message -- --nocapture`
预期：编译失败或运行时 `ClearTaskMessage` 未 spawn

- [ ] **步骤 3：编写实现代码**

在 `src/systems/command.rs` 的 `command_parse_system` 函数中：

1. 在文件顶部的 `use crate::domain::{` 块中添加 `ClearTaskMessage`：

```rust
use crate::domain::{
    ClearTaskMessage, CreateTaskMessage, DispatchHint, DispatchKind, DispatchStrategy, FinishTaskMessage,
    ...
};
```

2. 在 `match cmd` 块中，在 `UserCommand::FinishCurrentTask => { ... }` 之后添加：

```rust
UserCommand::ClearCurrentTask => {
    // /clear - 删除当前任务（不触发终态处理链路）
    let current_task = tasks.iter().find(|(t, _)| {
        !t.status.is_terminal()
            && t.origin_channel == Some(input.origin_channel.clone())
    });

    if let Some((task, _)) = current_task {
        debug!(
            event = "ClearCommandReceived",
            task_id = %task.id,
            task_status = ?task.status,
            task_content = %task.content,
            "clearing current task via /clear command"
        );
        commands.spawn(ClearTaskMessage { task_id: task.id });
    } else {
        debug!(event = "ClearCommandNoTask", "no active task to clear");
    }
    commands.entity(entity).despawn();
}
```

- [ ] **步骤 4：运行测试验证通过**

运行：`cargo test -p harness clear_command_spawns_clear_task_message -- --nocapture`
预期：PASS

- [ ] **步骤 5：Commit**

```bash
git add src/systems/command.rs
git commit -m "feat: command_parse_system 处理 /clear 命令"
```

---

### 任务 4：清理系统 — 新增 `clear_task_system`

**文件：**
- 修改：`src/systems/transform/task_lifecycle.rs`
- 修改：`src/systems/transform/mod.rs`
- 修改：`src/systems/mod.rs`

- [ ] **步骤 1：编写失败的测试**

在 `src/systems/transform/task_lifecycle.rs` 的 `#[cfg(test)] mod tests` 中新增测试：

```rust
#[test]
fn clear_task_system_despawns_task_entity() {
    use crate::ecs::EntityIndex;
    use crate::domain::{ClearTaskMessage, ChannelId, FrontendKind, PreviousTaskStatus, ShortTermMemory, Task, TaskStatus};

    let mut app = App::new();
    app.init_resource::<EntityIndex>();
    app.insert_resource(crate::app::MemoryConfig::default());
    app.insert_resource(crate::systems::NativeProcessBackend::default());
    app.add_systems(Update, clear_task_system);

    let channel = ChannelId {
        frontend: FrontendKind::Tui,
        user_id: "test".to_string(),
        thread_id: None,
    };
    let now = chrono::Utc::now();
    let task_id = uuid::Uuid::new_v4();
    let entity = app.world_mut().spawn((
        Task {
            id: task_id,
            content: "to clear".to_string(),
            creator: uuid::Uuid::nil(),
            delegate: None,
            status: TaskStatus::Running,
            pending_confirmation_id: None,
            input_summary: "test".to_string(),
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
            origin_channel: Some(channel),
            routing_policy: crate::domain::TaskRoutingPolicy::conversational(ChannelId {
                frontend: FrontendKind::Tui,
                user_id: "test".to_string(),
                thread_id: None,
            }),
            last_evaluated_turn: None,
        },
        ShortTermMemory::default(),
        PreviousTaskStatus(TaskStatus::Pending),
    )).id();

    // 写入 EntityIndex
    app.world_mut()
        .resource_mut::<EntityIndex>()
        .tasks
        .insert(task_id, entity);

    // Spawn ClearTaskMessage
    app.world_mut().spawn(ClearTaskMessage { task_id });

    app.update();

    // Task entity 应被 despawn
    assert!(
        app.world().get::<Task>(entity).is_none(),
        "task entity should be despawned after clear_task_system"
    );
    // EntityIndex 映射应被清除
    assert!(
        app.world().resource::<EntityIndex>().get_task(&task_id).is_none(),
        "EntityIndex mapping should be removed after clear_task_system"
    );
    // ClearTaskMessage 应被 despawn
    let remaining: Vec<_> = app
        .world_mut()
        .query::<&ClearTaskMessage>()
        .iter(app.world())
        .collect();
    assert!(remaining.is_empty(), "ClearTaskMessage should be despawned");
}

#[test]
fn clear_task_system_does_not_spawn_task_terminated_message() {
    use crate::ecs::EntityIndex;
    use crate::domain::{ClearTaskMessage, ChannelId, FrontendKind, PreviousTaskStatus, ShortTermMemory, Task, TaskStatus, TaskTerminatedMessage};

    let mut app = App::new();
    app.init_resource::<EntityIndex>();
    app.insert_resource(crate::app::MemoryConfig::default());
    app.insert_resource(crate::systems::NativeProcessBackend::default());
    app.add_systems(Update, (clear_task_system, task_termination_system));

    let channel = ChannelId {
        frontend: FrontendKind::Tui,
        user_id: "test".to_string(),
        thread_id: None,
    };
    let now = chrono::Utc::now();
    let task_id = uuid::Uuid::new_v4();
    let entity = app.world_mut().spawn((
        Task {
            id: task_id,
            content: "to clear".to_string(),
            creator: uuid::Uuid::nil(),
            delegate: None,
            status: TaskStatus::Running,
            pending_confirmation_id: None,
            input_summary: "test".to_string(),
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
            origin_channel: Some(channel),
            routing_policy: crate::domain::TaskRoutingPolicy::conversational(ChannelId {
                frontend: FrontendKind::Tui,
                user_id: "test".to_string(),
                thread_id: None,
            }),
            last_evaluated_turn: None,
        },
        ShortTermMemory::default(),
        PreviousTaskStatus(TaskStatus::Pending),
    )).id();

    app.world_mut()
        .resource_mut::<EntityIndex>()
        .tasks
        .insert(task_id, entity);

    app.world_mut().spawn(ClearTaskMessage { task_id });

    app.update();

    // 不应产出 TaskTerminatedMessage
    let terminated: Vec<_> = app
        .world_mut()
        .query::<&TaskTerminatedMessage>()
        .iter(app.world())
        .collect();
    assert!(
        terminated.is_empty(),
        "/clear should not spawn TaskTerminatedMessage"
    );
}

#[test]
fn clear_task_system_does_not_spawn_summarization_request() {
    use crate::ecs::EntityIndex;
    use crate::domain::{ClearTaskMessage, ChannelId, FrontendKind, PreviousTaskStatus, ShortTermMemory, SummarizationRequestMessage, Task, TaskStatus};

    let mut app = App::new();
    app.init_resource::<EntityIndex>();
    app.insert_resource(crate::app::MemoryConfig::default());
    app.insert_resource(crate::systems::NativeProcessBackend::default());
    app.add_systems(Update, (clear_task_system, task_termination_system));

    let channel = ChannelId {
        frontend: FrontendKind::Tui,
        user_id: "test".to_string(),
        thread_id: None,
    };
    let now = chrono::Utc::now();
    let task_id = uuid::Uuid::new_v4();

    // 创建带非空 STM 的 task
    let mut stm = ShortTermMemory::default();
    stm.add_entry(
        crate::domain::EntryRole::User,
        "some content to summarize",
        crate::domain::EntryMetadata::default(),
    );
    let entity = app.world_mut().spawn((
        Task {
            id: task_id,
            content: "to clear".to_string(),
            creator: uuid::Uuid::nil(),
            delegate: None,
            status: TaskStatus::Running,
            pending_confirmation_id: None,
            input_summary: "test".to_string(),
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
            origin_channel: Some(channel),
            routing_policy: crate::domain::TaskRoutingPolicy::conversational(ChannelId {
                frontend: FrontendKind::Tui,
                user_id: "test".to_string(),
                thread_id: None,
            }),
            last_evaluated_turn: None,
        },
        stm,
        PreviousTaskStatus(TaskStatus::Pending),
    )).id();

    app.world_mut()
        .resource_mut::<EntityIndex>()
        .tasks
        .insert(task_id, entity);

    app.world_mut().spawn(ClearTaskMessage { task_id });

    app.update();

    // 不应产出 SummarizationRequestMessage
    let summarize: Vec<_> = app
        .world_mut()
        .query::<&SummarizationRequestMessage>()
        .iter(app.world())
        .collect();
    assert!(
        summarize.is_empty(),
        "/clear should not spawn SummarizationRequestMessage"
    );
}

#[test]
fn clear_task_does_not_affect_other_channel() {
    use crate::domain::{ClearTaskMessage, ChannelId, FrontendKind, Task, TaskStatus};

    let mut app = App::new();
    app.init_resource::<crate::ecs::EntityIndex>();
    app.insert_resource(crate::app::MemoryConfig::default());
    app.insert_resource(crate::systems::NativeProcessBackend::default());
    app.add_systems(Update, clear_task_system);

    let qq_channel = ChannelId {
        frontend: FrontendKind::QQ,
        user_id: "qq-user".to_string(),
        thread_id: None,
    };
    let tg_channel = ChannelId {
        frontend: FrontendKind::Telegram,
        user_id: "tg-user".to_string(),
        thread_id: None,
    };
    let now = chrono::Utc::now();
    let qq_task_id = uuid::Uuid::new_v4();
    let qq_entity = app.world_mut().spawn((
        Task {
            id: qq_task_id,
            content: "qq task".to_string(),
            creator: uuid::Uuid::nil(),
            delegate: None,
            status: TaskStatus::Running,
            pending_confirmation_id: None,
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
            origin_channel: Some(qq_channel),
            routing_policy: crate::domain::TaskRoutingPolicy::conversational(ChannelId {
                frontend: FrontendKind::QQ,
                user_id: "qq-user".to_string(),
                thread_id: None,
            }),
            last_evaluated_turn: None,
        },
        crate::domain::ShortTermMemory::default(),
        crate::domain::PreviousTaskStatus(TaskStatus::Pending),
    )).id();

    app.world_mut()
        .resource_mut::<crate::ecs::EntityIndex>()
        .tasks
        .insert(qq_task_id, qq_entity);

    // Telegram 通道的 /clear 不应影响 QQ 任务
    app.world_mut().spawn(ClearTaskMessage { task_id: qq_task_id });

    // 使用正确的 task_id 清除（测试通道隔离应通过 command_parse_system 而非 clear_task_system）
    // 此处测试 clear_task_system 本身只按 task_id 操作，通道隔离由 command_parse_system 保证

    app.update();

    // QQ task 应被清除（因为 ClearTaskMessage 直接指定了 task_id）
    // 通道隔离测试在 command_parse_system 层面
    assert!(
        app.world().get::<Task>(qq_entity).is_none(),
        "task specified by ClearTaskMessage should be despawned"
    );
}
```

- [ ] **步骤 2：运行测试验证失败**

运行：`cargo test -p harness clear_task_system_despawns -- --nocapture`
预期：编译失败，`clear_task_system` 不存在

- [ ] **步骤 3：编写实现代码**

在 `src/systems/transform/task_lifecycle.rs` 中：

1. 在文件顶部 `use` 块添加 `ClearTaskMessage`：

```rust
use crate::{
    app::{Clock, MemoryConfig},
    contracts::SessionBackend,
    domain::{
        ClearTaskMessage, FailureReason, FinishTaskMessage, PreviousTaskStatus, RetryReadyMessage, ShortTermMemory,
        SubTaskConfig, SummarizationRequestMessage, SummarizationTrigger, Task, TaskStatus,
        TaskTerminatedMessage, ToolCallingState, ToolExecutionRequestMessage, WaitingReason,
    },
    ecs::EntityIndex,
    systems::NativeProcessBackend,
};
```

2. 在 `finish_task_system` 函数之后添加 `clear_task_system`：

```rust
/// 清除任务 System
///
/// 处理 /clear 命令，直接 despawn task entity 及其附属组件，
/// 不触发终态处理链路（摘要、经验收集、hook 派发等）。
pub fn clear_task_system(
    mut commands: Commands,
    mut index: ResMut<EntityIndex>,
    messages: Query<(Entity, &ClearTaskMessage)>,
    calling_states: Query<(Entity, &ToolCallingState)>,
    backend: Res<NativeProcessBackend>,
) {
    for (entity, msg) in &messages {
        // 停止关联 shell sessions
        match backend.stop_task_sessions(msg.task_id) {
            Ok(stopped_sessions) => {
                if !stopped_sessions.is_empty() {
                    debug!(
                        event = "TaskShellSessionsStopped",
                        task_id = %msg.task_id,
                        stopped_sessions = ?stopped_sessions,
                        "stopped active shell sessions on /clear"
                    );
                }
            }
            Err(e) => {
                debug!(
                    event = "TaskShellSessionsStopFailed",
                    task_id = %msg.task_id,
                    error = %e,
                    "failed to stop shell sessions on /clear"
                );
            }
        }

        // Despawn 关联的 ToolCallingState
        for (cs_entity, cs) in &calling_states {
            if cs.task_id == msg.task_id {
                debug!(
                    event = "ToolCallingStateCleared",
                    task_id = %msg.task_id,
                    iteration = cs.iteration,
                    "despawning ToolCallingState on /clear"
                );
                commands.entity(cs_entity).despawn();
            }
        }

        debug!(
            event = "TaskCleared",
            task_id = %msg.task_id,
            "clearing task via /clear command (no termination hooks)"
        );

        // 使用中心封装 despawn task（同步维护 EntityIndex）
        crate::ecs::despawn_task(&mut commands, &mut index, msg.task_id);

        commands.entity(entity).despawn();
    }
}
```

- [ ] **步骤 4：更新模块导出**

1. 在 `src/systems/transform/mod.rs` 的 `pub use task_lifecycle::{` 块中添加 `clear_task_system`：

```rust
pub use task_lifecycle::{
    clear_task_system, finish_task_system, init_previous_task_status_system, retry_ready_system,
    task_termination_system, tool_calling_turn_reset_system,
};
```

2. 在 `src/systems/mod.rs` 的 `pub(crate) use transform::{` 块中添加 `clear_task_system`：

```rust
pub(crate) use transform::{
    brain_decision_system, chat_round_block_system, chat_round_completion_system,
    chat_session_cleanup_system, clear_task_system, finish_task_system, ingest_execution_results_system,
    ...
};
```

- [ ] **步骤 5：运行测试验证通过**

运行：`cargo test -p harness clear_task_system -- --nocapture`
预期：全部 PASS

- [ ] **步骤 6：Commit**

```bash
git add src/systems/transform/task_lifecycle.rs src/systems/transform/mod.rs src/systems/mod.rs
git commit -m "feat: 新增 clear_task_system 实现 /clear 命令的静默删除"
```

---

### 任务 5：系统注册 — 将 `clear_task_system` 注册到 app

**文件：**
- 修改：`src/plugins/frontend.rs`

- [ ] **步骤 1：注册系统**

在 `src/plugins/frontend.rs` 中：

1. 在 `use crate::systems::{` 块中添加 `clear_task_system`：

```rust
use crate::systems::{
    HarnessSet, clear_task_system, command_parse_system, continue_task_system, finish_task_system,
    ...
};
```

2. 在 `FrontendPlugin::build` 的 `add_systems(Update, (...))` 中，在 `finish_task_system` 块之后添加：

```rust
// 任务清除（/clear，不触发终态处理链路）
clear_task_system
    .in_set(HarnessSet::Transform)
    .after(command_parse_system),
```

- [ ] **步骤 2：运行编译验证**

运行：`cargo check`
预期：编译通过

- [ ] **步骤 3：运行全量测试**

运行：`cargo test --all-features`
预期：全部 PASS

- [ ] **步骤 4：Commit**

```bash
git add src/plugins/frontend.rs
git commit -m "feat: 注册 clear_task_system 到 FrontendPlugin"
```

---

### 任务 6：通道隔离测试 — `/clear` 不影响其他通道

**文件：**
- 修改：`src/systems/command.rs`

- [ ] **步骤 1：编写失败的测试**

在 `src/systems/command.rs` 的 `#[cfg(test)] mod tests` 中新增测试：

```rust
#[test]
fn clear_does_not_clear_other_channel_task() {
    use crate::domain::{ClearTaskMessage, FrontendKind, Task, TaskStatus};

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
            pending_confirmation_id: None,
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
            origin_channel: Some(qq_channel.clone()),
            routing_policy: crate::domain::TaskRoutingPolicy::conversational(qq_channel),
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
        content: "/clear".to_string(),
        origin_channel: tg_channel,
    });

    app.update();

    // 断言：未生成 ClearTaskMessage
    let clear_count = app
        .world_mut()
        .query::<&ClearTaskMessage>()
        .iter(app.world())
        .count();
    assert_eq!(clear_count, 0, "Telegram /clear should not touch QQ task");
}
```

- [ ] **步骤 2：运行测试验证通过**

运行：`cargo test -p harness clear_does_not_clear_other_channel_task -- --nocapture`
预期：PASS（`command_parse_system` 已在任务 3 中实现通道隔离逻辑，`find` 使用 `origin_channel` 过滤）

- [ ] **步骤 3：Commit**

```bash
git add src/systems/command.rs
git commit -m "test: 新增 /clear 通道隔离测试"
```

---

### 任务 7：前端同步 — TUI 移除已清除任务

**背景：** `clear_task_system` 直接 despawn task entity，但 TUI 的任务列表仅通过 `TaskStatusChanged` 增改、仅通过终态清理移除，因此 `/clear` 后 TUI 会残留已清除任务的展示（任务面板 + 等待指示器）。

**文件：**
- 修改：`src/domain/frontend.rs`
- 修改：`src/systems/transform/task_lifecycle.rs`
- 修改：`src/tui/app.rs`

- [x] **步骤 1：新增 `EngineEvent::TaskCleared`**

在 `src/domain/frontend.rs` 的 `EngineEvent` 枚举中新增：

```rust
/// 任务被 /clear 移除，前端应同步移除对应展示
TaskCleared {
    target: EventTarget,
    task_id: TaskId,
},
```

并在 `target()` 方法中补充分支 `Self::TaskCleared { target, .. } => target`。

- [x] **步骤 2：`clear_task_system` 推送移除事件**

`clear_task_system` 新增 `registry: Res<FrontendRegistry>` 与 `tasks: Query<&Task>`，在 `despawn_task` 之前根据 `routing_policy.output_channel` 构造 `EngineEvent::TaskCleared` 并推送给所有前端（IM 通道的 `push_event` 已有 `_ => {}` 兜底，自动忽略）。

- [x] **步骤 3：TUI 处理 `TaskCleared`**

`src/tui/app.rs` 的 `handle_engine_event` 新增分支：从 `self.tasks` 移除该 task 及其子任务（`parent_id == task_id`）。

- [x] **步骤 4：更新与新增测试**

- 既有 3 个 `clear_task_system` 单测补充 `FrontendRegistry` resource
- 新增 `clear_task_system_pushes_task_cleared_event`（MockFrontend 捕获事件，断言 `target` 指向任务路由通道）
- 新增 `task_cleared_removes_task_and_children`（TUI 侧）

- [x] **步骤 5：运行测试与静态检查**

运行：`cargo test --all-features`、`cargo clippy --all-targets --all-features -- -D warnings`、`cargo fmt --all --check`
预期：全部 PASS

- [x] **步骤 6：Commit**

```bash
git add src/domain/frontend.rs src/systems/transform/task_lifecycle.rs src/tui/app.rs docs/superpowers/specs/2026-07-31-clear-command-design.md
git commit -m "feat: /clear 后推送 EngineEvent::TaskCleared，TUI 同步移除残留任务展示"
```

---

### 任务 8：全量验证

- [ ] **步骤 1：运行全量测试**

运行：`cargo test --all-features`
预期：全部 PASS

- [ ] **步骤 2：运行 clippy**

运行：`cargo clippy --all-targets --all-features -- -D warnings`
预期：无 warning

- [ ] **步骤 3：运行格式检查**

运行：`cargo fmt --all --check`
预期：无差异

- [ ] **步骤 4：最终 Commit（如有格式修正）**

```bash
cargo fmt
git add -A
git commit -m "chore: 格式修正"  # 仅在有变更时
```
