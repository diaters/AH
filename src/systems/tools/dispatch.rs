//! Tool 分发 System
//!
//! 检查 Tool 权限并决定直接执行、用户确认或父 Agent 审批。

use bevy::prelude::*;
use tracing::{debug, warn};
use uuid::Uuid;

use crate::{
    app::HarnessSettings,
    domain::{
        Agent, ApprovalRequestMessage, BuiltinToolExecutors, ConfirmationOption,
        ConfirmationSource, ExperienceStore, SharedKnowledgeBase, SpaceToolRegistry, Task,
        TaskStatus, ToolConfirmationRequestMessage, ToolContext, ToolError,
        ToolExecutionRequestMessage, ToolPermission, WaitingReason,
    },
    systems::NativeProcessBackend,
};

use super::orchestrator::restore_task_after_tool;
use super::orchestrator::{handle_tool_action, spawn_tool_error};

/// Tool 分发 System
///
/// 检查 Tool 权限并决定直接执行、用户确认或父 Agent 审批
#[allow(clippy::too_many_arguments)]
pub fn tool_dispatch_system(
    mut commands: Commands,
    mut tasks: Query<(Entity, &mut Task)>,
    registry: Res<SpaceToolRegistry>,
    executors: Res<BuiltinToolExecutors>,
    knowledge: Res<SharedKnowledgeBase>,
    mut experience_store: ResMut<ExperienceStore>,
    agents: Query<&Agent>,
    calling_states: Query<&crate::domain::ToolCallingState>,
    mut requests: Query<(Entity, &mut ToolExecutionRequestMessage)>,
    settings: Res<HarnessSettings>,
    backend: Res<NativeProcessBackend>,
) {
    for (entity, mut request) in &mut requests {
        // 跳过已经在等待确认的请求
        if request.pending_confirmation_id.is_some() {
            continue;
        }

        let tool_name = request.tool_name.clone();

        // 查找 Tool 定义
        let Some(tool_def) = registry.get(&tool_name) else {
            warn!(
                event = "ToolNotFound",
                tool_name = %tool_name,
                task_id = %request.request.task_id,
                agent_id = %request.request.agent_id,
                "tool not found in registry"
            );
            spawn_tool_error(
                &mut commands,
                entity,
                &request,
                ToolError::NotFound(tool_name.clone()),
            );
            continue;
        };

        // 获取 Agent 权限
        let Some(agent) = agents.iter().find(|a| a.id == request.request.agent_id) else {
            warn!(
                event = "AgentNotFound",
                agent_id = %request.request.agent_id,
                tool_name = %tool_name,
                "agent not found for tool execution"
            );
            spawn_tool_error(
                &mut commands,
                entity,
                &request,
                ToolError::NotFound(format!("agent {}", request.request.agent_id)),
            );
            continue;
        };

        // 检查 required_tag
        if let Some(required_tag) = &tool_def.required_tag
            && !agent.capabilities.tags.iter().any(|t| t == required_tag)
        {
            warn!(
                event = "ToolTagDenied",
                tool_name = %tool_name,
                agent_id = %agent.id,
                agent_name = %agent.profile.name,
                required_tag = %required_tag,
                "agent lacks required tag for tool"
            );
            spawn_tool_error(
                &mut commands,
                entity,
                &request,
                ToolError::PermissionDenied(format!(
                    "tool '{}' requires tag '{}'",
                    tool_name, required_tag
                )),
            );
            continue;
        }

        let permission = agent.tool_permissions.get_permission(&tool_name);

        debug!(
            event = "ToolDispatch",
            tool_name = %tool_name,
            agent_id = %agent.id,
            agent_name = %agent.profile.name,
            permission = ?permission,
            tool_input = ?request.tool_input,
            task_id = %request.request.task_id,
            "tool execution decision"
        );

        match permission {
            ToolPermission::Allow => {
                // 直接执行
                let Some(executor) = executors.get(&tool_name) else {
                    warn!(
                        event = "ToolExecutorNotFound",
                        tool_name = %tool_name,
                        "no executor registered for tool"
                    );
                    spawn_tool_error(
                        &mut commands,
                        entity,
                        &request,
                        ToolError::NotFound(format!("executor for {}", tool_name)),
                    );
                    continue;
                };

                debug!(
                    event = "ToolExecutionAllowed",
                    tool_name = %tool_name,
                    agent_id = %agent.id,
                    "tool execution allowed"
                );

                let ctx = ToolContext {
                    knowledge: &knowledge,
                    experience_store: &experience_store,
                    default_wait_tasks_timeout_secs: settings.0.default_wait_tasks_timeout_secs,
                    shell_default_tail_lines: settings.0.shell_default_tail_lines,
                    shell_max_tail_lines: settings.0.shell_max_tail_lines,
                    shell_default_exec_timeout_secs: settings.0.shell_default_exec_timeout_secs,
                    shell_default_stop_timeout_secs: settings.0.shell_default_stop_timeout_secs,
                    current_task_id: request.request.task_id,
                    current_agent_id: request.request.agent_id,
                };
                let action = executor.execute(&request.tool_input, &ctx);

                // Find the task entity
                if let Some((task_entity, _)) =
                    tasks.iter().find(|(_, t)| t.id == request.request.task_id)
                {
                    let parent_agent_id = agent.parent_id;
                    handle_tool_action(
                        &mut commands,
                        entity,
                        task_entity,
                        &request,
                        action,
                        &mut tasks,
                        &*backend,
                        &mut experience_store,
                        parent_agent_id,
                    );
                }

                restore_task_after_tool(&mut tasks, &calling_states, request.request.task_id);
            }
            ToolPermission::Confirm => {
                // 检查 Agent 是否有父 Agent，且父 Agent 有该工具的 Allow 权限
                if let Some(parent_id) = agent.parent_id
                    && let Some(parent) = agents.iter().find(|a| a.id == parent_id)
                    && parent.has_permission(&tool_name)
                {
                    debug!(
                        event = "ToolRequiresParentApproval",
                        tool_name = %tool_name,
                        agent_id = %agent.id,
                        parent_agent_id = %parent.id,
                        reason = "parent agent has permission",
                        "tool requires parent agent approval"
                    );

                    // 将 Task 设置为等待父 Agent 审批状态
                    if let Some((_, mut task)) = tasks
                        .iter_mut()
                        .find(|(_, t)| t.id == request.request.task_id)
                    {
                        task.status = TaskStatus::Waiting(WaitingReason::Approval);
                    }

                    // 生成父 Agent 审批请求消息
                    let request_id = Uuid::new_v4();
                    commands.spawn(ApprovalRequestMessage {
                        request_id,
                        tool_name: tool_name.clone(),
                        source_task_id: request.request.task_id,
                        parent_agent_id: parent.id,
                        child_agent_id: agent.id,
                        tool_input: request.tool_input.clone(),
                        approval_task_id: Uuid::new_v4(),
                        context: String::new(),
                    });

                    request.pending_confirmation_id = Some(request_id);
                    continue;
                }

                // 无父 Agent 或父 Agent 无权限 → 用户确认
                debug!(
                    event = "ToolRequiresUserConfirmation",
                    tool_name = %tool_name,
                    agent_id = %agent.id,
                    reason = "no parent agent or parent lacks permission",
                    "tool requires user confirmation"
                );

                // 将 Task 设置为等待用户确认状态
                if let Some((_, mut task)) = tasks
                    .iter_mut()
                    .find(|(_, t)| t.id == request.request.task_id)
                {
                    task.status = TaskStatus::Waiting(WaitingReason::User);
                }

                // 生成用户确认请求消息
                let request_id = Uuid::new_v4();
                let options = ConfirmationOption::default_options();
                commands.spawn(ToolConfirmationRequestMessage {
                    request_id,
                    task_id: request.request.task_id,
                    agent_id: agent.id,
                    tool_name: tool_name.clone(),
                    tool_input: request.tool_input.clone(),
                    options: options.clone(),
                    source: ConfirmationSource::User,
                    parent_agent_id: None,
                });

                request.pending_confirmation_id = Some(request_id);
                request.pending_confirmation_options = Some(options);
            }
            ToolPermission::Deny => {
                // 拒绝执行
                warn!(
                    event = "ToolExecutionDenied",
                    tool_name = %tool_name,
                    agent_id = %agent.id,
                    "tool execution denied"
                );
                spawn_tool_error(
                    &mut commands,
                    entity,
                    &request,
                    ToolError::PermissionDenied(tool_name.clone()),
                );
            }
        }
    }
}
