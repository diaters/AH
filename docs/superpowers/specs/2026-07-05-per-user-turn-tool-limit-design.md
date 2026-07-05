> **状态：当前有效**

# 按用户输入轮次的工具调用限制

## 背景

当前 `HARNESS_MAX_TOOL_ITERATIONS` 限制在整个任务生命周期内累计计算。当 Agent 需要在一次用户指令中完成环境准备、
依赖安装、脚本调试等多步操作时，很容易在尚未到达真正目标前就触发上限，导致任务被强制标记为 `Failed(AgentError)`。
本设计将该限制改为按“单次用户输入后的工具调用轮次”计算，并在超限时把决策权交还给用户，而不是直接失败任务。

## 目标

- 工具调用次数只在一次用户输入后的 LLM 执行周期内累计。
- 任务进入 `Waiting(User)` 等待下一轮用户输入时，计数自动重置。
- 达到上限后不再主动结束任务，而是向 LLM 返回合成 tool result，让 LLM 总结当前进展并询问用户是否
  继续。
- 父任务与子任务（`create_tasks`、`chat_with_agent` 等）保持独立计数。
- 内部 WorkItem（Summarization / ExperienceCollection 等）保持现有硬失败行为，不引入用户续杯。

## 非目标

- 不修改 `HARNESS_MAX_TOOL_ITERATIONS` 的默认值。
- 不改动除 `ToolCallingState` 以外的 LLM 重试、Token 阈值等机制。
- 不引入新的用户交互 UI（如“是否再授予 N 轮”的确认按钮），超限后依赖 LLM 文本回复让用户
  决策。

## 关键概念

- **User Turn**：从用户发送一条输入开始，到任务再次进入 `Waiting(User)` 为止的完整执行
  周期。
- **Tool Iteration**：一次 LLM 响应中包含一个或多个 tool calls，执行完成后再次请求 LLM，即为一轮
  迭代。
- **Soft Limit**：达到上限后不结束任务，而是返回错误信息给 LLM。
- **Synthetic Tool Result**：由框架生成的、未经过真实工具执行的 tool result，用于向 LLM 传达
  “预算已耗尽”。

## 数据模型改动

文件：`src/domain/tool_runtime.rs`

在 `ToolCallingState` 中新增 `turn_exhausted` 字段：

```rust
pub struct ToolCallingState {
    pub task_id: TaskId,
    pub agent_id: AgentId,
    pub pending_tool_call_ids: Vec<String>,
    pub iteration: u32,
    pub max_iterations: u32,
    /// 当前 user turn 是否已经触发过工具预算上限
    pub turn_exhausted: bool,
    pub conversation: Vec<ConversationMessage>,
    pub tools: Vec<ToolDefinition>,
    pub request_kind: AgentRequestKind,
    pub work_item_id: Option<uuid::Uuid>,
}
```

`turn_exhausted` 用于：

- 在日志中标记当前 turn 已耗尽。
- 作为辅助判断，避免在已耗尽状态下仍尝试调度真实工具（核心判断仍以 `iteration > max_iterations` 为准）。

## 状态流转

### User Turn 结束时重置 ToolCallingState

新增 System：`tool_calling_turn_reset_system`

运行阶段：在 routing / execution 之前。

行为：遍历所有 `Task`，当满足以下条件时，销毁该 `task_id` 对应的所有 `ToolCallingState`：

- `status == Waiting(WaitingReason::User)`
- `pending_confirmation_id == None`

```rust
pub fn tool_calling_turn_reset_system(
    mut commands: Commands,
    tasks: Query<&Task>,
    calling_states: Query<(Entity, &ToolCallingState)>,
) {
    for (state_entity, state) in &calling_states {
        if let Some(task) = tasks.iter().find(|t| t.id == state.task_id) {
            if task.status == TaskStatus::Waiting(WaitingReason::User)
                && task.pending_confirmation_id.is_none()
            {
                commands.entity(state_entity).despawn();
            }
        }
    }
}
```

等待工具确认时 `pending_confirmation_id` 为 `Some`，因此不会因为用户正在审批某个具体工具而误删状态。

#### System 注册位置

在 `src/systems/transform/mod.rs`（或现有的 transform SystemSet）中，将 `tool_calling_turn_reset_system` 注册到 `PreUpdate` 或
transform 阶段的早期，确保在用户输入路由、任务继续执行之前完成清理。建议放在 `task_lifecycle_system` 之后、`user_input_routing_system` 之前。

### 父子任务隔离

保持现有机制：`ToolCallingState` 始终按 `task_id` 绑定。子任务拥有独立 `task_id`，因此天然独立计数。任何后续改动不得让子任务共享父任务的 `ToolCallingState`。

## 超限后的软限制行为

### 处理 LLM 返回的工具调用请求

文件：`src/systems/transform/llm_response.rs`（约第 801 行）

当前逻辑在 `new_iteration > max_iterations` 时直接设置任务为 `Failed(AgentError)`。新逻辑改为：

```rust
if new_iteration > info.max_iterations {
    if info.work_item_id.is_some() {
        // WorkItem 保持硬失败
        task.last_error = Some(format!(
            "tool calling exceeded max iterations ({})",
            info.max_iterations
        ));
        task.status = TaskStatus::Failed(FailureReason::AgentError);
        task.updated_at = clock.0;
        commands.entity(info.entity).despawn();
        break;
    }

    // 普通任务：生成合成 tool result
    for call in calls {
        spawn_synthetic_limit_result(
            commands,
            task.id,
            result.agent_id,
            &call.id,
            info.iteration,
            info.max_iterations,
        );
    }

    // 更新 ToolCallingState，标记 turn_exhausted
    commands.entity(info.entity).despawn();
    commands.spawn(ToolCallingState {
        task_id: task.id,
        agent_id: result.agent_id,
        pending_tool_call_ids: calls.iter().map(|c| c.id.clone()).collect(),
        iteration: new_iteration,
        max_iterations: info.max_iterations,
        turn_exhausted: true,
        conversation: new_conversation,
        tools: info.tools.clone(),
        request_kind: info.request_kind.clone(),
        work_item_id: info.work_item_id,
    });

    // 不生成真实 ToolExecutionRequestMessage
    continue;
}
```

合成 tool result 的 `tool_output` 内容：

```rust
Ok(serde_json::json!({
    "exit_code": 1,
    "status": "tool_budget_exhausted",
    "output": format!(
        "[TOOL_BUDGET_EXHAUSTED] 本轮工具调用次数已达上限 ({}/{})。请总结你目前取得的进展，并向用户说明下一步需要什么，等待用户决策是否继续。",
        iteration, max_iterations
    )
}))
```

### 收集结果后允许 LLM 总结

文件：`src/systems/transform/llm_response.rs`（约第 1114 行）

当前逻辑在 `state.iteration >= state.max_iterations` 时直接失败任务。新逻辑改为：

```rust
if state.iteration >= state.max_iterations {
    if state.work_item_id.is_some() {
        // WorkItem 保持硬失败
        task.last_error = Some(format!(
            "tool calling reached max iterations ({})",
            state.max_iterations
        ));
        task.status = TaskStatus::Failed(FailureReason::AgentError);
        task.updated_at = clock.0;
    } else {
        // 普通任务：允许 LLM 再响应一次，用于总结和询问用户
        debug!(
            event = "ToolBudgetExhaustedAllowingSummary",
            task_id = %state.task_id,
            iteration = state.iteration,
            max_iterations = state.max_iterations,
            "tool budget exhausted, allowing LLM to summarize"
        );
    }
}
```

之后继续执行 follow-up LLM 请求生成逻辑。如果 LLM 再次请求工具，下一轮回到 4.1 仍会返回同样的合成错误；直到 LLM 生成文本回复，任务进入
`Waiting(User)`，`tool_calling_turn_reset_system` 会销毁 `ToolCallingState`，下轮重新开始计数。

## 错误处理与日志

- 触发软限制时记录 `ToolBudgetExhausted` DEBUG 日志，包含 `task_id`、`iteration`、`max_iterations`。
- 触发 WorkItem 硬失败时保持现有 `ToolCallingLimitExceeded` WARN 日志。
- 合成 tool result 的 `exit_code` 为 1，与普通 shell 错误一致，LLM 可通过输出前缀 `[TOOL_BUDGET_EXHAUSTED]` 识别。

## 测试策略

- 单元测试：`tool_calling_turn_reset_system` 在 `Waiting(User)` 且 `pending_confirmation_id == None` 时销毁
  `ToolCallingState`；在等待工具确认时不销毁。
- 集成测试：普通任务在单轮用户输入内进行 N 次工具迭代后，第 N+1 次请求返回合成 `TOOL_BUDGET_EXHAUSTED` 结果，任务进入 `Waiting(User)` 而不是 `Failed`。
- 集成测试：用户继续后，`ToolCallingState` 重新创建，`iteration` 从 1 开始。
- 集成测试：子任务达到上限不影响父任务状态；父任务达到上限不影响子任务。
- 集成测试：Summarization / ExperienceCollection WorkItem 达到上限仍按现有行为失败。

## 配置项

不新增配置项。继续使用现有环境变量：

| 环境变量 | 默认值 | 说明 |
|----------|--------|------|
| `HARNESS_MAX_TOOL_ITERATIONS` | `5` | 单次用户输入后，LLM 工具调用最大迭代次数 |

## 依赖与风险

- 依赖：现有 `Task::pending_confirmation_id` 字段能够正确区分“等待用户下一轮输入”和“等待工具确认”。
- 风险：如果 LLM 在收到合成错误后持续请求工具，会循环多次直到它生成文本回复。该行为符合用户要求，但日志中可能出现连续多条 `ToolBudgetExhausted` 记录。
- 回退：若后续发现软限制导致交互体验下降，可通过在 `HARNESS_MAX_TOOL_ITERATIONS` 检查处增加绝对上限（如 2 * max_iterations）来回退到半软限制。