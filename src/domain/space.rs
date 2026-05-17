//! Space 相关 Resource 定义
//!
//! Space 是全局共享的运行时语义容器，承载非任务级的共享资源。

use std::collections::HashMap;

use bevy::prelude::Resource;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{AgentCapabilities, AgentProfile, MemoryEntry};

/// Space 级别的长期知识（用户相关）
#[derive(Resource, Default)]
pub struct SpaceKnowledge {
    pub entries: Vec<MemoryEntry>,
}

/// Space 级别的默认偏好
#[derive(Resource)]
pub struct SpacePreferences {
    pub default_language: String,
    pub default_behavior: String,
    pub preferred_model: Option<String>,
}

impl Default for SpacePreferences {
    fn default() -> Self {
        Self {
            default_language: "zh-CN".to_string(),
            default_behavior: "helpful".to_string(),
            preferred_model: None,
        }
    }
}

/// 全局工具注册表
#[derive(Resource, Default)]
pub struct SpaceToolRegistry {
    pub tools: HashMap<String, ToolDefinition>,
}

impl SpaceToolRegistry {
    /// 注册新工具
    pub fn register(&mut self, tool: ToolDefinition) {
        self.tools.insert(tool.name.clone(), tool);
    }

    /// 获取工具定义
    pub fn get(&self, name: &str) -> Option<&ToolDefinition> {
        self.tools.get(name)
    }

    /// 检查工具是否存在
    pub fn exists(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }
}

/// Tool 定义
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    /// 工具名称（唯一标识）
    pub name: String,
    /// 工具描述（供 LLM 理解用途）
    pub description: String,
    /// JSON Schema 参数定义
    pub parameters: ToolSchema,
    /// 默认权限级别
    pub default_permission: ToolPermission,
    /// 执行器类型
    pub executor: ToolExecutorKind,
}

/// Tool 参数 Schema
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSchema {
    pub schema: serde_json::Value,
}

impl Default for ToolSchema {
    fn default() -> Self {
        Self {
            schema: serde_json::json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        }
    }
}

/// Tool 执行器类型
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ToolExecutorKind {
    /// 内置执行器，由系统内注册函数实现
    Builtin(String),
    /// 外部进程执行（后续扩展）
    External { command: String, args: Vec<String> },
    /// HTTP 调用（后续扩展）
    Http { endpoint: String },
}

/// Tool 权限级别
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolPermission {
    /// 允许直接执行
    Allow,
    /// 需要用户确认
    #[default]
    Confirm,
    /// 禁止执行
    Deny,
}

/// 持久性 Agent 配置镜像
#[derive(Resource, Default)]
pub struct SpaceAgentRegistry {
    pub agents: HashMap<String, PersistentAgentConfig>,
}

/// 持久性 Agent 配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistentAgentConfig {
    pub profile: AgentProfile,
    pub capabilities: AgentCapabilities,
    pub tools: Option<AgentToolsConfig>,
}

/// Agent 的 Tool 配置（来自 agents.toml）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentToolsConfig {
    /// 未显式配置的 Tool 默认权限
    pub default_permission: Option<ToolPermission>,
    /// 针对特定 Tool 的覆盖项
    #[serde(flatten)]
    pub overrides: HashMap<String, ToolPermission>,
}

/// 系统状态
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum SystemStatus {
    #[default]
    Running,
    ShuttingDown,
}

/// 全局运行时上下文
#[derive(Resource)]
pub struct SpaceRuntimeContext {
    pub current_time: DateTime<Utc>,
    pub environment_summary: HashMap<String, String>,
    pub system_status: SystemStatus,
}

impl Default for SpaceRuntimeContext {
    fn default() -> Self {
        Self {
            current_time: Utc::now(),
            environment_summary: HashMap::new(),
            system_status: SystemStatus::default(),
        }
    }
}
