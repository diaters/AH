//! 异步工具结果落地 system（Task 5 扩展点）。
//!
//! 本模块在 Task 4 阶段仅承担一个最小职责：提供 `build_scheduler_snapshot`
//! helper 给 `async_tool_dispatch_system` 在挂起现场调用——把
//! `SchedulerState` 的动态任务账本一次性克隆成 owned 快照丢给 worker。
//!
//! Task 5 将在本模块中扩展 `ingest_tool_results_system`：从
//! `ToolResultReceiver` 拉取 worker 回传的 `ToolAsyncResult`，按 payload
//! 分流（`Completed` → `ToolExecutionResultMessage`，`Effect` → spawn
//! `ToolEffectPending`），最后 despawn 对应的 `ToolRequestPending` 实体。

use crate::domain::SchedulerStateSnapshot;
use crate::triggers::scheduled_task::SchedulerState;

/// 从 `SchedulerState` 构造一份 owned 快照。
///
/// 调用现场是 dispatch 挂起时（主 ECS 线程），不在 worker 内：
/// dispatch 把快照丢进 `OwnedToolContext`，worker 零 ECS 接触。
pub(crate) fn build_scheduler_snapshot(state: &SchedulerState) -> SchedulerStateSnapshot {
    SchedulerStateSnapshot {
        dynamic_tasks: state
            .dynamic_tasks()
            .iter()
            .map(|t| crate::domain::DynamicScheduledTaskSnapshot {
                id: t.id,
                kind: t.kind.clone(),
                schedule: t.schedule.clone(),
                created_at: t.created_at,
            })
            .collect(),
    }
}
