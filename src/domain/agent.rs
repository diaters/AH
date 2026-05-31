//! Agent 相关类型定义
//!
//! 定义 Agent 实体、配置、权限等。

use bevy::prelude::Component;
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

/// Agent 长期经验
#[derive(Debug, Clone, Component, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentExperience {
    pub entries: Vec<super::MemoryEntry>,
}

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
    /// Agent 长期经验
    pub experience: AgentExperience,
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
