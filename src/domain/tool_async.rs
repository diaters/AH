//! 异步工具桥的领域类型。
//!
//! 本模块承担异步工具桥的全部「领域契约」定义：
//! - 通道消息：`ToolWorkerPayload` / `ToolAsyncResult` / `ToolResultReceiver`（发送端归属 contracts）
//! - 挂起实体与在飞标记：`ToolRequestPending` / `InFlightToolCall`
//! - 声明式写效果：`ToolEffect` / `ToolEffectPending`
//! - Owned 上下文与调度快照：`OwnedToolContext` / `SchedulerStateSnapshot`
//!
//! ## 结果落地单点原则
//!
//! `ToolExecutionResultMessage` 只能由 ingest 系统产生；sweeper 只发通道 + claim
//! （摘除 `InFlightToolCall`），不落地不 despawn。错误侧直接用 `ToolError`
//! （与 `ToolExecutionResultMessage.tool_output` 同型），ingest 落地零转换。
//!
//! ## 效果与值同通道
//!
//! worker 把最终值（`Completed`）或声明式效果（`Effect`）塞进同一个
//! `ToolAsyncResult.payload`，ingest 按 payload 枚举分流：值直接产
//! `ToolExecutionResultMessage`，效果 spawn 一个 `ToolEffectPending` 实体
//! 交给 `commit_tool_effects_system` 应用。

use std::sync::Arc;

use bevy_ecs::prelude::{Component, Resource};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::domain::SessionBackend;
use crate::domain::{AgentExecutionRequest, ChannelId, TaskId, ToolError};
use crate::domain::{ScheduleDefinition, ScheduleExpr, ScheduleSelector};

// ============ 通道消息 ============

/// worker 回传给 ECS 的载荷。
///
/// 两个变体共用同一个通道：值（含错误）走 `Completed`，声明式写效果走 `Effect`。
/// ingest 按 payload 枚举分流。
#[derive(Debug, Clone)]
pub enum ToolWorkerPayload {
    /// 工具执行完毕，可直接喂给 LLM 的结果（错误侧是 `ToolError`，
    /// 与 `ToolExecutionResultMessage.tool_output` 同型，ingest 零转换）。
    Completed(Result<serde_json::Value, ToolError>),
    /// 写路径效果，交 `commit_tool_effects_system` 应用后再产最终结果。
    Effect(ToolEffect),
}

/// worker 回传给 ECS 的异步结果。
///
/// 一条 `ToolAsyncResult` 对应一次工具调用的终态（或终态前的一次副作用）。
#[derive(Debug, Clone)]
pub struct ToolAsyncResult {
    /// LLM Tool Call ID（barrier 关联键）。
    pub tool_call_id: String,
    pub payload: ToolWorkerPayload,
}

impl ToolAsyncResult {
    /// 构造一条 `Completed` 结果。
    pub fn completed(
        tool_call_id: impl Into<String>,
        result: Result<serde_json::Value, ToolError>,
    ) -> Self {
        Self {
            tool_call_id: tool_call_id.into(),
            payload: ToolWorkerPayload::Completed(result),
        }
    }

    /// 构造一条 `Effect` 结果（声明式写效果，由 commit 系统落账）。
    pub fn effect(tool_call_id: impl Into<String>, effect: ToolEffect) -> Self {
        Self {
            tool_call_id: tool_call_id.into(),
            payload: ToolWorkerPayload::Effect(effect),
        }
    }
}

/// worker → ECS 通道的接收端，作为 Resource 注入 World。
///
/// 持有 `UnboundedReceiver`：唯一持有，因此 ingest 系统对它有排他访问权。
#[derive(Resource)]
pub struct ToolResultReceiver(pub mpsc::UnboundedReceiver<ToolAsyncResult>);

// ============ 挂起实体与在飞标记 ============

/// 挂起的工具请求。dispatch 创建、ingest despawn。
///
/// `original_request` 用 `Arc` 共享：ingest 重建 `ToolExecutionResultMessage`
/// 时零克隆读取完整字段（task_id / agent_id / request_kind / ...）。
#[derive(Component, Clone)]
pub struct ToolRequestPending {
    /// LLM Tool Call ID（与 `ToolAsyncResult.tool_call_id` 关联）。
    pub tool_call_id: String,
    /// 工具名（重建结果消息时用于日志与权限审计）。
    pub tool_name: String,
    /// 原始请求（重建 `ToolExecutionResultMessage` 的完整字段来源）。
    pub original_request: Arc<AgentExecutionRequest>,
    /// 工具入参：watchdog 取证与后台调用重试使用。
    pub tool_input: serde_json::Value,
}

/// 在飞标记。sweeper 扫描对象。
///
/// 被claim（超时处理中）时摘除本组件，实体保留到 ingest 落地结果后才 despawn
/// ——保证「结果落地」与「despawn」是同一动作，不会出现结果丢失或重复落地。
///
/// `cancel` 字段让 `cancel_monitor_system` 在父任务终态时通过同实体的
/// `ToolRequestPending.original_request.task_id` 找到本实体并触发取消——
/// worker 内 `tokio::select!` 监听 `cancel.cancelled()` 后 kill 子进程。
#[derive(Component, Debug, Clone)]
pub struct InFlightToolCall {
    /// 调用发起时间（来自 `Clock`，全局唯一时间源）。
    pub started_at: DateTime<Utc>,
    /// 调用超时阈值（worker `max_duration` 推导得出）。
    pub timeout: ChronoDuration,
    /// 取消令牌。dispatch 创建并 clone 一份给 worker（经 `OwnedToolContext`）；
    /// `cancel_monitor_system` 在父任务终态时调用 `cancel.cancel()`。
    pub cancel: CancellationToken,
    /// 超过该时刻仍未返回则自动转后台（`None` 表示该调用不转后台）。
    ///
    /// dispatch 仅为可后台化工具（`BuiltinTool::backgroundable`）的任务级调用填写，
    /// 且必须早于失联超时；转后台由 `tool_auto_background_system` 执行。
    pub background_at: Option<DateTime<Utc>>,
}

/// 已转后台的工具调用：LLM 已收到 `running_in_background` 合成结果，
/// 最终结果不再回填工具循环，而是作为 `<task-notification>` 投递到所属任务收件箱。
///
/// 挂在挂起实体（`ToolRequestPending`）上，exactly-once 仍由挂起实体是否存在裁决。
#[derive(Component, Debug, Clone)]
pub struct BackgroundedToolCall {
    /// 对应 `BackgroundJob.id`，也是暴露给 LLM 的 `job_id`。
    pub job_id: uuid::Uuid,
    /// 转后台时刻。
    pub detached_at: DateTime<Utc>,
    /// 结果到达时不再通知（Agent 主动停止或被重试取代）。
    pub silent: bool,
    /// 附在通知开头的说明（如 watchdog 取消原因）。
    pub note: Option<String>,
}

// ============ 声明式写效果 ============

/// worker 声明式写效果：交由 `commit_tool_effects_system` 落账。
///
/// 写路径工具（如 schedule_task 的取消语义）由 worker 声明意图，
/// 主 ECS 线程在 ingest 阶段统一应用，避免 worker 直接 mutate World。
///
/// 新增写效果 = 加一个变体 + commit 加一支 arm。
#[derive(Debug, Clone)]
pub enum ToolEffect {
    /// 删除指定的动态定时任务（如周期任务的「停掉」语义）。
    DeleteScheduledTask {
        /// 按 kind（`scheduled:<uuid>`）或名称定位。
        selector: ScheduleSelector,
    },
    /// 创建一次性或周期性动态定时任务（schedule_task 工具上桥后走此效果）。
    ///
    /// worker 声明意图，`commit_tool_effects_system` 经 `update_scheduler_state`
    /// 统一入口落账为一条 `ScheduleDefinition`，watch 一次广播。名称冲突、`next_trigger`
    /// 等「apply 时刻才知道的真相」由 commit 判定与计算，与 `DeleteScheduledTask::existed` 同律。
    ScheduleTask {
        /// 定义 ID（由 worker 生成），kind 派生为 `scheduled:<id>`
        id: uuid::Uuid,
        /// 用户可读名称；缺省时由定义取 id 前 8 位
        name: Option<String>,
        /// 任务内容/提示词
        content: String,
        /// 调度表达式（once 或 cron）
        schedule: ScheduleExpr,
        /// 输出通道（显式指定或从当前任务继承）
        output_channel: Option<ChannelId>,
        notify: crate::domain::NotifyPolicy,
        report_of: Option<uuid::Uuid>,
    },
    /// 修改指定的动态定时任务（内容、调度、启停），可选立即手动运行一次。
    ///
    /// 定位与修改都在 apply 时刻进行：worker 只校验入参，任务是否存在由 commit 判定。
    UpdateSchedule {
        /// 按 kind 或名称定位。
        selector: ScheduleSelector,
        /// 新的任务提示词。
        content: Option<String>,
        /// 新的调度表达式。
        schedule: Option<ScheduleExpr>,
        /// 启用（恢复）或暂停。
        enabled: Option<bool>,
        notify: Option<crate::domain::NotifyPolicy>,
        /// 缺省不改，Some(None) 清除，Some(Some(id)) 替换。
        report_of: Option<Option<uuid::Uuid>>,
        /// 是否立即手动运行一次（不受暂停影响，仍受重叠规则约束）。
        run_now: bool,
    },
    /// 替换任务进度清单：由 update_todos 工具声明，commit 在主线程写入 `TaskTodoList`。
    UpdateTodos {
        task_id: TaskId,
        items: Vec<crate::domain::TodoItem>,
    },
}

/// 效果待应用实体。ingest 收到 `Effect` payload 时 spawn，
/// `commit_tool_effects_system` 消费后 despawn。
#[derive(Component, Debug, Clone)]
pub struct ToolEffectPending {
    /// 关联的 Tool Call ID（用于 commit 后产最终结果消息）。
    pub tool_call_id: String,
    /// 待应用的效果。
    pub effect: ToolEffect,
}

// ============ Owned 上下文与快照 ============

/// worker 的只读上下文。
///
/// 与 `ToolContext<'a>`（borrowed，sync 路径用）相对，`OwnedToolContext`
/// 在 worker 内的 `'static` 上下文中可用——异步 dispatch 把所需状态从
/// ECS 抓一份快照过来丢给 worker，worker 不持有任何 borrowed ECS 引用。
///
/// 不含 `original_request`（由挂起实体 `ToolRequestPending` 携带）。
///
/// `backend` + `cancel` 字段是 shell_exec 上桥引入：worker 通过 `backend`
/// 拿到 `Arc<dyn SessionBackend>` 句柄调用 `exec_with_cancel`，通过 `cancel`
/// 监听父任务取消信号。`Option<Arc<...>>` 让不需要 backend 的工具（如
/// list_scheduled_tasks）零改动。
#[derive(Debug, Clone)]
pub struct OwnedToolContext {
    /// 调度状态快照（需要读定时任务的工具由 dispatch 填充）。
    pub scheduler_state: Option<Arc<SchedulerStateSnapshot>>,
    /// 全局失联超时（秒）—— sweeper 推导 max_duration 的全局缺省。
    pub tool_inflight_timeout_secs: u64,
    /// `shell_exec` 默认业务超时（秒）——入参 `timeout_secs` 缺省时的 fallback。
    /// 与 `HarnessConfig::shell_default_exec_timeout_secs` 同值，由 dispatch 注入。
    pub shell_default_exec_timeout_secs: u64,
    /// Session backend 句柄。shell_exec 等 native 进程工具由 dispatch
    /// 从 `Res<NativeProcessBackend>` clone 一份填入；不需要 backend 的
    /// 工具保持 `None`。
    pub backend: Option<Arc<dyn SessionBackend>>,
    /// 当前任务 ID。dispatch 从 `request.request.task_id` 取，供 worker
    /// 在日志或快照关联场景使用；不需要的工具保持 `None`。
    pub current_task_id: Option<TaskId>,
    /// 实际发起工具调用的 Agent，由 dispatch 从请求注入。
    pub current_agent_id: Option<crate::domain::AgentId>,
    /// 当前任务的 `origin_channel`，schedule_task 等需要继承输出通道的工具
    /// 由 dispatch 从 `Task.origin_channel` 读取并注入。未显式指定 `output_channel`
    /// 时用作 fallback；不需要继承通道的工具保持 `None`。
    pub current_origin_channel: Option<ChannelId>,
    /// 取消令牌。dispatch 创建并 clone 一份挂到 `InFlightToolCall.cancel`，
    /// 另一份放进本字段供 worker 在 `run_async` 内 `select!` 监听。
    pub cancel: CancellationToken,
}

impl Default for OwnedToolContext {
    fn default() -> Self {
        Self {
            scheduler_state: None,
            tool_inflight_timeout_secs: 0,
            shell_default_exec_timeout_secs: 0,
            backend: None,
            current_task_id: None,
            current_agent_id: None,
            current_origin_channel: None,
            cancel: CancellationToken::new(),
        }
    }
}

impl OwnedToolContext {
    /// 测试构造器：无快照，仅全局配置。
    ///
    /// 真实 dispatch 会用带快照的构造器填充 scheduler_state。
    pub fn empty_for_test(tool_inflight_timeout_secs: u64) -> Self {
        Self {
            scheduler_state: None,
            tool_inflight_timeout_secs,
            shell_default_exec_timeout_secs: tool_inflight_timeout_secs,
            backend: None,
            current_task_id: None,
            current_agent_id: None,
            current_origin_channel: None,
            cancel: CancellationToken::new(),
        }
    }
}

/// 调度状态快照（动态定时任务定义）。
///
/// 由 dispatch 从 `SchedulerState` 抓取，worker 只读。
#[derive(Debug, Clone, Default)]
pub struct SchedulerStateSnapshot {
    /// 当前所有动态定时任务定义的快照。
    pub dynamic_tasks: Vec<ScheduleDefinition>,
}
