//! Tool 契约
//!
//! 定义工具目录和审批策略相关的 trait 接口。

use crate::domain::{Agent, AgentId, ToolDefinition, ToolPermission};

/// 工具目录
///
/// 提供工具定义的查询和筛选接口。
pub trait ToolCatalog: Send + Sync + 'static {
    /// 列出所有可用工具
    fn list_tools(&self) -> Vec<ToolDefinition>;

    /// 获取指定工具的定义
    fn get_tool(&self, name: &str) -> Option<ToolDefinition>;

    /// 根据 Agent 权限筛选工具
    fn filter_by_permission(
        &self,
        agent_id: AgentId,
        permission: ToolPermission,
    ) -> Vec<ToolDefinition>;
}

/// 审批路由
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalRoute {
    /// 自动允许
    AutoAllow,
    /// 需要用户确认
    UserConfirmation,
    /// 需要父 Agent 审批
    ParentApproval { parent_agent_id: AgentId },
    /// 拒绝
    Deny,
}

/// 工具审批策略
///
/// 决定工具执行的审批路由。
pub trait ToolApprovalPolicy: Send + Sync + 'static {
    /// 根据工具和 Agent 决定审批路由
    fn determine_approval_route(&self, tool_name: &str, agent: &Agent) -> ApprovalRoute;
}

/// 默认工具审批策略
#[derive(Debug, Clone, Default)]
pub struct DefaultToolApprovalPolicy;

impl ToolApprovalPolicy for DefaultToolApprovalPolicy {
    fn determine_approval_route(&self, tool_name: &str, agent: &Agent) -> ApprovalRoute {
        let permission = agent.tool_permissions.get_permission(tool_name);

        match permission {
            ToolPermission::Allow => ApprovalRoute::AutoAllow,
            ToolPermission::Confirm => {
                // 检查是否有父 Agent 且父 Agent 有权限
                if let Some(parent_id) = agent.parent_id {
                    ApprovalRoute::ParentApproval {
                        parent_agent_id: parent_id,
                    }
                } else {
                    ApprovalRoute::UserConfirmation
                }
            }
            ToolPermission::Deny => ApprovalRoute::Deny,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        AgentCapabilities, AgentExperience, AgentKind, AgentProfile, AgentToolPermissions,
    };

    fn test_agent_with_permission(permission: ToolPermission) -> Agent {
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
            tool_permissions: AgentToolPermissions {
                default_permission: permission,
                ..Default::default()
            },
            experience: AgentExperience::default(),
        }
    }

    #[test]
    fn approval_route_allow() {
        let policy = DefaultToolApprovalPolicy;
        let agent = test_agent_with_permission(ToolPermission::Allow);
        assert_eq!(
            policy.determine_approval_route("any_tool", &agent),
            ApprovalRoute::AutoAllow
        );
    }

    #[test]
    fn approval_route_confirm_no_parent() {
        let policy = DefaultToolApprovalPolicy;
        let agent = test_agent_with_permission(ToolPermission::Confirm);
        assert_eq!(
            policy.determine_approval_route("any_tool", &agent),
            ApprovalRoute::UserConfirmation
        );
    }

    #[test]
    fn approval_route_deny() {
        let policy = DefaultToolApprovalPolicy;
        let agent = test_agent_with_permission(ToolPermission::Deny);
        assert_eq!(
            policy.determine_approval_route("any_tool", &agent),
            ApprovalRoute::Deny
        );
    }
}
