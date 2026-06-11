//! Space 相关 Resource 定义
//!
//! Space 是全局共享的运行时语义容器，承载非任务级的共享资源。

use std::collections::HashMap;

use bevy::prelude::Resource;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{
    AgentId, ExperienceKindHint, ExperienceStore, LongTermMemoryKind, MemoryImportance,
    SessionHandleId, SessionInputRequest, SessionReadRequest, SessionStartRequest,
    SubTaskDefinition, TaskId, ToolError,
};

/// 共享知识审核状态。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum KnowledgeValidationStatus {
    Candidate,
    Approved,
    Rejected,
    Deprecated,
}

/// 共享知识来源。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum KnowledgeSource {
    UserCommand,
    BrainReview,
    Migration,
}

/// 共享知识条目。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SharedKnowledgeEntry {
    pub content: String,
    pub kind: LongTermMemoryKind,
    pub scope_tags: Vec<String>,
    pub importance: MemoryImportance,
    pub created_at: DateTime<Utc>,
    pub last_accessed_at: Option<DateTime<Utc>>,
    pub reuse_count: u32,
    pub confidence: f32,
    pub validation_status: KnowledgeValidationStatus,
    pub approved_by: Option<String>,
    pub source: KnowledgeSource,
}

impl SharedKnowledgeEntry {
    /// 创建用户显式确认的共享知识条目。
    pub fn approved_from_user_input(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            kind: LongTermMemoryKind::Fact,
            scope_tags: Vec::new(),
            importance: MemoryImportance::High,
            created_at: Utc::now(),
            last_accessed_at: None,
            reuse_count: 0,
            confidence: 1.0,
            validation_status: KnowledgeValidationStatus::Approved,
            approved_by: Some("user:/remember".to_string()),
            source: KnowledgeSource::UserCommand,
        }
    }

    /// 创建待审核的共享知识候选条目。
    pub fn candidate(content: impl Into<String>, kind: LongTermMemoryKind) -> Self {
        Self {
            content: content.into(),
            kind,
            scope_tags: Vec::new(),
            importance: MemoryImportance::Medium,
            created_at: Utc::now(),
            last_accessed_at: None,
            reuse_count: 0,
            confidence: 0.6,
            validation_status: KnowledgeValidationStatus::Candidate,
            approved_by: None,
            source: KnowledgeSource::BrainReview,
        }
    }
}

/// 全局共享知识库。
#[derive(Resource, Default)]
pub struct SharedKnowledgeBase {
    pub entries: Vec<SharedKnowledgeEntry>,
}

/// 全局工具注册表
#[derive(Resource, Default)]
pub struct SpaceToolRegistry {
    tools: HashMap<String, ToolDefinition>,
}

impl SpaceToolRegistry {
    /// 注册新工具。
    pub fn register(&mut self, tool: ToolDefinition) {
        self.tools.insert(tool.name.clone(), tool);
    }

    /// 获取工具定义。
    pub fn get(&self, name: &str) -> Option<&ToolDefinition> {
        self.tools.get(name)
    }

    /// 遍历所有工具定义。
    pub fn iter(&self) -> impl Iterator<Item = &ToolDefinition> {
        self.tools.values()
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
    /// 读取 shell 会话状态和最新输出快照
    ReadSession(SessionReadRequest),
    /// 列出活动 shell 会话
    ListSessions,
    /// 发送交互输入到 shell 会话
    InputSession(SessionInputRequest),
    /// 停止 shell 会话
    StopSession(SessionHandleId),
    /// 提交经验候选
    SubmitExperienceCandidate(ExperienceCandidateSubmission),
}

/// 经验候选提交数据
#[derive(Debug, Clone)]
pub struct ExperienceCandidateSubmission {
    pub title: String,
    pub kind_hint: ExperienceKindHint,
    pub payload: serde_json::Value,
    pub dependency_refs: Vec<String>,
}

impl ExperienceCandidateSubmission {
    /// 从 JSON 工具输入构造候选提交数据。
    pub fn from_json(
        _task_id: TaskId,
        _agent_id: AgentId,
        title: &str,
        input: &serde_json::Value,
    ) -> Result<Self, ToolError> {
        let kind_str = input
            .get("kind_hint")
            .and_then(|v| v.as_str())
            .unwrap_or("knowledge");
        let kind_hint = match kind_str {
            "executable" => ExperienceKindHint::Executable,
            "shared_knowledge" => ExperienceKindHint::SharedKnowledge,
            "discard" => ExperienceKindHint::Discard,
            _ => ExperienceKindHint::Knowledge,
        };
        let payload = input
            .get("payload")
            .cloned()
            .unwrap_or(serde_json::json!({}));
        let dependency_refs = input
            .get("dependency_refs")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        Ok(Self {
            title: title.to_string(),
            kind_hint,
            payload,
            dependency_refs,
        })
    }
}

/// 内置 Tool 执行上下文
pub struct ToolContext<'a> {
    pub knowledge: &'a SharedKnowledgeBase,
    /// 经验候选仓库
    pub experience_store: &'a ExperienceStore,
    /// wait_tasks 工具的默认超时时间（秒）
    pub default_wait_tasks_timeout_secs: u64,
    /// shell 工具默认返回的最新输出行数
    pub shell_default_tail_lines: usize,
    /// shell 工具允许返回的最大输出行数
    pub shell_max_tail_lines: usize,
    /// shell.exec 默认超时时间（秒）
    pub shell_default_exec_timeout_secs: u64,
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

/// Agent 的 Tool 配置（来自 agents.toml）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentToolsConfig {
    /// 未显式配置的 Tool 默认权限
    pub default_permission: Option<ToolPermission>,
    /// 针对特定 Tool 的覆盖项
    #[serde(flatten)]
    pub overrides: HashMap<String, ToolPermission>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_knowledge_entry_from_user_is_approved() {
        let entry =
            SharedKnowledgeEntry::approved_from_user_input("Project docs are written in Chinese");

        assert_eq!(entry.validation_status, KnowledgeValidationStatus::Approved);
        assert_eq!(entry.source, KnowledgeSource::UserCommand);
    }
}
