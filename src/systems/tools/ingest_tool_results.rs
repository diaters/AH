//! 工具结果 ingest：全桥唯一的结果落地点。
//!
//! 本模块承担两个职责：
//! 1. `build_scheduler_snapshot`：dispatch 挂起时把 SchedulerState 抓成 owned 快照丢给 worker；
//! 2. `ingest_tool_results_system`：try_recv 排空 worker → ECS 通道，按 payload 分流——
//!    Completed 落地 ToolExecutionResultMessage + despawn 挂起实体；Effect 分流 spawn
//!    ToolEffectPending，挂起实体保留待 commit 回送最终结果。
//!
//! 结果落地单点原则：sweeper 的 error 也走通道经这里落地，exactly-once 由
//! 「挂起实体是否还在」唯一裁决。

use crate::prelude::*;
use tracing::warn;

use crate::domain::{
    AgentExecutionOutput, AgentExecutionResult, BackgroundedToolCall, ExecutionError,
    InboxDeliveryMessage, InboxItem, OutputContent, ToolEffectPending, ToolExecutionResultMessage,
    ToolRequestPending, ToolResultReceiver, ToolReturnedHookPending, ToolWorkerPayload,
};

/// 后台工具结果通知中保留的结果字节上限（超出部分保留尾部）。
const BACKGROUND_RESULT_MAX_BYTES: usize = 4 * 1024;
use crate::triggers::scheduled_task::SchedulerState;

/// ingest system：try_recv 排空通道。
/// - Completed：挂起实体在 → 落地结果 + despawn；不在 → drop + warn（重复/迟到）
/// - Effect：spawn ToolEffectPending，挂起实体保留（等 commit 回送最终结果）
///
/// 已转后台的调用（带 `BackgroundedToolCall`）不再回填工具循环：结果作为
/// `<task-notification>` 投递到所属任务收件箱，由空闲唤醒开启新的一轮。
pub fn ingest_tool_results_system(
    mut commands: Commands,
    clock: Option<Res<crate::contracts::Clock>>,
    mut receiver: ResMut<ToolResultReceiver>,
    pending: Query<(Entity, &ToolRequestPending, Option<&BackgroundedToolCall>)>,
    wake: Option<Res<crate::contracts::RuntimeWake>>,
) {
    let now = clock.map_or_else(chrono::Utc::now, |clock| clock.0);
    for _ in 0..256 {
        let Ok(result) = receiver.0.try_recv() else {
            break;
        };
        match result.payload {
            ToolWorkerPayload::Completed(output) => {
                let Some((entity, pending, backgrounded)) = pending
                    .iter()
                    .find(|(_, p, _)| p.tool_call_id == result.tool_call_id)
                else {
                    warn!(
                        event = "ToolResultDroppedNoPending",
                        tool_call_id = %result.tool_call_id,
                        "result arrived but pending entity gone (sweeper claimed or duplicate); dropping"
                    );
                    continue;
                };
                match backgrounded {
                    Some(backgrounded) => {
                        deliver_background_result(&mut commands, pending, backgrounded, output, now)
                    }
                    None => spawn_tool_result_message(&mut commands, pending, output),
                }
                commands.entity(entity).despawn();
            }
            ToolWorkerPayload::Effect(effect) => {
                // 挂起实体必须还在，否则效果来源不可考；不在则丢弃并告警
                if !pending
                    .iter()
                    .any(|(_, p, _)| p.tool_call_id == result.tool_call_id)
                {
                    warn!(
                        event = "ToolEffectDroppedNoPending",
                        tool_call_id = %result.tool_call_id,
                        "effect arrived but pending entity gone; dropping"
                    );
                    continue;
                }
                commands.spawn(ToolEffectPending {
                    tool_call_id: result.tool_call_id.clone(),
                    effect,
                });
            }
        }
    }
    if !receiver.0.is_empty()
        && let Some(wake) = wake
    {
        wake.notify();
    }
}

/// 后台工具调用的最终结果：投递到所属任务收件箱（被停止或被取代的调用静默丢弃）。
///
/// 作业实体由 `background_job_monitor_system` 在挂起实体消失后退役。
fn deliver_background_result(
    commands: &mut Commands,
    pending: &ToolRequestPending,
    backgrounded: &BackgroundedToolCall,
    output: Result<serde_json::Value, crate::domain::ToolError>,
    now: chrono::DateTime<chrono::Utc>,
) {
    let task_id = pending.original_request.task_id;
    if backgrounded.silent {
        tracing::debug!(
            event = "BackgroundToolResultDiscarded",
            task_id = %task_id,
            job_id = %backgrounded.job_id,
            tool_name = %pending.tool_name,
            "background tool call was stopped or superseded; result discarded"
        );
        return;
    }
    let (status, body) = match &output {
        Ok(value) => (
            "completed",
            serde_json::to_string(value).unwrap_or_else(|_| value.to_string()),
        ),
        Err(error) => ("failed", format!("error: {error}")),
    };
    let mut summary = format!("tool: {}\n", pending.tool_name);
    if let Some(note) = &backgrounded.note {
        summary.push_str(note);
        summary.push('\n');
    }
    summary.push_str("result:\n");
    summary.push_str(&tail_bytes(&body, BACKGROUND_RESULT_MAX_BYTES));
    tracing::debug!(
        event = "BackgroundToolResultDelivered",
        task_id = %task_id,
        job_id = %backgrounded.job_id,
        tool_name = %pending.tool_name,
        status,
        background_secs = (now - backgrounded.detached_at).num_seconds(),
        "background tool call finished; result delivered to owner inbox"
    );
    commands.spawn(InboxDeliveryMessage {
        task_id,
        item: InboxItem::TaskNotification {
            job_id: backgrounded.job_id,
            source: "tool".to_string(),
            status: status.to_string(),
            summary,
            received_at: now,
        },
    });
}

/// 保留 UTF-8 边界上的尾部（命令输出的结论通常在末尾）。
fn tail_bytes(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let mut start = text.len() - max_bytes;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    format!("…[前 {start} 字节已省略]{}", &text[start..])
}

/// 由挂起实体携带的 original_request 重建完整结果消息（9 字段对齐 execution.rs:102）。
pub(crate) fn spawn_tool_result_message(
    commands: &mut Commands,
    pending: &ToolRequestPending,
    output: Result<serde_json::Value, crate::domain::ToolError>,
) {
    let req = &pending.original_request;
    let execution_result = AgentExecutionResult {
        task_id: req.task_id,
        agent_id: req.agent_id,
        request_kind: req.request_kind.clone(),
        result: match &output {
            Ok(value) => Ok(AgentExecutionOutput {
                content: OutputContent::Text(
                    serde_json::to_string(value).unwrap_or_else(|_| "{}".to_string()),
                ),
                reasoning_content: None,
            }),
            Err(e) => Err(ExecutionError::Unknown(e.to_string())),
        },
        prompt: req.prompt.clone(),
        system_prompt: req.system_prompt.clone(),
        tools: req.tools.clone(),
        reasoning_content: None,
        work_item_id: req.work_item_id,
        conversation: None,
        turn_generation: None,
    };

    commands.spawn((
        ToolExecutionResultMessage {
            result: execution_result,
            tool_name: pending.tool_name.clone(),
            tool_output: output,
            tool_call_id: Some(pending.tool_call_id.clone()),
            processed: false,
            original_tool_output: None,
        },
        ToolReturnedHookPending,
    ));
}

/// 供 async_dispatch 复用的快照构造（list/delete 的读来源）。
pub(crate) fn build_scheduler_snapshot(
    state: &SchedulerState,
) -> crate::domain::SchedulerStateSnapshot {
    crate::domain::SchedulerStateSnapshot {
        dynamic_tasks: state.dynamic_tasks().to_vec(),
    }
}
