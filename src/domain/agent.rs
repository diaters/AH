//! Agent 相关类型定义
//!
//! 定义 Agent 实体、配置、权限等。

use crate::prelude::Component;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::{AgentId, TaskId, ToolPermission};

/// 权限来源标识，用于审计与调试
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionSource {
    /// Agent 的 overrides 显式配置（运行时 grant 或 agents.toml 显式）
    AgentOverride,
    /// Agent 的 default_permission
    AgentDefault,
    /// ToolDefinition.default_permission（最后回退层）
    ToolDefault,
}

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
    /// default_permission 是否由配置显式设置（true=agents.toml 写过 / 运行时改过；
    /// false=结构默认值 Confirm）。仅当 false 且 default==Confirm 时回退到 tool_default。
    #[serde(default)]
    pub default_permission_explicit: bool,
    /// 针对特定 Tool 的覆盖项
    pub overrides: HashMap<String, ToolPermission>,
}

impl AgentToolPermissions {
    /// 计算工具的生效权限，按三层回退：
    /// 1. self.overrides
    /// 2. self.default_permission
    ///    - 仅当 !default_permission_explicit && default == Confirm 时，回退到 (3)
    ///    - 显式 Allow / Deny / Confirm 直接使用
    /// 3. tool_def.default_permission（通过 registry 查询）
    ///
    /// 返回 (permission, source)。registry 缺失或工具未注册时
    /// 返回 (self.default_permission, AgentDefault)。
    pub fn effective_permission(
        &self,
        tool_name: &str,
        registry: Option<&crate::domain::SpaceToolRegistry>,
    ) -> (crate::domain::ToolPermission, PermissionSource) {
        use crate::domain::ToolPermission;
        if let Some(p) = self.overrides.get(tool_name).copied() {
            return (p, PermissionSource::AgentOverride);
        }
        let tool_default = registry
            .and_then(|r| r.get(tool_name))
            .map(|d| d.default_permission);
        let implicit_confirm = !self.default_permission_explicit
            && self.default_permission == ToolPermission::Confirm;
        match tool_default {
            Some(tp) if implicit_confirm => (tp, PermissionSource::ToolDefault),
            _ => (self.default_permission, PermissionSource::AgentDefault),
        }
    }
}

impl Default for AgentToolPermissions {
    fn default() -> Self {
        Self {
            default_permission: ToolPermission::Confirm,
            default_permission_explicit: false,
            overrides: HashMap::new(),
        }
    }
}

impl From<super::AgentToolsConfig> for AgentToolPermissions {
    fn from(config: super::AgentToolsConfig) -> Self {
        let (default_permission, default_permission_explicit) = match config.default_permission {
            Some(p) => (p, true),
            None => (ToolPermission::Confirm, false),
        };
        Self {
            default_permission,
            default_permission_explicit,
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
    /// Agent 级 system prompt（来自 agents.toml）：WorkItem 执行时作为 system_prompt 传递给 LLM。
    /// None 表示使用 WorkItem 自身的 system_prompt（保持向后兼容）。
    pub system_prompt: Option<String>,
}

impl Agent {
    /// 委托到 AgentToolPermissions::effective_permission
    pub fn effective_permission(
        &self,
        tool_name: &str,
        registry: Option<&crate::domain::SpaceToolRegistry>,
    ) -> (crate::domain::ToolPermission, PermissionSource) {
        self.tool_permissions
            .effective_permission(tool_name, registry)
    }

    /// 授予永久权限
    pub fn grant_permission(&mut self, tool_name: String) {
        tracing::info!(
            event = "PermissionGrant",
            agent_id = %self.id,
            tool_name = %tool_name,
            "永久授权写入"
        );
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
        overrides.insert("shell_exec".to_string(), ToolPermission::Allow);

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
                default_permission_explicit: true,
                overrides,
            },
            system_prompt: None,
        };

        assert_eq!(
            agent.effective_permission("shell_exec", None).0,
            ToolPermission::Allow
        );
    }

    #[test]
    fn default_permission_explicit_defaults_to_false() {
        let perms = AgentToolPermissions::default();
        assert!(!perms.default_permission_explicit);
        assert_eq!(perms.default_permission, ToolPermission::Confirm);
    }

    #[test]
    fn from_agent_tools_config_some_marks_explicit() {
        use crate::domain::AgentToolsConfig;
        let config = AgentToolsConfig {
            default_permission: Some(ToolPermission::Allow),
            overrides: std::collections::HashMap::new(),
        };
        let perms = AgentToolPermissions::from(config);
        assert!(perms.default_permission_explicit);
        assert_eq!(perms.default_permission, ToolPermission::Allow);
    }

    #[test]
    fn from_agent_tools_config_none_marks_implicit() {
        use crate::domain::AgentToolsConfig;
        let config = AgentToolsConfig {
            default_permission: None,
            overrides: std::collections::HashMap::new(),
        };
        let perms = AgentToolPermissions::from(config);
        assert!(!perms.default_permission_explicit);
        assert_eq!(perms.default_permission, ToolPermission::Confirm);
    }

    use crate::domain::{SpaceToolRegistry, ToolDefinition, ToolExecutorKind, ToolSchema};

    fn make_tool_def(name: &str, perm: ToolPermission) -> ToolDefinition {
        ToolDefinition {
            name: name.to_string(),
            description: "test".to_string(),
            parameters: ToolSchema::default(),
            default_permission: perm,
            executor: ToolExecutorKind::Builtin(name.to_string()),
            required_tag: None,
        }
    }

    fn make_agent_with_default(default: ToolPermission, explicit: bool) -> Agent {
        Agent {
            id: uuid::Uuid::nil(),
            profile: AgentProfile {
                name: "test".to_string(),
                model: "m".to_string(),
            },
            capabilities: AgentCapabilities {
                tags: vec![],
                description: String::new(),
            },
            kind: AgentKind::Persistent,
            parent_id: None,
            bound_task_id: None,
            tool_permissions: AgentToolPermissions {
                default_permission: default,
                default_permission_explicit: explicit,
                overrides: HashMap::new(),
            },
            system_prompt: None,
        }
    }

    #[test]
    fn effective_permission_override_hits() {
        let mut agent = make_agent_with_default(ToolPermission::Confirm, false);
        agent
            .tool_permissions
            .overrides
            .insert("shell_exec".to_string(), ToolPermission::Allow);
        let (perm, source) = agent.effective_permission("shell_exec", None);
        assert_eq!(perm, ToolPermission::Allow);
        assert_eq!(source, PermissionSource::AgentOverride);
    }

    #[test]
    fn effective_permission_implicit_confirm_falls_back_to_tool_default() {
        let agent = make_agent_with_default(ToolPermission::Confirm, false);
        let mut registry = SpaceToolRegistry::default();
        registry.register(make_tool_def("shell_exec", ToolPermission::Allow));
        let (perm, source) = agent.effective_permission("shell_exec", Some(&registry));
        assert_eq!(perm, ToolPermission::Allow);
        assert_eq!(source, PermissionSource::ToolDefault);
    }

    #[test]
    fn effective_permission_explicit_confirm_does_not_fall_back() {
        let agent = make_agent_with_default(ToolPermission::Confirm, true);
        let mut registry = SpaceToolRegistry::default();
        registry.register(make_tool_def("shell_exec", ToolPermission::Allow));
        let (perm, source) = agent.effective_permission("shell_exec", Some(&registry));
        assert_eq!(perm, ToolPermission::Confirm);
        assert_eq!(source, PermissionSource::AgentDefault);
    }

    #[test]
    fn effective_permission_explicit_allow_does_not_fall_back() {
        let agent = make_agent_with_default(ToolPermission::Allow, true);
        let mut registry = SpaceToolRegistry::default();
        registry.register(make_tool_def("shell_exec", ToolPermission::Confirm));
        let (perm, source) = agent.effective_permission("shell_exec", Some(&registry));
        assert_eq!(perm, ToolPermission::Allow);
        assert_eq!(source, PermissionSource::AgentDefault);
    }

    #[test]
    fn effective_permission_explicit_deny_does_not_fall_back() {
        let agent = make_agent_with_default(ToolPermission::Deny, true);
        let mut registry = SpaceToolRegistry::default();
        registry.register(make_tool_def("shell_exec", ToolPermission::Allow));
        let (perm, source) = agent.effective_permission("shell_exec", Some(&registry));
        assert_eq!(perm, ToolPermission::Deny);
        assert_eq!(source, PermissionSource::AgentDefault);
    }

    #[test]
    fn effective_permission_implicit_confirm_no_registry_returns_agent_default() {
        let agent = make_agent_with_default(ToolPermission::Confirm, false);
        let (perm, source) = agent.effective_permission("unknown_tool", None);
        assert_eq!(perm, ToolPermission::Confirm);
        assert_eq!(source, PermissionSource::AgentDefault);
    }

    #[test]
    fn effective_permission_implicit_confirm_unknown_tool_returns_agent_default() {
        let agent = make_agent_with_default(ToolPermission::Confirm, false);
        let registry = SpaceToolRegistry::default(); // 空 registry
        let (perm, source) = agent.effective_permission("unknown_tool", Some(&registry));
        assert_eq!(perm, ToolPermission::Confirm);
        assert_eq!(source, PermissionSource::AgentDefault);
    }
}
