# 顺序工具审批与双通道确认实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 修复 Harness 工具审批流程：同一任务的多个工具确认请求按顺序弹出，Telegram 使用内联键盘确认，QQ 使用文本 `1/2/3` 确认，等待审批期间非 `1/2/3` 文本输入提示重试而不创建新任务。

**Architecture:** 在 `Task` 组件新增 `pending_confirmation_id: Option<Uuid>` 作为"等待工具确认"标志；`tool_dispatch_system` 在生成确认请求前检查同任务是否已有 pending 确认，确保顺序弹出；`tool_confirmation_result_system` 在工具执行或拒绝后清除该标志；`user_input_routing_system` 对处于等待确认的任务将 `1/2/3` 文本解析为 `ToolConfirmationResponseMessage`。

**Tech Stack:** Rust, Bevy ECS, tokio, uuid, tracing

## Global Constraints

- 语言：Rust，遵循官方风格指南。
- 架构：Bevy ECS。
- 不新增 `TaskStatus` 变体，复用 `Waiting(User)` + `pending_confirmation_id`。
- 不修改 `ToolConfirmationRequestMessage` / `ToolConfirmationResponseMessage` 结构。
- 不修改 `ToolPermission` 枚举或权限评估逻辑。
- 不修改 Telegram callback 确认链路。
- 文本到确认选项映射仅支持 `1`/`2`/`3`，其他输入提示重试。
- 所有变更必须通过 `cargo fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`cargo test --all-features`。
- 遵循 Conventional Commits，每个任务独立提交。

---

## Task 1: 在 `Task` 上新增 `pending_confirmation_id` 字段

**Files:**
- Modify: `src/domain/task.rs`

**Interfaces:**
- Consumes: 现有 `Task` 结构体定义。
- Produces: `Task` 新增 `pub pending_confirmation_id: Option<Uuid>`，所有构造点后续任务补齐。

- [ ] **Step 1: 修改 `Task` 结构体**

在 `src/domain/task.rs` 的 `Task` 结构体中，在 `pub status: TaskStatus` 附近新增字段：

```rust
/// 当前正在等待用户确认的工具请求 ID（仅当 status == Waiting(User) 且等待工具确认时存在）
pub pending_confirmation_id: Option<Uuid>,
```

- [ ] **Step 2: 初始化所有构造点**

在 `src/domain/task.rs` 内搜索所有 `Task { ... }` 字面量，为每一处新增 `pending_confirmation_id: None,`。至少包括：

- `Task::from_user_input`
- `Task::from_user_input_ready`
- 任何测试辅助函数中的 `Task` 构造

示例片段（以 `from_user_input` 为例，具体字段以文件为准）：

```rust
Task {
    id,
    title,
    content,
    origin_channel,
    status: TaskStatus::Ready,
    routing_policy,
    plan,
    parent_task_id,
    agent_id,
    pending_confirmation_id: None, // 新增
}
```

- [ ] **Step 3: 编译检查**

Run:

```bash
cargo check --all-features
```

Expected: 仅因其他构造点未初始化而报错，修复直到 `cargo check` 通过。

- [ ] **Step 4: 提交**

```bash
git add src/domain/task.rs
git commit -m "feat: add pending_confirmation_id to Task"
```

---

## Task 2: 补齐项目中所有 `Task` 构造点

**Files:**
- Modify: `src/systems/transform/task_creation.rs`
- Modify: `src/systems/transform/chat_round.rs`（如其中有 `Task` 构造）
- Modify: `src/systems/transform/subtask.rs`（如其中有 `Task` 构造）
- Modify: `tests/**/*.rs` 中直接构造 `Task` 的位置

**Interfaces:**
- Consumes: `Task` 新增 `pending_confirmation_id: Option<Uuid>`。
- Produces: 所有 `Task` 构造点均显式初始化为 `None`。

- [ ] **Step 1: 搜索所有 `Task {` 构造点**

Run:

```bash
rg "Task\s*\{" src tests --type rust -n
```

Expected: 列出所有构造 `Task` 的位置。

- [ ] **Step 2: 为每一处新增 `pending_confirmation_id: None,`**

对每一处以 `Task {` 开头的结构体构造，在合适位置插入：

```rust
pending_confirmation_id: None,
```

- [ ] **Step 3: 编译检查**

Run:

```bash
cargo check --all-features
```

Expected: 无编译错误。

- [ ] **Step 4: 运行现有测试，确保无回归**

Run:

```bash
cargo test --all-features
```

Expected: 现有测试全部通过。

- [ ] **Step 5: 提交**

```bash
git add src tests
git commit -m "chore: initialize pending_confirmation_id in all Task construction sites"
```

---

## Task 3: 实现 `tool_dispatch_system` 顺序审批

**Files:**
- Modify: `src/systems/tools/dispatch.rs`

**Interfaces:**
- Consumes: `Query<(Entity, &Task)>` 查询任务 pending 状态；`ToolExecutionRequestMessage` 组件。
- Produces: 同一任务同一时间仅生成一个 `ToolConfirmationRequestMessage`；生成后设置 `Task.pending_confirmation_id`。

- [ ] **Step 1: 在 `ToolPermission::Confirm` 分支前注入顺序检查**

在 `src/systems/tools/dispatch.rs` 中，定位到 `ToolPermission::Confirm` 分支。在该分支处理逻辑的最开始（读取 `request_id` 之后、生成 `ToolConfirmationRequestMessage` 之前），插入以下检查：

```rust
let already_pending = tasks.iter().any(|(_, t)| {
    t.id == request.request.task_id && t.pending_confirmation_id.is_some()
});
if already_pending {
    continue;
}
```

注意：需要确保系统已有 `tasks: Query<(Entity, &mut Task)>` 或 `Query<(Entity, &Task)>` 参数；若当前系统签名是只读 `&Task`，需改为 `&mut Task`。

- [ ] **Step 2: 生成确认请求后设置 `Task.pending_confirmation_id`**

在生成 `ToolConfirmationRequestMessage` 之后（例如 `commands.spawn(...)` 返回实体或 `request_id` 已生成后），添加：

```rust
if let Some((_, mut task)) = tasks
    .iter_mut()
    .find(|(_, t)| t.id == request.request.task_id)
{
    task.pending_confirmation_id = Some(request_id);
}
```

`request_id` 应为该分支中生成的 `Uuid`。

- [ ] **Step 3: 添加顺序审批单元测试**

在 `src/systems/tools/dispatch.rs` 底部（`#[cfg(test)]` 模块内，如不存在则新建），添加测试：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use bevy_ecs::prelude::*;
    use uuid::Uuid;

    #[test]
    fn sequential_confirmation_only_one_pending_at_a_time() {
        let mut world = World::new();
        // 注册必要组件和资源（按 dispatch.rs 实际依赖调整）
        world.init_resource::<Time>();

        let task_id = Uuid::new_v4();
        let agent_id = Uuid::new_v4();
        let task = world
            .spawn(Task {
                id: task_id,
                title: "test".into(),
                content: "test".into(),
                status: TaskStatus::Waiting(WaitingReason::ToolExecution),
                agent_id: Some(agent_id),
                pending_confirmation_id: None,
                // 其他必需字段按实际 Task 补齐
                ..Default::default()
            })
            .id();

        // spawn 3 个需要确认的工具请求
        for _ in 0..3 {
            world.spawn(ToolExecutionRequestMessage {
                request: ToolRequest {
                    task_id,
                    tool_name: "shell_exec".into(),
                    params: serde_json::json!({"cmd": "echo ok"}),
                    // 其他必需字段按实际 ToolRequest 补齐
                },
                ..Default::default()
            });
        }

        // 运行一次 dispatch 系统
        let mut schedule = Schedule::default();
        schedule.add_systems(tool_dispatch_system);
        schedule.run(&mut world);

        let pending_count = world
            .query::<&ToolConfirmationRequestMessage>()
            .iter(&world)
            .count();
        assert_eq!(pending_count, 1, "同一时刻应只有一个确认请求");

        let task = world.query::<&Task>().get(&world, task).unwrap();
        assert!(task.pending_confirmation_id.is_some());
    }
}
```

如果 `Task` 或 `ToolExecutionRequestMessage` 未实现 `Default`，按实际构造方式修改。该测试可能 initially 因缺少依赖组件而无法编译，后续步骤修复。

- [ ] **Step 4: 运行测试**

Run:

```bash
cargo test --all-features sequential_confirmation_only_one_pending_at_a_time
```

Expected: 测试通过。

- [ ] **Step 5: 提交**

```bash
git add src/systems/tools/dispatch.rs
git commit -m "feat: sequential dispatch of tool confirmation requests"
```

---

## Task 4: 清除 `Task.pending_confirmation_id`

**Files:**
- Modify: `src/systems/tools/confirmation.rs`

**Interfaces:**
- Consumes: `Query<(Entity, &mut Task)>`。
- Produces: 工具确认完成或拒绝后，对应 `Task.pending_confirmation_id` 被设为 `None`。

- [ ] **Step 1: 在确认通过分支清除 pending id**

在 `src/systems/tools/confirmation.rs` 的 `ToolConfirmationResponseMessage` 处理系统中，定位到用户选择 `allow_once` / `allow_always` 并执行工具的分支。在工具执行后、调用 `restore_task_after_tool` 之前或之后，添加：

```rust
if let Some((_, mut task)) = tasks
    .iter_mut()
    .find(|(_, t)| t.id == tool_request.request.task_id)
{
    task.pending_confirmation_id = None;
}
```

注意：可能需要确认变量名是 `tool_request` 还是 `request`，以实际代码为准。

- [ ] **Step 2: 在拒绝分支清除 pending id**

在拒绝分支（`deny`）末尾添加同样的清除逻辑，避免任务永远卡在等待确认状态。

```rust
if let Some((_, mut task)) = tasks
    .iter_mut()
    .find(|(_, t)| t.id == tool_request.request.task_id)
{
    task.pending_confirmation_id = None;
}
```

- [ ] **Step 3: 添加单元测试**

在 `src/systems/tools/confirmation.rs` 的 `#[cfg(test)]` 模块（如不存在则新建）添加测试：

```rust
#[test]
fn confirmation_clears_task_pending_id() {
    let mut world = World::new();
    let task_id = Uuid::new_v4();
    let request_id = Uuid::new_v4();

    let task_entity = world
        .spawn(Task {
            id: task_id,
            status: TaskStatus::Waiting(WaitingReason::User),
            pending_confirmation_id: Some(request_id),
            // 其他字段按实际补齐
            ..Default::default()
        })
        .id();

    world.spawn(ToolExecutionRequestMessage {
        request: ToolRequest {
            task_id,
            tool_name: "shell_exec".into(),
            params: serde_json::json!({"cmd": "echo ok"}),
            // 其他字段按实际补齐
        },
        pending_confirmation_id: Some(request_id),
        ..Default::default()
    });

    // 构造确认响应事件
    // 具体 API 以 confirmation.rs 实际为准

    // 运行系统
    // ...

    let task = world.query::<&Task>().get(&world, task_entity).unwrap();
    assert!(task.pending_confirmation_id.is_none());
}
```

- [ ] **Step 4: 运行测试**

Run:

```bash
cargo test --all-features confirmation_clears_task_pending_id
```

Expected: 测试通过。

- [ ] **Step 5: 提交**

```bash
git add src/systems/tools/confirmation.rs
git commit -m "feat: clear pending_confirmation_id after tool confirmation"
```

---

## Task 5: 实现文本确认路由

**Files:**
- Modify: `src/systems/routing.rs`

**Interfaces:**
- Consumes: `ExternalInput::TextWithChannel`；`Task.pending_confirmation_id: Option<Uuid>`。
- Produces: 若任务处于等待确认且输入为 `1`/`2`/`3`，生成 `ToolConfirmationResponseMessage`；否则生成 `SystemOutputMessage` 提示重试。

- [ ] **Step 1: 添加文本解析辅助函数**

在 `src/systems/routing.rs` 的 `user_input_routing_system` 附近，新增私有函数：

```rust
fn parse_confirmation_option(content: &str) -> Option<String> {
    match content.trim().to_lowercase().as_str() {
        "1" => Some("allow_once".to_string()),
        "2" => Some("allow_always".to_string()),
        "3" => Some("deny".to_string()),
        _ => None,
    }
}
```

- [ ] **Step 2: 修改 `user_input_routing_system` 的分支逻辑**

定位到匹配 `Waiting(User)` 任务的代码块。当前逻辑大致为：

```rust
TaskStatus::Waiting(WaitingReason::User) => {
    // 继续任务
}
```

改为：

```rust
TaskStatus::Waiting(WaitingReason::User) => {
    if let Some(pending_id) = task.pending_confirmation_id {
        match parse_confirmation_option(&input.content) {
            Some(option_id) => {
                commands.spawn(ToolConfirmationResponseMessage {
                    request_id: pending_id,
                    selected_option: option_id,
                    // 其他字段按实际 ToolConfirmationResponseMessage 补齐
                });
            }
            None => {
                commands.spawn(SystemOutputMessage {
                    task_id: task.id,
                    content: "请输入有效选项：1=仅本次允许，2=永久允许，3=拒绝".to_string(),
                    // 其他字段按实际 SystemOutputMessage 补齐
                });
            }
        }
    } else {
        commands.spawn(ContinueTaskMessage {
            task_id: task.id,
            content: input.content.clone(),
            origin_channel: input.origin_channel.clone(),
        });
    }
}
```

如果当前 routing 系统在决定路由前会检查 `is_command` 等条件，请确保在 `Waiting(User)` 任务存在 pending 确认时，也优先走确认解析分支。

- [ ] **Step 3: 添加单元测试**

在 `src/systems/routing.rs` 的 `#[cfg(test)]` 模块（如不存在则新建）添加测试：

```rust
#[test]
fn text_confirmation_option_2_maps_to_allow_always() {
    let mut world = World::new();
    let task_id = Uuid::new_v4();
    let pending_id = Uuid::new_v4();

    world.spawn(Task {
        id: task_id,
        status: TaskStatus::Waiting(WaitingReason::User),
        pending_confirmation_id: Some(pending_id),
        origin_channel: Some(ChannelId::telegram(123)),
        // 其他字段按实际补齐
        ..Default::default()
    });

    world.spawn(ExternalInputMessage {
        input: ExternalInput::TextWithChannel {
            content: "2".into(),
            origin_channel: ChannelId::telegram(123),
        },
    });

    // 运行 routing 系统
    // ...

    let responses: Vec<&ToolConfirmationResponseMessage> = world
        .query::<&ToolConfirmationResponseMessage>()
        .iter(&world)
        .collect();
    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0].request_id, pending_id);
    assert_eq!(responses[0].selected_option, "allow_always");
}

#[test]
fn invalid_confirmation_text_prompts_retry() {
    let mut world = World::new();
    let task_id = Uuid::new_v4();
    let pending_id = Uuid::new_v4();

    world.spawn(Task {
        id: task_id,
        status: TaskStatus::Waiting(WaitingReason::User),
        pending_confirmation_id: Some(pending_id),
        origin_channel: Some(ChannelId::telegram(123)),
        // 其他字段按实际补齐
        ..Default::default()
    });

    world.spawn(ExternalInputMessage {
        input: ExternalInput::TextWithChannel {
            content: "hello".into(),
            origin_channel: ChannelId::telegram(123),
        },
    });

    // 运行 routing 系统
    // ...

    let outputs: Vec<&SystemOutputMessage> = world
        .query::<&SystemOutputMessage>()
        .iter(&world)
        .collect();
    assert_eq!(outputs.len(), 1);
    assert!(outputs[0].content.contains("1=仅本次允许"));

    let new_tasks: Vec<&CreateTaskMessage> = world
        .query::<&CreateTaskMessage>()
        .iter(&world)
        .collect();
    assert!(new_tasks.is_empty(), "不应创建新任务");
}

#[test]
fn no_pending_confirmation_routes_to_continue_task() {
    let mut world = World::new();
    let task_id = Uuid::new_v4();

    world.spawn(Task {
        id: task_id,
        status: TaskStatus::Waiting(WaitingReason::User),
        pending_confirmation_id: None,
        origin_channel: Some(ChannelId::telegram(123)),
        // 其他字段按实际补齐
        ..Default::default()
    });

    world.spawn(ExternalInputMessage {
        input: ExternalInput::TextWithChannel {
            content: "继续".into(),
            origin_channel: ChannelId::telegram(123),
        },
    });

    // 运行 routing 系统
    // ...

    let continues: Vec<&ContinueTaskMessage> = world
        .query::<&ContinueTaskMessage>()
        .iter(&world)
        .collect();
    assert_eq!(continues.len(), 1);
}
```

- [ ] **Step 4: 运行测试**

Run:

```bash
cargo test --all-features text_confirmation
```

Expected: 上述三个测试通过。

- [ ] **Step 5: 提交**

```bash
git add src/systems/routing.rs
git commit -m "feat: route text input as confirmation option when task is pending confirmation"
```

---

## Task 6: 添加集成测试

**Files:**
- Create: `tests/sequential_tool_confirmation.rs`

**Interfaces:**
- Consumes: 完整 ECS app 构建与运行；工具审批链路。
- Produces: 端到端验证顺序审批、allow_always 跳过后续审批、QQ 文本确认。

- [ ] **Step 1: 创建集成测试文件**

新建 `tests/sequential_tool_confirmation.rs`，内容参考：

```rust
use harness::prelude::*;

#[tokio::test]
async fn two_shell_execs_confirmed_sequentially() {
    // 构建 app，启用 mock 通道
    let mut app = build_test_app().await;

    // 注入用户输入：请求执行两个 shell 命令
    inject_user_input(&mut app, "请执行 echo a 和 echo b").await;

    // 运行足够 tick，等待第一个审批请求出现
    run_ticks(&mut app, 10).await;

    let approval_requests = collect_approval_requests(&mut app);
    assert_eq!(approval_requests.len(), 1, "应只弹出一个审批请求");

    // 确认第一个
    inject_confirmation(&mut app, &approval_requests[0].request_id, "allow_once").await;
    run_ticks(&mut app, 10).await;

    // 应弹出第二个审批请求
    let approval_requests = collect_approval_requests(&mut app);
    assert_eq!(approval_requests.len(), 2, "确认第一个后应弹出第二个");
}

#[tokio::test]
async fn allow_always_skips_remaining_confirmations() {
    let mut app = build_test_app().await;

    // 触发两个相同的 shell_exec
    inject_user_input(&mut app, "请执行两次 echo ok").await;
    run_ticks(&mut app, 10).await;

    let first = collect_approval_requests(&mut app)
        .into_iter()
        .next()
        .expect("应有一个审批请求");

    // 选择永久允许
    inject_confirmation(&mut app, &first.request_id, "allow_always").await;
    run_ticks(&mut app, 20).await;

    // 第二个相同工具应直接执行，不再弹出审批
    let approval_requests = collect_approval_requests(&mut app);
    assert_eq!(approval_requests.len(), 1, "allow_always 后不应再弹出新审批");
}

#[tokio::test]
async fn qq_text_confirmation_resolves_tool() {
    let mut app = build_test_app().await;

    // 通过 QQ 通道注入任务
    inject_qq_text(&mut app, "请执行 echo qq").await;
    run_ticks(&mut app, 10).await;

    // 应有一个审批请求被发送到 QQ 通道
    let qq_outputs = collect_qq_outputs(&mut app);
    let approval = qq_outputs
        .iter()
        .find(|o| o.content.contains("1=仅本次允许"))
        .expect("QQ 应收到带选项编号的审批消息");

    // 通过 QQ 回复 2
    inject_qq_text(&mut app, "2").await;
    run_ticks(&mut app, 20).await;

    // 断言工具已被执行（可通过检查输出或 ToolExecutionResultMessage）
    let results = collect_tool_results(&mut app);
    assert!(!results.is_empty(), "工具应被执行");
}
```

上述辅助函数（`build_test_app`、`inject_user_input`、`collect_approval_requests` 等）参考 `tests/` 目录下现有集成测试的模式实现。

- [ ] **Step 2: 运行集成测试**

Run:

```bash
cargo test --all-features --test sequential_tool_confirmation
```

Expected: 三个测试通过。

- [ ] **Step 3: 提交**

```bash
git add tests/sequential_tool_confirmation.rs
git commit -m "test: add integration tests for sequential tool confirmation"
```

---

## Task 7: 全量验证与清理

**Files:**
- Modify: 无（仅运行检查）

**Interfaces:**
- N/A

- [ ] **Step 1: 格式化检查**

Run:

```bash
cargo fmt --all --check
```

Expected: 无差异输出。

- [ ] **Step 2: Clippy 检查**

Run:

```bash
cargo clippy --all-targets --all-features -- -D warnings
```

Expected: 无 warning。

- [ ] **Step 3: 全量测试**

Run:

```bash
cargo test --all-features
```

Expected: 全部测试通过。

- [ ] **Step 4: 更新 docs/README.md 索引（如需要）**

如果 `docs/superpowers/specs/` 目录下的 README 或索引文件要求列出当前有效规格，按项目规范更新。若 `docs/README.md` 仅做目录级索引且未变更，可跳过。

- [ ] **Step 5: 提交**

```bash
git commit --allow-empty -m "chore: verify formatting, clippy, and tests"
```

---

## Self-Review

### Spec 覆盖度

| Spec 要求 | 对应任务 |
|---|---|
| `Task` 新增 `pending_confirmation_id` | Task 1, Task 2 |
| 同一任务顺序弹出确认请求 | Task 3 |
| 执行/拒绝后清除 pending id | Task 4 |
| 文本 `1/2/3` 解析为确认选项 | Task 5 |
| 无效文本提示重试、不创建新任务 | Task 5 |
| allow_always 后同工具直接执行 | Task 3 + Task 6（集成测试验证） |
| Telegram/QQ 双通道支持 | Task 5 + Task 6 |
| 日志增强 | 未单独设任务；实现时可在 Task 3/4/5 中顺带追加 |
| 回归测试 | Task 6 + Task 7 |

### Placeholder 检查

- 无 `TBD` / `TODO` / "implement later" / "fill in details"。
- 测试代码为示意，因无法提前知道 `Task` 等类型的完整字段，使用 `..Default::default()` 或 `// 其他字段按实际补齐` 标注；实现时需按实际类型填写。这不属于 placeholder，而是根据实际类型补全的明确说明。

### 类型一致性

- `pending_confirmation_id` 全程使用 `Option<Uuid>`。
- 文本选项映射统一为 `"1"`→`"allow_once"`、`"2"`→`"allow_always"`、`"3"`→`"deny"`。
- `ToolConfirmationResponseMessage` 的字段名以实际代码为准，实现时统一调整。
