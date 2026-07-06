# Timer 本地时区与 schedule_task 工具设计

> **状态：当前有效**

## 目标

1. 让现有 `triggers.toml` 中的 Timer cron 表达式按**系统本地时区**解析和触发。
2. 新增 `schedule_task` 内置工具，允许 Agent/用户动态设置未来的 AI 任务，并指定输出通道（output_channel）。

## 背景

当前 Timer scheduler 使用 `Utc::now()` 计算下一次触发时间。用户配置 `cron = "0 9 * * 1-5"` 时，实际触发的是 UTC 9:00，与本地作息不一致。同时，现有事件触发任务（Timer/Webhook）只有 `approval_channel`，没有 `output_channel`，普通文本输出无法自动路由回会话。

本设计通过统一调度器同时服务静态 Timer 和动态 `schedule_task` 任务，解决上述两个问题。

## 关键约束

- cron 表达式保持 5 字段输入（分 时 日 月 周），内部补齐为 7 字段（秒固定为 0，年固定为 `*`）。
- `schedule_task` 工具以**一次性任务为主**，可选 cron 周期性任务。
- `schedule_task` 任务的 output_channel **默认继承当前任务的 origin_channel**，允许显式覆盖。
- 动态 scheduled task 到点直接执行，**不需要审批**。
- 动态 scheduled task **仅存内存**，进程重启后丢失。

## 总体架构

```text
┌─────────────────────────────────────────────────────────────┐
│                        统一 Scheduler                          │
│  (tokio task, 使用系统本地时区计算下一次触发)                     │
└─────────────────────────────────────────────────────────────┘
                              ▲
        static routes         │         dynamic tasks
    ┌─────────────────────────┴─────────────────────────┐
    │                                                     │
triggers.toml ──reload──→ SchedulerState.static_routes  │
                                                          │
schedule_task 工具 ─────→ SchedulerState.dynamic_tasks   │
    (通过 ECS system 追加到 watch 通道)                    │
```

触发后链路保持不变：

```text
Scheduler ──→ ExternalInput::Timer ──→ signal_ingest_system
                                    ──→ trigger_task_routing_system
                                    ──→ CreateTaskMessage
                                    ──→ Brain 调度执行
                                    ──→ 结果路由到 output_channel
```

## 核心类型改动

### SchedulerState

替换现有 `TriggerConfigState`：

```rust
#[derive(Resource, Clone, Default)]
pub struct SchedulerState {
    /// 来自 triggers.toml 的静态路由
    pub static_routes: Option<SchedulerRoutes>,
    /// 由 schedule_task 工具动态添加的任务
    pub dynamic_tasks: Vec<DynamicScheduledTask>,
}

pub struct SchedulerRoutes {
    pub timer: TimerConfig,
    pub webhook: WebhookConfig,
}

pub struct DynamicScheduledTask {
    pub id: Uuid,
    pub kind: String,
    pub schedule: ScheduleSpec,
    pub created_at: DateTime<Utc>,
}

pub enum ScheduleSpec {
    Once(DateTime<Local>),
    Cron(String),
}
```

### ScheduledTaskRegistry

新增 ECS Resource，保存动态 scheduled task 的完整执行信息：

```rust
#[derive(Resource, Default)]
pub struct ScheduledTaskRegistry {
    tasks: HashMap<String, ScheduledTaskInfo>,
}

pub struct ScheduledTaskInfo {
    pub content: String,
    pub output_channel: Option<ChannelId>,
}
```

`SignalTriggerRegistry` 保持原有结构，只负责静态 webhook/timer 路由。动态任务触发时由 `trigger_task_routing_system` 先查 `SignalTriggerRegistry`，未命中再查 `ScheduledTaskRegistry`。

### ToolAction 扩展

在 `ToolAction` 枚举中新增：

```rust
pub enum ToolAction {
    // ... 现有变体
    ScheduleTask {
        id: Uuid,
        kind: String,
        content: String,
        schedule: ScheduleSpec,
        output_channel: Option<ChannelId>,
    },
}
```

`schedule_task` 工具返回该变体，由 `handle_tool_action` 统一处理。

### SignalPayload::Timer

保持不变：

```rust
pub enum SignalPayload {
    // ...
    Timer { kind: String },
}
```

`kind` 区分来源：

- 静态 Timer：`kind = "daily_summary"`（来自 triggers.toml）
- 动态任务：`kind = "scheduled:{uuid}"`

`trigger_task_routing_system` 根据 kind 查不同 registry 路由表。

## Scheduler 改动

### 时区处理

当前：

```rust
let now = Utc::now();
let next = s.upcoming(Utc).next();
```

改为：

```rust
let now = Local::now();
let next = s.upcoming(Local).next();
```

cron 解析仍使用 5 字段输入，内部补齐：

```rust
let cron_expr = format!("0 {} *", route.cron);
Schedule::from_str(&cron_expr)
```

### Watch 通道

当前 `TriggerConfigWatcher` Resource 保存 `tokio::sync::watch::Sender<TriggerConfig>`。改为保存 `tokio::sync::watch::Sender<SchedulerState>`，对应 Resource 改名为 `SchedulerStateWatcher`。

`run_timer_scheduler` 签名改为：

```rust
pub async fn run_timer_scheduler(
    input_tx: crossbeam_channel::Sender<ExternalInput>,
    mut state_rx: tokio::sync::watch::Receiver<SchedulerState>,
) -> Result<()>
```

### SchedulerState 同步机制

`SchedulerState` 同时承担两个角色：

1. **Bevy Resource**：供 ECS systems 读取和修改。
2. **watch channel payload**：tokio scheduler task 通过 watch Receiver 获取最新状态。

为避免 Resource 与 watch 之间状态不一致，所有对 `SchedulerState` 的修改都通过一个统一辅助方法完成：

```rust
fn update_scheduler_state(
    world: &mut World,
    f: impl FnOnce(&mut SchedulerState),
) {
    let mut state = world.resource_mut::<SchedulerState>().clone();
    f(&mut state);
    world.insert_resource(state.clone());
    if let Some(watcher) = world.resource::<SchedulerStateWatcher>().0.as_ref() {
        let _ = watcher.send(state);
    }
}
```

**reload 流程**：

1. 读取当前 `SchedulerState` Resource。
2. 重建 `SignalTriggerRegistry`。
3. 用新的 `TriggerConfig` 构造新的 `SchedulerRoutes` 并赋值给 `state.static_routes`。
4. **保留** `state.dynamic_tasks` 不变。
5. 调用 `update_scheduler_state` 写入并通知 scheduler。

**schedule_task 工具执行流程**：

1. `tool_dispatch_system` 调用 `handle_tool_action`，对 `ToolAction::ScheduleTask` 生成一个 `ScheduleTaskRequestMessage`。
2. 新增 `schedule_task_commit_system` 消费该 message，调用 `update_scheduler_state`：
   - 向 `dynamic_tasks` 追加 `DynamicScheduledTask`。
   - 向 `ScheduledTaskRegistry` 插入 `ScheduledTaskInfo`。

### 执行顺序保证

`schedule_task_commit_system` 与 `reload_triggers_system` 都通过 `update_scheduler_state` 修改 `SchedulerState`：

- `update_scheduler_state` 采用"读-改-写"模式，每次基于当前 Resource 状态生成新状态。
- Bevy ECS 在同一 schedule 内按注册顺序顺序执行，不存在并发写。
- 无论两个 system 谁先执行，结果都是基于最新状态追加/保留 `dynamic_tasks`，不会丢失已添加的动态任务。

### 启动策略

为了让 `schedule_task` 工具在没有 `triggers.toml` 的情况下也能工作，timer scheduler **始终启动**。当未配置 `triggers.toml` 时，`SchedulerState.static_routes` 为 `None`，`dynamic_tasks` 为空；schedule_task 工具仍可向 `dynamic_tasks` 追加任务。

### 输入源统一

`run_timer_scheduler` 改为 watch `SchedulerState`。每次 state 更新时：

1. 从 `static_routes.timer.routes` 构建 cron schedules。
2. 从 `dynamic_tasks` 构建一次性或 cron schedules。
3. 合并为统一的 schedule 列表。
4. 重新计算下一次触发时间并 sleep。

### 一次性任务触发与清理

一次性任务直接比较 `DateTime<Local>`：

```rust
for task in dynamic_tasks {
    if let ScheduleSpec::Once(at) = &task.schedule {
        if *at <= now {
            external_input_tx.send(ExternalInput::Timer { ... })?;
            // 从 dynamic_tasks 移除
        }
    }
}
```

Cron 动态任务与普通静态 Timer 一样用 `s.upcoming(Local)` 处理，触发后不移除。

## schedule_task 工具设计

### 工具 Schema

```json
{
  "name": "schedule_task",
  "description": "安排一个未来由 AI 执行的任务。支持一次性触发或按 cron 周期触发，结果会发送到指定输出通道。schedule 字段格式：once:<ISO 8601 时间> 或 cron:<5字段 cron 表达式>。",
  "parameters": {
    "type": "object",
    "properties": {
      "content": {
        "type": "string",
        "description": "任务要执行的提示词/内容"
      },
      "schedule": {
        "type": "string",
        "description": "调度表达式。一次性: 'once:2026-07-07T09:00:00' 或 'once:2026-07-07T09:00:00+08:00'；周期性: 'cron:0 9 * * 1-5'（5字段：分 时 日 月 周）"
      },
      "output_channel": {
        "type": "string",
        "enum": ["telegram", "qq", "feishu"],
        "description": "可选，显式指定输出通道类型"
      },
      "target": {
        "type": "string",
        "description": "可选，输出通道内的目标标识（对应 ChannelId.user_id，如 Telegram 的 chat_id）；省略时继承当前任务的 origin_channel.user_id"
      }
    },
    "required": ["content", "schedule"]
  }
}
```

### 执行逻辑

1. 解析 `schedule` 字段：
   - 以 `once:` 开头，解析后续为 `DateTime<Local>`（带偏移按偏移解析，无偏移按系统本地时区解释）。
   - 以 `cron:` 开头，解析后续为 5 字段 cron 表达式。
   - 其他前缀返回错误。
2. 构造 `output_channel`：
   - 若提供了 `output_channel` 参数，使用它作为 `ChannelId.frontend`；若同时提供了 `target`，作为 `ChannelId.user_id`；否则 `user_id` 继承当前任务的 `origin_channel.user_id`。
   - 若未提供 `output_channel`：从当前任务的 `origin_channel` 完整继承；若当前任务无通道，返回错误。
3. 生成唯一 `id` 和 `kind = "scheduled:{id}"`。
4. 返回 `ToolAction::ScheduleTask { ... }`。
5. `handle_tool_action` 中新增分支，把任务追加到 `SchedulerState.dynamic_tasks`、把 content/output_channel 写入 `ScheduledTaskRegistry`，并通过 watch sender 通知 scheduler。这三步在同一个 ECS system 中顺序执行，保证 ECS 内部状态一致。

### 返回结果

成功：

```json
{
  "status": "scheduled",
  "schedule_id": "550e8400-e29b-41d4-a716-446655440000",
  "kind": "scheduled:550e8400-e29b-41d4-a716-446655440000",
  "next_trigger": "2026-07-07T09:00:00+08:00"
}
```

错误：

```json
{
  "status": "error",
  "error_code": "invalid_schedule_prefix",
  "error": "schedule must start with 'once:' or 'cron:'"
}
```

错误码列表：

| 错误码 | 含义 |
|--------|------|
| `missing_schedule` | 未提供 `schedule` 字段 |
| `invalid_schedule_prefix` | `schedule` 前缀不是 `once:` 或 `cron:` |
| `invalid_once_time` | `once:` 后的时间格式无效 |
| `invalid_cron` | `cron:` 后的表达式解析失败 |
| `missing_output_channel` | 未提供 output_channel 且当前任务无 origin_channel |
| `past_once_time` | `once:` 指定的时间已经过去 |
| `scheduler_unavailable` | scheduler watch 通道不可用 |

### 权限

默认 `ToolPermission::Allow`。结果发回用户自己的通道，无需审批。

## 任务触发与路由

动态 scheduled task 触发时，scheduler 发送：

```rust
ExternalInput::Timer {
    source: SignalSource("timer".to_string()),
    kind: "scheduled:{id}".to_string(),
}
```

`signal_ingest_system` 生成 `TriggerTaskMessage`，`trigger_task_routing_system` 根据 kind 查找：

- 先在 `SignalTriggerRegistry.timer_routes` 中查静态 Timer 路由
- 未命中则在 `ScheduledTaskRegistry` 中查动态 scheduled task
- reload triggers.toml 会重建 `SignalTriggerRegistry`，但不会影响 `ScheduledTaskRegistry`；动态任务查找因此不受 reload 影响
- 找到后构造 `CreateTaskMessage`：

```rust
CreateTaskMessage {
    content,
    origin_channel: None, // 事件触发任务无来源会话
    routing_policy: TaskRoutingPolicy {
        output_channel,
        approval_channel: None,
        approval_context: Some("scheduled task".to_string()),
    },
}
```

## 错误处理与边界情况

### 参数错误

| 场景 | 返回 |
|------|------|
| 未提供 `schedule` | `error_code: missing_schedule` |
| `schedule` 前缀无效 | `error_code: invalid_schedule_prefix` |
| `once:` 后的时间格式无效 | `error_code: invalid_once_time` |
| `cron:` 后的表达式解析失败 | `error_code: invalid_cron` |
| 未提供 `output_channel` 且当前任务无通道 | `error_code: missing_output_channel` |
| `once:` 指定的时间已经过去 | `error_code: past_once_time` |

### 运行时错误

| 场景 | 处理 |
|------|------|
| scheduler watch 通道发送失败 | 记录错误日志，工具返回 `scheduler_unavailable`，任务不加入 registry |
| 动态任务触发时找不到 ScheduledTaskRegistry 记录 | 记录 `ScheduledTaskNotFound` 警告，丢弃该次触发 |
| cron 动态任务 schedule 解析异常 | 记录警告，跳过该任务，不影响其他任务 |
| 一次性任务触发后 | 同时从 `SchedulerState.dynamic_tasks` 和 `ScheduledTaskRegistry` 移除对应条目，避免内存累积 |
| 进程重启 | 所有动态任务丢失（符合"内存中即可"约束） |

### triggers.toml reload 边界

- reload 只重建 `static_routes`，`dynamic_tasks` 原样保留。
- 当前动态任务 kind 使用 `scheduled:{uuid}` 前缀，不会与静态 Timer kind 冲突。未来若支持用户自定义动态 kind，此条款保证动态任务优先。
- reload 成功后 scheduler 立即收到新的 `SchedulerState`，重新计算触发时间。

## 测试策略

### 单元测试

1. **cron 本地时区解析**：在已知时区下构造 `Schedule`，验证 `upcoming(Local)` 返回本地时间。
2. **`SchedulerState` 合并**：静态 routes + 动态 tasks 合并后正确排序；reload 只替换 static routes。
3. **schedule_task 参数解析**：覆盖有效/无效 `schedule`（`once:` / `cron:`）、output_channel 继承与覆盖、target 字段映射。

### 集成测试

1. **扩展 `tests/triggers_timer_scheduler.rs`**：验证 cron 按本地时区触发；验证动态 cron 任务触发。
2. **新增 `tests/schedule_task_tool.rs`**：
   - 调用工具后任务加入 `SchedulerState`。
   - 模拟时间推进，到期后生成 `CreateTaskMessage`。
   - 验证 `routing_policy.output_channel` 正确。
   - 验证一次性任务触发后从 `dynamic_tasks` 移除。
3. **新增 `tests/scheduled_task_local_time.rs`**：固定系统时区，验证 cron 在本地时区触发。

### 端到端验证

1. 在 Telegram/QQ 对话中调用 `schedule_task`。
2. 等待到点后收到 AI 执行结果。
3. 验证 cron 任务周期性触发。

## 未引入的能力

以下能力不在本次范围内，避免范围蔓延：

- 秒级 cron 调度（秒字段仍固定为 0）。
- 动态任务持久化（重启丢失）。
- 已调度任务的查询、取消、修改。
- 多个输出通道同时发送。
