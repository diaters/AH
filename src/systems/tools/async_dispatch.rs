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
//! - **CancellationToken 接线**：dispatch 建 token，挂一份到 `InFlightToolCall.cancel`
//!   （cancel_monitor_system 用），另一份到 `OwnedToolContext.cancel`（worker 在
//!   `run_async` 内 `select!` 监听用）。`OwnedToolContext.backend` 注入
//!   `Arc<dyn SessionBackend>` 让 shell_exec worker 拿到 backend 句柄。
//! - **权限/确认**：本 system 只做「工具存在 + executor 存在 + kind==Async」检查；
//!   带 `pending_confirmation_id` 的请求直接跳过（Confirm 路径先行）。权限校验仍由
//!   调用链上游与 Sync 路径既有逻辑各管各的。

use std::sync::Arc;

use crate::prelude::*;
use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::{
    app::{AsyncRuntime, Clock, HarnessSettings},
    contracts::SessionBackend,
    domain::{
        Agent, BuiltinToolExecutors, InFlightToolCall, OwnedToolContext, ScheduledTaskInfoSnapshot,
        ScheduledTaskRegistrySnapshot, SpaceToolRegistry, Task, ToolActionKind, ToolAsyncResult,
        ToolExecutionRequestMessage, ToolPermission, ToolRequestPending, ToolResultSender,
        ToolWorkerOutput, ToolWorkerPayload,
    },
    triggers::scheduled_task::{ScheduledTaskRegistry, SchedulerState},
};

use super::ingest_tool_results::build_scheduler_snapshot;

/// 异步工具 dispatch system。排在 `tool_dispatch_system` 之前运行：
/// 只认领 `kind()==Async` 的请求，Sync 请求原样留给旧路径。
///
/// **权限分流**：需要 `Confirm` 的请求跳过——留给 `tool_dispatch_system` 设置
/// `pending_confirmation_id` 并派发审批。用户确认后 `tool_confirmation_result_system`
/// 清除 `pending_confirmation_id`，下一帧本系统再认领。`Allow` / `Deny` 的 Async
/// 请求由本系统直接认领（`Deny` 会被 `tool_dispatch_system` 报错，但 `Deny` 在
/// 实践中极少出现且 `async_tool_dispatch_system` 在前会先认领——`Deny` 语义靠
/// `tool_dispatch_system` 兜底，本系统不重复实现 `Deny` 报错路径）。
///
/// **权限检查条件性**：`registry` / `agents` 为 `Option`/`Query`——测试世界可能
/// 不装 `SpaceToolRegistry` 或不 spawn `Agent`，此时跳过权限检查直接认领
/// （测试工具通常用 `Allow` 权限，生产世界始终装齐两资源）。
#[allow(clippy::too_many_arguments)]
pub fn async_tool_dispatch_system(
    mut commands: Commands,
    runtime: Res<AsyncRuntime>,
    clock: Res<Clock>,
    settings: Res<HarnessSettings>,
    executors: Res<BuiltinToolExecutors>,
    registry: Option<Res<SpaceToolRegistry>>,
    agents: Query<&Agent>,
    tasks: Query<&Task>,
    sender: Res<ToolResultSender>,
    backend: Option<Res<crate::systems::tools::NativeProcessBackend>>,
    scheduler_state: Option<Res<SchedulerState>>,
    scheduled_registry: Option<Res<ScheduledTaskRegistry>>,
    experience_store: Option<Res<crate::domain::ExperienceStore>>,
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

        // 权限分流：需要 Confirm 的请求留给 sync 路径（tool_dispatch_system）
        // 设置 pending_confirmation_id 并派发审批。用户确认后
        // tool_confirmation_result_system 清除 pending_confirmation_id，
        // 下一帧本系统再认领。
        //
        // **allow_once 路径**：`tool_confirmation_result_system` 在 Async 分支
        // 设置 `confirmed_once = true`，本系统据此跳过权限检查直接认领——
        // 否则 Confirm 权限的 Async 工具会陷入循环。
        // `allow_always` 路径已通过 `overrides.insert(Allow)` 更新永久权限，
        // 本系统会直接认领，无需 `confirmed_once`。
        //
        // 仅在 registry + agent 都可见时检查——生产世界两者始终装齐；
        // 测试世界可能缺一，此时跳过检查直接认领（测试工具通常 Allow）。
        if !request.confirmed_once
            && let (Some(registry), Some(agent)) = (
                registry.as_deref(),
                agents.iter().find(|a| a.id == request.request.agent_id),
            )
        {
            if registry.get(&request.tool_name).is_none() {
                continue; // 工具定义不在 registry → 留给 sync 路径
            }
            let permission = agent.tool_permissions.get_permission(&request.tool_name);
            if matches!(permission, ToolPermission::Confirm) {
                continue; // 需要确认 → 留给 sync 路径设置 confirmation
            }
        }

        let tool_call_id = request
            .tool_call_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let tool_name = request.tool_name.clone();
        let input = request.tool_input.clone();

        // 挂起现场：算 sweeper 超时（钩子在此刻调用，不在 worker 内）。
        // 传 max(shell_default_exec, inflight) 而非纯 inflight：ShellExecTool 等
        // 工具的业务超时 fallback 用 shell_default_exec_timeout_secs，sweeper 必须
        // 晚于业务超时（D5），故取两者较大值确保 margin 恒正。
        let effective_inflight = settings
            .0
            .shell_default_exec_timeout_secs
            .max(settings.0.tool_inflight_timeout_secs);
        let timeout_std = executor.max_duration(&input, effective_inflight);
        let timeout = chrono::Duration::from_std(timeout_std).unwrap_or_else(|_| {
            chrono::Duration::seconds(settings.0.tool_inflight_timeout_secs as i64)
        });

        // CancellationToken：dispatch 创建，挂一份到 InFlightToolCall（cancel_monitor 用），
        // 另一份到 OwnedToolContext.cancel（worker 在 run_async 内 select! 监听用）。
        let cancel = CancellationToken::new();

        // 挂起现场：克隆 owned 快照（worker 零 ECS 接触）。
        // backend 句柄：从 Res<NativeProcessBackend> clone 一份（内部全是
        // Arc<Mutex<...>>，clone 廉价），擦除为 Arc<dyn SessionBackend> 让 worker
        // 不耦合具体 backend 类型。
        let backend_arc: Option<Arc<dyn SessionBackend>> = backend
            .as_deref()
            .map(|b| Arc::new(b.clone()) as Arc<dyn SessionBackend>);
        // 经验候选快照：从 ExperienceStore 按 task_id 抓一份 cloned 列表，
        // 包成 Arc<Vec<...>> 给 worker。dispatch 在此完成 task_id 过滤——
        // worker 直接遍历，不再二次过滤。
        let task_id = request.request.task_id;
        let experience_candidates: Option<Arc<Vec<crate::domain::ExperienceCandidate>>> =
            experience_store.as_deref().map(|store| {
                Arc::new(
                    store
                        .list_for_task(task_id)
                        .into_iter()
                        .cloned()
                        .collect::<Vec<_>>(),
                )
            });
        // 从 Task.origin_channel 注入 current_origin_channel（Task 14 Step E）：
        // schedule_task 等需要继承通道的异步工具靠此字段在 worker 内拿到真值。
        // Task 不存在时（测试世界或异常状态）降级为 None，与既有容忍语义一致。
        let current_origin_channel = tasks
            .iter()
            .find(|t| t.id == task_id)
            .and_then(|t| t.origin_channel.clone());
        let owned_ctx = OwnedToolContext {
            scheduler_state: scheduler_state
                .as_deref()
                .map(build_scheduler_snapshot)
                .map(Arc::new),
            registry: scheduled_registry
                .as_deref()
                .map(build_registry_snapshot)
                .map(Arc::new),
            tool_inflight_timeout_secs: settings.0.tool_inflight_timeout_secs,
            shell_default_exec_timeout_secs: settings.0.shell_default_exec_timeout_secs,
            backend: backend_arc,
            experience_candidates,
            current_task_id: Some(task_id),
            current_origin_channel,
            cancel: cancel.clone(),
        };

        // 请求实体原地改造：摘请求消息 → 挂 Pending + InFlight
        let original_request = Arc::new(request.request.clone());
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
                cancel: cancel.clone(),
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
