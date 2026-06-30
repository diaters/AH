//! Tool 契约
//!
//! 定义工具目录和审批策略相关的 trait 接口。

use bevy::prelude::Entity;
use bevy::prelude::Query;

use crate::domain::{Agent, AgentId, Task, ToolDefinition, ToolPermission};

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
    /// 根据工具、Agent 和任务上下文决定审批路由
    fn determine_approval_route(
        &self,
        tool_name: &str,
        agent: &Agent,
        task: &Task,
        tasks: &Query<(Entity, &Task)>,
        agents: &Query<&Agent>,
    ) -> ApprovalRoute;
}

/// 默认工具审批策略
#[derive(Debug, Clone, Default)]
pub struct DefaultToolApprovalPolicy;

impl ToolApprovalPolicy for DefaultToolApprovalPolicy {
    fn determine_approval_route(
        &self,
        tool_name: &str,
        agent: &Agent,
        task: &Task,
        tasks: &Query<(Entity, &Task)>,
        agents: &Query<&Agent>,
    ) -> ApprovalRoute {
        let permission = agent.tool_permissions.get_permission(tool_name);

        match permission {
            ToolPermission::Allow => ApprovalRoute::AutoAllow,
            ToolPermission::Confirm => {
                if let Some(parent_task_id) = task.parent_task_id {
                    if let Some((_, parent_task)) = tasks.iter().find(|(_, t)| t.id == parent_task_id)
                    {
                        if let Some(parent_agent_id) = parent_task.delegate {
                            if let Some(parent_agent) =
                                agents.iter().find(|a| a.id == parent_agent_id)
                            {
                                if parent_agent.has_permission(tool_name) {
                                    return ApprovalRoute::ParentApproval { parent_agent_id };
                                }
                            }
                        }
                    }
                }
                ApprovalRoute::UserConfirmation
            }
            ToolPermission::Deny => ApprovalRoute::Deny,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        AgentCapabilities, AgentKind, AgentProfile, AgentToolPermissions, ChannelId, FrontendKind,
        Task,
    };
    use bevy::ecs::system::SystemState;
    use bevy::prelude::*;
    use uuid::Uuid;

    fn default_channel() -> ChannelId {
        ChannelId {
            frontend: FrontendKind::Tui,
            user_id: "default".to_string(),
            thread_id: None,
        }
    }

    fn make_agent(permission: ToolPermission) -> Agent {
        Agent {
            id: Uuid::new_v4(),
            profile: AgentProfile {
                name: "test".to_string(),
                model: "test".to_string(),
            },
            capabilities: AgentCapabilities {
                tags: vec![],
                description: String::new(),
            },
            kind: AgentKind::Persistent,
            parent_id: None,
            bound_task_id: None,
            tool_permissions: AgentToolPermissions {
                default_permission: permission,
                overrides: std::collections::HashMap::new(),
            },
        }
    }

    #[test]
    fn allow_returns_auto_allow() {
        let mut app = App::new();
        let agent = make_agent(ToolPermission::Allow);
        let task = Task::from_user_input("test", 3, default_channel());
        app.world_mut().spawn(agent.clone());
        app.world_mut().spawn(task.clone());

        let policy = DefaultToolApprovalPolicy;
        let world = app.world_mut();
        let mut state = SystemState::<(Query<(Entity, &Task)>, Query<&Agent>)>::new(world);
        let (tasks, agents) = state.get(world);

        assert_eq!(
            policy.determine_approval_route("test_tool", &agent, &task, &tasks, &agents),
            ApprovalRoute::AutoAllow
        );
    }

    #[test]
    fn deny_returns_deny() {
        let mut app = App::new();
        let agent = make_agent(ToolPermission::Deny);
        let task = Task::from_user_input("test", 3, default_channel());
        app.world_mut().spawn(agent.clone());
        app.world_mut().spawn(task.clone());

        let policy = DefaultToolApprovalPolicy;
        let world = app.world_mut();
        let mut state = SystemState::<(Query<(Entity, &Task)>, Query<&Agent>)>::new(world);
        let (tasks, agents) = state.get(world);

        assert_eq!(
            policy.determine_approval_route("test_tool", &agent, &task, &tasks, &agents),
            ApprovalRoute::Deny
        );
    }

    #[test]
    fn confirm_without_parent_returns_user_confirmation() {
        let mut app = App::new();
        let agent = make_agent(ToolPermission::Confirm);
        let task = Task::from_user_input("test", 3, default_channel());
        app.world_mut().spawn(agent.clone());
        app.world_mut().spawn(task.clone());

        let policy = DefaultToolApprovalPolicy;
        let world = app.world_mut();
        let mut state = SystemState::<(Query<(Entity, &Task)>, Query<&Agent>)>::new(world);
        let (tasks, agents) = state.get(world);

        assert_eq!(
            policy.determine_approval_route("test_tool", &agent, &task, &tasks, &agents),
            ApprovalRoute::UserConfirmation
        );
    }
}
