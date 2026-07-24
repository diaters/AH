//! 异步工具 dispatch：把 `kind==Async` 的工具请求原地改造为挂起实体，
//! 并把执行投给 tokio worker。结果经 `ToolResultSender` 通道回传，
//! 由 `ingest_tool_results_system` 落地。
//!
//! 设计要点：
//! - **独立 system，不开进 `tool_dispatch_system`**——后者已 16+ 参数靠 tuple 合并
//!   压着 Bevy 上限。本 system 在 schedule 中排在 `tool_dispatch_system` **之前**运行：
//!   只认领 `executor.kind() == Async` 的请求实体并原地改造（移除
//!   `ToolExecutionRequestMessage`、挂上 `ToolRequestPending + InFlightToolCall`）；
//!   Sync 请求原样留给后面的 `tool_dispatch_system`，双轨分流零干扰。
//! - **worker 模板照抄 LLM 桥**（`execution.rs:70-105`）：`runtime.0.spawn` + 通道回传。
//! - **挂起现场一次性算齐**：`max_duration` 钩子调用、std→chrono Duration 转换、
//!   快照克隆，全部发生在 dispatch（此刻还有 Res 只读访问）。
//! - **权限/确认**：本 system 只做「工具存在 + executor 存在 + kind==Async」检查；
//!   带 `pending_confirmation_id` 的请求直接跳过（Confirm 路径先行）。权限校验仍由
//!   调用链上游与 Sync 路径既有逻辑各管各的。

use crate::prelude::*;
use tracing::debug;

use crate::{
    app::{AsyncRuntime, Clock, HarnessSettings},
    domain::{
        BuiltinToolExecutors, InFlightToolCall, OwnedToolContext, ScheduledTaskInfoSnapshot,
        ScheduledTaskRegistrySnapshot, ToolActionKind, ToolAsyncResult,
        ToolExecutionRequestMessage, ToolRequestPending, ToolResultSender, ToolWorkerOutput,
        ToolWorkerPayload,
    },
    triggers::scheduled_task::{ScheduledTaskRegistry, SchedulerState},
};

use super::ingest_tool_results::build_scheduler_snapshot;

/// 异步工具 dispatch system。排在 `tool_dispatch_system` 之前运行：
/// 只认领 `kind()==Async` 的请求，Sync 请求原样留给旧路径。
#[allow(clippy::too_many_arguments)]
pub fn async_tool_dispatch_system(
    mut commands: Commands,
    runtime: Res<AsyncRuntime>,
    clock: Res<Clock>,
    settings: Res<HarnessSettings>,
    executors: Res<BuiltinToolExecutors>,
    sender: Res<ToolResultSender>,
    scheduler_state: Option<Res<SchedulerState>>,
    registry: Option<Res<ScheduledTaskRegistry>>,
    requests: Query<(Entity, &ToolExecutionRequestMessage)>,
) {
    for (entity, request) in &requests {
        // 等待确认的请求不归这里管（Confirm 路径先行）
        if request.pending_confirmation_id.is_some() {
            continue;
        }

        let Some(executor) = executors.get(&request.tool_name) else {
            continue; // 未知工具留给 sync 路径统一报 NotFound
        };
        if executor.kind() != ToolActionKind::Async {
            continue; // Sync 工具走旧路径
        }

        let tool_call_id = request
            .tool_call_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let tool_name = request.tool_name.clone();
        let input = request.tool_input.clone();

        // 挂起现场：算 sweeper 超时（钩子在此刻调用，不在 worker 内）
        let timeout_std = executor.max_duration(&input, settings.0.tool_inflight_timeout_secs);
        let timeout = chrono::Duration::from_std(timeout_std).unwrap_or_else(|_| {
            chrono::Duration::seconds(settings.0.tool_inflight_timeout_secs as i64)
        });

        // 挂起现场：克隆 owned 快照（worker 零 ECS 接触）
        let owned_ctx = OwnedToolContext {
            scheduler_state: scheduler_state
                .as_deref()
                .map(build_scheduler_snapshot)
                .map(std::sync::Arc::new),
            registry: registry
                .as_deref()
                .map(build_registry_snapshot)
                .map(std::sync::Arc::new),
            tool_inflight_timeout_secs: settings.0.tool_inflight_timeout_secs,
        };

        // 请求实体原地改造：摘请求消息 → 挂 Pending + InFlight
        let original_request = std::sync::Arc::new(request.request.clone());
        commands
            .entity(entity)
            .remove::<ToolExecutionRequestMessage>();
        commands.entity(entity).insert((
            ToolRequestPending {
                tool_call_id: tool_call_id.clone(),
                tool_name: tool_name.clone(),
                original_request,
            },
            InFlightToolCall {
                started_at: clock.0,
                timeout,
            },
        ));

        // 投 worker（照抄 LLM 桥模板：runtime.0.spawn + 通道回传）
        let tx = sender.0.clone();
        let call_id = tool_call_id.clone();
        let future = executor.run_async(input, owned_ctx);
        runtime.0.spawn(async move {
            use futures_util::FutureExt;
            // catch_unwind：worker panic 也合成 error 结果回传（快速失败路径；
            // 兜底仍靠 sweeper——发送失败/通道断开时 sweeper claim 补 error）
            let outcome = std::panic::AssertUnwindSafe(future).catch_unwind().await;
            let payload = match outcome {
                Ok(Ok(output)) => match output {
                    ToolWorkerOutput::Value(v) => ToolWorkerPayload::Completed(Ok(v)),
                    ToolWorkerOutput::Effect(effect) => ToolWorkerPayload::Effect(effect),
                },
                Ok(Err(e)) => ToolWorkerPayload::Completed(Err(e)),
                Err(_) => ToolWorkerPayload::Completed(Err(
                    crate::domain::ToolError::ExecutionFailed("worker panicked".to_string()),
                )),
            };
            let _ = tx.send(ToolAsyncResult {
                tool_call_id: call_id,
                payload,
            });
        });

        debug!(
            event = "AsyncToolDispatched",
            tool_call_id = %tool_call_id,
            tool_name = %tool_name,
            timeout_secs = timeout.num_seconds(),
            "async tool parked and worker spawned"
        );
    }
}

/// 从 `ScheduledTaskRegistry` 构造一份 owned 快照。
///
/// 与 `build_scheduler_snapshot` 对称：dispatch 挂起现场调用，worker 零 ECS 接触。
/// Task 5 不需要本函数，故留作本模块私有。
fn build_registry_snapshot(registry: &ScheduledTaskRegistry) -> ScheduledTaskRegistrySnapshot {
    ScheduledTaskRegistrySnapshot {
        tasks: registry
            .iter()
            .map(|(kind, info)| {
                (
                    kind.clone(),
                    ScheduledTaskInfoSnapshot {
                        content: info.content.clone(),
                        output_channel: info.output_channel.clone(),
                        is_once: info.is_once,
                    },
                )
            })
            .collect(),
    }
}
