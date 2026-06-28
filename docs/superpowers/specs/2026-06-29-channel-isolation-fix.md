# 通道隔离修复设计

> **状态：当前有效**
>
> 本设计文档描述对 Harness 中跨通道接管 bug 的修复方案，覆盖路由系统、命令解析系统、子任务编排系统与插件 dispatcher 中存在的通道隔离缺失与 `origin_channel` 硬编码问题。

## 背景与问题

### 现象

从日志 `logs/harness_2026-06-28_22-59-59.jsonl` 观察到：用户通过 Telegram 通道发送消息后，后续在 TUI 和 QQ 通道发送的纯文本消息会被错误地路由到该 Telegram 任务上，并以 Telegram 通道回复。

### 根因

`src/systems/routing.rs:25-28` 的 `user_input_routing_system` 在过滤 `Waiting(User)` 任务时，**仅按 `TaskStatus::Waiting(WaitingReason::User)` 过滤，完全不比较 `origin_channel`**。`waiting_tasks.first()` 取第一个匹配项，无通道优先级或匹配逻辑。`ChannelId` 已派生 `PartialEq`/`Eq`（[src/domain/frontend.rs:17](../../../src/domain/frontend.rs)），但路由代码未使用。

类似问题还存在于 `command_parse_system` 与 `spawn_create_tasks_messages` 中，详见下文。

### 影响范围

| 严重度 | 位置 | 问题 |
| --- | --- | --- |
| 严重 | `src/systems/routing.rs:25-28` | `Waiting(User)` 任务过滤无通道比较，跨通道接管 |
| 严重 | `src/systems/command.rs:37-39` | `/btw` 父任务查找无通道过滤 |
| 严重 | `src/systems/command.rs:84` | `/finish` 任务查找无通道过滤 |
| 严重 | `src/systems/command.rs:102` | `/summarize` 任务查找无通道过滤 |
| 严重 | `src/systems/command.rs:57-61` | `/btw` 子任务 `origin_channel` 硬编码 `Tui/default` |
| 严重 | `src/systems/command.rs:73-77` | `/btw` 回退 `CreateTaskMessage` 的 `origin_channel` 硬编码 `Tui/default` |
| 严重 | `src/systems/tools/orchestrator.rs:141-145` | `create_tasks` 子任务 `origin_channel` 硬编码 `Tui/default`，未继承父任务 |
| 低 | `src/user_plugins/dispatcher.rs:209-213` | 插件 `CreateTask` 硬编码 `Tui/plugin`（设计意图，保留但加注释） |

### 不在本次修复范围

以下相关但低风险的延伸问题不在本次修复范围，记录供后续评估：

- `ToolConfirmationResponseMessage` 不携带 `origin_channel`，仅凭 `request_id` UUID 匹配（`src/systems/tools/confirmation.rs:51-71`）。UUID 唯一性使误接管概率极低，缺纵深防御但收益边际递减。
- `ContinueTaskMessage` 不携带 `origin_channel` 字段（`src/domain/message.rs`）。上游过滤已阻止跨通道接管，下游校验属过度工程。
- `frontend_output_system` 本身是频道感知的（使用 `task.origin_channel`），无需修改；但其正确性依赖上游 `origin_channel` 不被污染——本次修复正好关闭污染源。

## 设计目标

1. **阻止跨通道接管**：来自通道 A 的纯文本输入不会被路由到通道 B 的 `Waiting(User)` 任务。
2. **命令作用域隔离**：`/btw`、`/finish`、`/summarize` 仅作用于发起命令的通道内的任务。
3. **通道上下文继承**：通过 `/btw` 或 `create_tasks` 创建的子任务继承父任务（或发起用户）的 `origin_channel`，确保后续输出路由到正确通道。
4. **不引入新抽象**：仅复用已派生 `PartialEq`/`Eq` 的 `ChannelId`，不新增 message 字段或组件。
5. **不破坏既有测试**：现有 `multi_turn_routing.rs`、`origin_channel_flow.rs`、`frontend_routing.rs` 等测试需继续通过；本次新增跨通道隔离测试。

## 修复方案

### 修改 1：`user_input_routing_system` 加入通道过滤

**文件**：`src/systems/routing.rs`

**修改点**：第 25-28 行的 `waiting_tasks` 过滤条件。

```rust
let waiting_tasks: Vec<_> = tasks
    .iter()
    .filter(|t| {
        t.status == TaskStatus::Waiting(WaitingReason::User)
            && t.origin_channel == input.origin_channel
    })
    .collect();
```

**行为变化**：来自通道 A 的输入只会被路由到同样 `origin_channel == A` 的 `Waiting(User)` 任务。若无匹配任务，则走 `create_new` 分支创建新任务（保留 `input.origin_channel`，已有逻辑正确）。

**日志增强**：在 `continue_existing` 分支的 `debug!` 中追加 `input_channel = ?input.origin_channel` 与 `task_channel = ?task.origin_channel` 字段，便于排查。

### 修改 2：`/btw` 父任务查找加入通道过滤

**文件**：`src/systems/command.rs`

**修改点**：第 37-39 行的 `parent_task` 查找。

```rust
let parent_task = tasks
    .iter()
    .find(|(t, _)| {
        !t.status.is_terminal()
            && t.status != TaskStatus::Pending
            && t.origin_channel == input.origin_channel
    });
```

**行为变化**：`/btw` 仅在发起命令的通道内查找父任务。若该通道无活跃任务，则回退到 `CreateTaskMessage` 分支（见修改 3）。

### 修改 3：`/btw` 子任务与回退 `CreateTaskMessage` 使用 `input.origin_channel`

**文件**：`src/systems/command.rs`

**修改点 A**：第 56-62 行子任务构造的硬编码 `ChannelId` 替换为 `input.origin_channel.clone()`。

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

**修改点 B**：第 71-78 行回退 `CreateTaskMessage` 的硬编码 `origin_channel` 替换为 `input.origin_channel.clone()`。

```rust
commands.spawn(CreateTaskMessage {
    content: input.content.clone(),
    origin_channel: input.origin_channel.clone(),
});
```

**行为变化**：`/btw` 创建的子任务与回退任务的 `origin_channel` 与发起命令的通道一致，后续输出路由到正确通道。

### 修改 4：`/finish` 任务查找加入通道过滤

**文件**：`src/systems/command.rs`

**修改点**：第 84 行的 `current_task` 查找。

```rust
let current_task = tasks
    .iter()
    .find(|(t, _)| !t.status.is_terminal() && t.origin_channel == input.origin_channel);
```

**行为变化**：`/finish` 仅终结发起命令的通道内的活跃任务。若无匹配任务，走现有的 `FinishCommandNoTask` 分支。

### 修改 5：`/summarize` 任务查找加入通道过滤

**文件**：`src/systems/command.rs`

**修改点**：第 102 行的 `active_task` 查找。

```rust
let active_task = tasks
    .iter()
    .find(|(t, _)| !t.status.is_terminal() && t.origin_channel == input.origin_channel);
```

**行为变化**：`/summarize` 仅对发起命令的通道内的活跃任务触发摘要。

### 修改 6：`spawn_create_tasks_messages` 子任务继承父任务 `origin_channel`

**文件**：`src/systems/tools/orchestrator.rs`

**修改点 A**：`spawn_create_tasks_messages` 函数签名新增 `parent_origin_channel: ChannelId` 参数。

```rust
pub fn spawn_create_tasks_messages(
    commands: &mut Commands,
    request_entity: Entity,
    agent_id: AgentId,
    task_id: TaskId,
    request_kind: crate::domain::AgentRequestKind,
    definitions: Vec<SubTaskDefinition>,
    tool_call_id: Option<String>,
    parent_origin_channel: ChannelId,  // 新增
) {
    // ...
    let child_task = Task {
        // ...
        origin_channel: parent_origin_channel.clone(),
        // ...
    };
    // ...
}
```

**修改点 B**：调用方 `handle_tool_action`（`src/systems/tools/orchestrator.rs:451-461`）在调用前从父任务实体取 `origin_channel`。

`handle_tool_action` 已接收 `task_entity: Entity` 与 `tasks: &mut Query<(Entity, &mut Task)>`，可直接获取：

```rust
Ok(ToolAction::CreateBatch(definitions)) => {
    let parent_origin_channel = tasks
        .get(task_entity)
        .map(|(_, t)| t.origin_channel.clone())
        .unwrap_or_else(|_| {
            tracing::warn!(
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

**行为变化**：`create_tasks` 创建的子任务继承父任务的 `origin_channel`，后续输出与审批请求路由到正确通道。父任务实体缺失时回退到 `Tui/default` 并 warn（不应发生但保留防御性）。

### 修改 7：插件 `CreateTask` 保留硬编码但加注释

**文件**：`src/user_plugins/dispatcher.rs`

**修改点**：第 206-216 行的 `WorldCommand::CreateTask` 分支。

不修改代码逻辑，仅在 `let channel = ChannelId { ... }` 上方追加注释说明设计意图：

```rust
WorldCommand::CreateTask { title, parent: _ } => {
    // 插件创建的任务不属于任何 IM 通道，使用 Tui/plugin 标识其来源。
    // 这是有意为之：插件通过 host API 创建的任务不绑定到具体用户会话。
    let channel = ChannelId {
        frontend: FrontendKind::Tui,
        user_id: "plugin".to_string(),
        thread_id: None,
    };
    let task = Task::from_user_input(title, 0, channel);
    world.spawn((task, crate::domain::ShortTermMemory::default()));
}
```

### 修改 8：同步 `project_memory.md`

**文件**：`/Users/diater/.trae-cn/memory/projects/-Users-diater-workspace-Harness/project_memory.md`

`project_memory.md` 的"Engineering Conventions"中已记录"Subtasks inherit parent task's routing_policy in src/systems/tools/orchestrator.rs"，本次修复使代码与该记录一致，无需修改记忆。但在修复完成后，需在 `Lessons Learned` 中追加本次发现的跨通道接管 bug 模式。

## 测试策略

### 新增单元测试

**文件**：`src/systems/routing.rs`（`#[cfg(test)]` 模块）

1. `cross_channel_input_not_routed_to_other_channel_waiting_task`
   - 创建一个 `origin_channel = Telegram` 的 `Waiting(User)` 任务
   - 发起一个 `origin_channel = QQ` 的 `UserInputMessage`
   - 断言：生成 `CreateTaskMessage`（而非 `ContinueTaskMessage`），新任务的 `origin_channel == QQ`

2. `same_channel_input_routed_to_waiting_task`
   - 创建一个 `origin_channel = Telegram` 的 `Waiting(User)` 任务
   - 发起一个 `origin_channel = Telegram` 的 `UserInputMessage`
   - 断言：生成 `ContinueTaskMessage`，`task_id` 指向原任务

### 新增集成测试

**文件**：`tests/cross_channel_isolation.rs`（新增）

1. `cross_channel_plain_text_does_not_takeover_waiting_task`
   - 模拟 Telegram 任务进入 `Waiting(User)`
   - 从 QQ 通道发送纯文本
   - 断言：QQ 通道创建新任务，Telegram 任务仍处于 `Waiting(User)`

2. `cross_channel_btw_does_not_pick_other_channel_parent`
   - 在 QQ 通道创建活跃任务
   - 从 Telegram 通道发送 `/btw topic`
   - 断言：Telegram 通道走 `CreateTaskMessage` 分支（无父任务），新任务 `origin_channel == Telegram`

3. `cross_channel_finish_does_not_finish_other_channel_task`
   - 在 QQ 通道创建活跃任务
   - 从 Telegram 通道发送 `/finish`
   - 断言：QQ 任务未终结，日志包含 `FinishCommandNoTask`

4. `cross_channel_summarize_does_not_summarize_other_channel_task`
   - 在 QQ 通道创建带 STM 的活跃任务
   - 从 Telegram 通道发送 `/summarize`
   - 断言：QQ 任务未触发摘要，日志包含 `SummarizeCommandNoTask`

5. `create_tasks_subtask_inherits_parent_origin_channel`
   - 创建 `origin_channel = Telegram` 的父任务
   - 触发 `create_tasks` 工具调用
   - 断言：所有子任务的 `origin_channel == Telegram`

6. `btw_subtask_inherits_input_origin_channel`
   - 在 QQ 通道创建活跃父任务
   - 从 QQ 通道发送 `/btw topic`
   - 断言：子任务 `origin_channel == QQ`

### 既有测试回归

- `tests/multi_turn_routing.rs`：所有任务使用 `default_channel()`（Tui/default/None），同一通道内行为不变，应继续通过。
- `tests/origin_channel_flow.rs`：单通道场景，行为不变。
- `tests/frontend_routing.rs`：频道感知路由，行为不变。
- `tests/command_*.rs`：命令解析单元测试，行为不变。

## 实施顺序

1. 修改 `src/systems/routing.rs`（修改 1）+ 单元测试
2. 修改 `src/systems/command.rs`（修改 2-5）+ 单元测试
3. 修改 `src/systems/tools/orchestrator.rs`（修改 6）
4. 修改 `src/user_plugins/dispatcher.rs`（修改 7，仅注释）
5. 新增集成测试 `tests/cross_channel_isolation.rs`
6. 运行 `cargo fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`cargo test --all-features`
7. 同步 `project_memory.md` 的 `Lessons Learned`

## 风险与回滚

### 风险

- **既有测试可能因通道默认值变化而失败**：`command.rs` 测试中的 `UserInputMessage` 大多硬编码 `Tui/default`，而修改后的 `/btw`/`/finish`/`/summarize` 会用 `input.origin_channel` 比对，若测试任务的 `origin_channel` 也是 `Tui/default`，则行为一致。需在实施时逐一验证。
- **`spawn_create_tasks_messages` 签名变更**：仅一处调用方（`handle_tool_action`），修改面可控。

### 回滚

所有修改集中在 4 个源文件 + 1 个测试文件 + 1 个记忆文件，可通过 `git revert` 单次提交回滚。

## 不引入的变更

- 不在 `ContinueTaskMessage`、`ToolConfirmationResponseMessage` 上增加 `origin_channel` 字段。
- 不在 `continue_task_system`、`confirmation.rs` 中增加下游通道校验。
- 不修改 `frontend_output_system`（已正确）。
- 不修改 `ToolConfirmationRequestMessage` 结构。
- 不修改 `Signal` 事件触发的 `origin_channel` 处理（事件任务无通道上下文是设计意图）。
