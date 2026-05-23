//! Tool 执行相关 System
//!
//! 实现 Tool 的分发、执行和结果处理。

use bevy::prelude::*;
use tracing::{debug, warn};
use uuid::Uuid;

use crate::domain::{
    Agent, AgentExecutionResult, AgentSpawnRequestMessage, ApprovalDecision,
    ApprovalRequestMessage, ApprovalResultMessage, ConfirmationOption, ConfirmationSource,
    ExecutionError, GrantMode, OutputMessage, ShortTermMemory, SpaceKnowledge, SpaceToolRegistry,
    Task, TaskStatus, ToolConfirmationRequestMessage, ToolConfirmationResponseMessage,
    ToolDefinition, ToolError, ToolExecutionRequestMessage, ToolExecutionResultMessage,
    ToolPermission, WaitingReason,
};

/// Builtin Tool 执行器函数签名
#[allow(dead_code)]
pub type BuiltinToolExecutor =
    fn(&serde_json::Value, &SpaceKnowledge) -> Result<serde_json::Value, ToolError>;

/// 注册内置 Tool
pub fn register_builtin_tools(registry: &mut SpaceToolRegistry) {
    use crate::domain::{ToolExecutorKind, ToolSchema};

    // 示例：echo 工具（用于测试）
    registry.register(ToolDefinition {
        name: "echo".to_string(),
        description: "Echo back the input message".to_string(),
        parameters: ToolSchema::default(),
        default_permission: ToolPermission::Allow,
        executor: ToolExecutorKind::Builtin("echo".to_string()),
    });

    // knowledge_search 工具（从 SpaceKnowledge 检索）
    registry.register(ToolDefinition {
        name: "knowledge_search".to_string(),
        description: "Search for relevant information in the shared knowledge base. Use this when you need to access global knowledge, user preferences, or context that is not in your personal memory.".to_string(),
        parameters: ToolSchema {
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "The search query or keywords to look for"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of results to return (default: 3)",
                        "default": 3
                    }
                },
                "required": ["query"]
            }),
        },
        default_permission: ToolPermission::Allow,
        executor: ToolExecutorKind::Builtin("knowledge_search".to_string()),
    });

    // spawn_agent 工具（创建子 Agent）
    registry.register(ToolDefinition {
        name: "spawn_agent".to_string(),
        description: "Create a child agent with specified tools and capabilities. The child agent will be bound to the current task and automatically terminated when the task completes.".to_string(),
        parameters: ToolSchema {
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Name for the child agent"
                    },
                    "model": {
                        "type": "string",
                        "description": "Optional model to use. Defaults to parent agent's model."
                    },
                    "description": {
                        "type": "string",
                        "description": "Description of the child agent's capabilities"
                    },
                    "tools": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "List of tool names the child agent can use"
                    }
                },
                "required": ["name", "description", "tools"]
            }),
        },
        default_permission: ToolPermission::Confirm,
        executor: ToolExecutorKind::Builtin("spawn_agent".to_string()),
    });
}

/// 执行内置 Tool
fn execute_builtin_tool(
    name: &str,
    input: &serde_json::Value,
    knowledge: &SpaceKnowledge,
) -> Result<serde_json::Value, ToolError> {
    match name {
        "echo" => {
            // 简单 echo 实现
            Ok(input.clone())
        }
        "knowledge_search" => {
            let query = input
                .get("query")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ToolError::InvalidInput("missing 'query' parameter".to_string()))?;

            let limit = input.get("limit").and_then(|v| v.as_u64()).unwrap_or(3) as usize;

            // 简单关键词匹配检索
            let results: Vec<&str> = knowledge
                .entries
                .iter()
                .filter(|entry| entry.content.to_lowercase().contains(&query.to_lowercase()))
                .take(limit)
                .map(|entry| entry.content.as_str())
                .collect();

            Ok(serde_json::json!({
                "query": query,
                "results": results,
                "count": results.len()
            }))
        }
        "spawn_agent" => {
            // spawn_agent 不在这里执行，因为它需要访问 ECS World
            // 这里返回一个标记，由 tool_confirmation_result_system 特殊处理
            Ok(serde_json::json!({
                "status": "spawn_request_created",
                "message": "Agent spawn request has been submitted"
            }))
        }
        _ => Err(ToolError::NotFound(name.to_string())),
    }
}

/// Tool 分发 System
///
/// 检查 Tool 权限并决定直接执行、用户确认或父 Agent 审批
pub(crate) fn tool_dispatch_system(
    mut commands: Commands,
    mut tasks: Query<&mut Task>,
    registry: Res<SpaceToolRegistry>,
    knowledge: Res<SpaceKnowledge>,
    agents: Query<&Agent>,
    mut requests: Query<(Entity, &mut ToolExecutionRequestMessage)>,
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
                // spawn_agent 工具特殊处理：即使是 Allow 权限也需要创建 spawn 请求
                if tool_name == "spawn_agent" {
                    debug!(
                        event = "SpawnAgentDirectExecution",
                        tool_name = %tool_name,
                        agent_id = %agent.id,
                        "spawn_agent with Allow permission, creating spawn request directly"
                    );

                    // 解析参数
                    let name = request
                        .tool_input
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("child-agent")
                        .to_string();

                    let model = request
                        .tool_input
                        .get("model")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());

                    let description = request
                        .tool_input
                        .get("description")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();

                    let tools: Vec<String> = request
                        .tool_input
                        .get("tools")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|t| t.as_str().map(|s| s.to_string()))
                                .collect()
                        })
                        .unwrap_or_default();

                    debug!(
                        event = "SpawnAgentRequestCreated",
                        parent_agent_id = %agent.id,
                        task_id = %request.request.task_id,
                        name = %name,
                        model = ?model,
                        description = %description,
                        tools = ?tools,
                        "spawn_agent request submitted with Allow permission"
                    );

                    // 生成 AgentSpawnRequestMessage
                    commands.spawn(AgentSpawnRequestMessage {
                        parent_agent_id: agent.id,
                        task_id: request.request.task_id,
                        name,
                        model,
                        description,
                        tools,
                    });

                    // 生成成功结果
                    let execution_result = AgentExecutionResult {
                        task_id: request.request.task_id,
                        agent_id: agent.id,
                        request_kind: request.request.request_kind.clone(),
                        result: Ok("spawn_agent request submitted".to_string()),
                    };

                    commands.spawn(ToolExecutionResultMessage {
                        result: execution_result,
                        tool_name: "spawn_agent".to_string(),
                        tool_output: Ok(serde_json::json!({
                            "status": "spawn_request_created"
                        })),
                    });

                    // 清理请求
                    commands.entity(entity).despawn();
                    continue;
                }

                // 直接执行
                debug!(
                    event = "ToolExecutionAllowed",
                    tool_name = %tool_name,
                    agent_id = %agent.id,
                    "tool execution allowed"
                );
                execute_tool(&mut commands, entity, &request, tool_def, &knowledge);
            }
            ToolPermission::Confirm => {
                // 需要确认：根据工具类型和 Agent 层级决定路由

                // 1. spawn_agent 工具始终需要用户确认
                if tool_name == "spawn_agent" {
                    debug!(
                        event = "ToolRequiresUserConfirmation",
                        tool_name = %tool_name,
                        agent_id = %agent.id,
                        reason = "spawn_agent requires user approval",
                        "tool requires user confirmation"
                    );

                    // 将 Task 设置为等待用户确认状态
                    if let Some(mut task) =
                        tasks.iter_mut().find(|t| t.id == request.request.task_id)
                    {
                        task.status = TaskStatus::Waiting(WaitingReason::User);
                    }

                    // 生成用户确认请求消息
                    let request_id = Uuid::new_v4();
                    commands.spawn(ToolConfirmationRequestMessage {
                        request_id,
                        task_id: request.request.task_id,
                        agent_id: agent.id,
                        tool_name: tool_name.clone(),
                        tool_input: request.tool_input.clone(),
                        options: ConfirmationOption::default_options(),
                        source: ConfirmationSource::User,
                        parent_agent_id: None,
                    });

                    // 更新 ToolExecutionRequestMessage 的 pending_confirmation_id
                    request.pending_confirmation_id = Some(request_id);
                    continue;
                }

                // 2. 检查 Agent 是否有父 Agent，且父 Agent 有该工具的 Allow 权限
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
                    if let Some(mut task) =
                        tasks.iter_mut().find(|t| t.id == request.request.task_id)
                    {
                        task.status = TaskStatus::Waiting(WaitingReason::Approval);
                    }

                    // 生成父 Agent 审批请求消息
                    let request_id = Uuid::new_v4();
                    commands.spawn(ToolConfirmationRequestMessage {
                        request_id,
                        task_id: request.request.task_id,
                        agent_id: agent.id,
                        tool_name: tool_name.clone(),
                        tool_input: request.tool_input.clone(),
                        options: ConfirmationOption::default_options(),
                        source: ConfirmationSource::ParentAgent,
                        parent_agent_id: Some(parent.id),
                    });

                    // 更新 ToolExecutionRequestMessage 的 pending_confirmation_id
                    request.pending_confirmation_id = Some(request_id);
                    continue;
                }

                // 3. 无父 Agent 或父 Agent 无权限 → 用户确认
                debug!(
                    event = "ToolRequiresUserConfirmation",
                    tool_name = %tool_name,
                    agent_id = %agent.id,
                    reason = "no parent agent or parent lacks permission",
                    "tool requires user confirmation"
                );

                // 将 Task 设置为等待用户确认状态
                if let Some(mut task) = tasks.iter_mut().find(|t| t.id == request.request.task_id) {
                    task.status = TaskStatus::Waiting(WaitingReason::User);
                }

                // 生成用户确认请求消息
                let request_id = Uuid::new_v4();
                commands.spawn(ToolConfirmationRequestMessage {
                    request_id,
                    task_id: request.request.task_id,
                    agent_id: agent.id,
                    tool_name: tool_name.clone(),
                    tool_input: request.tool_input.clone(),
                    options: ConfirmationOption::default_options(),
                    source: ConfirmationSource::User,
                    parent_agent_id: None,
                });

                // 更新 ToolExecutionRequestMessage 的 pending_confirmation_id
                request.pending_confirmation_id = Some(request_id);
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

/// 执行 Tool
fn execute_tool(
    commands: &mut Commands,
    request_entity: Entity,
    request: &ToolExecutionRequestMessage,
    tool_def: &ToolDefinition,
    knowledge: &SpaceKnowledge,
) {
    let result = match &tool_def.executor {
        crate::domain::ToolExecutorKind::Builtin(name) => {
            execute_builtin_tool(name, &request.tool_input, knowledge)
        }
        crate::domain::ToolExecutorKind::External { .. } => Err(ToolError::NotFound(
            "external executor not supported in MVP".to_string(),
        )),
        crate::domain::ToolExecutorKind::Http { .. } => Err(ToolError::NotFound(
            "http executor not supported in MVP".to_string(),
        )),
    };

    // 生成结果消息
    let execution_result = AgentExecutionResult {
        task_id: request.request.task_id,
        agent_id: request.request.agent_id,
        request_kind: request.request.request_kind.clone(),
        result: Ok("tool executed".to_string()),
    };

    commands.spawn(ToolExecutionResultMessage {
        result: execution_result,
        tool_name: request.tool_name.clone(),
        tool_output: result,
    });

    // 清理请求
    commands.entity(request_entity).despawn();
}

/// 生成 Tool 错误结果
fn spawn_tool_error(
    commands: &mut Commands,
    request_entity: Entity,
    request: &ToolExecutionRequestMessage,
    error: ToolError,
) {
    let execution_result = AgentExecutionResult {
        task_id: request.request.task_id,
        agent_id: request.request.agent_id,
        request_kind: request.request.request_kind.clone(),
        result: Err(ExecutionError::Unknown(error.to_string())),
    };

    commands.spawn(ToolExecutionResultMessage {
        result: execution_result,
        tool_name: request.tool_name.clone(),
        tool_output: Err(error),
    });

    commands.entity(request_entity).despawn();
}

/// Tool 结果处理 System
///
/// 处理 Tool 执行结果，记录 ToolCall，恢复原 Task
pub(crate) fn tool_result_system(
    mut commands: Commands,
    clock: Res<crate::app::Clock>,
    results: Query<(Entity, &ToolExecutionResultMessage)>,
    mut tasks: Query<(&Task, Option<&mut ShortTermMemory>)>,
) {
    for (entity, result) in &results {
        // 查找对应的 Task 及其 ShortTermMemory
        for (task, short_term_memory) in &mut tasks {
            if task.id != result.result.task_id {
                continue;
            }

            match &result.tool_output {
                Ok(output) => {
                    let output_str =
                        serde_json::to_string(output).unwrap_or_else(|_| output.to_string());
                    debug!(
                        event = "ToolExecuted",
                        tool_name = %result.tool_name,
                        task_id = %task.id,
                        agent_id = %result.result.agent_id,
                        success = true,
                        output = %output_str,
                        output_len = output_str.len(),
                        "tool execution completed"
                    );

                    // 记录 ToolCall 到 ShortTermMemory
                    if let Some(mut stm) = short_term_memory {
                        stm.record_tool_call(
                            result.tool_name.clone(),
                            serde_json::to_string(output).unwrap_or_default(),
                            output_str,
                            clock.0,
                        );
                    }
                }
                Err(e) => {
                    warn!(
                        event = "ToolExecutionFailed",
                        tool_name = %result.tool_name,
                        task_id = %task.id,
                        agent_id = %result.result.agent_id,
                        success = false,
                        error = %e,
                        "tool execution failed"
                    );
                }
            }
            break;
        }

        commands.entity(entity).despawn();
    }
}

/// 审批分发 System
///
/// 为需要父 Agent 决策的请求创建审批任务
#[allow(dead_code)]
pub(crate) fn approval_dispatch_system(
    mut commands: Commands,
    tasks: Query<&Task>,
    approval_requests: Query<(Entity, &ApprovalRequestMessage)>,
) {
    for (entity, request) in &approval_requests {
        // 创建审批任务（简化实现：直接拒绝，因为 MVP 没有完整的审批 UI）
        debug!(
            event = "ApprovalRequestReceived",
            request_id = %request.request_id,
            tool_name = %request.tool_name,
            source_task_id = %request.source_task_id,
            parent_agent_id = %request.parent_agent_id,
            child_agent_id = %request.child_agent_id,
            tool_input = ?request.tool_input,
            "approval request received - auto-rejecting in MVP"
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

        // 生成拒绝结果
        commands.spawn(ApprovalResultMessage {
            request_id: request.request_id,
            source_task_id: request.source_task_id,
            approval_task_id: request.approval_task_id,
            decision: ApprovalDecision::Rejected,
            reasoning: "MVP auto-reject: approval UI not implemented".to_string(),
            grant_mode: GrantMode::Once,
        });

        commands.entity(entity).despawn();
    }
}

/// 审批结果处理 System
///
/// 处理父 Agent 审批结果，更新权限，恢复任务
pub(crate) fn approval_result_system(
    mut commands: Commands,
    mut agents: Query<&mut Agent>,
    mut tasks: Query<&mut Task>,
    registry: Res<SpaceToolRegistry>,
    knowledge: Res<SpaceKnowledge>,
    approval_results: Query<(Entity, &ApprovalResultMessage)>,
    tool_requests: Query<(Entity, &ToolExecutionRequestMessage)>,
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
                warn!(
                    event = "ToolApprovalRejected",
                    tool_name = %tool_request.tool_name,
                    task_id = %tool_request.request.task_id,
                    agent_id = %tool_request.request.agent_id,
                    reasoning = %result.reasoning,
                    "tool execution rejected by parent agent"
                );

                let execution_result = AgentExecutionResult {
                    task_id: tool_request.request.task_id,
                    agent_id: tool_request.request.agent_id,
                    request_kind: tool_request.request.request_kind.clone(),
                    result: Err(ExecutionError::UserCancelled(format!(
                        "parent agent rejected: {}",
                        result.reasoning
                    ))),
                };

                commands.spawn(ToolExecutionResultMessage {
                    result: execution_result,
                    tool_name: tool_request.tool_name.clone(),
                    tool_output: Err(ToolError::PermissionDenied(format!(
                        "parent agent rejected: {}",
                        result.reasoning
                    ))),
                });

                // 恢复 Task 状态
                if let Some(mut task) = tasks.iter_mut().find(|t| t.id == result.source_task_id) {
                    task.status = TaskStatus::Ready;
                }

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
                if let Some(tool_def) = registry.get(&tool_request.tool_name) {
                    execute_tool(
                        &mut commands,
                        request_entity,
                        tool_request,
                        tool_def,
                        &knowledge,
                    );
                }

                // 恢复 Task 状态
                if let Some(mut task) = tasks.iter_mut().find(|t| t.id == result.source_task_id) {
                    task.status = TaskStatus::Ready;
                }
            }
        }

        commands.entity(entity).despawn();
    }
}

/// Agent 演化 System
///
/// 将批准后的长期权限修正或经验写回 Agent
#[allow(dead_code)]
pub(crate) fn agent_evolution_system(agents: Query<&Agent>) {
    // MVP 阶段暂不实现具体演化逻辑
    // 后续扩展：
    // - 从 Tool 执行结果中提取经验
    // - 更新 Agent.experience
    // - 根据 Permanent 确认更新 Agent.tool_permissions
    let _ = agents;
}

/// Tool 确认请求输出 System
///
/// 将确认请求发送到输出 channel
pub(crate) fn tool_confirmation_request_system(
    mut commands: Commands,
    agents: Query<&Agent>,
    sender: Res<crate::app::OutputSender>,
    requests: Query<(Entity, &ToolConfirmationRequestMessage)>,
) {
    for (entity, request) in &requests {
        // 获取 Agent 名称
        let agent_name = agents
            .iter()
            .find(|a| a.id == request.agent_id)
            .map(|a| a.profile.name.as_str())
            .unwrap_or("unknown");

        // 格式化 tool_input 摘要
        let input_summary = serde_json::to_string(&request.tool_input)
            .unwrap_or_else(|_| request.tool_input.to_string());
        let input_display = if input_summary.len() > 100 {
            format!("{}...", &input_summary[..100])
        } else {
            input_summary.clone()
        };

        debug!(
            event = "ToolConfirmationRequest",
            request_id = %request.request_id,
            tool_name = %request.tool_name,
            agent_id = %request.agent_id,
            agent_name = %agent_name,
            task_id = %request.task_id,
            tool_input = ?request.tool_input,
            options_count = request.options.len(),
            "sending tool confirmation request to user"
        );

        // 构建标题
        let title = format!(
            "[Tool Confirm] Agent \"{}\" requests to execute \"{}\"\nInput: {}",
            agent_name, request.tool_name, input_display
        );

        // 发送确认请求
        let output =
            OutputMessage::confirmation_request(request.request_id, title, request.options.clone());

        if let Err(e) = sender.0.send(output) {
            warn!(
                event = "ConfirmationRequestSendFailed",
                request_id = %request.request_id,
                error = %e,
                "failed to send confirmation request"
            );
        }

        commands.entity(entity).despawn();
    }
}

/// Tool 确认响应处理 System
///
/// 处理用户的确认响应
pub(crate) fn tool_confirmation_result_system(
    mut commands: Commands,
    mut agents: Query<&mut Agent>,
    mut tasks: Query<&mut Task>,
    registry: Res<SpaceToolRegistry>,
    knowledge: Res<SpaceKnowledge>,
    tool_requests: Query<(Entity, &ToolExecutionRequestMessage)>,
    responses: Query<(Entity, &ToolConfirmationResponseMessage)>,
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

        // 查找选中的选项
        let default_options = ConfirmationOption::default_options();
        let selected_option = default_options
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
                let execution_result = AgentExecutionResult {
                    task_id: tool_request.request.task_id,
                    agent_id: tool_request.request.agent_id,
                    request_kind: tool_request.request.request_kind.clone(),
                    result: Err(ExecutionError::UserCancelled(
                        "user denied tool execution".to_string(),
                    )),
                };

                commands.spawn(ToolExecutionResultMessage {
                    result: execution_result,
                    tool_name: tool_request.tool_name.clone(),
                    tool_output: Err(ToolError::PermissionDenied("user denied".to_string())),
                });

                // 恢复 Task 状态
                if let Some(mut task) = tasks
                    .iter_mut()
                    .find(|t| t.id == tool_request.request.task_id)
                {
                    task.status = TaskStatus::Ready;
                }

                // 清理请求
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
                if option.mode == crate::domain::ConfirmMode::Permanent
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

                // spawn_agent 工具特殊处理：不执行 builtin，而是生成 spawn 请求
                if tool_request.tool_name == "spawn_agent" {
                    // 解析参数
                    let name = tool_request
                        .tool_input
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("child-agent")
                        .to_string();

                    let model = tool_request
                        .tool_input
                        .get("model")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());

                    let description = tool_request
                        .tool_input
                        .get("description")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();

                    let tools: Vec<String> = tool_request
                        .tool_input
                        .get("tools")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|t| t.as_str().map(|s| s.to_string()))
                                .collect()
                        })
                        .unwrap_or_default();

                    debug!(
                        event = "SpawnAgentRequestCreated",
                        parent_agent_id = %tool_request.request.agent_id,
                        task_id = %tool_request.request.task_id,
                        name = %name,
                        model = ?model,
                        description = %description,
                        tools = ?tools,
                        "spawn_agent request submitted"
                    );

                    // 生成 AgentSpawnRequestMessage
                    commands.spawn(AgentSpawnRequestMessage {
                        parent_agent_id: tool_request.request.agent_id,
                        task_id: tool_request.request.task_id,
                        name,
                        model,
                        description,
                        tools,
                    });

                    // 生成成功结果
                    let execution_result = AgentExecutionResult {
                        task_id: tool_request.request.task_id,
                        agent_id: tool_request.request.agent_id,
                        request_kind: tool_request.request.request_kind.clone(),
                        result: Ok("spawn_agent request submitted".to_string()),
                    };

                    commands.spawn(ToolExecutionResultMessage {
                        result: execution_result,
                        tool_name: "spawn_agent".to_string(),
                        tool_output: Ok(serde_json::json!({
                            "status": "spawn_request_created"
                        })),
                    });

                    // 恢复 Task 状态
                    if let Some(mut task) = tasks
                        .iter_mut()
                        .find(|t| t.id == tool_request.request.task_id)
                    {
                        task.status = TaskStatus::Ready;
                    }

                    // 清理请求
                    commands.entity(request_entity).despawn();
                    commands.entity(entity).despawn();
                    continue;
                }

                // 执行 Tool
                if let Some(tool_def) = registry.get(&tool_request.tool_name) {
                    execute_tool(
                        &mut commands,
                        request_entity,
                        tool_request,
                        tool_def,
                        &knowledge,
                    );
                }

                // 恢复 Task 状态
                if let Some(mut task) = tasks
                    .iter_mut()
                    .find(|t| t.id == tool_request.request.task_id)
                {
                    task.status = TaskStatus::Ready;
                }
            }
            None => {
                warn!(
                    event = "ToolConfirmationUnknownOption",
                    request_id = %response.request_id,
                    selected_option = %response.selected_option,
                    "unknown option selected"
                );
            }
        }

        commands.entity(entity).despawn();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        AgentCapabilities, AgentExperience, AgentKind, AgentProfile, AgentToolPermissions,
    };

    #[allow(dead_code)]
    fn test_agent() -> Agent {
        Agent {
            id: uuid::Uuid::nil(),
            profile: AgentProfile {
                name: "test".to_string(),
                model: "test-model".to_string(),
            },
            capabilities: AgentCapabilities {
                tags: vec![],
                description: "test agent".to_string(),
            },
            kind: AgentKind::Persistent,
            parent_id: None,
            bound_task_id: None,
            tool_permissions: AgentToolPermissions::default(),
            experience: AgentExperience::default(),
        }
    }

    #[test]
    fn register_builtin_tools_adds_echo() {
        let mut registry = SpaceToolRegistry::default();
        register_builtin_tools(&mut registry);
        assert!(registry.exists("echo"));
    }

    #[test]
    fn execute_builtin_echo() {
        let input = serde_json::json!({"message": "hello"});
        let knowledge = SpaceKnowledge::default();
        let result = execute_builtin_tool("echo", &input, &knowledge);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), input);
    }

    #[test]
    fn execute_builtin_unknown_returns_error() {
        let input = serde_json::json!({});
        let knowledge = SpaceKnowledge::default();
        let result = execute_builtin_tool("unknown", &input, &knowledge);
        assert!(matches!(result, Err(ToolError::NotFound(_))));
    }

    #[test]
    fn execute_builtin_knowledge_search() {
        use crate::domain::{EntryRole, MemoryEntry};

        let mut knowledge = SpaceKnowledge::default();
        knowledge.entries.push(MemoryEntry::new(
            EntryRole::User,
            "The project uses Rust and Bevy framework",
        ));
        knowledge.entries.push(MemoryEntry::new(
            EntryRole::User,
            "The system follows ECS architecture",
        ));

        // Search for "rust"
        let input = serde_json::json!({"query": "rust"});
        let result = execute_builtin_tool("knowledge_search", &input, &knowledge);
        assert!(result.is_ok());
        let output = result.unwrap();
        assert_eq!(output["count"], 1);
        assert!(output["results"].as_array().unwrap().len() == 1);

        // Search for "bevy"
        let input = serde_json::json!({"query": "bevy"});
        let result = execute_builtin_tool("knowledge_search", &input, &knowledge);
        assert!(result.is_ok());
        let output = result.unwrap();
        assert_eq!(output["count"], 1);

        // Search for non-existent
        let input = serde_json::json!({"query": "python"});
        let result = execute_builtin_tool("knowledge_search", &input, &knowledge);
        assert!(result.is_ok());
        let output = result.unwrap();
        assert_eq!(output["count"], 0);
    }

    #[test]
    fn agent_tool_permissions_default_is_confirm() {
        let perms = AgentToolPermissions::default();
        assert_eq!(
            perms.get_permission("unknown_tool"),
            ToolPermission::Confirm
        );
    }

    #[test]
    fn agent_tool_permissions_override() {
        let mut perms = AgentToolPermissions {
            default_permission: ToolPermission::Deny,
            ..Default::default()
        };
        perms
            .overrides
            .insert("echo".to_string(), ToolPermission::Allow);

        assert_eq!(perms.get_permission("echo"), ToolPermission::Allow);
        assert_eq!(perms.get_permission("other"), ToolPermission::Deny);
    }
}
