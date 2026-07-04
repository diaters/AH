//! Agent 相关类型定义
//!
//! 定义 Agent 实体、配置、权限等。

use crate::prelude::Component;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::{AgentId, TaskId, ToolPermission};

/// Agent 能力描述
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentCapabilities {
    pub tags: Vec<String>,
    pub description: String,
}

/// Agent 配置档案
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentProfile {
    pub name: String,
    pub model: String,
}

/// Agent 类型
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum AgentKind {
    Persistent,
    TaskScoped,
}

/// Agent 的 Tool 权限配置
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentToolPermissions {
    /// 未显式配置的 Tool 默认权限
    pub default_permission: ToolPermission,
    /// 针对特定 Tool 的覆盖项
    pub overrides: HashMap<String, ToolPermission>,
}

impl AgentToolPermissions {
    /// 获取指定 Tool 的权限
    pub fn get_permission(&self, tool_name: &str) -> ToolPermission {
        self.overrides
            .get(tool_name)
            .copied()
            .unwrap_or(self.default_permission)
    }
}

impl Default for AgentToolPermissions {
    fn default() -> Self {
        Self {
            default_permission: ToolPermission::Confirm,
            overrides: HashMap::new(),
        }
    }
}

impl From<super::AgentToolsConfig> for AgentToolPermissions {
    fn from(config: super::AgentToolsConfig) -> Self {
        Self {
            default_permission: config.default_permission.unwrap_or(ToolPermission::Confirm),
            overrides: config.overrides,
        }
    }
}

/// Agent 实体即将被 despawn 前的标记组件。
///
/// 由 `handle_termination` 在 Agent 绑定的 Task 进入终态时插入，
/// 由 `agent_stopped_hook_system` 派发 `OnAgentStopped` hook 后负责 despawn。
/// 无需内含 `HookPoint` —— 此标记固定对应 `OnAgentStopped`。
#[derive(Component, Debug, Clone)]
pub struct AgentStoppingHookPending;

/// Agent 实体
#[derive(Debug, Clone, Component)]
pub struct Agent {
    pub id: AgentId,
    pub profile: AgentProfile,
    pub capabilities: AgentCapabilities,
    pub kind: AgentKind,
    pub parent_id: Option<AgentId>,
    pub bound_task_id: Option<TaskId>,
    /// Tool 权限配置：启动加载、父 Agent 授权或后续修正
    pub tool_permissions: AgentToolPermissions,
}

impl Agent {
    /// 判断是否拥有某 Tool 的 Allow 权限
    pub fn has_permission(&self, tool_name: &str) -> bool {
        self.tool_permissions.get_permission(tool_name) == ToolPermission::Allow
    }

    /// 授予永久权限
    pub fn grant_permission(&mut self, tool_name: String) {
        self.tool_permissions
            .overrides
            .insert(tool_name, ToolPermission::Allow);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证移除 experience 字段后，Agent 仍可基于权限覆盖正常授权。
    #[test]
    fn agent_without_experience_still_grants_permissions() {
        let mut overrides = HashMap::new();
        overrides.insert("knowledge_search".to_string(), ToolPermission::Allow);

        let agent = Agent {
            id: uuid::Uuid::nil(),
            profile: AgentProfile {
                name: "memory-agent".to_string(),
                model: "test-model".to_string(),
            },
            capabilities: AgentCapabilities {
                tags: vec!["memory".to_string()],
                description: "memory agent".to_string(),
            },
            kind: AgentKind::Persistent,
            parent_id: None,
            bound_task_id: None,
            tool_permissions: AgentToolPermissions {
                default_permission: ToolPermission::Confirm,
                overrides,
            },
        };

        assert!(agent.has_permission("knowledge_search"));
    }
}
