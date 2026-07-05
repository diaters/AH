> **状态：当前有效**
>
> 评审历史：2026-07-05 第一轮评审 → 修订 → 第二轮评审 → 本版按第二轮评审意见修订。

# 按用户输入轮次的工具调用软限制

## 背景

当前实现中，`ToolCallingState` 在 LLM 产出文本回复后即被销毁（[llm_response.rs:695-702](../../../src/systems/transform/llm_response.rs#L695-L702)），任务进入 `Waiting(User)` 后下一次用户输入会触发新的 LLM 请求；当 LLM 再次返回 tool calls 时，`ToolCallingState` 以 `iteration: 1` 重新创建（[llm_response.rs:881-891](../../../src/systems/transform/llm_response.rs#L881-L891)）。因此从效果上看，`HARNESS_MAX_TOOL_ITERATIONS` 在每次文本回复后都会自动重置，已经具备"按用户输入轮次计数"的特征。

真正的问题是：**在一次用户输入内，LLM 连续多轮请求工具而不产出文本时，会在第 N 轮后触顶失败**。此时任务被直接标记为 `Failed(AgentError)`，用户没有机会了解当前进展并决定是否继续。本设计保留"按用户输入轮次计数"的现有语义，但把"达到上限即硬失败"改为"返回合成 tool result 让 LLM 总结并询问用户"，同时为极端循环场景增加绝对硬上限，避免无限消耗 LLM 调用。

## 目标

- 不改变现有计数周期：一次用户输入后，从 LLM 首次返回 tool calls 开始累计；LLM 产出文本回复或任务进入 `Waiting(User)` 后计数自然重置。
- 达到 `HARNESS_MAX_TOOL_ITERATIONS` 后，普通任务不直接失败，而是向 LLM 返回合成 tool result，让 LLM 总结当前进展并询问用户是否继续。
- 用户继续发送输入后，新轮次重新从 `iteration: 1` 开始计数。
- 父任务与子任务（`create_tasks`、`chat_with_agent` 等）保持独立计数。
- 内部 WorkItem（Summarization / ExperienceCollection 等）保持现有"硬失败"语义：不返回合成结果让用户续杯；WorkItem 超限时不得修改原任务状态，由 WorkItem 发起方自行处理缺失结果。
- 引入绝对硬上限，防止 LLM 在收到合成结果后持续请求工具造成无限循环。

## 非目标

- 不修改 `HARNESS_MAX_TOOL_ITERATIONS` 的默认值。
- 不改动除超限处理以外的 LLM 重试、Token 阈值等机制。
- 不引入新的用户交互 UI（如"是否再授予 N 轮"的确认按钮），超限后依赖 LLM 文本回复让用户决策。

## 关键概念

- **User Turn**：从用户发送一条输入开始，到任务再次进入 `Waiting(User)` 为止的完整执行周期。
- **Tool Iteration**：一次 LLM 响应中包含一个或多个 tool calls，执行完成后再次请求 LLM，即为一轮迭代。
- **Soft Limit**：达到 `max_iterations` 后不结束任务，而是返回错误信息给 LLM。
- **Hard Limit**：达到 `HARD_LIMIT_MULTIPLIER * max_iterations` 后强制结束任务，防止无限循环。
- **Synthetic Tool Result**：由框架生成的、未经过真实工具执行的 tool result，用于向 LLM 传达"本回合工具预算已耗尽"。

## 数据模型改动

文件：`src/domain/tool_runtime.rs`

`ToolCallingState` 保持现有字段不变。评审确认无需新增 `turn_exhausted`：LLM 产出文本时状态已销毁，进入 `Waiting(User)` 时也不存在需要清理的状态，因此该字段没有实际消费者。如需在日志中标记超限，直接通过日志事件 `ToolBudgetExhausted` 表达即可。

```rust
pub struct ToolCallingState {
    pub task_id: TaskId,
    pub agent_id: AgentId,
    pub pending_tool_call_ids: Vec<String>,
    pub iteration: u32,
    pub max_iterations: u32,
    pub conversation: Vec<ConversationMessage>,
    pub tools: Vec<ToolDefinition>,
    pub request_kind: AgentRequestKind,
    pub work_item_id: Option<uuid::Uuid>,
}
```

### 绝对硬上限常量

```rust
/// 绝对硬上限倍数：iteration 超过此值 × max_iterations 时强制失败任务
const HARD_LIMIT_MULTIPLIER: u32 = 2;
```

## 状态流转

### User Turn 结束时重置 ToolCallingState（安全网）

新增 System：`tool_calling_turn_reset_system`

定位：边界场景安全网，不是核心重置机制。核心重置已由 LLM 产出文本时的 `ToolCallingState` despawn 完成。

运行阶段：`Update`，在 `task_runtime.rs` 的 Transform 阶段注册，靠近 `tool_calling_orchestrator_system`。

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

`chat_with_agent` 子任务在产出文本后进入 `Waiting(ChatAgent)`（而非 `Waiting(User)`），因此不会被本 system 误删。

#### System 注册位置

在 `src/plugins/task_runtime.rs` 的 `Update` SystemSet 中注册：

```rust
// 用户输入轮次重置（安全网）
tool_calling_turn_reset_system
    .in_set(HarnessSet::Transform),
```

### 父子任务隔离

保持现有机制：`ToolCallingState` 始终按 `task_id` 绑定。子任务拥有独立 `task_id`，因此天然独立计数。任何后续改动不得让子任务共享父任务的 `ToolCallingState`。

## 超限后的软限制行为

### 绝对硬上限

在以下两处超限检查之前，先判断 `iteration > HARD_LIMIT_MULTIPLIER * max_iterations`：

- `src/systems/transform/llm_response.rs` 约第 801 行（新 iteration 创建前）
- `src/systems/transform/llm_response.rs` 约第 1114 行（结果收集后 follow-up 前）

超过绝对硬上限后，按现有行为强制失败任务，不再返回合成结果。WorkItem 同样不修改原任务状态。

### 处理 LLM 返回的工具调用请求

文件：`src/systems/transform/llm_response.rs`（约第 801 行）

当前逻辑在 `new_iteration > max_iterations` 时直接设置任务为 `Failed(AgentError)`。新逻辑改为：

```rust
if new_iteration > info.max_iterations {
    // 绝对硬上限：任何情况下都强制失败，防止无限循环
    if new_iteration > HARD_LIMIT_MULTIPLIER * info.max_iterations {
        warn!(
            event = "ToolCallingHardLimitExceeded",
            task_id = %task.id,
            iteration = new_iteration,
            max_iterations = info.max_iterations,
            "tool calling exceeded absolute hard limit"
        );
        if info.work_item_id.is_none() {
            task.last_error = Some(format!(
                "tool calling exceeded absolute hard limit ({}/{})",
                new_iteration, info.max_iterations
            ));
            task.status = TaskStatus::Failed(FailureReason::AgentError);
            task.updated_at = clock.0;
        }
        commands.entity(info.entity).despawn();
        break;
    }

    if info.work_item_id.is_some() {
        // WorkItem 保持硬失败语义，但不修改原任务状态
        warn!(
            event = "ToolCallingLimitExceeded",
            task_id = %task.id,
            work_item_id = ?info.work_item_id,
            iteration = new_iteration,
            max_iterations = info.max_iterations,
            "work item tool calling exceeded max iterations"
        );
        commands.entity(info.entity).despawn();
        break;
    }

    // 普通任务：生成合成 tool result
    debug!(
        event = "ToolBudgetExhausted",
        task_id = %task.id,
        iteration = new_iteration,
        max_iterations = info.max_iterations,
        "tool budget exhausted, returning synthetic result"
    );

    for call in calls {
        spawn_synthetic_limit_result(
            &mut commands,
            task.id,
            result.agent_id,
            &call.id,
            &call.name,
            info.iteration,
            info.max_iterations,
        );
    }

    // 更新 ToolCallingState，记录这些 tool_call_id 正在等待合成结果
    commands.entity(info.entity).despawn();
    commands.spawn(ToolCallingState {
        task_id: task.id,
        agent_id: result.agent_id,
        pending_tool_call_ids: calls.iter().map(|c| c.id.clone()).collect(),
        iteration: new_iteration,
        max_iterations: info.max_iterations,
        conversation: new_conversation,
        tools: info.tools.clone(),
        request_kind: info.request_kind.clone(),
        work_item_id: info.work_item_id,
    });

    // 不生成真实 ToolExecutionRequestMessage，避免真实工具执行
    // 任务保持在原有状态（通常是 Waiting(ToolExecution)），
    // tool_calling_orchestrator_system 入口检查允许此状态继续处理合成结果
    continue;
}
```

#### 合成结果构造

新增辅助函数，签名简化为仅传递必要参数。`AgentExecutionResult` 使用 `ToolExecution` request_kind 和空字段做最小化构造；真正给 LLM 看的语义来自 `tool_output`。

```rust
fn spawn_synthetic_limit_result(
    commands: &mut Commands,
    task_id: TaskId,
    agent_id: AgentId,
    tool_call_id: &str,
    tool_name: &str,
    iteration: u32,
    max_iterations: u32,
) {
    let tool_output = Ok(serde_json::json!({
        "exit_code": 1,
        "status": "tool_budget_exhausted",
        "output": format!(
            "[TOOL_BUDGET_EXHAUSTED] 本轮工具调用次数已达上限 ({}/{})。请总结你目前取得的进展，并向用户说明下一步需要什么，等待用户决策是否继续。",
            iteration, max_iterations
        )
    }));

    let result = AgentExecutionResult {
        task_id,
        agent_id,
        request_kind: AgentRequestKind::ToolExecution {
            tool_name: tool_name.to_string(),
        },
        result: Ok(AgentExecutionOutput {
            content: OutputContent::Text(String::new()),
            reasoning_content: None,
        }),
        prompt: String::new(),
        system_prompt: None,
        tools: vec![],
        reasoning_content: None,
        work_item_id: None,
    };

    commands.spawn((
        ToolExecutionResultMessage {
            result,
            tool_name: tool_name.to_string(),
            tool_output,
            tool_call_id: Some(tool_call_id.to_string()),
            processed: false,
            original_tool_output: None,
        },
        ToolReturnedHookPending,
    ));
}
```

说明：

- `AgentExecutionResult` 按最小化方式构造：`request_kind` 使用 `ToolExecution`（与代码库中所有其他工具结果一致），`prompt` / `system_prompt` / `tools` 为空，`work_item_id` 为 `None`。真正给 LLM 看的内容放在 `tool_output` 里。
- `tool_output` 模拟 `shell_exec` 失败格式（`exit_code: 1` + 文本前缀），使 LLM 无需特殊解析逻辑即可识别预算耗尽。
- 附加 `ToolReturnedHookPending` 标记，使合成结果进入 `on_tool_returned` hook 流水线，与代码库中其他所有 `ToolExecutionResultMessage` 构造方式一致。

### 收集结果后允许 LLM 总结

文件：`src/systems/transform/llm_response.rs`（约第 1114 行）

当前逻辑在 `state.iteration >= state.max_iterations` 时直接失败任务。新逻辑改为：

```rust
if state.iteration >= state.max_iterations {
    // 绝对硬上限
    if state.iteration > HARD_LIMIT_MULTIPLIER * state.max_iterations {
        warn!(
            event = "ToolCallingHardLimitExceeded",
            task_id = %state.task_id,
            iteration = state.iteration,
            max_iterations = state.max_iterations,
            "tool calling exceeded absolute hard limit on result collection"
        );
        // WorkItem 不修改原任务状态
        if state.work_item_id.is_none()
            && let Some(mut task) = tasks.iter_mut().find(|t| t.id == state.task_id)
        {
            task.last_error = Some(format!(
                "tool calling exceeded absolute hard limit ({}/{})",
                state.iteration, state.max_iterations
            ));
            task.status = TaskStatus::Failed(FailureReason::AgentError);
            task.updated_at = clock.0;
        }
        commands.entity(state_entity).despawn();
        continue;
    }

    if state.work_item_id.is_some() {
        // WorkItem 保持现有隔离语义：不修改原任务状态，停止 follow-up
        warn!(
            event = "ToolCallingLimitExceeded",
            task_id = %state.task_id,
            work_item_id = ?state.work_item_id,
            iteration = state.iteration,
            max_iterations = state.max_iterations,
            "work item tool calling reached max iterations"
        );
        commands.entity(state_entity).despawn();
        continue;
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

关键行为说明：

- WorkItem 分支加入 `despawn + continue`，阻止后续 follow-up 请求生成，保持硬失败语义。
- 普通任务分支**不** `despawn + continue`，代码落入后续的 follow-up LLM 请求生成逻辑。任务当前处于 `Waiting(ToolExecution)` 状态（由首次 tool calls 接收时设置，合成路径不改变此状态），`tool_calling_orchestrator_system` 入口检查（`llm_response.rs:1032-1045`）允许此状态继续处理合成结果。
- 如果 LLM 再次请求工具，下一轮回到超限检查仍会返回同样的合成错误；直到 LLM 生成文本回复，任务进入 `Waiting(User)`（`multi_turn == true` 时）或 `Done`（`multi_turn == false` 时），`ToolCallingState` 被销毁，下轮从 `iteration: 1` 重新开始。

## Hook 链路说明

合成 `ToolExecutionResultMessage` 附加 `ToolReturnedHookPending`，会进入 `on_tool_returned` hook 流水线；但不 spawn `ToolCalledHookPending` + `ToolExecutionRequestMessage`，因此**跳过 `on_tool_called` hook**。这是有意为之：预算耗尽时不应再执行真实工具，也不应给插件机会拦截或修改即将执行的工具调用。

插件若同时依赖 `on_tool_called` 和 `on_tool_returned`，需要注意合成结果只会触发后者。若插件需要在工具调用前做审计或拦截，可在 `on_tool_returned` 中检查 `tool_output` 是否包含 `tool_budget_exhausted` 状态并做相应处理。

## 错误处理与日志

| 事件 | 级别 | 触发条件 |
|------|------|----------|
| `ToolBudgetExhausted` | DEBUG | 普通任务达到软限制，返回合成结果 |
| `ToolBudgetExhaustedAllowingSummary` | DEBUG | 合成结果收集后允许 LLM follow-up |
| `ToolCallingHardLimitExceeded` | WARN | 达到绝对硬上限，强制失败 |
| `ToolCallingLimitExceeded` | WARN | WorkItem 达到软限制，硬失败 |
| `ToolCallingStateTurnReset` | DEBUG | 安全网清理残留 ToolCallingState |

合成 tool result 的 `exit_code` 为 1，与普通 shell 错误一致，LLM 可通过输出前缀 `[TOOL_BUDGET_EXHAUSTED]` 识别。

## 测试策略

- 单元测试：`tool_calling_turn_reset_system` 在 `Waiting(User)` 且 `pending_confirmation_id == None` 时销毁 `ToolCallingState`；在等待工具确认时不销毁；在 `Waiting(ChatAgent)` 时不销毁。
- 集成测试：普通任务在单轮用户输入内进行 N 次工具迭代后，第 N+1 次请求返回合成 `TOOL_BUDGET_EXHAUSTED` 结果，任务不进入 `Failed`。
- 集成测试：用户继续发送输入后，新 `ToolCallingState` 的 `iteration` 从 1 开始。
- 集成测试：子任务达到上限不影响父任务状态；父任务达到上限不影响子任务。
- 集成测试：Summarization / ExperienceCollection WorkItem 达到上限不修改原任务状态，且不生成 follow-up 请求。
- 集成测试：当 `iteration > HARD_LIMIT_MULTIPLIER * max_iterations` 时，普通任务强制失败。

## 配置项

不新增配置项。继续使用现有环境变量：

| 环境变量 | 默认值 | 说明 |
|----------|--------|------|
| `HARNESS_MAX_TOOL_ITERATIONS` | `5` | 单次用户输入后，LLM 工具调用最大迭代次数 |

绝对硬上限倍数由 `HARD_LIMIT_MULTIPLIER` 常量控制（默认 2），不暴露为配置，避免用户误设导致无限循环。若后续需要调整，再评估是否加入配置。

## 文档同步

实施完成后需同步更新：

- `docs/configuration.md`：确认第 75 行"单轮工具调用最大迭代次数"描述与本设计一致。
- `docs/current-state.md`：补充"单轮工具调用超限后返回合成结果而非直接失败"的能力说明。
- `docs/logs.md`：若新增日志事件被采纳为稳定日志规范，补充 `ToolBudgetExhausted`、`ToolCallingHardLimitExceeded` 说明。

## 依赖与风险

- 依赖：现有 `Task::pending_confirmation_id` 字段能够正确区分"等待用户下一轮输入"和"等待工具确认"。
- 依赖：`ToolExecutionResultMessage` 的消费者（`tool_result_system` 等）能够正确解析带 `tool_call_id` 的合成结果。
- 依赖：合成结果会由 `tool_result_system` 写入 STM（`result.rs:70-78`），每次超限都会留下 tool call 记录。这是可接受的——记录了预算耗尽事件，有助于事后审计。
- 风险：LLM 在收到合成错误后仍可能再次请求工具，但绝对硬上限会在 `HARD_LIMIT_MULTIPLIER * max_iterations` 处强制失败，避免无限循环。
- 风险：合成结果跳过 `on_tool_called` hook，若插件强依赖该 hook 做调用前处理，可能感知不到预算耗尽事件。
