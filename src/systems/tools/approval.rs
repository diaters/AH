//! 审批 System
//!
//! 处理父 Agent 审批请求和结果。

use bevy::prelude::*;
use tracing::debug;

use crate::{
    app::HarnessSettings,
    domain::{
        Agent, ApprovalDecision, ApprovalRequestMessage, ApprovalResultMessage,
        BuiltinToolExecutors, ExecutionError, GrantMode, SpaceKnowledge, Task, TaskStatus,
        ToolCallingState, ToolContext, ToolError, ToolExecutionRequestMessage,
        ToolExecutionResultMessage, WaitingReason,
    },
    systems::NativeProcessBackend,
};

use super::orchestrator::{handle_tool_action, restore_task_after_tool, spawn_tool_error};

/// 审批分发 System
///
/// 为需要父 Agent 决策的请求创建审批任务。
///
/// TODO: 当前为 MVP 硬编码自动通过，需替换为真实父 Agent LLM 审查：
///       1. 给父 Agent 创建审批用 LLM 调用，传入 tool 信息和上下文
///       2. 解析 LLM 返回的决策（Approved/Rejected + reasoning）
///       3. 支持 GrantMode::Permanent 将权限写入 Agent
pub fn approval_dispatch_system(
    mut commands: Commands,
    tasks: Query<&Task>,
    approval_requests: Query<(Entity, &ApprovalRequestMessage)>,
) {
    for (entity, request) in &approval_requests {
        debug!(
            event = "ApprovalRequestReceived",
            request_id = %request.request_id,
            tool_name = %request.tool_name,
            source_task_id = %request.source_task_id,
            parent_agent_id = %request.parent_agent_id,
            child_agent_id = %request.child_agent_id,
            tool_input = ?request.tool_input,
            "approval request received - auto-approving in MVP"
        );

        // 记录原 Task 状态
        if let Some(task) = tasks.iter().find(|t| t.id == request.source_task_id)
            && task.status == TaskStatus::Waiting(WaitingReason::Approval)
        {
            debug!(
                event = "SourceTaskWaiting",
                task_id = %task.id,
                "source task is waiting for approval"
            );
        }

        // 生成自动批准结果
        commands.spawn(ApprovalResultMessage {
            request_id: request.request_id,
            source_task_id: request.source_task_id,
            approval_task_id: request.approval_task_id,
            decision: ApprovalDecision::Approved,
            reasoning: "MVP auto-approve: parent agent approval".to_string(),
            grant_mode: GrantMode::Once,
        });

        commands.entity(entity).despawn();
    }
}

/// 审批结果处理 System
///
/// 处理父 Agent 审批结果，更新权限，恢复任务
#[allow(clippy::too_many_arguments)]
pub fn approval_result_system(
    mut commands: Commands,
    mut agents: Query<&mut Agent>,
    mut tasks: Query<(Entity, &mut Task)>,
    executors: Res<BuiltinToolExecutors>,
    knowledge: Res<SpaceKnowledge>,
    approval_results: Query<(Entity, &ApprovalResultMessage)>,
    tool_requests: Query<(Entity, &ToolExecutionRequestMessage)>,
    calling_states: Query<&ToolCallingState>,
    settings: Res<HarnessSettings>,
    backend: Res<NativeProcessBackend>,
) {
    for (entity, result) in &approval_results {
        // 查找对应的 Tool 执行请求
        let Some((request_entity, tool_request)) = tool_requests
            .iter()
            .find(|(_, r)| r.pending_confirmation_id == Some(result.request_id))
        else {
            debug!(
                event = "ApprovalResultNoMatch",
                request_id = %result.request_id,
                "no matching tool request found, may have been processed"
            );
            commands.entity(entity).despawn();
            continue;
        };

        match result.decision {
            ApprovalDecision::Rejected => {
                debug!(
                    event = "ToolApprovalRejected",
                    tool_name = %tool_request.tool_name,
                    task_id = %tool_request.request.task_id,
                    agent_id = %tool_request.request.agent_id,
                    reasoning = %result.reasoning,
                    "tool execution rejected by parent agent"
                );

                let execution_result = crate::domain::AgentExecutionResult {
                    task_id: tool_request.request.task_id,
                    agent_id: tool_request.request.agent_id,
                    request_kind: tool_request.request.request_kind.clone(),
                    result: Err(ExecutionError::UserCancelled(format!(
                        "parent agent rejected: {}",
                        result.reasoning
                    ))),
                    prompt: String::new(),
                    system_prompt: None,
                    tools: vec![],
                    reasoning_content: None,
                    work_item_id: None,
                };

                commands.spawn(ToolExecutionResultMessage {
                    result: execution_result,
                    tool_name: tool_request.tool_name.clone(),
                    tool_output: Err(ToolError::PermissionDenied(format!(
                        "parent agent rejected: {}",
                        result.reasoning
                    ))),
                    tool_call_id: tool_request.tool_call_id.clone(),
                    processed: false,
                });

                restore_task_after_tool(&mut tasks, &calling_states, result.source_task_id);
                commands.entity(request_entity).despawn();
            }
            ApprovalDecision::Approved => {
                debug!(
                    event = "ToolApprovalGranted",
                    tool_name = %tool_request.tool_name,
                    task_id = %tool_request.request.task_id,
                    agent_id = %tool_request.request.agent_id,
                    grant_mode = ?result.grant_mode,
                    "tool execution approved by parent agent"
                );

                // Permanent 模式：更新 Agent 权限
                if result.grant_mode == GrantMode::Permanent
                    && let Some(mut agent) = agents
                        .iter_mut()
                        .find(|a| a.id == tool_request.request.agent_id)
                {
                    agent.grant_permission(tool_request.tool_name.clone());
                    debug!(
                        event = "AgentPermissionUpdated",
                        agent_id = %agent.id,
                        tool_name = %tool_request.tool_name,
                        "agent permission updated to Allow permanently"
                    );
                }

                // 执行 Tool
                let Some(executor) = executors.get(&tool_request.tool_name) else {
                    warn!(
                        event = "ToolExecutorNotFound",
                        tool_name = %tool_request.tool_name,
                        "no executor registered for tool after approval"
                    );
                    spawn_tool_error(
                        &mut commands,
                        request_entity,
                        tool_request,
                        ToolError::NotFound(format!("executor for {}", tool_request.tool_name)),
                    );
                    restore_task_after_tool(&mut tasks, &calling_states, result.source_task_id);
                    commands.entity(entity).despawn();
                    continue;
                };

                let ctx = ToolContext {
                    knowledge: &knowledge,
                    default_wait_tasks_timeout_secs: settings.0.default_wait_tasks_timeout_secs,
                    shell_default_tail_lines: settings.0.shell_default_tail_lines,
                    shell_max_tail_lines: settings.0.shell_max_tail_lines,
                    shell_default_wait_timeout_secs: settings.0.shell_default_wait_timeout_secs,
                    shell_default_stop_timeout_secs: settings.0.shell_default_stop_timeout_secs,
                    current_task_id: tool_request.request.task_id,
                    current_agent_id: tool_request.request.agent_id,
                };
                let action = executor.execute(&tool_request.tool_input, &ctx);

                // Find the task entity
                if let Some((task_entity, _)) = tasks
                    .iter_mut()
                    .find(|(_, t)| t.id == tool_request.request.task_id)
                {
                    handle_tool_action(
                        &mut commands,
                        request_entity,
                        task_entity,
                        tool_request,
                        action,
                        &mut tasks,
                        &*backend,
                    );
                }

                restore_task_after_tool(&mut tasks, &calling_states, result.source_task_id);
            }
        }

        commands.entity(entity).despawn();
    }
}

use tracing::warn;
