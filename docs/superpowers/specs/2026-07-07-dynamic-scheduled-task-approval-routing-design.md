# 动态 scheduled task 审批路由与事件任务审批通道检查

> **状态：当前有效**

## 背景

2026-07-07 的运行日志（`logs/harness_2026-07-07_15-00-32.jsonl`）显示：一个由 `schedule_task` 创建的动态一次性任务触发后，执行 Agent 调用了需要用户确认的 `shell_exec`。系统在尝试推送审批请求时，因任务没有可用的审批通道，将任务标记为 `Failed(Unknown)`。

根因：`schedule_task` 工具指定的 `output_channel` 仅被写入 `TaskRoutingPolicy.output_channel`，而 `approval_channel` 为 `None`。当工具需要用户确认时，`frontend_output_system` 找不到审批目标，导致任务失败。

此外，日志中没有观察到动态一次性任务触发后从调度列表移除的显式日志（如 `DynamicTaskRemoved`），不利于运行时诊断。

## 设计目标

- **G1**：`schedule_task` 创建的动态任务触发后，其指定的 `output_channel` 同时作为审批通道，工具确认请求能正常路由到目标 IM 用户。若 `output_channel` 为 `None`，则 `approval_channel` 亦为 `None`，审批请求将按“缺少审批通道”处理，保持与当前事件任务策略一致。
- **G2**：一次性动态任务触发并从 `ScheduledTaskRegistry` / `SchedulerState.dynamic_tasks` 清理时，输出结构化日志 `DynamicTaskRemoved`。
- **G3**：任何事件任务（静态 Timer/Webhook、动态 scheduled task）的 `approval_channel` 指向**未注册**的前端时，任务应明确失败并记录原因，而不是卡在等待态或出现 `FrontendApprovalRouteMissing`。本次检查仅覆盖“frontend 类型未启用”；frontend 已注册但底层 channel 不可用（如网络/token 故障）属于另一个运行时问题，不在本次修复范围内。

## 范围

### 纳入

- `src/domain/task.rs`：`TaskRoutingPolicy::scheduled_task` 语义调整及对应测试。
- `src/triggers/scheduled_task.rs`：更新 `ScheduledTaskInfo::build_routing_policy` 相关单元测试（实现本身无需改动，仍调用 `TaskRoutingPolicy::scheduled_task`）。
- `src/systems/transform/trigger_task.rs`：一次性动态任务清理日志。
- `src/app/mod.rs`：`FrontendRegistry` 新增 `has_frontend` 辅助方法，语义为“指定类型的 frontend 已在注册表中”。
- `src/systems/frontend_output.rs`：审批请求分支增加“frontend 类型是否已注册”检查。
- 相关单元测试与集成测试。

### 不纳入

- 不新增 `schedule_task` 工具参数（如独立的 `approval_channel`）。
- 不调整静态 Timer/Webhook 的配置格式；`approval_channel` 仍为必填。
- 不验证具体 `user_id` 是否真实存在（由各前端实现自行处理）。
- 不改动 `timer_scheduler.rs` 本地 `schedules` 副本的移除逻辑。

## 根因分析

日志链（07:32:47 ~ 07:32:53）：

1. `TimerTriggered kind=scheduled:e9f2a483-...`：动态一次性任务触发。
2. `trigger_task_routing_system` 构造 `CreateTaskMessage`：
   - `origin_channel: None`
   - `routing_policy: TaskRoutingPolicy::scheduled_task(output_channel=qq, approval_channel=None)`
3. `user_message_to_task_system` 调用 `Task::from_trigger`，保持 `origin_channel: None`，`approval_channel: None`。
4. 执行 Agent 调用 `shell_exec`（默认 `Confirm` 权限）。
5. `tool_dispatch_system` 未找到有权限的父 Agent，fallback 到用户确认，生成 `ToolConfirmationRequestMessage`。
6. `frontend_output_system` 读取 `task.routing_policy.approval_channel`，发现为 `None`，按事件任务策略标记任务 `Failed(Unknown)`，日志 `FrontendApprovalRouteMissing`。

静态 Timer 的审批链路当前正常：配置中的 `approval_channel` 会经 `EventTaskRoute` 传入 `TaskRoutingPolicy::event`，`frontend_output_system` 可正常读取。但若 `approval_channel.frontend` 未启用，当前代码无法在通用层提前失败，本次一并处理。

## 方案选择

### 问题一：动态任务 output_channel → approval_channel

**推荐方案**：统一修改 `TaskRoutingPolicy::scheduled_task`，将 `output_channel` 同时作为 `approval_channel`。

理由：

- 修改点集中，所有动态任务触发路径自动获得一致行为。
- 符合当前设计直觉：动态任务没有"原始对话"，输出通道就是唯一可交互的通道。
- 不引入 `multi_turn` 语义变化（对比在 `trigger_task_routing_system` 中回填 `origin_channel` 的方案）。

### 问题二：一次性动态任务清理日志

在 `trigger_task_routing_system::cleanup_scheduled_task_if_once` 中，从 `ScheduledTaskRegistry` 和 `SchedulerState.dynamic_tasks` 移除一次性任务后，记录结构化日志 `DynamicTaskRemoved`。

### 问题三：审批通道前端启用状态检查

在 `frontend_output_system` 的审批请求分支中，新增对 `approval_channel.frontend` 是否已在 `FrontendRegistry` 中启用的检查。若未启用，标记任务 `Failed(Unknown)` 并记录 `FrontendApprovalRouteInvalid`。

## 具体修改

### `src/domain/task.rs`

```rust
impl TaskRoutingPolicy {
    /// 构造 schedule_task 动态任务的路由策略：output_channel 同时作为审批通道。
    pub fn scheduled_task(output_channel: Option<ChannelId>, approval_context: &str) -> Self {
        let approval_channel = output_channel.clone();
        Self {
            output_channel,
            approval_channel,
            approval_context: Some(approval_context.to_string()),
        }
    }
}
```

### `src/systems/transform/trigger_task.rs`

在 `cleanup_scheduled_task_if_once` 清理逻辑后增加日志：

```rust
info!(
    event = "DynamicTaskRemoved",
    kind = %kind,
    reason = "once scheduled task triggered",
    "dynamic once scheduled task removed after trigger"
);
```

### `src/app/mod.rs`

为 `FrontendRegistry` 增加：

```rust
impl FrontendRegistry {
    /// 检查指定类型的 frontend 是否已在注册表中。
    /// 注意：返回 true 仅表示该 frontend 类型已注册，不保证底层 channel 当前可用
    ///（channel 可用性由 ChannelManager 的运行时发送结果覆盖）。
    pub fn has_frontend(&self, kind: FrontendKind) -> bool {
        self.frontends.iter().any(|f| f.kind() == kind)
    }
}
```

### `src/systems/frontend_output.rs`

审批请求分支扩展为：

1. `approval_channel` 为 `None` → 现有 `FrontendApprovalRouteMissing` 失败逻辑。
2. `approval_channel` 存在但 `registry.has_frontend(channel.frontend)` 为 `false` → 标记任务 `Failed(Unknown)`，`last_error = "approval channel frontend is not enabled"`，日志 `FrontendApprovalRouteInvalid`。
3. 否则正常推送 `EngineEvent::ApprovalRequest`。

## 数据流

修复后的动态任务触发数据流：

```text
TimerScheduler -> ExternalInput::Timer
                      |
                      v
         signal_ingest_system -> TriggerTaskMessage
                      |
                      v
         trigger_task_routing_system
                      |
          kind starts with "scheduled:"
                      |
                      v
         ScheduledTaskRegistry.get(kind)
                      |
      +-> build_task_input()            +-> cleanup_scheduled_task_if_once()
      |   build_routing_policy()        |   (log DynamicTaskRemoved)
      |                                 |
      v                                 v
CreateTaskMessage {            ScheduledTaskRegistry.remove(kind)
    origin_channel: None,      SchedulerState.dynamic_tasks.retain(...)
    routing_policy: scheduled_task(
        output_channel=qq,
        approval_channel=qq,   <-- 修复后
        approval_context="scheduled task"
    )
}
                      |
                      v
         user_message_to_task_system -> Task::from_trigger
                      |
                      v
              Task entity spawned
                      |
                      v
             tool_dispatch_system
                      |
           shell_exec (Confirm)
                      |
           fallback to user confirmation
                      |
                      v
          ToolConfirmationRequestMessage
                      |
                      v
          frontend_output_system
                      |
        approval_channel = qq (registered)
                      |
                      v
          EngineEvent::ApprovalRequest -> QQ frontend
```

## 错误处理

| 场景 | 处理 | 日志 |
|---|---|---|
| 动态任务 output_channel 有效且 frontend 已注册 | 审批请求正常发送 | `FrontendOutputApprovalRequest` |
| 动态任务 output_channel 存在但 frontend 未注册 | 任务标记 `Failed(Unknown)` | `FrontendApprovalRouteInvalid` |
| 静态 Timer/Webhook approval_channel 的 frontend 未注册 | 任务标记 `Failed(Unknown)` | `FrontendApprovalRouteInvalid` |
| 事件任务 approval_channel 为 `None` | 任务标记 `Failed(Unknown)` | `FrontendApprovalRouteMissing` |
| 一次性动态任务触发后清理 | 正常清理 | `DynamicTaskRemoved` |
| frontend 已注册但底层 channel 不可用（如网络/token 故障） | 任务不因此失败；`ChannelManager.send` 记录 `ChannelSendFailed` | `ChannelSendFailed` |

## 测试

### 单元测试

- `src/triggers/scheduled_task.rs`
  - 更新 `build_routing_policy_uses_scheduled_task_constructor`：断言 `approval_channel == output_channel`。
  - 更新 `build_routing_policy_supports_no_output_channel`：断言 `approval_channel` 为 `None`。
- `src/domain/task.rs`
  - 更新 `scheduled_task_routing_policy_has_output_channel_no_approval` 为验证 `approval_channel == output_channel`。
- `src/systems/transform/trigger_task.rs`
  - 更新 `scheduled_task_route_creates_create_task_message`：断言 `routing_policy.approval_channel == routing_policy.output_channel`。
  - 验证一次性任务触发后 `ScheduledTaskRegistry` 和 `SchedulerState.dynamic_tasks` 均已移除。
- `src/systems/frontend_output.rs`
  - 新增 `approval_request_with_disabled_frontend_marks_task_failed`。
  - 新增 `scheduled_task_approval_request_routes_to_output_channel`。

### 集成测试

- `tests/schedule_task_approval_routing.rs`：通过 `schedule_task` 创建一次性任务，触发后执行 Agent 调用 `shell_exec`，验证审批请求被发送到指定 QQ 用户。
- `tests/disabled_approval_channel_fails_event_task.rs`：配置静态 Timer 的 `approval_channel` 为未启用 frontend，触发后验证任务进入 Failed 状态。

### 手动验证

复现本次日志场景：

1. 通过 QQ 通道让用户调用 `schedule_task`，`output_channel=qq`，`target=<用户ID>`，`schedule=once:<未来时间>`。
2. 等待触发。
3. 任务执行 Agent 调用 `shell_exec`。
4. 验证收到 QQ 审批请求，任务未进入 Failed。
5. 验证日志中出现 `DynamicTaskRemoved`。

## 边界说明

### `schedule_task` 工具调用 vs 任务执行中工具确认

- `schedule_task` 工具调用本身**不需要用户审批**。该工具用于设定未来执行的任务，参数校验通过后立即返回 `schedule_id` 和 `next_trigger`。
- 动态任务到点触发后，执行 Agent 在运行过程中仍可能调用需要用户确认的工具（如默认 `Confirm` 权限的 `shell_exec`）。此时系统需通过任务的 `approval_channel` 向目标 IM 用户发送审批请求。
- 本次修复解决的是后者：动态任务触发后的**执行期工具确认**路由问题，而不是 `schedule_task` 工具调用本身的审批问题。

### `has_frontend` 语义边界

- `FrontendRegistry::has_frontend` 仅检查指定类型的 frontend 是否已在注册表中，等价于“该 frontend 类型是否启用”。
- 它不检查底层 channel 是否真正可用。如果 QQ 已启用但 token 无效导致 `listen` 持续失败，`has_frontend(QQ)` 仍返回 `true`，审批请求会被尝试发送，`ChannelManager` 的发送失败会记录 `ChannelSendFailed` 日志，但任务本身不会因此进入 Failed 状态。这类运行时可用性问题属于另一个独立主题。

## 兼容性

- 本次修改不新增工具参数，不修改配置格式，不影响现有 API。
- 仅调整 `TaskRoutingPolicy::scheduled_task` 的语义：原本 `approval_channel` 为 `None`，现在与 `output_channel` 相同。所有消费 `approval_channel` 的系统都会因此受益。
