# 异步工具桥（Async Tool Bridge）

> __状态：当前有效__
>
> 本文档描述 AI Harness 异步工具桥的架构、不变量、新工具开发指南与失联兜底语义。
> 实施依据见 `docs/superpowers/plans/异步工具桥实施手册 v1（独立版）.md`；
> pilot 验收数据见 `docs/async-tool-bridge-pilot-report.md`。

## 1. 定位与背景

异步工具桥是工具执行的统一运行时：所有 `BuiltinTool::kind() == Async` 的工具请求
经「dispatch 挂起 → tokio worker 异步执行 → 通道回传 → ingest 落地」闭环，
同步工具（`kind() == Sync`）退化为「恰好很快完成的旧路径调用」。

改造前，工具在 ECS schedule 线程内同步执行，`shell_exec` 的阻塞轮询会把一帧拉长到
秒级，整个助手在工具执行期间冻结。异步桥把「不堵 ECS」从约定升级为构造——执行照抄
已验证的 LLM 桥模板（`runtime.0.spawn` + mpsc 通道回传），工具是这条桥的乘客而非
特例。

## 2. 执行时序

### 2.1 读路径（list_scheduled_tasks 等纯读工具）

```text
LLM tool-calling loop
   │  spawn ToolExecutionRequestMessage（含 tool_call_id）
   ▼
async_tool_dispatch_system（Frame N）
   │  认领 kind()==Async 的请求 → 挂起现场一次性做齐：
   │    ① 调 max_duration 钩子算 sweeper 超时（std→chrono 转换只在这里做一次）
   │    ② 克隆 OwnedToolContext 只读快照（worker 零 ECS 接触）
   │    ③ 请求实体原地改造：摘 ToolExecutionRequestMessage → 挂 ToolRequestPending + InFlightToolCall
   │  runtime.0.spawn(worker) —— worker 持 sender，catch_unwind 兜底 panic
   ▼
tokio worker（schedule 线程之外）
   │  纯函数式计算：吃 owned 快照，吐 ToolWorkerOutput::Value
   │  业务超时由 worker 侧 tokio::time::timeout 管
   ▼  sender.send(ToolAsyncResult { payload: Completed(value) })
ingest_tool_results_system（Frame N+k）—— 全桥唯一结果落地点
   │  try_recv 排空通道：
   │    挂起实体在 → spawn ToolExecutionResultMessage + despawn 挂起实体
   │    挂起实体不在 → drop + warn（晚到的重复结果）
   ▼
既有 barrier / restore（一行不改）
   │  pending_tool_call_ids 收齐 → 清空 → restore_task_after_tool 恢复任务续跑
   ▼
tool_calling_orchestrator_system
   spawn follow-up AgentExecutionRequest 让 LLM 看到工具结果
```

### 2.2 写路径（schedule_task / delete_scheduled_task 等声明式效果）

```text
async_tool_dispatch_system（Frame N）
   │  同 2.1，挂起 + spawn worker
   ▼
tokio worker
   │  纯函数式 compute：吃快照，吐 ToolWorkerOutput::Effect(ToolEffect::...)
   │  worker 不碰 ECS 状态，只声明意图
   ▼  sender.send(ToolAsyncResult { payload: Effect(effect) })
ingest_tool_results_system（Frame N+k）
   │  Effect 分流：spawn ToolEffectPending，挂起实体保留（等 commit 回送最终结果）
   ▼
commit_tool_effects_system（Maintenance set，exclusive system）
   │  逐效果 arm 调 update_scheduler_state 双资源入口原子落账
   │  apply 时刻计算「真相」（existed / next_trigger 等）
   │  把最终结果经 ToolResultSender 送回通道
   ▼
ingest_tool_results_system（Frame N+k+1）
   │  这次收到 Completed payload → spawn ToolExecutionResultMessage + despawn 挂起实体
   ▼
既有 barrier / restore
```

写路径比读路径多走一帧，但__结果落地仍是单点__——commit 只回送通道，不直接落地
`ToolExecutionResultMessage`。

## 3. 六条架构不变量

### 3.1 统一异步

所有上桥工具走同一座桥。回复协议不变——统一回复载体仍是 `ToolExecutionResultMessage`
实体，接受任何系统、任何帧 spawn 的结果。双轨期 `kind() == Sync` 的工具继续走旧路径，
但禁止新增 Sync 工具。

### 3.2 快照进、效果出（snapshot-in / effect-out）

worker 跨帧存活，不能再借用 ECS 现场。dispatch 挂起时（此刻仍有 `Res` 只读访问）把
工具需要的只读数据克隆进 `OwnedToolContext`，worker 纯函数式计算；写操作只返回声明式
`ToolEffect`，apply 阶段经 `update_scheduler_state` 落账。

__边界__：`original_request` / `task_id` / `agent_id` __不属于 worker 上下文__——它们由
挂起实体 `ToolRequestPending` 携带，供 ingest/restore 重建结果消息；`OwnedToolContext`
只装 worker 干活需要的东西（只读快照 + 全局配置 + 取消句柄）。

### 3.3 compute / apply 切分

worker 只做 compute（I/O、轮询、格式化）；改状态全部在 ECS 侧 apply 阶段
（`commit_tool_effects_system`）。`ToolEffect` 枚举是 compute 与 apply 之间的契约——
每加一个写效果 = 枚举加一个变体 + commit 加一支 arm，是有意的「慢动作」设计，不接受
绕过。

### 3.4 双账本单一修改入口

`SchedulerState` + `ScheduledTaskRegistry` 的一切修改走 `update_scheduler_state(world,
|state, registry| ...)` 单一入口，watch 只广播一次。两条账本在任何写路径下保持原子
一致——单边删除时记 `LedgerDriftOnDelete` 警告但不回滚（已 dispatched 的任务无法回收）。

### 3.5 结果落地单点 + exactly-once

`ToolExecutionResultMessage` 只能由 `ingest_tool_results_system` 产生（含 sweeper 兜底
的 error——sweeper 只发通道 + claim，不落地）。任何失联路径（panic / 超时 / 通道断开）
最终恰好落地一条结果，由「挂起实体是否还在」唯一裁决：

- 挂起实体在 → 落地结果 + despawn
- 挂起实体不在 → drop + warn（重复或迟到）

### 3.6 双超时分层

业务超时在 worker 侧（`tokio::time::timeout`），sweeper 超时由 `BuiltinTool::max_duration`
钩子推导且恒大于业务超时——间距由构造保证，不靠工具作者自觉。dispatch 挂起现场算 sweeper
超时时取 `max(shell_default_exec_timeout_secs, tool_inflight_timeout_secs)` 作为有效基数，
确保 shell_exec 等工具的业务超时 fallback 不会越过 sweeper 边界。

## 4. 失联兜底语义

三条失联路径殊途同归到 ingest 单点落地：

### 4.1 worker panic

dispatch 的 `catch_unwind` 兜底，worker 把 `Err(ExecutionFailed("worker panicked"))`
送回通道。这是快速失败路径——结果仍经 ingest 落地，挂起实体 despawn。

### 4.2 worker 内业务超时

worker 侧 `tokio::time::timeout` 失败时，worker 自己送 `Err(Timeout)` 回通道。与 4.1
同路径，ingest 落地。

### 4.3 sweeper claim（兜底的兜底）

`InFlightToolCall.timeout` 由 sweeper 在 ECS 侧判定。超时时 sweeper 做两件事：

1. 发 `Err(Timeout)` 入通道（`let _ = sender.send(...)` 发送失败静默吞掉）
2. claim：摘除 `InFlightToolCall`（不 despawn 挂起实体）

claim 后 sweeper 不再扫这个实体；挂起实体保留等 ingest 收到 error 后落地 + despawn。
若 worker 在 sweeper claim 后才回结果：

- sweeper 的 error 先到 → ingest 落地 error + despawn
- worker 的结果迟到 → 挂起实体已没 → ingest drop + warn

exactly-once 闭合。sweeper claim 是「兜底的兜底」——`catch_unwind` 与 worker 侧 timeout
在前，sweeper 只兜真失联（通道断开后恢复、worker 永久 hang 等）。

### 4.4 父任务取消（cancel_monitor）

`cancel_monitor_system` 与 sweeper 对称：sweeper 扫「超时」发 error + claim；
cancel_monitor 扫「父任务终态」调用 `CancellationToken.cancel()` + claim。

触发链：父任务终态 → cancel_monitor `token.cancel()` → worker `tokio::select!` 监听
`cancel.cancelled()` → kill 子进程 → 回 `Err(ExecutionFailed("cancelled"))` → ingest
落地。claim 后 sweeper 不再扫这个实体，挂起实体保留等 ingest 收到 cancelled error。

## 5. 新工具开发指南

### 5.1 三件套：kind / max_duration / run_async

新工具实现 `BuiltinTool` trait 时必须声明以下三项：

```rust
impl BuiltinTool for MyTool {
    fn name(&self) -> &str { "my_tool" }

    // ① 声明为异步工具（缺省 Sync，但双轨期禁止新增 Sync 工具）
    fn kind(&self) -> ToolActionKind {
        ToolActionKind::Async
    }

    // ② 可选：覆盖 sweeper 超时推导
    //    缺省返回全局 tool_inflight_timeout_secs
    //    shell_exec 等长任务工具 override 为业务超时 + margin
    fn max_duration(
        &self,
        _input: &serde_json::Value,
        tool_inflight_timeout_secs: u64,
    ) -> std::time::Duration {
        std::time::Duration::from_secs(tool_inflight_timeout_secs)
    }

    // ③ 异步执行入口：吃 owned 快照，吐 ToolWorkerOutput
    //    纯读工具吐 ToolWorkerOutput::Value(serde_json::Value)
    //    写工具吐 ToolWorkerOutput::Effect(ToolEffect::...)
    fn run_async(&self, input: serde_json::Value, ctx: OwnedToolContext) -> ToolFuture {
        Box::pin(async move {
            // ... compute ...
            Ok(ToolWorkerOutput::Value(serde_json::json!({ "result": "..." })))
        })
    }

    // execute 是 Sync 路径的遗留入口，Async 工具不会走到这里
    // 防御性返回 InternalState 错误，避免误调时静默
    fn execute(
        &self,
        _: &serde_json::Value,
        _: &ToolContext,
    ) -> Result<ToolAction, ToolError> {
        Err(ToolError::InternalState(
            "my_tool is async-only, must go through run_async".to_string(),
        ))
    }
}
```

### 5.2 读工具 vs 写工具

__读工具__（list_scheduled_tasks 等）：worker 直接返回 `ToolWorkerOutput::Value`。
快照来源由 dispatch 注入到 `OwnedToolContext`（如 `scheduler_state`、`registry`），
worker 只读不写。

__写工具__（schedule_task / delete_scheduled_task 等）：worker 不碰状态，只返回
`ToolWorkerOutput::Effect(ToolEffect::...)`。新增写效果时：

1. 在 `ToolEffect` 枚举加一个变体（`src/domain/tool_async.rs`）
2. 在 `commit_tool_effects_system` 的 `apply_effect` 加一支 arm（`src/systems/tools/effect_commit.rs`）
3. arm 内调 `update_scheduler_state` 双资源入口落账
4. apply 时刻计算「真相」（如 `existed` / `next_trigger`）回送通道

效果枚举膨胀是有意的「慢动作」设计——拒绝绕过，每个写效果都经评审显式落地。

### 5.3 OwnedToolContext 字段速查

| 字段 | 用途 | 谁填充 |
|------|------|--------|
| `scheduler_state` | `SchedulerState` 只读快照 | dispatch（按需） |
| `registry` | `ScheduledTaskRegistry` 只读快照 | dispatch（按需） |
| `tool_inflight_timeout_secs` | 全局 sweeper 缺省秒数 | dispatch |
| `shell_default_exec_timeout_secs` | shell 业务超时 fallback | dispatch |
| `backend` | `Arc<dyn SessionBackend>` 句柄 | dispatch（按需） |
| `experience_candidates` | 经验候选快照 | dispatch（按 task_id 过滤） |
| `current_task_id` | 当前任务 ID | dispatch |
| `current_origin_channel` | 当前任务 origin_channel（schedule_task 继承用） | dispatch（从 `Task.origin_channel`） |
| `cancel` | `CancellationToken` 句柄 | dispatch（worker `select!` 监听） |

`OwnedToolContext::empty_for_test(secs)` 提供测试构造捷径。

### 5.4 CancellationToken 接线

dispatch 创建 `CancellationToken`，挂两份：

- 一份到 `InFlightToolCall.cancel`（`cancel_monitor_system` 用）
- 一份到 `OwnedToolContext.cancel`（worker 在 `run_async` 内 `tokio::select!` 监听用）

长任务工具（shell_exec 等）在 worker 内必须 `select!` 监听 `cancel.canceled()`，父任务
终态时能及时 kill 子进程并回送 cancelled error。

### 5.5 测试纪律

- 一律 `#[test]`，禁止 `#[tokio::test]`（runtime 嵌套 panic）
- 异步等待用 harness 的 `block_on` / 轮询 `try_recv`
- 跑 system 一律 `world.run_system_once(...)`
- 时间源唯一：测试里一切「现在」都来自 `world.resource::<Clock>().0`，禁止 `Utc::now()`
  出现在测试体（fixture 数据如 `created_at` 例外）

## 6. 关键代码锚点

| 角色 | 文件 |
|------|------|
| 异步 dispatch（桥本体） | `src/systems/tools/async_dispatch.rs` |
| 结果 ingest（单点落地） | `src/systems/tools/ingest_tool_results.rs` |
| 效果 commit（写路径 apply） | `src/systems/tools/effect_commit.rs` |
| 失联清扫 sweeper | `src/systems/sweeper.rs` |
| 父任务取消监听 | `src/systems/tools/cancel_monitor.rs` |
| 异步类型定义 | `src/domain/tool_async.rs` |
| `BuiltinTool` trait | `src/domain/space.rs` |
| `schedule_task` 工具（写路径样例） | `src/systems/tools/builtin/schedule_task.rs` |
| `list_scheduled_tasks` 工具（读路径样例） | `src/systems/tools/builtin/scheduled/list.rs` |
| `delete_scheduled_task` 工具（写路径样例） | `src/systems/tools/builtin/scheduled/delete.rs` |

## 7. 已实现 / 待完善 / 已废弃

### 已实现

- 异步桥全套零件：`OwnedToolContext`、`ToolRequestPending`、`InFlightToolCall`、
  `ToolResultSender/Receiver`、`async_tool_dispatch_system`、`ingest_tool_results_system`、
  `sweep_inflight_tool_calls`、`cancel_monitor_system`、`ToolEffect` 枚举、
  `commit_tool_effects_system`
- `list_scheduled_tasks` / `delete_scheduled_task` / `schedule_task` 三个动态定时任务
  管理工具已上桥
- `shell_exec` 已迁移（CancellationToken 取消路径）
- Rhai 插件 `spawn_blocking` 包裹（插件 API 不变）
- 三条失联路径（panic / 超时 / 通道断开）的 exactly-once 兜底
- 父任务终态触发的 worker 取消链路
- pilot e2e 验收六条用例（`tests/async_bridge_e2e_test.rs`）
- 背压实验结论：保持无界 `mpsc::unbounded_channel`，不切换有界通道
  （详见 `docs/async-tool-bridge-pilot-report.md`）

### 待完善

- `ToolContext<'a>` 借用上下文尚未完全退役——双轨期 Sync 工具仍走 `execute` 路径，
  后续随剩余 Sync 工具迁移逐步收敛
- `channel_send` 工具维持现状（本已跨帧合规），登记为「后续候选收编项」
- 静态路由（`triggers.toml`）的 list/delete 工具另起一组命名，不混进动态任务命名空间
- `tool_inflight_timeout_secs` 的 per-tool 覆盖：当前由 `max_duration` 钩子提供，
  多工具上线后可能需要按工具类型分组配置
- 背压阈值监控落地：sweeper claim 频次 / 单 task pending 数两个指标需接入实际 metrics
  pipeline

### 已废弃

- `ToolAction::ScheduleTask` 变体已删除——写路径统一经 `ToolEffect::ScheduleTask` +
  `commit_tool_effects_system` 落账
- `schedule_task_commit_system` 专用 commit system 已删除
- `ScheduleTaskRequestMessage` / `ScheduleTaskCommitPending` 组件已删除

## 8. 参考

- 实施手册：`docs/superpowers/plans/异步工具桥实施手册 v1（独立版）.md`
- Pilot 验收报告：`docs/async-tool-bridge-pilot-report.md`
- 当前状态：`docs/current-state.md`
