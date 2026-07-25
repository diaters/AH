//! 失联清扫器：超时 claim —— 发 error 入通道 + 摘除 InFlightToolCall。
//! 不 despawn 挂起实体、不落地结果：落地与 despawn 只发生在 ingest。
//!
//! 三条失联路径（panic / 超时 / 通道断开后恢复）全部经此殊途同归到 ingest 单点：
//! - panic：dispatch 的 catch_unwind 兜底，worker 把 Err(ExecutionFailed) 送回通道
//! - 超时（worker 内 tokio::time::timeout 失败）：worker 自己送 Err(Timeout) 回通道
//! - 通道断开后恢复 / 其他逃脱路径：sweeper 在 ECS 侧构造超时 error 入通道
//!
//! claim 语义：摘除 InFlightToolCall 后，后续帧不再扫这个实体；挂起实体保留，
//! 等 ingest 收到 error 后落地 + despawn。若 worker 在 sweeper claim 后才回结果，
//! ingest 因挂起实体还在会先落地 sweeper 的 error；worker 的结果迟到时实体已没，
//! ingest drop + warn，exactly-once 闭合。

use crate::prelude::*;
use tracing::warn;

use crate::{
    app::Clock,
    domain::{InFlightToolCall, ToolAsyncResult, ToolError, ToolRequestPending, ToolResultSender},
};

/// 失联兜底 sweeper：扫在飞标记，超时则发 error 入通道 + claim（摘除 InFlightToolCall）。
///
/// 不 despawn 挂起实体（落地 + despawn 只在 ingest）；不发结果消息（spawn
/// ToolExecutionResultMessage 只在 ingest）。时间判定用 `Res<Clock>`，测试
/// 通过推进假时钟验证超时语义。
pub fn sweep_inflight_tool_calls(
    mut commands: Commands,
    clock: Res<Clock>,
    sender: Res<ToolResultSender>,
    query: Query<(Entity, &ToolRequestPending, &InFlightToolCall)>,
) {
    let now = clock.0;

    for (entity, pending, in_flight) in &query {
        let elapsed = now.signed_duration_since(in_flight.started_at);
        if elapsed <= in_flight.timeout {
            continue;
        }

        warn!(
            event = "ToolCallSweepTimeout",
            tool_call_id = %pending.tool_call_id,
            tool_name = %pending.tool_name,
            timeout_secs = in_flight.timeout.num_seconds(),
            elapsed_secs = elapsed.num_seconds(),
            "tool call lost contact; claiming and sending error result"
        );

        let _ = sender.0.send(ToolAsyncResult::completed(
            pending.tool_call_id.clone(),
            Err(ToolError::Timeout(format!(
                "tool '{}' lost contact after {}s (sweeper)",
                pending.tool_name,
                elapsed.num_seconds()
            ))),
        ));

        // claim：摘除在飞标记，实体留给 ingest 落地后 despawn
        commands.entity(entity).remove::<InFlightToolCall>();
    }
}
