# 子任务 EntityIndex 登记缺失修复 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 修复 `create_tasks` 绕过 `spawn_task` 中心封装导致的子任务系统性挂死——子任务未登记进 `EntityIndex.tasks`，brain 决策结果被静默丢弃。

**Architecture:** 将 `spawn_create_tasks_messages` 中的 `commands.spawn` 替换为中心 `spawn_task` 封装调用，确保子任务进入 `EntityIndex`；spawn 后立即移除占位 `PendingDispatch`，保持由 `subtask_dispatch_preparation_system` 负责附加正式的 `PendingDispatch`（含 DAG 依赖检查和兄弟任务结果注入）。在 `brain_decision_system` 的 else 分支添加 warn 日志作为防御性措施。

**Tech Stack:** Rust, Bevy ECS

## Global Constraints

- 遵循 Conventional Commits
- 代码遵循项目已有风格（中文注释、tracing 日志）
- 所有 `Task` 实体创建必须经中心 `spawn_task` 封装（`src/ecs/entity_index.rs`）
- `EntityIndex` 的 `tasks` 写入只能通过 `spawn_task` / `despawn_task` 封装

## 关键设计决策

### PendingDispatch 处理策略

`spawn_task` 签名要求 `PendingDispatch` 参数，但子任务的 `PendingDispatch` 应由 `subtask_dispatch_preparation_system` 在 DAG 依赖检查通过后附加（含 `AgentSpawnSpec` 和兄弟任务结果注入）。

**决策**：spawn 时传入占位 `PendingDispatch` 以满足 `spawn_task` 签名，spawn 后立即 `remove::<PendingDispatch>()`，让 `subtask_dispatch_preparation_system` 后续附加正式版本。这样：
1. `EntityIndex.tasks` 得到登记（核心修复）
2. `NewlyCreatedTask` 标记正确附加（触发 `on_task_created` hook）
3. 不干扰 `subtask_dispatch_preparation_system` 的 DAG 依赖检查流程

---

## 文件结构

| 文件 | 职责 | 操作 |
|---|---|---|
| `src/systems/tools/orchestrator.rs` | `spawn_create_tasks_messages` 函数 | **修改** — 核心修复 |
| `src/systems/tools/dispatch.rs` | `tool_dispatch_system` | **修改** — 传递 `ResMut<EntityIndex>` |
| `src/systems/tools/confirmation.rs` | `tool_confirmation_system` | **修改** — 传递 `ResMut<EntityIndex>` |
| `src/systems/transform/brain_decision.rs` | `brain_decision_system` | **修改** — 防御性日志 |

---

### Task 1: 核心修复 — 修改 `spawn_create_tasks_messages` 与调用链

**Files:**
- Modify: `src/systems/tools/orchestrator.rs:50-128`（`spawn_create_tasks_messages` 函数体）
- Modify: `src/systems/tools/orchestrator.rs:354`（`handle_tool_action` 签名）
- Modify: `src/systems/tools/orchestrator.rs:404-431`（`handle_tool_action` 中 CreateBatch 分支）
- Modify: `src/systems/tools/dispatch.rs:59-66`（`Res` → `ResMut`，传递 index）
- Modify: `src/systems/tools/dispatch.rs:282-301`（调用 `handle_tool_action` 传入 index）
- Modify: `src/systems/tools/confirmation.rs:62-69`（`Res` → `ResMut`，传递 index）
- Modify: `src/systems/tools/confirmation.rs:374-393`（调用 `handle_tool_action` 传入 index）

**Interfaces:**
- Consumes: `spawn_task(commands, index, task, stm, NewlyCreatedTask, PendingDispatch) -> Entity` from `src/ecs/entity_index.rs:42-54`
- Produces: `handle_tool_action` 新签名含 `index: &mut EntityIndex` 参数；`spawn_create_tasks_messages` 新签名含 `index: &mut EntityIndex` 参数

- [ ] **Step 1: 修改 `spawn_create_tasks_messages` 签名与实现**

1a. 修改函数签名（`orchestrator.rs:50`），在 `commands` 后增加 `index: &mut EntityIndex`：

```rust
pub fn spawn_create_tasks_messages(
    commands: &mut Commands,
    index: &mut EntityIndex,           // ← 新增
    request_entity: Entity,
    agent_id: AgentId,
    task_id: TaskId,
    request_kind: crate::domain::AgentRequestKind,
    definitions: Vec<SubTaskDefinition>,
    tool_call_id: Option<String>,
    parent_origin_channel: Option<ChannelId>,
) {
```

1b. 替换 `orchestrator.rs:118` 的 `commands.spawn` 为 `spawn_task` + `remove::<PendingDispatch>()` + `insert`：

原代码：
```rust
commands.spawn((child_task, sub_task_config, ShortTermMemory::default()));
```

替换为：
```rust
let child_entity = crate::ecs::spawn_task(
    commands,
    index,
    child_task,
    ShortTermMemory::default(),
    NewlyCreatedTask,
    PendingDispatch {
        kind: DispatchKind::Task,
        hint: DispatchHint {
            strategy: DispatchStrategy::BrainLlm,
            preferred_agent_name: None,
            required_skill_id: None,
            agent_spawn_spec: None,
        },
    },
);
// 移除 spawn_task 附加的占位 PendingDispatch，由 subtask_dispatch_preparation_system
// 在 DAG 依赖检查通过后重新附加（含 AgentSpawnSpec 和兄弟任务结果注入）。
commands.entity(child_entity).remove::<PendingDispatch>();
commands.entity(child_entity).insert(sub_task_config);
```

1c. 在文件顶部 imports 中确保有 `EntityIndex`、`NewlyCreatedTask`、`PendingDispatch`、`DispatchKind`、`DispatchStrategy` 的引用。检查 `orchestrator.rs:1-30` 的现有 imports，按需补充。

- [ ] **Step 2: 修改 `handle_tool_action` 签名**

在 `orchestrator.rs:354`，在 `commands` 参数后添加 `index: &mut EntityIndex`：

```rust
pub fn handle_tool_action<B: SessionBackend>(
    commands: &mut Commands,
    index: &mut EntityIndex,           // ← 新增
    request_entity: Entity,
    task_entity: Entity,
    request: &ToolExecutionRequestMessage,
    action: Result<ToolAction, ToolError>,
    // ... 其余不变
```

- [ ] **Step 3: 修改 `handle_tool_action` 中 CreateBatch 分支**

在 `orchestrator.rs:421-430`，将 `index` 传入 `spawn_create_tasks_messages`：

```rust
spawn_create_tasks_messages(
    commands,
    index,                              // ← 新增
    request_entity,
    request.request.agent_id,
    request.request.task_id,
    request.request.request_kind.clone(),
    definitions,
    request.tool_call_id.clone(),
    parent_origin_channel,
);
```

- [ ] **Step 4: 修改 `tool_dispatch_system` 传递 `ResMut<EntityIndex>`**

4a. 在 `dispatch.rs:59-64`，将 `Res<EntityIndex>` 改为 `ResMut<EntityIndex>`：

```rust
index_clock_loader: (
    ResMut<EntityIndex>,
    Res<Clock>,
    Res<SkillLoader>,
    Res<FrontendRegistry>,
),
```

4b. 在 `dispatch.rs:66`，`index` 变量从 `&Res` 变为 `&ResMut`（Deref 自动适配，只读调用不需要改动）：

```rust
let index = &index_clock_loader.0;
```

4c. 在 `dispatch.rs:282-301`，将 `index` 可变引用传入 `handle_tool_action`：

```rust
handle_tool_action(
    &mut commands,
    &mut index_clock_loader.0,     // ← 新增：可变引用传入
    entity,
    task_entity,
    &request,
    action,
    // ... 其余不变
);
```

注意：`&mut index_clock_loader.0` 在同一 `for` 循环迭代中不能与 `index.get_task()` / `index.get_agent()` 的只读借用同时存在。需检查是否有借用冲突。如有冲突，将只读查询移到 `handle_tool_action` 调用之前（先用局部变量缓存结果）。

- [ ] **Step 5: 修改 `tool_confirmation_system` 传递 `ResMut<EntityIndex>`**

5a. 在 `confirmation.rs:62-67`，将 `Res<EntityIndex>` 改为 `ResMut<EntityIndex>`：

```rust
index_clock_loader_frontends: (
    ResMut<EntityIndex>,
    Res<Clock>,
    Res<SkillLoader>,
    Res<FrontendRegistry>,
),
```

5b. 在 `confirmation.rs:69`，`index` 变量自动适配。

5c. 在 `confirmation.rs:374-393`，将 `index` 可变引用传入 `handle_tool_action`：

```rust
handle_tool_action(
    &mut commands,
    &mut index_clock_loader_frontends.0,   // ← 新增
    request_entity,
    task_entity,
    tool_request,
    action,
    // ... 其余不变
);
```

同样检查借用冲突。

- [ ] **Step 6: 修改已有测试 `spawn_subtasks_for_inheritance_test`**

`orchestrator.rs` 中 `spawn_subtasks_for_inheritance_test` 函数（约行 1520）需要更新签名，增加 `ResMut<EntityIndex>` 参数，并在调用 `spawn_create_tasks_messages` 时传入。

同时在对应测试 `create_tasks_subtask_inherits_parent_origin_channel` 中添加 `app.init_resource::<EntityIndex>();`。

- [ ] **Step 7: 写回归测试**

在 `orchestrator.rs` 的 `mod tests` 中添加测试，验证子任务被登记进 `EntityIndex`：

```rust
/// 验证 spawn_create_tasks_messages 将子任务登记进 EntityIndex。
///
/// 回归保护：子任务曾因直接 commands.spawn 绕过中心封装，
/// 导致 EntityIndex.tasks 中查无子任务，brain_decision_system 静默丢弃决策结果。
#[test]
fn create_tasks_subtask_registered_in_entity_index() {
    use crate::ecs::EntityIndex;

    let mut app = App::new();
    app.init_resource::<EntityIndex>();

    let parent_task_id = uuid::Uuid::new_v4();
    let now = chrono::Utc::now();
    let parent_task = Task {
        id: parent_task_id,
        content: "parent".to_string(),
        creator: uuid::Uuid::nil(),
        delegate: None,
        status: TaskStatus::Pending,
        pending_confirmation_id: None,
        input_summary: String::new(),
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
        origin_channel: Some(ChannelId {
            frontend: FrontendKind::Tui,
            user_id: "default".to_string(),
            thread_id: None,
        }),
        routing_policy: crate::domain::TaskRoutingPolicy::conversational(ChannelId {
            frontend: FrontendKind::Tui,
            user_id: "default".to_string(),
            thread_id: None,
        }),
        last_evaluated_turn: None,
    };
    app.world_mut().spawn((parent_task, ShortTermMemory::default()));

    app.add_systems(Update, spawn_subtasks_for_index_test);
    app.update();

    // 验证子任务在 EntityIndex 中
    let index = app.world().resource::<EntityIndex>();
    let child_tasks: Vec<_> = app
        .world_mut()
        .query::<&Task>()
        .iter(app.world())
        .filter(|t| t.parent_task_id == Some(parent_task_id))
        .collect();

    assert_eq!(
        child_tasks.len(),
        1,
        "exactly one child task should be spawned"
    );

    let child_task_id = child_tasks[0].id;
    assert!(
        index.get_task(&child_task_id).is_some(),
        "child task {} must be registered in EntityIndex.tasks",
        child_task_id
    );
}

/// 测试用系统：调用 spawn_create_tasks_messages 并传入 EntityIndex。
fn spawn_subtasks_for_index_test(
    mut commands: Commands,
    mut index: ResMut<EntityIndex>,
    tasks: Query<&Task>,
) {
    let parent_task = tasks
        .iter()
        .find(|t| t.content == "parent")
        .expect("parent task should exist");
    let parent_task_id = parent_task.id;
    let parent_origin_channel = parent_task.origin_channel.clone();

    let request_entity = commands.spawn(()).id();

    spawn_create_tasks_messages(
        &mut commands,
        &mut index,
        request_entity,
        uuid::Uuid::nil(),
        parent_task_id,
        AgentRequestKind::LlmCompletion,
        vec![SubTaskDefinition {
            name: "child-agent".to_string(),
            content: "do something".to_string(),
            tools: vec![],
            depends_on: vec![],
            model: None,
        }],
        None,
        parent_origin_channel,
    );
}
```

- [ ] **Step 8: 运行编译检查**

Run: `cargo check -p harness 2>&1 | tail -30`
Expected: 编译通过

- [ ] **Step 9: 运行受影响的测试**

Run: `cargo test -p harness create_tasks_subtask -- --nocapture 2>&1 | tail -20`
Expected: 所有测试 PASS

- [ ] **Step 10: 提交**

```bash
git add src/systems/tools/orchestrator.rs src/systems/tools/dispatch.rs src/systems/tools/confirmation.rs
git commit -m "fix(subtask): register child tasks in EntityIndex via spawn_task

Previously, spawn_create_tasks_messages used commands.spawn directly,
bypassing the central spawn_task wrapper. This caused child tasks to
never be registered in EntityIndex.tasks, so brain_decision_system
silently dropped their brain decisions (index.get_task returned None),
leaving all sub-tasks permanently stuck at Waiting(Agent).

Now uses spawn_task to ensure index registration + NewlyCreatedTask
marker, then removes the placeholder PendingDispatch so
subtask_dispatch_preparation_system can attach the proper one after
DAG dependency checks.

Both tool_dispatch_system and tool_confirmation_system are updated to
hold ResMut<EntityIndex> and forward it to handle_tool_action."
```

---

### Task 2: 在 `brain_decision_system` 的 else 分支添加防御性 warn 日志

**Files:**
- Modify: `src/systems/transform/brain_decision.rs:65-71`

**Interfaces:**
- Consumes: 无
- Produces: 无新接口

- [ ] **Step 1: 在 else 分支添加 warn 日志**

将 `brain_decision.rs:65-71` 从：

```rust
let Some((task_entity, mut task, awaiting)) = index
    .get_task(&result.task_id)
    .and_then(|e| tasks.get_mut(e).ok())
else {
    commands.entity(entity).despawn();
    continue;
};
```

改为：

```rust
let Some((task_entity, mut task, awaiting)) = index
    .get_task(&result.task_id)
    .and_then(|e| tasks.get_mut(e).ok())
else {
    warn!(
        event = "BrainDecisionDroppedTaskNotFound",
        task_id = %result.task_id,
        "brain decision result dropped: task not found in EntityIndex"
    );
    commands.entity(entity).despawn();
    continue;
};
```

- [ ] **Step 2: 运行编译和测试**

Run: `cargo test -p harness brain_decision -- --nocapture 2>&1 | tail -20`
Expected: 编译通过，已有测试 PASS

- [ ] **Step 3: 提交**

```bash
git add src/systems/transform/brain_decision.rs
git commit -m "fix(brain): add warn log when brain decision dropped due to missing index entry

Previously, if a task was not found in EntityIndex, the brain decision
result was silently despawned with no trace. This defensive log ensures
future index inconsistencies are visible in structured logs rather than
requiring root-cause analysis from symptoms."
```

---

### Task 3: 全量验证与回归测试

**Files:**
- Modify: `logs/bug-workflow/root-cause-subtask-brainllm-dispatch.md`（更新修复状态）

- [ ] **Step 1: 运行 cargo clippy**

Run: `cargo clippy -p harness --all-targets --all-features -- -D warnings 2>&1 | tail -30`
Expected: 无警告

- [ ] **Step 2: 运行 cargo test**

Run: `cargo test -p harness --all-features 2>&1 | tail -40`
Expected: 所有测试通过

- [ ] **Step 3: 运行 cargo fmt 检查**

Run: `cargo fmt --all -- --check 2>&1 | tail -10`
Expected: 无输出（格式一致）

- [ ] **Step 4: 更新根因分析文档**

在 `logs/bug-workflow/root-cause-subtask-brainllm-dispatch.md` 末尾（第 95 行 `> 说明：本文仅记录根因与现象，不含修复对策。` 之后）添加修复记录：

```markdown

## 修复状态

- ✅ 根因已修复：`spawn_create_tasks_messages` 改用 `spawn_task` 中心封装
- ✅ 防御性日志：`brain_decision_system` 在 else 分支添加 warn
- 修复提交：[对应 commit hash]
```

同时将第 95 行 `> 说明：本文仅记录根因与现象，不含修复对策。` 更新为 `> 说明：根因已修复，详见下方修复状态。`

- [ ] **Step 5: 最终提交**

```bash
git add logs/bug-workflow/root-cause-subtask-brainllm-dispatch.md
git commit -m "docs: update root-cause analysis with fix status"
```
