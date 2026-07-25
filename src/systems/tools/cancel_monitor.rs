//! 取消监听：父任务终态 → 触发 worker 取消。
//!
//! 与 `sweeper` 对称：sweeper 扫「超时」发 error + claim；
//! cancel_monitor 扫「父任务终态」调用 `token.cancel()` + claim。
//! 两者操作不同条件子集，claim 后都不 despawn 挂起实体——落地与 despawn
//! 只发生在 `ingest_tool_results_system` 单点。
//!
//! 触发链：父任务终态 → cancel_monitor `token.cancel()` → worker
//! `tokio::select!` 监听 `cancel.cancelled()` → kill 子进程 → 回
//! `Err(ToolError::ExecutionFailed("cancelled"))` → ingest 落地。
//!
//! claim 语义：摘除 `InFlightToolCall` 后 sweeper 不再扫这个实体（防重复发 error）；
//! 挂起实体保留等 ingest 收到 worker 回送的 cancelled error 后落地 + despawn。
//! 若 worker 在 cancel 后未能及时回送（极端 race），sweeper 已无法兜底
//! （`InFlightToolCall` 已摘除）——但 `CancellationToken` 的 `select!` 路径
//! 保证 worker 一定会退出，回送通道不会断。

use crate::prelude::*;
use tracing::debug;

use crate::domain::{InFlightToolCall, Task, ToolRequestPending};

/// cancel_monitor system：扫挂起实体，若其父任务已终态则触发取消。
///
/// 终态判定：`Task::status.is_terminal()`（`Done` 或 `Failed(_)`）。
/// 父任务实体找不到（已 despawn）同样视同终态——任务已不存在，工具结果无消费方。
pub fn cancel_monitor_system(
    mut commands: Commands,
    tasks: Query<&Task>,
    pending_inflight: Query<(Entity, &ToolRequestPending, &InFlightToolCall)>,
) {
    for (entity, pending, in_flight) in &pending_inflight {
        let task_id = pending.original_request.task_id;

        // 查父任务：找不到（已 despawn）或已终态 → 取消
        let should_cancel = tasks
            .iter()
            .find(|t| t.id == task_id)
            .map(|t| t.status.is_terminal())
            .unwrap_or(true);

        if !should_cancel {
            continue;
        }

        debug!(
            event = "ToolCallCancelledByParentTerminal",
            tool_call_id = %pending.tool_call_id,
            tool_name = %pending.tool_name,
            "parent task terminal; cancelling worker"
        );

        // 触发取消（worker tokio::select! 监听 → kill 子进程 → 回 error）
        in_flight.cancel.cancel();

        // claim：摘除在飞标记，防止 sweeper 重复发 error
        commands.entity(entity).remove::<InFlightToolCall>();
    }
}
