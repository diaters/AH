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
    AgentExecutionOutput, AgentExecutionResult, ExecutionError, OutputContent, ToolEffectPending,
    ToolExecutionResultMessage, ToolRequestPending, ToolResultReceiver, ToolReturnedHookPending,
    ToolWorkerPayload,
};
use crate::triggers::scheduled_task::SchedulerState;

/// ingest system：try_recv 排空通道。
/// - Completed：挂起实体在 → 落地结果 + despawn；不在 → drop + warn（重复/迟到）
/// - Effect：spawn ToolEffectPending，挂起实体保留（等 commit 回送最终结果）
pub fn ingest_tool_results_system(
    mut commands: Commands,
    mut receiver: ResMut<ToolResultReceiver>,
    pending: Query<(Entity, &ToolRequestPending)>,
) {
    while let Ok(result) = receiver.0.try_recv() {
        match result.payload {
            ToolWorkerPayload::Completed(output) => {
                let Some((entity, pending)) = pending
                    .iter()
                    .find(|(_, p)| p.tool_call_id == result.tool_call_id)
                else {
                    warn!(
                        event = "ToolResultDroppedNoPending",
                        tool_call_id = %result.tool_call_id,
                        "result arrived but pending entity gone (sweeper claimed or duplicate); dropping"
                    );
                    continue;
                };
                spawn_tool_result_message(&mut commands, pending, output);
                commands.entity(entity).despawn();
            }
            ToolWorkerPayload::Effect(effect) => {
                // 挂起实体必须还在，否则效果来源不可考；不在则丢弃并告警
                if !pending
                    .iter()
                    .any(|(_, p)| p.tool_call_id == result.tool_call_id)
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
        dynamic_tasks: state
            .dynamic_tasks()
            .iter()
            .map(|dt| crate::domain::DynamicScheduledTaskSnapshot {
                id: dt.id,
                kind: dt.kind.clone(),
                schedule: dt.schedule.clone(),
                created_at: dt.created_at,
            })
            .collect(),
    }
}
