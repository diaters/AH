# Agent Loop 优化：引导式插入、后台通知拉起、中断与进度清单

## 文档信息

| 属性 | 值 |
|------|-----|
| 状态 | 当前有效 |
| 创建日期 | 2026-10-28 |
| 关联 ADR | `docs/adr/ADR-007-task-todo-list-as-progress-display.md`（Accepted） |
| 相关文档 | `docs/async-tool-bridge.md`、ECS 关系建模设计（07-26）、Plan/Evaluation 重评估设计（06-06） |

---

## 1. 背景

### 1.1 现状缺口

本节结论均已对照代码核实。

- __运行中无法补充输入__：`user_input_routing_system` 只把输入路由给同通道中处于
  `Waiting(User)` 的任务（`src/systems/routing.rs:133`）。任务处于 `Running`、
  `Waiting(Agent | ToolExecution | SubTaskBatch)` 时，用户输入会__新建一个 Task__，
  无法纠正或补充正在执行的任务。
- __没有中断__：用户只能 `/finish`（结束任务、触发终态链路）或 `/clear`（删除任务），
  不能"停下当前这一轮、保留上下文、等我重新说"。LLM 请求在 `agent_execution_system`
  中 `runtime.spawn` 后没有任何取消或作废手段（`src/systems/execution.rs`）。
- __后台工作不会唤醒任务__：
  - `shell_start` 会话结束后只能靠 LLM 主动 `shell_read` 轮询，`NativeProcessBackend`
    的退出状态不会产生事件。`WaitingReason::Session` 只出现在匹配分支中，没有任何代码设置它。
  - `create_tasks` 总会经 `sub_task_batch_block_system` 把父任务置为
    `Waiting(SubTaskBatch)`（`src/systems/transform/subtask.rs:20`），不支持
    "派出去先做别的、做完再通知"。
  - 任务空闲（`Waiting(User)`）时，没有任何机制能由系统侧事件拉起新一轮。
- __没有进度可视化__：用户在 TUI / IM 中看不到 Agent 当前进行到哪一步。

### 1.2 参考设计对照

| 能力 | Claude Code | Codex CLI | 本设计 |
|------|-------------|-----------|--------|
| 运行中输入 | 排队消息在下一个工具结果边界送达 LLM | `steer`（当前轮注入）与 queue（轮末作为下一轮） | 默认 steer；轮末仍有积压则自动续轮 |
| 中断 | `Esc` 中断当前轮，保留上下文 | `Esc` 发送 `Interrupt`，`TurnAborted` | `/stop` 与 TUI `Esc`，作废在飞请求与工具 |
| 后台任务 | 后台 shell / 子 Agent 完成后注入 `<task-notification>`，空闲时重新拉起 | 后台进程事件 | `BackgroundJob` → `TaskInbox` → 循环内注入或空闲拉起 |
| 进度清单 | `TodoWrite`，UI 渲染 | `update_plan` | `update_todos`，纯展示，不参与调度 |
| 上下文注入 | `system-reminder` 附件 | 回合上下文项 | `ContextAttachment` 请求期注入管道 |

### 1.3 目标

- 任务运行中的用户输入以__不打断__的方式送达当前任务（steer）。
- 后台工作（shell 会话、后台子任务批次）完成时通知所属任务；任务空闲时自动拉起新一轮。
- 提供保留上下文的中断能力。
- Agent 维护可见的进度清单。
- 以上能力共用__一套__注入机制，与现有 ECS 管线、异步工具桥不变量保持一致。

### 1.4 非目标

- 不做 LLM 流式输出、死循环检测、会话持久化 / resume（另行立项）。
- 不把通道上下文等现有 `system_prompt` 拼接迁入注入管道（后续可迁）。
- 不改变 `/btw`、`ask_user`、工具确认的既有路由语义。
- 不中止 provider 侧已发出的 HTTP 请求（只作废其结果，见 §6.4）。

---

## 2. 总体设计

### 2.1 核心概念

| 概念 | ECS 形态 | 职责 |
|------|----------|------|
| `TaskInbox` | Task 实体上的 Component | 暂存尚未送达 LLM 的外部输入（steer、后台通知） |
| `InboxDeliveryMessage` | 消息实体 | 投递事件；`TaskInbox` 唯一写入来源 |
| `ContextAttachment` | 值类型（非 Component） | 请求期注入项，分 Durable / Ephemeral 两类 |
| `TurnGeneration` | Task 实体上的 Component | 回合代次；`/stop` 递增，用于作废过期 LLM 结果 |
| `BackgroundJob` | 独立实体 + `JobOwner` 关系 | 登记后台工作及其完成判定 |
| `TaskTodoList` | Task 实体上的 Component | Agent 自报进度清单，仅供展示与上下文回填 |

### 2.2 数据流

```text
                        ┌────────────── 用户输入（任务活跃中）
                        │      routing（改造）
                        ▼
后台 shell 退出 ──► InboxDeliveryMessage ◄── 后台子任务批次完成
                        │
                        ▼  inbox_ingest_system（唯一写入点）
                    TaskInbox
              ┌─────────┴──────────┐
              │ 任务在循环中         │ 任务空闲 Waiting(User | BackgroundJobs)
              ▼                    ▼
 context_attachment_system     inbox_wake_system
 （LlmCompletion 请求经过时     （生成 ContinueTaskMessage，
   取出并追加到对话尾部）          走既有续轮链路）
              │                    │
              ▼                    ▼
      agent_execution_system ◄─ continue_task_system → 请求
```

两个出口对应两种状态：任务在循环中时，下一次 LLM 请求必然经过注入管道，积压随请求送达；
任务空闲时没有请求可搭车，由唤醒系统主动发起一轮。

---

## 3. ECS 融洽性分析

本节逐项说明新设计如何落在现有 ECS 约定内，以及发现的冲突和处理方式。

### 3.1 不引入新的"会话循环对象"

当前 loop 并不存在一个显式的 while 循环，而是
"消息实体 → 系统变换 → 消息实体"的帧驱动管线：
`AgentExecutionRequestMessage` → `agent_execution_system` → 通道 → `ingest_execution_results_system`
→ `llm_response_system` / `tool_calling_orchestrator_system` → 下一个请求实体。

本设计__不新增__ tokio 侧状态机，也不新增 `TurnRuntime` 之类的聚合对象，新能力全部表达为：

- 状态：挂在 Task 实体上的 Component（`TaskInbox`、`TurnGeneration`、`TaskTodoList`）。
- 事件：一次性消息实体（`InboxDeliveryMessage`、`StopTaskMessage`）。
- 变换：插入既有 `HarnessSet` 的系统。

### 3.2 单一写入点

延续异步工具桥"双账本单一修改入口""结果落地单点"的约定：

- `TaskInbox` 的 __push__ 只发生在 `inbox_ingest_system`。其他系统一律 spawn
  `InboxDeliveryMessage`，不直接 `get_mut::<TaskInbox>`。
- `TaskInbox` 的 __drain__ 只有两个出口：`context_attachment_system` 与 `inbox_wake_system`。
  二者作用于互斥的任务状态（前者要求存在该任务的 `LlmCompletion` 请求，后者要求任务空闲），
  且 drain 是 `take` 语义，同帧先后运行也不会重复投递。Bevy 调度器会因二者都声明
  `&mut TaskInbox` 而自动串行。
- `TaskTodoList` 只经 `ToolEffect::UpdateTodos` 由 `commit_tool_effects_system` 落账，
  遵循"快照进、效果出、compute/apply 切分"。

### 3.3 调度位置

| 系统 | Set | 顺序约束 | 新增/改造 |
|------|-----|----------|-----------|
| `background_job_monitor_system` | `Transform` | 节流轮询（默认 500ms），`.before(inbox_ingest_system)` | 新增 |
| `user_input_routing_system` | `Transform` | 不变 | 改造：活跃任务 → `InboxDeliveryMessage` |
| `stop_task_system` | `Transform` | `.after(command_parse_system)`、`.before(tool_calling_turn_reset_system)` | 新增 |
| `inbox_ingest_system` | `Transform` | `.after(user_input_routing_system)`、`.before(llm_response_system)` | 新增 |
| `inbox_wake_system` | `Transform` | `.after(inbox_ingest_system)`、`.before(continue_task_system)` | 新增 |
| `ingest_execution_results_system` | `Transform` | 不变 | 改造：丢弃过期代次 |
| `llm_response_system` | `Transform` | 不变 | 改造：终态约束（§7.3）、丢弃终态任务的迟到响应 |
| `sub_task_completion_system` | `Transform` | 不变 | 修复：仅解除阻塞在本批次上的父任务 |
| `context_attachment_system` | `Execution` | `.before(agent_execution_system)` | 新增 |
| `cancel_monitor_system` | `Maintenance` | 不变 | 改造：`StopRequested` 也触发取消 |
| `clear_stop_requested_system` | `Maintenance` | `.after(cancel_monitor_system)` | 新增 |
| `background_job_cleanup_system` | `Maintenance` | `.after(task_termination_system)` | 新增 |

`inbox_ingest_system` 排在 `llm_response_system` 之前：与最终回复同帧到达的后台通知
能被完成判定看到，不会在任务进入终态后才入箱而丢失。

`context_attachment_system` 放在 `Execution` 首位，是因为 `AgentExecutionRequestMessage`
由 `Transform`（工具循环 follow-up、续轮）、`Dispatch`（`dispatch_system`、
`on_message_dispatched_hook_system`）等十余处构造。放在汇合点可以只改一处，
而不必逐个修改这些构造点。Set 之间是 `.chain()` 关系，Bevy 会在其间自动插入 command
同步点，因此它能看到当帧 spawn 的全部请求实体，注入不会增加帧延迟。

### 3.4 关系建模（对齐 ADR-005）

- `BackgroundJob` 是独立实体，通过自定义关系 `JobOwner(Entity)` 指向所属 Task。
  Task 侧由 `RelationshipTarget` 自动维护 `OwnedJobs`，用于"该任务还有几个活跃后台任务"的
  O(1) 查询。
- 这是代码库首次引入自定义关系（Bevy 0.18 原生 `#[relationship]` 派生）。未使用 `linked_spawn`：
  所属 Task 被 despawn（如 `/clear`）时 Bevy 移除 Job 上的 `JobOwner`，由清理系统停止并回收孤儿 Job。
- 不复用 `ChildOf`：`ChildOf` 已承载 Task 父子层级，混入 Job 会污染 `Children` 的语义与
  既有级联逻辑。
- ADR-005 v2 已确认 `SessionHandle` 不是 ECS 实体，shell 会话的真源仍在
  `NativeProcessBackend`。`BackgroundJob::Shell` 只持有 `SessionHandleId` 作为 ECS 侧的薄句柄，
  不复制会话状态，与"shell session 真源位于 backend"的既有结论一致。

### 3.5 状态机

- 新增 `WaitingReason::BackgroundJobs`，表示任务已自然完成，但仍有后台任务未结束（见 §7.3）。
  - `frontend_output.rs` 的 `WaitingReasonKind` 映射归入 `Tool`。
  - `tool_calling_orchestrator_system` 的门控__不__包含此状态，因为此时没有挂起的工具调用。
- 所有转换经 `mark_waiting` 等方法完成，满足 `scripts/check_status_assignment.sh` 的 CI 断言。
- 不新增 `TaskStatus` 变体。

### 3.6 发现的冲突与处理

| 冲突 | 处理 |
|------|------|
| 规范已明确 `Plan` 不作为独立模块，`update_todos` 易被理解为 Plan 复活 | 定位为纯展示数据，零调度语义，见 ADR-007 |
| 路由语义变化：运行中输入从"新建任务"变为"注入当前任务" | 属于对外能力变化：同步 `README.md`、`current-state.md`；`/btw` 继续承担"开新话题" |
| `ExperienceInbox`（经验候选收件箱）与 `TaskInbox` 命名相近 | 保留 `TaskInbox`，模块文档注释明确二者无关 |
| 父任务 `Waiting(SubTaskBatch)` 期间 steer 无法立即送达 | 维持现状：积压到下一次 follow-up 请求中送达，前端回执提示 |
| 终态任务收到迟到的 LLM 响应会被拉回 `Waiting(User)`（既有缺陷，破坏终态约束） | `llm_response_system` 丢弃终态任务的任务级响应 |
| 批次完成时无条件把父任务置为 `Ready` / `Waiting(ToolExecution)`（既有缺陷，会覆盖空闲父任务） | 仅当父任务仍处于 `Waiting(SubTaskBatch{本批次})` 时解除阻塞 |

### 3.7 模块依赖

新类型集中在 `domain/loop_control.rs`（`domain/attachment.rs` 已用于消息附件，避免混名），
新系统放在 `systems/loop_control/`（`attachment`、`inbox`、`stop`、`background`），
插件注册放在 `plugins/loop_control.rs`（`LoopControlPlugin`，并入默认插件组），满足
`domain ← … ← systems ← plugins` 的单向依赖（`scripts/check_module_deps.sh`）。

---

## 4. 上下文注入管道

### 4.1 模型

```rust
/// Durable：送达后成为对话历史的一部分
pub enum InboxItem {
    UserSteer { content: String, received_at: DateTime<Utc> },
    TaskNotification { job_id: Uuid, source: String, status: String, summary: String, received_at: DateTime<Utc> },
}

pub fn render_inbox_items(items: &[InboxItem]) -> String; // <user_steer> / <task-notification> 标签块
pub fn render_todos(items: &[TodoItem]) -> String;        // 进度清单快照（Ephemeral）
```

- __Durable__：成为对话历史的一部分。追加到 `request.conversation`，同时追加到该任务
  `ToolCallingState.conversation`（否则下一次 follow-up 会从 state 重建对话，导致注入项丢失），
  并以 `EntryRole::User` 记入 `ShortTermMemory`。
- __Ephemeral__：只追加到本次请求，不写 STM。进度清单快照只在新一轮首个请求回填，
  会随首个响应的对话进入本轮 `ToolCallingState`，下一轮重新生成。

### 4.2 `context_attachment_system`

对每个 `AgentExecutionRequestMessage`：

1. 只处理 `request_kind == LlmCompletion` 且 `work_item_id == None` 的请求。
   摘要、评估、Brain 决策等内部请求不受影响。
2. 打上 `turn_generation`（读 Task 上的 `TurnGeneration`）。
3. 从 `TaskInbox` 取出全部积压，按到达顺序渲染为__一条__ User 内容并追加到对话尾部；
   对话尾部已是 User 消息时合并进去，避免出现连续多个 User 轮次。
4. 若请求是新一轮的首个请求（该任务不存在 `ToolCallingState`）且 `TaskTodoList` 非空，
   追加 `<todo_snapshot>`（Ephemeral）。
5. `conversation == None`（纯 prompt 路径）时，把渲染文本追加到 `prompt` 末尾。

追加位置始终在对话尾部，`system_prompt` 保持不变，不破坏 provider 的前缀缓存。

### 4.3 Provider 兼容性

在 Tool 消息之后紧跟一条 User 消息：

- OpenAI 兼容端点：合法。
- Anthropic：工具结果本身在 user 轮内。genai 会把它们生成为相邻的两条 user 消息，
  API 侧会合并相邻同角色消息。
- Gemini：允许。

验证：

- `tests/real_llm_smoke.rs` 的 wiremock 用例（随 CI 运行）断言 OpenAI 兼容协议的线上消息顺序为
  `user → assistant(tool_calls) → tool → user(<user_steer>)`，且响应可正常解析；
- 同文件的真实 API 用例 `steer_after_tool_result_roundtrip_is_accepted`（`#[ignore]` + 环境变量门控）
  按 `HARNESS_TEST_PROVIDER` 逐个 provider 验证真实接受度。

若某个 adapter 拒绝，在 `src/llm/genai.rs` 的 `build_chat_messages` 边界把该文本并入最后一条
tool result 作为兜底，不影响上层。

---

## 5. P1：引导式插入（Steer）

### 5.1 路由改造

`user_input_routing_system` 在既有"工具确认 → ask_user → `Waiting(User)` 续轮"三个分支
之后、"新建任务"之前，增加一个分支：

- 匹配条件：同 `origin_channel`、顶层任务（`parent_task_id == None`）、状态属于活跃集合
  非终态且不处于 `Waiting(User | AskUser)`，并要求 `multi_turn == true`（单轮任务与子任务维持新建任务）。
  有多个时取 `updated_at` 最新的一个。同通道存在空闲 `Waiting(User)` 任务时仍优先续轮。
- 动作：spawn `InboxDeliveryMessage { task_id, item: UserSteer }`，并向该通道输出一条轻量
  `SystemOutputMessage` 回执，并清零自动拉起计数。
- 既有优先分支保持不变：处于工具确认中的 `1/2/3` 输入、`ask_user` 回复仍优先匹配。

### 5.2 两种送达时机

- __循环中__：下一次工具结果 follow-up 或重试请求经过 `context_attachment_system` 时送达。
  效果等同 Codex 的 steer：不打断当前工具执行，不丢弃已完成的工作。
- __轮末仍有积压__：LLM 已给出最终文本，任务按既有逻辑进入 `Waiting(User)`，最终回复照常输出。
  下一帧 `inbox_wake_system` 发现积压，生成 `ContinueTaskMessage` 续一轮。效果等同 Codex 的 queue。

### 5.3 预算语义

- steer __不__重置 `HARNESS_MAX_TOOL_ITERATIONS` 计数，防止通过持续注入绕过软限制。
- 轮末续轮走 `ContinueTaskMessage`，与用户正常输入一样开启新的一轮预算。

### 5.4 前端

- TUI 与 IM 均以一条系统消息回执"已收到，将在当前任务的下一步送达"；送达后作为
  `<user_steer>` 块进入对话与 STM。
- 排队数量：`inbox_status_sync_system`（`Output`）经 `Changed<TaskInbox>` 单点发出
  `EngineEvent::InboxChanged { task_id, pending }`，TUI 任务行显示 `✉N 排队`，送达后归零。

---

## 6. P2：中断（`/stop`）

### 6.1 入口

- 命令：`/stop`（`UserCommand::StopCurrentTask`），作用于同通道最近的活跃顶层任务。
- TUI：`Chat` 模式下输入框为空且存在活跃任务时，`Esc` 发送 `/stop`。
  现有 `Esc` 只在审批 / 反馈模式下生效，不冲突。

### 6.2 `stop_task_system`

对 `StopTaskMessage`：

1. `TurnGeneration += 1`。
2. 给 Task 插入 `StopRequested` 标记，使 `cancel_monitor_system` 对该任务的在飞异步工具调用
   `CancellationToken.cancel()`（复用既有单点 claim 逻辑，只扩展触发条件）。
3. 清理挂起的确认：`pending_confirmation_id = None`，撤回前端审批卡片，移除 `AskUserPending`。
4. 撤回本任务排队中的工具请求（含待确认请求，并推送 `ApprovalResult{decision: "cancelled"}`
   撤回审批卡片），移除 `WaitingForTasksInfo` / `WaitingForSessionInfo`。
5. 向 STM 追加一条 `[用户中断了上一轮执行…]` 的 User 条目，让下一轮 LLM 知道上下文断点。
6. `mark_waiting(WaitingReason::User)`。随后 `tool_calling_turn_reset_system` 按既有逻辑
   清理残留的 `ToolCallingState`。
7. 子任务批次：移除批次状态，并对未完成的子任务级联 `mark_failed_reason(UserCancelled)`。
8. `TaskInbox` 中的积压__保留__，随用户下一条输入一并送达。
9. `clear_stop_requested_system` 在 `cancel_monitor_system` 之后移除 `StopRequested`。

对空闲任务（`Waiting(User)` 且无待确认请求）执行 `/stop` 只回执"当前没有正在执行的回合"。

### 6.3 过期结果作废

- `AgentExecutionRequest` / `AgentExecutionResult` 增加 `turn_generation: Option<u64>`，
  由 `context_attachment_system` 打标，`agent_execution_system` 原样带回。
- `ingest_execution_results_system` 比较代次：小于 Task 当前代次的结果直接丢弃，
  记录 `StaleExecutionResultDropped` 日志。exactly-once 仍由"结果落地单点"保证。
- 被取消的异步工具结果以 `cancelled` 错误回到 ingest。经核实，`tool_result_system` 在没有
  `ToolCallingState` 追踪时会记账后直接 despawn 结果实体，无需新增孤儿清理系统。

### 6.4 请求级中止

- `agent_execution_system` 把任务级 `LlmCompletion` 请求的 tokio `AbortHandle` 登记到
  `InflightLlmRequests` Resource（登记时顺带清理已结束句柄）。
- `stop_task_system` 中止该任务全部在飞请求：future 被 drop，provider 侧 HTTP 连接随之取消。
- 代次递增仍保留，兜底作废"中止前已回送通道"的结果。

---

## 7. P3：后台任务通知与重新拉起

### 7.1 `BackgroundJob`

```rust
#[derive(Component)]
pub struct BackgroundJob {
    pub id: Uuid,
    pub owner_task_id: TaskId,
    pub source: JobSource,       // Shell { handle_id } | SubTaskBatch { batch_id, task_ids }
    pub started_at: DateTime<Utc>,
    pub claimed: bool,           // 被 wait_tasks 显式等待时置位，不再单独发通知
}

#[derive(Component)]
#[relationship(relationship_target = OwnedJobs)]
pub struct JobOwner(pub Entity);
```

- __Shell__：`shell_start` 增加参数 `notify_on_exit`（默认 `true`）。为真时，在启动成功处
  spawn `BackgroundJob::Shell`。`background_job_monitor_system` 节流读取 backend 状态，
  会话进入 `Completed | ExitedWithError | FailedToStart` 后生成通知（退出码 + 末尾 20 行输出）；
  `Stopped`（LLM 主动 `shell_stop`）或会话已不存在时静默回收。
- __子任务批次__：经核实，`create_tasks` 本就立即返回工具结果、不阻塞当前回合，
  子任务结果此前只能经 `wait_tasks` 取回，否则丢失。因此不新增 `background` 参数，
  而是__每个批次__都登记为 `BackgroundJob::SubTaskBatch`：
  - 批次内全部子任务终态时，汇总各子任务结果投递通知；
  - 父任务若对批次内任务调用 `wait_tasks`（存在 `WaitingForTasksInfo`），Job 置为 `claimed`，
    结果走既有 `wait_tasks` 返回路径，不重复通知；
  - `create_tasks` 的工具结果与工具描述同步说明这一行为。

### 7.2 通知投递与合并

- 所有完成事件统一 spawn `InboxDeliveryMessage { item: TaskNotification }`，
  与 steer 共用同一收件箱。
- 合并：`inbox_wake_system` 对空闲任务的唤醒有 `HARNESS_NOTIFICATION_COALESCE_MS`
  （默认 300ms）去抖窗口，窗口内到达的多条通知合并为一轮。
- 通知文本格式：

```text
<task-notification source="shell" job="…" status="exited(0)">
…末尾输出…
</task-notification>
```

### 7.3 终态约束：任务终态时不允许存在后台任务

按评审确认的"禁止终态时有后台任务"，用两条规则保证不变量：
__终态 Task 的 `OwnedJobs` 为空__。

- __LLM 自然完成（非多轮任务），但仍有活跃 Job 或收件箱中有未送达通知__：不调用 `mark_done`，改为
  `mark_waiting(BackgroundJobs)`，候选结果暂存于 `DeferredResult` Component。
  Job 全部结束后，由 `inbox_wake_system` 带通知续一轮，LLM 结合后台结果重新给出最终结果。
- __`/finish`、`/clear`、父任务级联取消、任务 `Failed`__：`background_job_cleanup_system` 强制停止。
  Shell 调用 `stop_session`；子任务批次级联 `mark_failed_reason(UserCancelled)`；随后 despawn Job。
- __多轮顶层任务处于 `Waiting(User)`__：不是终态，允许 Job 存活，完成后正常唤醒。

第一条在 `llm_response_system` 的完成分支检查 `OwnedJobs` 与 `TaskInbox`；第二条是兜底，
保证任何终态路径都不会留下孤儿 Job。为使兜底可靠，`llm_response_system` 丢弃终态任务的
迟到任务级响应（如 LLM 调用在飞期间用户 `/finish`），避免终态任务被拉回 `Waiting(User)`。

子任务被唤醒时经 `continue_task_system` 走 Brain 重新派发（`DirectDelegate` 仅适用于
Persistent Agent），短期记忆保存在任务实体上，上下文不丢失。

### 7.4 自动拉起预算

- Task 上的 `AutoWakeBudget { consecutive: u32 }` 记录"无用户输入的连续自动拉起轮数"，
  收到真实用户输入（路由的 continue 或 steer 分支）时清零。
- 超过 `HARNESS_MAX_AUTO_WAKES`（默认 5）后不再拉起：只向 `output_channel` 推送一条
  "有 N 条后台通知待处理"，积压保留在收件箱，随用户下一条输入一并送达。
- 拉起的一轮使用独立的工具迭代预算（与用户输入开启的一轮一致）。

### 7.5 输出路由

拉起的轮次复用任务既有的 `TaskRoutingPolicy`。IM 来源的任务，其回复自动推回来源通道，
无需额外处理。

---

## 8. P4：进度清单（`update_todos`）

### 8.1 工具定义

- 名称 `update_todos`，`kind() == Async`（遵守"禁止新增 Sync 工具"）。
- 输入：`{ "todos": [{ "content": str, "status": "pending|in_progress|completed" }] }`，
  每次提交全量列表。
- worker 侧做纯校验：最多 30 项，`content` 非空，最多 1 项 `in_progress`。
  校验通过返回 `ToolEffect::UpdateTodos { task_id, items }`，
  由 `commit_tool_effects_system` 插入或替换 `TaskTodoList`，并发出前端事件。
- 工具描述明确：仅用于向用户展示进度；需要并行或委派执行时应使用 `create_tasks`。

### 8.2 展示

- TUI：任务状态面板（`src/tui/status.rs`）中，任务行下方渲染清单（`✓` / `▶` / `○`）。
- IM：清单变化时更新一条 `MessageKind::TaskStatus` 状态消息（发新撤旧），
  只在 `in_progress` 项变化、全部完成或清单新建/清空时推送（`progress_changed`），避免刷屏。
- 前端事件：`EngineEvent::TodoUpdated { target, task_id, items, progress_changed }`。

### 8.3 生命周期与回填

- 多轮任务跨轮保留清单；任务终态时随实体销毁；LLM 可提交空列表清空。
- 新一轮的首个请求经 §4.2 回填 `TodoSnapshot`（Ephemeral），保证短期记忆压缩后
  LLM 仍能看到当前清单。

### 8.4 边界（详见 ADR-007）

`TaskTodoList` 不参与派发、终态判定、评估或重试，只有前端输出与注入管道读取它。

---

## 9. 配置项

| 环境变量 | 默认值 | 说明 |
|----------|--------|------|
| `HARNESS_MAX_AUTO_WAKES` | `5` | 无用户输入时连续自动拉起的轮数上限 |
| `HARNESS_NOTIFICATION_COALESCE_MS` | `300` | 空闲任务唤醒的通知合并窗口 |
| `HARNESS_BACKGROUND_POLL_MS` | `500` | 后台 shell 会话状态轮询间隔 |

已同步 `docs/configuration.md` 与 `.env.example`。

---

## 10. 测试计划

单元测试与系统级测试使用 `#[cfg(test)]` + 最小 `World`，集成测试使用 mock executor。

- 路由：活跃任务输入投递到收件箱；`Waiting(User)` 任务仍走续轮；工具确认与
  `ask_user` 优先级不变；跨通道隔离；子任务不接收 steer。
- 注入：follow-up 请求携带合并后的 steer；`ToolCallingState` 与 STM 同步写入；
  Ephemeral 项不进 STM；非 `LlmCompletion` 请求不受影响；`conversation == None` 走 prompt 追加。
- 轮末积压：最终回复后自动续轮，且只续一轮。
- `/stop`：过期代次结果被丢弃；在飞 shell 被取消；审批卡片撤回；收件箱积压保留。
- 后台 shell：退出后通知；空闲任务被拉起；循环中的任务在下一边界收到通知；去抖合并。
- 后台批次：父任务不阻塞；完成后发通知；`wait_tasks` claim 后不重复通知。
- 终态约束：自然完成遇活跃 Job 时进入 `Waiting(BackgroundJobs)`；`/finish` 强制停止 Job；
  所有终态路径后 `OwnedJobs` 为空（性质测试）。
- 自动拉起预算：超过上限只通知不拉起；用户输入后清零。
- `update_todos`：校验规则；effect 落账；新一轮首个请求回填；不影响派发。
- 端到端（mock executor）：`tests/loop_steering_flow.rs` 覆盖 steer 送达、`/stop` 作废在飞结果、
  后台 shell 唤醒空闲任务、自然完成等待后台任务、`/finish` 强制停止后台 shell、进度清单回填。
- Layer 1：wiremock 线上消息形态用例（CI）+ 真实 provider 往返用例（门控，§4.3）。
- Layer 2：`tests/scenarios/steer_mid_tool_loop.toml`、`background_shell_notify.toml`
  （runner 新增 `steer` / `multi_turn` 字段，稳定态额外等待后台任务与收件箱清空），
  并以按请求内容作答的脚本化执行器做 mock 自检，随 CI 运行。

---

## 11. 实施分期

四期在同一分支按期分提交、合并为一个 PR（每期一个 Conventional Commit，可按提交回滚）。

| 期 | 内容 | 依赖 |
|----|------|------|
| 1 | 注入管道 + `TaskInbox` + steer 路由 + 轮末续轮 | — |
| 2 | `TurnGeneration` + `/stop` + TUI `Esc` + 请求级中止 | 1 |
| 3 | `BackgroundJob` + shell 通知 + 子任务批次通知 + 终态约束 + 自动拉起预算 | 1 |
| 4 | `update_todos` + 展示 + 回填 + ADR-007 转 Accepted | 1 |

---

## 12. 评审结论

1. 运行中输入默认 steer；`/btw` 保留开启新话题。
2. 保留 `TaskInbox` 命名。
3. `shell_start` 的 `notify_on_exit` 默认 `true`。
4. "禁止终态时有后台任务"：自然完成时等待后台任务结束后续轮给出最终结果；
   `/finish`、`/clear`、失败时强制停止后台任务。
5. TUI 中断键为单次 `Esc`（仅输入框为空且存在执行中的顶层任务时生效）。
