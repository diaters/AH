//! 用户确认 System
//!
//! 处理用户对 Tool 执行的确认请求和响应。

use bevy::prelude::*;
use tracing::{debug, warn};

use crate::{
    app::HarnessSettings,
    domain::{
        Agent, ExecutionError, GrantMode, SpaceKnowledge,
        Task, ToolCallingState, ToolConfirmationRequestMessage,
        ToolConfirmationResponseMessage, ToolExecutionRequestMessage, ToolExecutionResultMessage,
        ToolError, ToolPermission, BuiltinToolExecutors, ConfirmationOption, ToolContext,
    },
};

use super::orchestrator::{handle_tool_action, restore_task_after_tool, spawn_tool_error};

/// Tool 确认请求输出 System
///
/// 将确认请求通过 frontend_output_system 推送给前端
pub fn tool_confirmation_request_system(
    _agents: Query<&Agent>,
    _requests: Query<(Entity, &ToolConfirmationRequestMessage)>,
) {
    // frontend_output_system 负责监听 Added<ToolConfirmationRequestMessage> 并推送给前端，
    // 此 system 保留为占位，后续可在此添加额外逻辑（如日志增强）
}

/// Tool 确认响应处理 System
///
/// 处理用户的确认响应
#[allow(clippy::too_many_arguments)]
pub fn tool_confirmation_result_system(
    mut commands: Commands,
    mut agents: Query<&mut Agent>,
    mut tasks: Query<(Entity, &mut Task)>,
    executors: Res<BuiltinToolExecutors>,
    knowledge: Res<SpaceKnowledge>,
    tool_requests: Query<(Entity, &ToolExecutionRequestMessage)>,
    responses: Query<(Entity, &ToolConfirmationResponseMessage)>,
    calling_states: Query<&ToolCallingState>,
    settings: Res<HarnessSettings>,
) {
    for (entity, response) in &responses {
        // 查找对应的 Tool 执行请求（通过 pending_confirmation_id 关联）
        let Some((request_entity, tool_request)) = tool_requests
            .iter()
            .find(|(_, r)| r.pending_confirmation_id == Some(response.request_id))
        else {
            warn!(
                event = "ToolConfirmationNoMatch",
                request_id = %response.request_id,
                "no matching tool request found"
            );
            commands.entity(entity).despawn();
            continue;
        };

        // 从 ToolExecutionRequestMessage 保存的选项中查找
        let options = tool_request
            .pending_confirmation_options
            .clone()
            .unwrap_or_else(ConfirmationOption::default_options);
        let selected_option = options
            .iter()
            .find(|opt| opt.id == response.selected_option);

        match selected_option {
            Some(option) if option.is_deny() => {
                // 用户拒绝
                warn!(
                    event = "ToolConfirmationDenied",
                    tool_name = %tool_request.tool_name,
                    task_id = %tool_request.request.task_id,
                    agent_id = %tool_request.request.agent_id,
                    "tool execution denied by user"
                );

                // 生成错误结果
                let execution_result = crate::domain::AgentExecutionResult {
                    task_id: tool_request.request.task_id,
                    agent_id: tool_request.request.agent_id,
                    request_kind: tool_request.request.request_kind.clone(),
                    result: Err(ExecutionError::UserCancelled(
                        "user denied tool execution".to_string(),
                    )),
                    prompt: String::new(),
                    system_prompt: None,
                    tools: vec![],
                    reasoning_content: None,
                };

                commands.spawn(ToolExecutionResultMessage {
                    result: execution_result,
                    tool_name: tool_request.tool_name.clone(),
                    tool_output: Err(ToolError::PermissionDenied("user denied".to_string())),
                    tool_call_id: tool_request.tool_call_id.clone(),
                    processed: false,
                });

                restore_task_after_tool(&mut tasks, &calling_states, tool_request.request.task_id);
                commands.entity(request_entity).despawn();
            }
            Some(option) => {
                // 用户确认
                debug!(
                    event = "ToolConfirmationApproved",
                    tool_name = %tool_request.tool_name,
                    task_id = %tool_request.request.task_id,
                    agent_id = %tool_request.request.agent_id,
                    mode = ?option.mode,
                    "tool execution confirmed by user"
                );

                // Permanent 模式：更新 Agent 权限
                if option.mode == GrantMode::Permanent
                    && let Some(mut agent) = agents
                        .iter_mut()
                        .find(|a| a.id == tool_request.request.agent_id)
                {
                    agent
                        .tool_permissions
                        .overrides
                        .insert(tool_request.tool_name.clone(), ToolPermission::Allow);
                    debug!(
                        event = "AgentPermissionUpdated",
                        agent_id = %agent.id,
                        tool_name = %tool_request.tool_name,
                        new_permission = ?ToolPermission::Allow,
                        "agent permission updated to Allow permanently"
                    );
                }

                // 执行 Tool
                let Some(executor) = executors.get(&tool_request.tool_name) else {
                    warn!(
                        event = "ToolExecutorNotFound",
                        tool_name = %tool_request.tool_name,
                        "no executor registered for tool after confirmation"
                    );
                    spawn_tool_error(
                        &mut commands,
                        request_entity,
                        tool_request,
                        ToolError::NotFound(format!("executor for {}", tool_request.tool_name)),
                    );
                    restore_task_after_tool(
                        &mut tasks,
                        &calling_states,
                        tool_request.request.task_id,
                    );
                    commands.entity(entity).despawn();
                    continue;
                };

                let ctx = ToolContext {
                    knowledge: &knowledge,
                    default_wait_tasks_timeout_secs: settings.0.default_wait_tasks_timeout_secs,
                };
                let action = executor.execute(&tool_request.tool_input, &ctx);

                // Find the task entity
                if let Some((task_entity, _)) = tasks
                    .iter()
                    .find(|(_, t)| t.id == tool_request.request.task_id)
                {
                    handle_tool_action(
                        &mut commands,
                        request_entity,
                        task_entity,
                        tool_request,
                        action,
                        &tasks,
                    );
                }

                restore_task_after_tool(&mut tasks, &calling_states, tool_request.request.task_id);
            }
            None => {
                warn!(
                    event = "ToolConfirmationUnknownOption",
                    request_id = %response.request_id,
                    selected_option = %response.selected_option,
                    "unknown option selected"
                );
                // 清理残留的请求 entity，避免永久泄漏
                commands.entity(request_entity).despawn();
            }
        }

        commands.entity(entity).despawn();
    }
}
