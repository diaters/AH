//! 后台作业控制：`stop_background_job` 工具与 watchdog 的统一执行点。
//!
//! - Stop：后台工具调用触发取消令牌；子任务批次取消未完成子任务；shell 会话停止。
//!   Agent 主动停止的作业不再通知；watchdog 停止的作业照常通知并附原因。
//! - Retry：取消当前后台工具调用，用相同入参重新派发；新调用继承同一 `job_id`，
//!   结果照常以通知送达，旧调用的迟到结果静默丢弃。

use tracing::{debug, warn};

use crate::contracts::Clock;
use crate::domain::{
    AgentExecutionOutput, AgentExecutionRequest, AgentExecutionResult, AgentId, AgentRequestKind,
    BackgroundJob, BackgroundJobControl, BackgroundedToolCall, FailureReason, InFlightToolCall,
    JobControlAction, JobControlOrigin, JobSource, OutputContent, SessionBackend, Task, TaskId,
    ToolError, ToolExecutionRequestMessage, ToolExecutionResultMessage, ToolRequestPending,
    ToolReturnedHookPending,
};
use crate::ecs::EntityIndex;
use crate::prelude::*;
use crate::systems::NativeProcessBackend;

const STOP_BACKGROUND_JOB_TOOL: &str = "stop_background_job";

fn reply(
    commands: &mut Commands,
    task_id: TaskId,
    agent_id: AgentId,
    tool_call_id: &str,
    output: Result<serde_json::Value, ToolError>,
) {
    commands.spawn((
        ToolExecutionResultMessage {
            result: AgentExecutionResult {
                task_id,
                agent_id,
                request_kind: AgentRequestKind::ToolExecution {
                    tool_name: STOP_BACKGROUND_JOB_TOOL.to_string(),
                },
                result: Ok(AgentExecutionOutput {
                    content: OutputContent::Text(String::new()),
                    reasoning_content: None,
                }),
                prompt: String::new(),
                system_prompt: None,
                tools: vec![],
                reasoning_content: None,
                work_item_id: None,
                conversation: None,
                turn_generation: None,
            },
            tool_name: STOP_BACKGROUND_JOB_TOOL.to_string(),
            tool_output: output,
            tool_call_id: Some(tool_call_id.to_string()),
            processed: false,
            original_tool_output: None,
        },
        ToolReturnedHookPending,
    ));
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub(crate) fn background_job_control_system(
    mut commands: Commands,
    clock: Res<Clock>,
    index: Res<EntityIndex>,
    backend: Option<Res<NativeProcessBackend>>,
    requests: Query<(Entity, &BackgroundJobControl)>,
    mut jobs: Query<(Entity, &mut BackgroundJob)>,
    mut pendings: Query<(
        &ToolRequestPending,
        &mut BackgroundedToolCall,
        Option<&InFlightToolCall>,
    )>,
    mut tasks: Query<&mut Task>,
) {
    for (request_entity, request) in &requests {
        commands.entity(request_entity).despawn();
        let outcome = control_job(
            &mut commands,
            clock.0,
            &index,
            backend.as_deref(),
            request,
            &mut jobs,
            &mut pendings,
            &mut tasks,
        );
        match (&request.origin, outcome) {
            (
                JobControlOrigin::Agent {
                    task_id,
                    agent_id,
                    tool_call_id,
                },
                outcome,
            ) => reply(
                &mut commands,
                *task_id,
                *agent_id,
                tool_call_id,
                outcome.map_err(ToolError::InvalidInput),
            ),
            (JobControlOrigin::Watchdog { .. }, Err(error)) => warn!(
                event = "WatchdogJobControlFailed",
                job_id = %request.job_id,
                action = ?request.action,
                error_type = "JobControl",
                error = %error,
                "watchdog control request could not be applied"
            ),
            (JobControlOrigin::Watchdog { .. }, Ok(_)) => {}
        }
    }
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn control_job(
    commands: &mut Commands,
    now: chrono::DateTime<chrono::Utc>,
    index: &EntityIndex,
    backend: Option<&NativeProcessBackend>,
    request: &BackgroundJobControl,
    jobs: &mut Query<(Entity, &mut BackgroundJob)>,
    pendings: &mut Query<(
        &ToolRequestPending,
        &mut BackgroundedToolCall,
        Option<&InFlightToolCall>,
    )>,
    tasks: &mut Query<&mut Task>,
) -> Result<serde_json::Value, String> {
    let Some((job_entity, mut job)) = jobs.iter_mut().find(|(_, j)| j.id == request.job_id) else {
        return Err(format!(
            "background job {} not found (already finished or never existed)",
            request.job_id
        ));
    };
    let by_agent = matches!(request.origin, JobControlOrigin::Agent { .. });
    if let JobControlOrigin::Agent { task_id, .. } = &request.origin
        && *task_id != job.owner_task_id
    {
        return Err(format!(
            "background job {} does not belong to the current task",
            request.job_id
        ));
    }
    let reason = match &request.origin {
        JobControlOrigin::Agent { .. } => "stopped by agent via stop_background_job".to_string(),
        JobControlOrigin::Watchdog { reason } => format!("watchdog: {reason}"),
    };
    let source = job.source.label();
    let job_id = job.id;
    debug!(
        event = "BackgroundJobControlApplied",
        task_id = %job.owner_task_id,
        job_id = %job.id,
        source,
        action = ?request.action,
        by_agent,
        reason = %reason,
        "applying background job control"
    );

    match (&request.action, &mut job.source) {
        (JobControlAction::Stop, JobSource::ToolCall { tool_call_id, .. }) => {
            let Some((_, mut backgrounded, in_flight)) = pendings
                .iter_mut()
                .find(|(pending, _, _)| &pending.tool_call_id == tool_call_id)
            else {
                return Err("background tool call already finished".to_string());
            };
            if by_agent {
                backgrounded.silent = true;
            } else {
                backgrounded.note = Some(format!("[已被 watchdog 取消：{reason}]"));
            }
            if let Some(in_flight) = in_flight {
                in_flight.cancel.cancel();
            }
        }
        (JobControlAction::Retry, JobSource::ToolCall { tool_call_id, .. }) => {
            let Some((pending, mut backgrounded, in_flight)) = pendings
                .iter_mut()
                .find(|(pending, _, _)| &pending.tool_call_id == tool_call_id)
            else {
                return Err("background tool call already finished".to_string());
            };
            // 旧调用取消且结果静默；新调用继承 job_id，转后台标记随请求实体进入挂起态，
            // 不会再次转后台，结果照常通知。
            backgrounded.silent = true;
            if let Some(in_flight) = in_flight {
                in_flight.cancel.cancel();
            }
            let new_call_id = format!(
                "{}-retry-{}",
                job_id.simple(),
                uuid::Uuid::new_v4().simple()
            );
            commands.spawn((
                ToolExecutionRequestMessage {
                    request: AgentExecutionRequest::clone(&pending.original_request),
                    tool_name: pending.tool_name.clone(),
                    tool_input: pending.tool_input.clone(),
                    pending_confirmation_id: None,
                    tool_call_id: Some(new_call_id.clone()),
                    pending_confirmation_options: None,
                    // 原调用已通过权限与确认；重试同一入参不再重复审批。
                    confirmed_once: true,
                },
                BackgroundedToolCall {
                    job_id,
                    detached_at: now,
                    silent: false,
                    note: Some(format!("[watchdog 已用相同参数重试：{reason}]")),
                },
            ));
            *tool_call_id = new_call_id;
        }
        (JobControlAction::Retry, _) => {
            return Err(format!(
                "retry is only supported for background tool calls, not {source}"
            ));
        }
        (JobControlAction::Stop, JobSource::SubTaskBatch { task_ids, .. }) => {
            for task_id in task_ids.iter() {
                if let Some(mut child) = index.get_task(task_id).and_then(|e| tasks.get_mut(e).ok())
                    && !child.status.is_terminal()
                {
                    let failure = if by_agent {
                        FailureReason::UserCancelled
                    } else {
                        FailureReason::WatchdogCancelled
                    };
                    child.mark_failed_reason(failure, reason.clone(), now);
                }
            }
            // watchdog 停止的批次保留作业：完成通知会带上失败原因。
            if by_agent {
                commands.entity(job_entity).try_despawn();
            }
        }
        (JobControlAction::Stop, JobSource::Shell { handle_id, .. }) => {
            if let Some(backend) = backend {
                backend.stop_session(*handle_id)?;
            }
            if by_agent {
                commands.entity(job_entity).try_despawn();
            }
        }
    }
    Ok(serde_json::json!({
        "status": match request.action {
            JobControlAction::Stop => "stopped",
            JobControlAction::Retry => "retrying",
        },
        "job_id": request.job_id.to_string(),
        "source": source,
    }))
}
