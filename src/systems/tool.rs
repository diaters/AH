//! Tool 执行相关 System
//!
//! 实现 Tool 的分发、执行和结果处理。

use bevy::prelude::*;
use tracing::{info, warn};

use crate::domain::{
    Agent, AgentExecutionResult, ApprovalDecision, ApprovalRequestMessage, ApprovalResultMessage,
    ExecutionError, ShortTermMemory, SpaceToolRegistry, Task, TaskStatus, ToolDefinition,
    ToolError, ToolExecutionRequestMessage, ToolExecutionResultMessage, ToolPermission,
    WaitingReason,
};

/// Builtin Tool 执行器函数签名
#[allow(dead_code)]
pub type BuiltinToolExecutor = fn(&serde_json::Value) -> Result<serde_json::Value, ToolError>;

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
}

/// 执行内置 Tool
fn execute_builtin_tool(name: &str, input: &serde_json::Value) -> Result<serde_json::Value, ToolError> {
    match name {
        "echo" => {
            // 简单 echo 实现
            Ok(input.clone())
        }
        _ => Err(ToolError::NotFound(name.to_string())),
    }
}

/// Tool 分发 System
///
/// 检查 Tool 权限并决定直接执行、用户确认或父 Agent 审批
pub(crate) fn tool_dispatch_system(
    mut commands: Commands,
    registry: Res<SpaceToolRegistry>,
    agents: Query<&Agent>,
    requests: Query<(Entity, &ToolExecutionRequestMessage)>,
) {
    for (entity, request) in &requests {
        let tool_name = &request.tool_name;

        // 查找 Tool 定义
        let Some(tool_def) = registry.get(tool_name) else {
            warn!(tool_name = %tool_name, "tool not found in registry");
            spawn_tool_error(
                &mut commands,
                entity,
                request,
                ToolError::NotFound(tool_name.clone()),
            );
            continue;
        };

        // 获取 Agent 权限
        let Some(agent) = agents.iter().find(|a| a.id == request.request.agent_id) else {
            warn!(agent_id = %request.request.agent_id, "agent not found for tool execution");
            spawn_tool_error(
                &mut commands,
                entity,
                request,
                ToolError::NotFound(format!("agent {}", request.request.agent_id)),
            );
            continue;
        };

        let permission = agent.tool_permissions.get_permission(tool_name);

        match permission {
            ToolPermission::Allow => {
                // 直接执行
                info!(tool_name = %tool_name, agent_id = %agent.id, "tool execution allowed");
                execute_tool(&mut commands, entity, request, tool_def);
            }
            ToolPermission::Confirm => {
                // 需要用户确认（P2 实现）
                warn!(tool_name = %tool_name, "tool requires user confirmation - not implemented in P1");
                spawn_tool_error(
                    &mut commands,
                    entity,
                    request,
                    ToolError::PermissionDenied(format!("{} requires user confirmation", tool_name)),
                );
            }
            ToolPermission::Deny => {
                // 拒绝执行
                warn!(tool_name = %tool_name, agent_id = %agent.id, "tool execution denied");
                spawn_tool_error(
                    &mut commands,
                    entity,
                    request,
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
) {
    let result = match &tool_def.executor {
        crate::domain::ToolExecutorKind::Builtin(name) => {
            execute_builtin_tool(name, &request.tool_input)
        }
        crate::domain::ToolExecutorKind::External { .. } => {
            Err(ToolError::NotFound("external executor not supported in MVP".to_string()))
        }
        crate::domain::ToolExecutorKind::Http { .. } => {
            Err(ToolError::NotFound("http executor not supported in MVP".to_string()))
        }
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
                    info!(
                        tool_name = %result.tool_name,
                        task_id = %task.id,
                        "tool execution completed"
                    );

                    // 记录 ToolCall 到 ShortTermMemory
                    if let Some(mut stm) = short_term_memory {
                        let output_str = serde_json::to_string(output)
                            .unwrap_or_else(|_| output.to_string());
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
                        tool_name = %result.tool_name,
                        task_id = %task.id,
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
        warn!(
            request_id = %request.request_id,
            tool_name = %request.tool_name,
            "approval request received - auto-rejecting in MVP"
        );

        // 记录原 Task 状态
        if let Some(task) = tasks.iter().find(|t| t.id == request.source_task_id) {
            if task.status == TaskStatus::Waiting(WaitingReason::Approval) {
                info!(task_id = %task.id, "source task is waiting for approval");
            }
        }

        // 生成拒绝结果
        commands.spawn(ApprovalResultMessage {
            request_id: request.request_id,
            source_task_id: request.source_task_id,
            approval_task_id: request.approval_task_id,
            decision: ApprovalDecision::Rejected,
            reasoning: "MVP auto-reject: approval UI not implemented".to_string(),
        });

        commands.entity(entity).despawn();
    }
}

/// 审批结果处理 System
///
/// 处理审批结果并恢复待执行 Tool 请求
#[allow(dead_code)]
pub(crate) fn approval_result_system(
    mut commands: Commands,
    agents: Query<&Agent>,
    approval_results: Query<(Entity, &ApprovalResultMessage)>,
) {
    for (entity, result) in &approval_results {
        info!(
            request_id = %result.request_id,
            decision = ?result.decision,
            "approval result processed"
        );

        // 如果审批通过，更新 Agent 权限（Permanent 模式）
        if result.decision == ApprovalDecision::Approved {
            // 查找子 Agent 并更新权限
            if let Some(agent) = agents.iter().find(|a| a.id == result.source_task_id) {
                // 这里需要知道具体的 tool_name，简化处理
                info!(agent_id = %agent.id, "agent permission would be updated");
            }
        }

        commands.entity(entity).despawn();
    }
}

/// Agent 演化 System
///
/// 将批准后的长期权限修正或经验写回 Agent
#[allow(dead_code)]
pub(crate) fn agent_evolution_system(
    agents: Query<&Agent>,
) {
    // MVP 阶段暂不实现具体演化逻辑
    // 后续扩展：
    // - 从 Tool 执行结果中提取经验
    // - 更新 Agent.experience
    // - 根据 Permanent 确认更新 Agent.tool_permissions
    let _ = agents;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{AgentCapabilities, AgentExperience, AgentKind, AgentProfile, AgentToolPermissions};

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
        let result = execute_builtin_tool("echo", &input);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), input);
    }

    #[test]
    fn execute_builtin_unknown_returns_error() {
        let input = serde_json::json!({});
        let result = execute_builtin_tool("unknown", &input);
        assert!(matches!(result, Err(ToolError::NotFound(_))));
    }

    #[test]
    fn agent_tool_permissions_default_is_confirm() {
        let perms = AgentToolPermissions::default();
        assert_eq!(perms.get_permission("unknown_tool"), ToolPermission::Confirm);
    }

    #[test]
    fn agent_tool_permissions_override() {
        let mut perms = AgentToolPermissions::default();
        perms.default_permission = ToolPermission::Deny;
        perms.overrides.insert("echo".to_string(), ToolPermission::Allow);

        assert_eq!(perms.get_permission("echo"), ToolPermission::Allow);
        assert_eq!(perms.get_permission("other"), ToolPermission::Deny);
    }
}
