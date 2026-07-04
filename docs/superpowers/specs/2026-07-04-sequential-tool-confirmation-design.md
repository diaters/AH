# 顺序工具审批与双通道确认设计

> **状态：当前有效**
>
> 本设计文档描述对 Harness 工具审批流程的修复：同一任务的多个工具确认请求按顺序弹出，支持 Telegram 内联键盘确认与 QQ 文本确认，并在等待审批期间将文本输入识别为审批选项。

## 背景与问题

### 现象

从日志 `logs/harness_2026-07-04_19-13-04.jsonl` 观察到：

1. LLM 在一次响应中请求两个 `shell_exec`（检查 `pkg` 与 `uname -a`），系统同时向用户弹出两个审批请求。
2. 用户只确认了第二个审批（`uname -a`）并选择 `allow_always`；第一个审批（检查 `pkg`）始终未被确认。
3. 已确认的工具执行后，任务被 `restore_task_after_tool` 恢复为 `Waiting(ToolExecution)`，未确认的 sibling 请求因 `pending_confirmation_id` 已设置而被 `tool_dispatch_system` 永久跳过，任务卡住。
4. 用户随后发送 `2`，由于任务已不在 `Waiting(User)` 状态，`routing.rs` 将其当作新任务创建。

### 根因

- `tool_dispatch_system` 对 `ToolPermission::Confirm` 的工具会立即生成 `ToolConfirmationRequestMessage`，多个 sibling 请求会同时推送。
- `ToolExecutionRequestMessage` 一旦设置 `pending_confirmation_id`，后续 tick 即被跳过，即使 `Agent.tool_permissions` 已因 `allow_always` 变为 `Allow`。
- `routing.rs` 只匹配 `Waiting(User)` 任务；任务等待工具确认期间状态为 `Waiting(ToolExecution)`，导致文本输入无法关联到原任务。

## 设计目标

1. **顺序审批**：同一任务内同一时间只向用户展示一个工具审批请求，前一个审批完成后再弹出下一个。
2. **权限复用**：若用户已选择 `allow_always`，后续同工具请求直接执行，无需再次审批。
3. **双通道确认**：Telegram 继续使用内联键盘 callback；QQ 使用文本回复 `1`/`2`/`3` 进行确认。
4. **无效文本提示**：等待审批期间，非 `1`/`2`/`3` 的文本输入不创建新任务，而是提示用户重新输入。
5. **不破坏既有行为**：普通多轮对话、跨通道隔离、父 Agent 审批等现有逻辑保持不变。

## 方案概述

在 `Task` 上新增 `pending_confirmation_id: Option<Uuid>`，作为路由系统判断任务是否处于"等待工具确认"的唯一依据：

- `Waiting(User)` + `pending_confirmation_id = Some(...)` → 等待用户对工具确认。
- `Waiting(User)` + `pending_confirmation_id = None` → 等待用户自由回复（多轮对话）。

`tool_dispatch_system` 在生成确认请求前，先检查同任务是否已有 `pending_confirmation_id`；若有则跳过当前工具，等待前一个确认完成。`tool_confirmation_result_system` 在执行完工具后清除该字段，使下一个 sibling 工具在下一 tick 被重新评估。

`routing.rs` 匹配到 `Waiting(User)` 任务时，若 `pending_confirmation_id` 存在，将输入文本解析为确认选项；仅识别 `1`/`2`/`3`，其他输入生成提示消息要求重新输入。

## 详细设计

### 数据模型

#### `src/domain/task.rs`

`Task` 新增字段：

```rust
/// 当前正在等待用户确认的工具请求 ID（仅当 status == Waiting(User) 且等待工具确认时存在）
pub pending_confirmation_id: Option<Uuid>,
```

所有构造 `Task` 的位置初始化为 `None`。

### 状态流转

```text
LLM 返回 ToolCalls
    │
    ▼
Task = Waiting(ToolExecution)
spawn N 个 ToolExecutionRequestMessage（均无 pending_confirmation_id）
    │
    ▼
dispatch.rs 处理第 1 个工具：
  权限 = Confirm → 检查同任务无 pending_confirmation_id
                → Task = Waiting(User), pending_confirmation_id = req_id
                → spawn ToolConfirmationRequestMessage
    │
    ▼
用户确认（Telegram callback 或 QQ 文本）
    │
    ▼
confirmation.rs 执行工具
  → 清除 Task.pending_confirmation_id
  → restore_task_after_tool → Task = Waiting(ToolExecution)
    │
    ▼
dispatch.rs 处理第 2 个工具：
  若权限已因 allow_always 变为 Allow → 直接执行
  若权限仍为 Confirm → 检查同任务无 pending_confirmation_id
                     → Task = Waiting(User), pending_confirmation_id = req_id
                     → spawn 新的 ToolConfirmationRequestMessage
    │
    ▼
全部 ToolExecutionRequestMessage 处理完毕
  → tool_calling_orchestrator_system 收集结果
  → Task = Waiting(Agent)，发起 follow-up LLM
```

### 修改点

#### 1. `src/domain/task.rs`

- `Task` 结构体新增 `pending_confirmation_id: Option<Uuid>`。
- `Task::from_user_input` 与 `Task::from_user_input_ready` 初始化为 `None`。
- 其他直接构造 `Task` 的位置（如 `spawn_create_tasks_messages`、`chat_with_agent`）显式初始化为 `None`。

#### 2. `src/systems/tools/dispatch.rs`

在 `ToolPermission::Confirm` 分支中、生成确认请求之前，加入顺序审批检查：

```rust
let already_pending = tasks.iter().any(|(_, t)| {
    t.id == request.request.task_id && t.pending_confirmation_id.is_some()
});
if already_pending {
    continue;
}
```

生成确认请求后，设置对应任务的 `pending_confirmation_id`：

```rust
if let Some((_, mut task)) = tasks
    .iter_mut()
    .find(|(_, t)| t.id == request.request.task_id)
{
    task.pending_confirmation_id = Some(request_id);
}
```

#### 3. `src/systems/tools/confirmation.rs`

用户确认并执行工具后，在处理分支末尾清除 `pending_confirmation_id`：

```rust
if let Some((_, mut task)) = tasks
    .iter_mut()
    .find(|(_, t)| t.id == tool_request.request.task_id)
{
    task.pending_confirmation_id = None;
}
```

拒绝分支同样清除该字段，避免任务永远停留在等待确认状态。

#### 4. `src/systems/routing.rs`

匹配到 `Waiting(User)` 任务后，按 `pending_confirmation_id` 分流：

```rust
if let Some(task) = waiting_tasks.first() {
    if task.pending_confirmation_id.is_some() {
        match parse_confirmation_option(&input.content) {
            Some(option_id) => {
                // 生成确认响应
                commands.spawn(ToolConfirmationResponseMessage {
                    request_id: task.pending_confirmation_id.unwrap(),
                    selected_option: option_id,
                });
            }
            None => {
                // 无效输入：提示用户重新输入 1/2/3
                commands.spawn(SystemOutputMessage {
                    task_id: task.id,
                    content: "请输入有效选项：1=仅本次允许，2=永久允许，3=拒绝".to_string(),
                });
            }
        }
    } else {
        // 普通多轮等待，继续现有任务
        commands.spawn(ContinueTaskMessage { ... });
    }
}
```

**文本到选项映射**

| 输入 | 选项 |
| --- | --- |
| `1` | `allow_once` |
| `2` | `allow_always` |
| `3` | `deny` |

大小写不敏感，前后去空白。非 `1`/`2`/`3` 的输入一律提示错误。

### Telegram 与 QQ 差异

| 通道 | 审批 UI | 用户操作 | 内部事件 |
| --- | --- | --- | --- |
| Telegram | 内联键盘（`allow_once` / `allow_always` / `deny`） | 点击按钮 | `ExternalInput::Confirmation` |
| QQ | 文本消息列出 `1/2/3` 选项 | 回复 `1`/`2`/`3` | `ExternalInput::TextWithChannel` → `routing.rs` 解析 |

`frontend_output_system` 向两种通道推送的 `EngineEvent::ApprovalRequest` 格式不变；Telegram 前端渲染为内联键盘，QQ 前端渲染为带编号的文本。`routing.rs` 的文本解析仅作为 QQ 通道和 Telegram 误触文本的兜底。

### 日志增强

在以下位置追加结构化日志字段，便于排查：

- `RoutingDecision`：当输入被识别为审批响应时，记录 `decision = "confirmation_response"` 与 `selected_option`。
- `ToolConfirmationApproved` / `ToolConfirmationDenied`：记录 `pending_sibling_count`（剩余待确认 sibling 数量）。
- `tool_dispatch_system` 顺序跳过时，记录 `event = "ToolConfirmationQueued"` 与 `queued_task_id`。

## 测试策略

### 单元测试

**文件**：`src/systems/tools/dispatch.rs`（`#[cfg(test)]` 模块，新增）

1. `sequential_confirmation_only_one_pending_at_a_time`
   - 同一任务 spawn 3 个需要确认的 `shell_exec`。
   - 断言第一 tick 后只有一个 `ToolConfirmationRequestMessage`。
   - 模拟确认第一个工具后，断言第二个 `ToolConfirmationRequestMessage` 生成。

**文件**：`src/systems/tools/confirmation.rs`（`#[cfg(test)]` 模块，新增）

2. `confirmation_clears_task_pending_id`
   - 确认工具后断言对应 `Task.pending_confirmation_id == None`。

**文件**：`src/systems/routing.rs`（`#[cfg(test)]` 模块，新增）

3. `text_confirmation_option_1_maps_to_allow_once`
4. `text_confirmation_option_2_maps_to_allow_always`
5. `text_confirmation_option_3_maps_to_deny`
6. `invalid_confirmation_text_prompts_retry`
   - 任务 `Waiting(User)` + `pending_confirmation_id = Some(...)`。
   - 输入 `hello`，断言生成 `SystemOutputMessage` 且内容包含重试提示，不生成 `CreateTaskMessage`。
7. `no_pending_confirmation_routes_to_continue_task`
   - 任务 `Waiting(User)` + `pending_confirmation_id = None`。
   - 输入任意文本，断言生成 `ContinueTaskMessage`，行为不变。

### 集成测试

**文件**：`tests/sequential_tool_confirmation.rs`（新增）

1. `two_shell_execs_confirmed_sequentially`
   - 触发需要两个 `shell_exec` 的任务。
   - 断言只收到一个审批请求；确认后收到第二个。
2. `allow_always_skips_remaining_confirmations`
   - 触发两个相同的 `shell_exec`。
   - 第一个选择 `allow_always`；断言第二个工具直接执行，无第二个审批请求。
3. `qq_text_confirmation_resolves_tool`
   - 模拟 QQ 通道任务，通过文本 `2` 确认；断言工具执行并生成 follow-up LLM 请求。

### 回归测试

- `tests/multi_turn_routing.rs`：无 pending 确认的多轮任务行为不变。
- `tests/cross_channel_isolation.rs`：跨通道隔离不受影响。
- `tests/origin_channel_flow.rs`：输出路由不受影响。

## 风险与回滚

### 风险

1. **`Task` 字段增加**：所有直接构造 `Task` 的位置需初始化新字段，漏改会导致编译失败（可被编译器捕获）。
2. **QQ 前端渲染**：需要 QQ 通道前端将 `EngineEvent::ApprovalRequest` 渲染为带 `1/2/3` 选项的文本，否则用户不知道回复什么。该改动在 QQ channel 前端模块内完成，不在本设计的 ECS 核心代码中。
3. **普通多轮对话误判**：只要 `pending_confirmation_id` 被正确清除，普通 `Waiting(User)` 任务不会进入审批解析分支。

### 回滚

所有修改集中在 `src/domain/task.rs`、`src/systems/tools/dispatch.rs`、`src/systems/tools/confirmation.rs`、`src/systems/routing.rs` 及新增测试文件，可通过单次 `git revert` 回滚。

## 不引入的变更

- 不新增 `TaskStatus` 变体（复用 `Waiting(User)` + `pending_confirmation_id`）。
- 不修改 `ToolConfirmationRequestMessage` / `ToolConfirmationResponseMessage` 结构。
- 不修改 `ToolPermission` 枚举或权限评估逻辑。
- 不修改 Telegram callback 确认链路。
- 不在 `ContinueTaskMessage` 上增加字段。
