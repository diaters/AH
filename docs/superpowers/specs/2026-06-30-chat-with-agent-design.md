# chat_with_agent 工具设计

> **状态：当前有效 / 已实现**
>
> 本设计描述新增 `chat_with_agent` 内置工具，支持主任务与持久化 Agent 之间进行多轮同步对话，适用于文档评审、迭代修改等需要父子 Agent 多次往返交互的场景。`create_tasks` 工具不做修改。

## 背景与动机

### 现状

当前任务分解能力由 `create_tasks` 工具承担，其执行模型为：

- 异步创建一批子任务并立即返回 `batch_id`
- 子任务绑定新建的 `TaskScoped` Agent（无长期记忆）
- 父任务需显式调用 `wait_tasks` 才能阻塞等待结果
- 子任务单轮执行完毕即进入终态

该模型适合"分发-汇总"型任务，但无法支持需要父子 Agent 之间多次往返交互的场景，例如：

- 主任务请求子 Agent 评审文档，子 Agent 给出意见后，主任务修改文档再请子 Agent 复审
- 主任务与子 Agent 就某个方案进行多轮讨论后收敛

### 需求

新增 `chat_with_agent` 工具：

- 第一轮调用创建对话子任务并返回 `handle`
- 后续轮次通过 `handle` 继续对话
- 子任务绑定**持久化 Agent**（Persistent Agent），具备经验积累能力
- 父任务调用后**同步阻塞**，等待子 Agent 本轮回复
- 子任务生命周期跟随父任务
- 子 Agent 调用 `Confirm` 工具时，审批请求路由到父 Agent

## 设计决策

| 决策点 | 选择 | 理由 |
| --- | --- | --- |
| 工具形态 | 单工具两用 | 对 LLM 暴露接口少；`handle` 有无区分新建与继续 |
| Agent 绑定 | 名称优先，tag 兜底 | 兼顾确定性与灵活性，复用 Persistent Agent |
| Agent 实例化 | 直接复用 Persistent Agent ID | 保留 LTM 与权限配置，无需复制 |
| 交互方式 | 同步阻塞等待单轮回复 | 符合"发送-等待-再发送"的评审场景 |
| 超时 | 暂不支持 | 当前工具系统无统一单工具超时；后续可扩展 |
| 结果内容 | 仅返回子 Agent 本轮最终回复文本 | 简洁，符合 LLM 消费习惯 |
| 生命周期 | 跟随父任务，父结束时自动结束 | 无悬挂对话风险 |
| 审批路由 | 按 `task.parent_task_id` 查找父 Agent | 统一审批链路，不污染 Persistent Agent 状态 |
| 嵌套 | 允许嵌套 | 依赖现有 parent_task_id 链路控制 |
| 工具权限 | 继承 Persistent Agent 自带配置 | Persistent Agent 本身具备权限定义 |

## 工具契约

### JSON Schema

```json
{
  "name": "chat_with_agent",
  "description": "与一个持久化 Agent 开始或继续多轮对话。第一轮不传 handle，后续轮次传入 handle。",
  "parameters": {
    "type": "object",
    "properties": {
      "agent": {
        "type": "string",
        "description": "目标 Persistent Agent 名称。第一轮必填；后续若提供可用来校验。"
      },
      "agent_tags": {
        "type": "array",
        "items": {"type": "string"},
        "description": "agent 不存在时的备选匹配标签。第一轮至少提供 agent 或 agent_tags 之一。"
      },
      "message": {
        "type": "string",
        "description": "本轮要发送给子 Agent 的消息。"
      },
      "handle": {
        "type": "string",
        "description": "已有对话的 handle（即子任务 task_id）。不传表示开始新对话。"
      },
      "context": {
        "type": "string",
        "description": "仅在第一轮生效的额外系统上下文。"
      }
    },
    "required": ["message"]
  }
}
```

### 返回示例

```json
{
  "handle": "task-uuid",
  "response": "子 Agent 本轮的回复文本",
  "agent": "reviewer"
}
```

### 校验规则

- `message` 必填。
- 若未传 `handle`，则必须提供 `agent` 或 `agent_tags`。
- 若传了 `handle`，则对应子任务必须存在、带有 `ChatSession` 组件、且 `parent_task_id` 等于当前任务。
- 若传了 `handle`，则子任务状态必须为 `Waiting(ChatAgent)`。
- 命中的 Agent 必须是 `AgentKind::Persistent`。

## 核心概念

### ChatSession 组件

标记一个子任务为"对话型子任务"，并存储每轮变化的状态：

```rust
#[derive(Component, Debug, Clone)]
pub struct ChatSession {
    /// 目标对话 Agent 名称（创建时设置，不变）
    pub child_agent_name: String,
    /// 本轮父任务的 tool_call_id（每轮更新）
    pub parent_tool_call_id: String,
    /// 本轮父任务等待用的 batch_id（每轮更新）
    pub current_batch_id: Uuid,
}
```

`child_agent_name` 在子任务创建时设置后不再变化，用于工具返回值中的 `agent` 字段；
其余两个字段每轮更新。

### WaitingReason 新增变体

```rust
pub enum WaitingReason {
    // ... 已有变体
    /// chat_with_agent 子任务等待父 Agent 下一轮调用
    ChatAgent,
}
```

子任务本轮回复后进入 `Waiting(ChatAgent)`，而非 `Waiting(User)`。`user_input_routing_system` 仅匹配 `Waiting(User)`，因此用户消息不会误路由到对话子任务。

### ChatRoundReadyMessage 消息组件

```rust
/// 子任务本轮回复就绪（未完成，仅一轮回复结束）
#[derive(Debug, Clone, Component)]
pub struct ChatRoundReadyMessage {
    pub child_task_id: TaskId,
    pub parent_task_id: TaskId,
    pub parent_agent_id: AgentId,
    pub batch_id: Uuid,
    pub parent_tool_call_id: String,
    pub response: String,
    /// 目标对话 Agent 名称（用于工具返回值中的 `agent` 字段）
    pub child_agent_name: String,
}
```

注：`parent_agent_id` 与 `child_agent_name` 为实施阶段新增字段，分别用于
`AgentExecutionResult.agent_id` 和工具返回值中的 `agent` 字段。

### 子 Task 关键字段

| 字段 | 值 |
| --- | --- |
| `parent_task_id` | 父任务 ID（生命周期绑定） |
| `delegate` | Persistent Agent ID |
| `origin_channel` | 继承父任务的 `origin_channel` |
| `multi_turn` | `true` |
| `status` | 初始为 `Pending` |
| 附加组件 | `ChatSession`、`ShortTermMemory` |

## 审批路由统一

### 现状问题

现有审批链路（`src/contracts/tools.rs` 的 `DefaultToolApprovalPolicy` 与 `src/systems/tools/dispatch.rs`）依赖 `Agent.parent_id` 决定审批路由：

- 有 `parent_id` 且父 Agent 有权限 → 父 Agent 审批
- 无 `parent_id` → 用户确认

Persistent Agent 的 `parent_id` 通常为 `None`。若直接复用 Persistent Agent ID，子任务调用 `Confirm` 工具会走用户确认路径，而非父 Agent 审批。

### 统一方案

将审批路由从"按 Agent.parent_id"改为"按 Task.parent_task_id"：

1. 通过 `task.parent_task_id` 查找父 Task
2. 通过父 Task 的 `delegate` 字段获取父 Agent ID
3. 检查父 Agent 是否有该工具的 Allow 权限
4. 有 → 父 Agent 审批；无或不存在 → 用户确认

### 等价性

对 `create_tasks` 场景，TaskScoped Agent 的 `parent_id` 本身就是从父 Task 的 delegate 来的，因此新旧逻辑等价，现有行为不变。

### 收益

1. 语义统一：审批责任始终跟随任务层级
2. Persistent Agent 自然支持：无需伪造 `parent_id`
3. 消除 `ChatSession.parent_agent_id` 字段需求
4. 减少 Agent 静态状态依赖

### 修改点

| 位置 | 修改内容 |
| --- | --- |
| `src/contracts/tools.rs` `ToolApprovalPolicy` trait | `determine_approval_route` 签名增加 `task: &Task` 与 `agents: &Query<&Agent>` 参数 |
| `src/contracts/tools.rs` `DefaultToolApprovalPolicy` | 从 `agent.parent_id` 改为 `task.parent_task_id` 推导父 Agent，并检查父 Agent 是否有该工具 Allow 权限 |
| `src/systems/tools/dispatch.rs` Confirm 分支（约183-230行） | 该分支当前直接内联实现，未调用 policy trait。需同步改为：通过 `task.parent_task_id` 查父 Task → 父 Task.delegate → 父 Agent → 检查权限 |
| 边界处理 | 父 Task 不存在 / 父 Task.delegate 为 None / 父 Agent 无该工具 Allow 权限 / 顶层任务 → fallback 用户确认 |

> **注意**：`dispatch.rs` 的 Confirm 分支与 `DefaultToolApprovalPolicy` 是两套独立实现，实施时必须同时修改，不能只做其一。建议在 `dispatch.rs` 的 Confirm 分支中直接复用 `DefaultToolApprovalPolicy`，避免双份逻辑。

## 完整流程时序

### 第一轮

```text
父任务执行 → LLM 输出 tool_call(chat_with_agent)
  ↓
tool_dispatch_system 识别为 Builtin 工具，交给 chat_with_agent executor
  ↓
chat_with_agent executor:
  1. 校验入参（无 handle → 需要 agent 或 agent_tags）
  2. 匹配 Persistent Agent（名称优先，tag 兜底）
  3. 创建子 Task:
     - id = 新 UUID（即 handle）
     - content = message
     - delegate = Some(persistent_agent_id)
     - multi_turn = true
     - parent_task_id = Some(父 task id)
     - origin_channel = 父 task 的 origin_channel
     - status = Pending
  4. 为子 Task 附带 ChatSession 组件
  5. 为子 Task 附带 ShortTermMemory（独立 STM）
  6. spawn ChatRoundStartedMessage:
     - parent_task_id = 父 task id
     - child_task_id = 子 task id
     - batch_id = 新 UUID
     - parent_tool_call_id = 当前 tool_call_id
  7. 返回 ToolAction（不立即回填结果）
  ↓
chat_round_block_system（新增，复用 sub_task_batch_block_system 同款逻辑）:
  - 消费 ChatRoundStartedMessage
  - 父 task.status = Waiting(SubTaskBatch { batch_id })
  - 父 task 持有 ToolCallingState（pending_tool_call_ids 含当前 tool_call_id）
  - despawn ChatRoundStartedMessage
  ↓
Brain 分派子 task → task_dispatch_system → AgentExecutionRequest
  - 子 task 进入 Ready → 调度 Persistent Agent 执行
  ↓
Persistent Agent 处理 message → LLM 产生 Assistant 回复
  ↓
chat_round_capture_system:
  - 在 `llm_response_system` 处理 multi_turn 任务产生 Assistant 文本回复时，检查任务是否带 `ChatSession`
  - 若带 `ChatSession`：
    - 捕获回复内容
    - 子 task.status = Waiting(ChatAgent)（替代默认的 Waiting(User)）
    - spawn ChatRoundReadyMessage
    - 不进入默认的 `Waiting(User)` 分支
  - 若不带 `ChatSession`：保持原有 `Waiting(User)` 行为
  ↓
chat_round_completion_system:
  - 消费 ChatRoundReadyMessage
  - spawn ToolExecutionResultMessage:
    - tool_output: Ok(json!({ "handle", "response", "agent" }))
    - tool_call_id: Some(parent_tool_call_id)
    - 标记 ToolReturnedHookPending
  - 父 task 若有 ToolCallingState → Waiting(ToolExecution)
    否则 → Ready
  - despawn ChatRoundReadyMessage
  ↓
tool_result_system（现有）:
  - 消费 ToolExecutionResultMessage
  - 将工具结果注入父 task 的 STM（EntryRole::Tool）
  - 父 task 恢复 Ready
  - 父 Agent 下一轮 LLM 调用拿到工具返回值
```

### 后续轮次

```text
父任务执行 → LLM 输出 tool_call(chat_with_agent, { handle, message })
  ↓
chat_with_agent executor:
  1. 校验入参
  2. 查 handle 对应子 task:
     - 存在
     - 带 ChatSession 组件
     - parent_task_id == 当前父 task id
     - status == Waiting(ChatAgent)
  3. 更新 ChatSession:
     - parent_tool_call_id = 当前 tool_call_id
     - current_batch_id = 新 UUID
  4. 将 message 作为 EntryRole::User 追加到子 task STM
  5. 子 task.status = Ready
  6. spawn ChatRoundStartedMessage（batch_size=1）
  7. 父 task 进入 Waiting(SubTaskBatch)
  ↓
（同第一轮，从 Brain 分派开始）
```

### 父任务终止清理

```text
父 task 进入终态（Done/Failed）
  ↓
chat_session_cleanup_system:
  - 遍历所有带 ChatSession 组件的子 task，其 Task.parent_task_id == 父 task id
  - 对每个子 task 直接 despawn（不做中间状态标记），依赖 ECS despawn 链路级联清理关联实体
```

> 注：实现采用直接 despawn 策略，与 `create_tasks` 子任务的 Failed 标记策略不同。
> 这是因为 chat 子任务的生命周期紧密跟随父任务，父任务终止即意味着对话无条件结束，
> 无需保留审计状态。若后续需要审计追踪，可改为先标记 Failed 再交由 task_lifecycle 链路清理。

### 嵌套

子 Agent 调用 `chat_with_agent` 创建孙任务时，孙任务同样设置 `parent_task_id = 子 task id` 并附带 `ChatSession`。嵌套深度无硬限制，由 LLM 自行控制。

## 与 create_tasks 的差异

| 维度 | create_tasks | chat_with_agent |
| --- | --- | --- |
| 批次大小 | 1~N 个，支持 depends_on 依赖 | 恒定 1 个，无依赖 |
| Agent 绑定 | delegate=None，Brain 创建 TaskScoped Agent | delegate=Persistent Agent ID |
| Agent 类型 | TaskScoped（新建） | Persistent（复用） |
| Agent 记忆 | 新建 STM，无 LTM | 新建 STM，复用 LTM |
| Agent 权限 | 从父 Agent 过滤 allowed_tools | Persistent Agent 自带配置 |
| multi_turn | false | true |
| 轮次 | 单轮，完成即终态 | 多轮，回复后保持活跃 |
| 工具返回时机 | 立即返回 batch_id | 阻塞等待返回 response |
| 父任务阻塞 | 需显式调 wait_tasks | 工具调用即阻塞 |
| 结果内容 | result_summary | 本轮 response 全文 |
| 唤醒事件 | SubTaskCompletedMessage | ChatRoundReadyMessage |
| 子任务终态触发 | 自行 Done/Failed | 父任务终止时强制结束 |
| 附加组件 | SubTaskConfig | ChatSession |

### 共用流程

两者都复用：

- `WaitingReason::SubTaskBatch`（父任务等待状态）
- `ToolCallingState` + `ToolExecutionResultMessage` + `tool_result_system`（结果回填）
- `Task.parent_task_id`（生命周期绑定与审批路由）
- Brain 分派链路（`task_dispatch_system`）

`chat_with_agent` 不直接复用 `SubTaskBatchCreatedMessage` / `sub_task_batch_block_system`，因为它们与 `Vec<SubTaskDefinition>` 强绑定。改为新增 `ChatRoundStartedMessage` / `chat_round_block_system`，复用同样的阻塞语义。

## 系统清单

### 新增

| 系统 | 职责 |
| --- | --- |
| `chat_with_agent_executor` | 工具执行入口：校验、匹配 Agent、创建/复用子 Task |
| `chat_round_block_system` | 消费 ChatRoundStartedMessage，阻塞父任务为 Waiting(SubTaskBatch) |
| `chat_round_capture_system` | 在 llm_response 中捕获带 ChatSession 子任务的 Assistant 回复 |
| `chat_round_completion_system` | 消费 ChatRoundReadyMessage，回填父任务 ToolExecutionResultMessage，唤醒父任务 |
| `chat_session_cleanup_system` | 父任务终止时清理子任务 |

### 修改

| 系统 | 修改内容 |
| --- | --- |
| `tool_dispatch_system` | Confirm 分支审批路由改为按 `task.parent_task_id` |
| `DefaultToolApprovalPolicy` | 同上 |

### 不修改

- `create_tasks` 工具及其 executor
- `create_tasks` 创建的子任务行为
- `SubTaskBatchState` 结构
- `SubTaskCompletedMessage` 及 `sub_task_completion_system`
- 现有 `multi_turn` Task 的 `Waiting(User)` 行为
- `user_input_routing_system`

## 新增领域对象

| 对象 | 位置 |
| --- | --- |
| `ChatSession` 组件 | `src/domain/task.rs` 或 `src/domain/chat_session.rs` |
| `ChatRoundStartedMessage` 消息组件 | `src/domain/message.rs` |
| `ChatRoundReadyMessage` 消息组件 | `src/domain/message.rs` |
| `WaitingReason::ChatAgent` 变体 | `src/domain/message.rs` |
| `chat_with_agent` 工具定义与 executor | `src/systems/tools/builtin/chat_with_agent.rs` |

### ChatRoundStartedMessage

由于 `SubTaskBatchCreatedMessage.tasks` 字段类型为 `Vec<SubTaskDefinition>`，与 `chat_with_agent` 创建的子任务结构不同（已有 `delegate`、无 DAG 依赖），引入独立消息组件：

```rust
#[derive(Debug, Clone, Component)]
pub struct ChatRoundStartedMessage {
    pub parent_task_id: TaskId,
    pub child_task_id: TaskId,
    pub batch_id: Uuid,
    pub parent_tool_call_id: String,
}
```

由 `chat_with_agent` executor 在新建对话或继续对话时 spawn，由阻塞系统消费，将父任务置为 `Waiting(SubTaskBatch { batch_id })`。

### ChatRoundReadyMessage

```rust
#[derive(Debug, Clone, Component)]
pub struct ChatRoundReadyMessage {
    pub child_task_id: TaskId,
    pub parent_task_id: TaskId,
    pub batch_id: Uuid,
    pub parent_tool_call_id: String,
    pub response: String,
}
```

由 `llm_response_system` 在带 `ChatSession` 的子任务产生 Assistant 回复时 spawn，由 `chat_round_completion_system` 消费。使用 Component 而非 Event，是为了与现有消息传递风格（`SubTaskBatchCreatedMessage`、`SubTaskCompletedMessage` 等）保持一致。

## 错误处理

| 场景 | 处理 |
| --- | --- |
| Agent 名称和 tags 都未命中 | 返回 `ToolError::NotFound`，工具执行失败 |
| handle 对应子任务不存在 | 返回 `ToolError::NotFound` |
| 子任务不带 ChatSession | 返回 `ToolError::InvalidInput`（"handle 不是 chat_with_agent 创建的"） |
| 子任务 parent_task_id 不匹配 | 返回 `ToolError::PermissionDenied`（防止跨任务访问） |
| 子任务状态非 Waiting(ChatAgent) | 返回 `ToolError::InvalidInput`（"子任务不处于可继续对话状态"） |
| Persistent Agent 不存在 | 返回 `ToolError::NotFound` |
| 父任务终止时子任务清理 | 遍历子任务中间状态，取消进行中的 LLM 请求，清理关联的 ToolCallingState / ToolExecutionRequestMessage，标记子任务 Failed |

## 测试策略

### 单元测试

- `chat_with_agent` 参数解析与校验
- Persistent Agent 匹配逻辑（名称命中、tag 兜底、都未命中）
- `ChatSession` 组件构造与更新

### 集成测试

- 第一轮对话：创建子任务 → 子任务回复 → 父任务收到 response
- 多轮对话：第二轮 handle 校验 → 子任务唤醒 → 父任务收到新 response
- 父任务终止：子任务被清理为 Failed
- 嵌套对话：子 Agent 调用 chat_with_agent 创建孙任务
- 审批路由：子 Agent 调用 Confirm 工具，审批请求到达父 Agent
- 路由隔离：用户消息不会误路由到 Waiting(ChatAgent) 子任务

### 回归测试

- `create_tasks` 现有行为不变
- `wait_tasks` 现有行为不变
- `user_input_routing_system` 现有行为不变

## 不在本次范围

- 每轮超时机制（`timeout_secs` 参数）
- 子 Agent 本轮工具调用摘要返回
- 完整对话历史快照返回
- 对话型子任务的 `result_summary` 生成策略
