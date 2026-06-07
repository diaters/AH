//! Space 相关 Resource 定义
//!
//! Space 是全局共享的运行时语义容器，承载非任务级的共享资源。

use std::collections::HashMap;

use bevy::prelude::Resource;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{AgentCapabilities, AgentId, AgentProfile, MemoryEntry, SessionCommand, SessionOutputRequest, SessionStartRequest, SessionStopRequest, SessionWaitRequest, SubTaskDefinition, TaskId, ToolError};

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

    /// 遍历所有工具定义
    pub fn iter(&self) -> impl Iterator<Item = &ToolDefinition> {
        self.tools.values()
    }

    /// 获取所有工具名称
    pub fn tool_names(&self) -> Vec<&str> {
        self.tools.keys().map(|k| k.as_str()).collect()
    }
}

/// Tool 定义
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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
    /// 执行所需的最小 tag（如 "brain"）
    #[serde(default)]
    pub required_tag: Option<String>,
}

/// Tool 参数 Schema
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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

/// Tool 执行动作
#[derive(Debug, Clone)]
pub enum ToolAction {
    /// 直接返回结果
    Direct(serde_json::Value),
    /// 创建子 Agent 请求
    SpawnAgent {
        name: String,
        model: Option<String>,
        description: String,
        tools: Vec<String>,
    },
    /// 创建子任务批次
    CreateBatch(Vec<SubTaskDefinition>),
    /// 等待子任务完成
    WaitForTasks {
        task_ids: Vec<TaskId>,
        timeout_secs: u64,
    },
    /// 阻塞执行 shell 命令
    ExecSession(SessionStartRequest),
    /// 启动后台 shell 会话
    StartSession(SessionStartRequest),
    /// 读取 shell 会话输出
    ReadSessionOutput(SessionOutputRequest),
    /// 发送交互输入到 shell 会话
    SendSessionInput(SessionCommand),
    /// 发送控制信号到 shell 会话
    SendSessionSignal(SessionCommand),
    /// 等待 shell 会话完成
    WaitForSession(SessionWaitRequest),
    /// 停止 shell 会话
    StopSession(SessionStopRequest),
}

/// 内置 Tool 执行上下文
pub struct ToolContext<'a> {
    pub knowledge: &'a SpaceKnowledge,
    /// wait_tasks 工具的默认超时时间（秒）
    pub default_wait_tasks_timeout_secs: u64,
    /// shell 工具默认返回的最新输出行数
    pub shell_default_tail_lines: usize,
    /// shell 工具允许返回的最大输出行数
    pub shell_max_tail_lines: usize,
    /// shell.wait 默认超时时间（秒）
    pub shell_default_wait_timeout_secs: u64,
    /// shell.stop(wait_for_exit=true) 默认超时时间（秒）
    pub shell_default_stop_timeout_secs: u64,
    /// 当前 task ID
    pub current_task_id: TaskId,
    /// 当前 agent ID
    pub current_agent_id: AgentId,
}

/// 内置 Tool trait
pub trait BuiltinTool: Send + Sync + 'static {
    /// 工具名称
    fn name(&self) -> &str;
    /// 执行工具并返回动作
    fn execute(
        &self,
        input: &serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolAction, ToolError>;
}

/// 内置 Tool 执行器注册表
#[derive(Resource, Default)]
pub struct BuiltinToolExecutors {
    executors: HashMap<String, Box<dyn BuiltinTool>>,
}

impl BuiltinToolExecutors {
    pub fn register(&mut self, executor: Box<dyn BuiltinTool>) {
        self.executors.insert(executor.name().to_string(), executor);
    }

    pub fn get(&self, name: &str) -> Option<&dyn BuiltinTool> {
        self.executors.get(name).map(|e| e.as_ref())
    }
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
