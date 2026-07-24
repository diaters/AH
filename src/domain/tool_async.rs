//! 异步工具桥的领域类型（最小骨架）。
//!
//! 本模块当前包含：
//! - 通道类型（Phase 0 测试 harness 所需）：
//!   - `ToolWorkerPayload`：worker 回传给 ECS 的载荷枚举
//!   - `ToolAsyncResult`：worker 回传的完整结果
//!   - `ToolResultSender` / `ToolResultReceiver`：作为 Resource 注入 World 的通道端
//! - Task 1 引入的 trait 异步三件套所需支撑类型：
//!   - `ToolEffect`：worker 声明式写效果（dispatch 后由 commit 系统落账）
//!   - `OwnedToolContext`：异步执行入口的 owned 上下文（最小骨架，仅含全局配置）
//!
//! Phase 1（Task 2）会在此骨架上扩展：
//! - 给 `ToolWorkerPayload` 增加 `Effect(ToolEffect)` 变体
//! - 补充 `ToolEffectPending`、`ToolRequestPending`、`InFlightToolCall`
//!   与快照类型，并把 `OwnedToolContext` 补全（scheduler_state / registry 等）
//!
//! 在此之前，本模块仅承担“让 harness 能编译 + Task 1 trait 能落地”的职责，
//! 不引入任何业务逻辑。

use bevy_ecs::prelude::Resource;
use tokio::sync::mpsc;

use crate::domain::ToolError;

/// worker 回传给 ECS 的载荷。
///
/// 当前只支持 `Completed`（工具执行完毕，附带成功值或错误）。
/// Task 2 会补 `Effect(ToolEffect)` 变体用于副作用回传。
#[derive(Debug, Clone)]
pub enum ToolWorkerPayload {
    Completed(Result<serde_json::Value, ToolError>),
    // Task 2 会补 Effect(ToolEffect) 变体
}

/// worker 回传给 ECS 的异步结果。
///
/// 一条 `ToolAsyncResult` 对应一次工具调用的终态（或终态前的一次副作用）。
#[derive(Debug, Clone)]
pub struct ToolAsyncResult {
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
}

/// worker → ECS 通道的发送端，作为 Resource 注入 World。
#[derive(Resource)]
pub struct ToolResultSender(pub mpsc::UnboundedSender<ToolAsyncResult>);

/// worker → ECS 通道的接收端，作为 Resource 注入 World。
///
/// 持有 `UnboundedReceiver`：唯一持有，因此 ingest 系统对它有排他访问权。
#[derive(Resource)]
pub struct ToolResultReceiver(pub mpsc::UnboundedReceiver<ToolAsyncResult>);

/// worker 声明式写效果：交由 commit_tool_effects_system 落账。
///
/// 写路径工具（如 schedule_task 的取消语义）由 worker 声明意图，
/// 主 ECS 线程在 ingest 阶段统一应用，避免 worker 直接 mutate World。
///
/// 当前仅 `DeleteScheduledTask` 一个变体；后续若有新写效果，扩展本枚举。
#[derive(Debug, Clone)]
pub enum ToolEffect {
    /// 删除一个调度任务（如周期任务的「停掉」语义）
    DeleteScheduledTask {
        /// 任务类型字符串，形如 `scheduled:<uuid>`
        kind: String,
    },
}

/// 异步工具执行的 owned 上下文（最小骨架）。
///
/// 与 `ToolContext<'a>`（borrowed，sync 路径用）相对，
/// `OwnedToolContext` 在 worker 内的 `'static` 上下文中可用——
/// 异步 dispatch 把所需状态从 ECS 抓一份快照过来，丢给 worker，
/// worker 不持有任何 borrowed ECS 引用。
///
/// Task 1 仅落最小骨架：只有 `tool_inflight_timeout_secs` 一个字段，
/// 因为 trait 的 `max_duration` 钩子签名收的是裸 `u64`，
/// 不需要 scheduler / registry 句柄。
///
/// Task 2 会补 `scheduler_state` / `registry` / 快照类型等字段。
#[derive(Debug, Clone, Default)]
pub struct OwnedToolContext {
    /// 全局失联超时（秒）—— sweeper 推导 max_duration 的全局缺省。
    pub tool_inflight_timeout_secs: u64,
    // Task 2 会补 scheduler_state / registry 等字段
}

impl OwnedToolContext {
    /// 测试构造器：无快照，仅全局配置。
    ///
    /// 仅用于 Task 1 trait 行为测试；Task 2 引入完整快照后，
    /// 真实 dispatch 会用带快照的构造器。
    pub fn empty_for_test(tool_inflight_timeout_secs: u64) -> Self {
        Self {
            tool_inflight_timeout_secs,
        }
    }
}
