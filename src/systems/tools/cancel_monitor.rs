//! 取消监听：父任务终态 → 触发 worker 取消。
//!
//! 与 `sweeper` 对称：sweeper 扫「超时」发 error + claim；
//! cancel_monitor 扫「父任务终态」调用 `token.cancel()` + claim。
//! 两者操作不同条件子集，claim 后都不 despawn 挂起实体——落地与 despawn
//! 只发生在 `ingest_tool_results_system` 单点。
//!
//! 触发链：父任务终态 → cancel_monitor `token.cancel()` → worker 或 backend
//! 协作消费取消信号 → 清理所属资源 → 回送 cancelled error → ingest 落地。
//! `shell_exec` 等待 backend 完成进程清理和回收后，才回送
//! `Err(ToolError::ExecutionFailed("cancelled"))`。
//!
//! claim 语义：摘除 `InFlightToolCall` 后 sweeper 不再扫这个实体（防重复发 error）；
//! 挂起实体保留等 ingest 收到 worker 回送的 cancelled error 后落地 + despawn。
//! claim 后 sweeper 已无法兜底（`InFlightToolCall` 已摘除）。取消是协作式约定，
//! 此 system 不等待 worker 退出，也不保证其一定回送；具体工具与 backend
//! 负责消费 token 并完成清理，不能把发出取消信号等同于资源已经释放。

use crate::prelude::*;
use tracing::debug;

use crate::domain::{
    BackgroundedToolCall, InFlightToolCall, StopRequested, Task, ToolRequestPending,
};

/// cancel_monitor system：扫挂起实体，若其父任务已终态则触发取消。
///
/// 终态判定：`Task::status.is_terminal()`（`Done` 或 `Failed(_)`）。
/// 父任务实体找不到（已 despawn）同样视同终态——任务已不存在，工具结果无消费方。
/// 父任务带 `StopRequested`（用户 `/stop` 中断当前回合）同样触发取消；
/// 已转后台的调用不属于当前回合，`/stop` 不取消它，只在所属任务终态时取消。
pub fn cancel_monitor_system(
    mut commands: Commands,
    index: Res<crate::ecs::EntityIndex>,
    tasks: Query<(&Task, Has<StopRequested>)>,
    pending_inflight: Query<(
        Entity,
        &ToolRequestPending,
        &InFlightToolCall,
        Has<BackgroundedToolCall>,
    )>,
) {
    for (entity, pending, in_flight, backgrounded) in &pending_inflight {
        let task_id = pending.original_request.task_id;

        // 查父任务：找不到（已 despawn）或已终态 → 取消
        let should_cancel = index
            .get_task(&task_id)
            .and_then(|entity| tasks.get(entity).ok())
            .map(|(t, stop_requested)| t.status.is_terminal() || (stop_requested && !backgrounded))
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

        // 发出取消信号，由 worker 或 backend 协作消费并完成资源清理。
        in_flight.cancel.cancel();

        // claim：摘除在飞标记，防止 sweeper 重复发 error
        commands.entity(entity).remove::<InFlightToolCall>();
    }
}
