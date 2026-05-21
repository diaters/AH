mod contribution;
mod evaluation;
mod memory;
mod space;

use std::{collections::HashMap, future::Future, pin::Pin, time::Duration};

use bevy::prelude::Component;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

pub use contribution::{
    AbsorbedMemory, ContributionEvaluation, DiscardedMemory, MemoryAbsorptionMessage,
    MemoryContributionRequestMessage, TaskSummary,
};
pub use evaluation::{
    EvaluationDecision, EvaluationRequestMessage, EvaluationResult, EvaluationResultMessage,
    EvaluationTrigger, OffTrackPolicy, TaskEvaluationConfig,
};
pub use memory::{
    EntryMetadata, EntryRole, LongTermMemory, MemoryEntry, ShortTermMemory, ToolCall,
};
pub use space::{
    AgentToolsConfig, PersistentAgentConfig, SpaceAgentRegistry, SpaceKnowledge, SpacePreferences,
    SpaceRuntimeContext, SpaceToolRegistry, SystemStatus, ToolDefinition, ToolExecutorKind,
    ToolPermission, ToolSchema,
};

pub type TaskId = Uuid;
pub type AgentId = Uuid;
pub type ExecutorFuture = Pin<Box<dyn Future<Output = Result<String, ExecutionError>> + Send>>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SignalType {
    UserInput,
    RetryWakeup,
    SystemWakeup,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum WaitingReason {
    Agent,
    User,      // 等待用户输入
    Evaluator, // 等待评估器判定
    RetryBackoff,
    Approval,      // 等待审批
    Summarization, // 等待摘要完成
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum FailureReason {
    Timeout,
    RateLimited,
    Authentication,
    QuotaExhausted,
    AgentError,
    UserCancelled,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TaskStatus {
    Pending,
    Ready,
    Running,
    Waiting(WaitingReason),
    Done,
    Failed(FailureReason),
}

impl TaskStatus {
    /// 判断任务是否已经到达终态。
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Done | Self::Failed(_))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SignalPayload {
    UserInput(String),
    RetryWakeup(TaskId),
    SystemWakeup,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ExternalInput {
    Text(String),
    Shutdown,
    /// Tool 确认响应
    Confirmation {
        request_id: Uuid,
        option: String,
    },
}

/// 输出类型
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum OutputKind {
    /// 普通文本输出
    #[default]
    Text,
    /// Tool 确认请求
    ConfirmationRequest {
        request_id: Uuid,
        title: String,
        options: Vec<ConfirmationOption>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OutputMessage {
    pub content: String,
    pub kind: OutputKind,
}

impl OutputMessage {
    /// 构造普通文本输出消息。
    pub fn new(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            kind: OutputKind::Text,
        }
    }

    /// 构造确认请求输出消息。
    pub fn confirmation_request(
        request_id: Uuid,
        title: impl Into<String>,
        options: Vec<ConfirmationOption>,
    ) -> Self {
        Self {
            content: String::new(),
            kind: OutputKind::ConfirmationRequest {
                request_id,
                title: title.into(),
                options,
            },
        }
    }
}

#[derive(Debug, Clone, Component)]
pub struct Signal {
    pub kind: SignalType,
    pub payload: SignalPayload,
}

impl Signal {
    /// 构造用户输入信号。
    pub fn user_input(content: impl Into<String>) -> Self {
        Self {
            kind: SignalType::UserInput,
            payload: SignalPayload::UserInput(content.into()),
        }
    }

    /// 构造重试唤醒信号。
    pub fn retry_wakeup(task_id: TaskId) -> Self {
        Self {
            kind: SignalType::RetryWakeup,
            payload: SignalPayload::RetryWakeup(task_id),
        }
    }
}

#[derive(Debug, Clone, Component)]
pub struct UserInputMessage {
    pub content: String,
}

#[derive(Debug, Clone, Component)]
pub struct RetryReadyMessage {
    pub task_id: TaskId,
}

#[derive(Debug, Clone, Component)]
pub struct AgentExecutionRequestMessage {
    pub request: AgentExecutionRequest,
}

#[derive(Debug, Clone, Component)]
pub struct AgentExecutionResultMessage {
    pub result: AgentExecutionResult,
}

#[derive(Debug, Clone, Component)]
pub struct UserOutputMessage {
    pub content: String,
}

/// 创建新任务消息
#[derive(Debug, Clone, Component)]
pub struct CreateTaskMessage {
    pub content: String,
}

/// 继续现有任务消息
#[derive(Debug, Clone, Component)]
pub struct ContinueTaskMessage {
    pub task_id: TaskId,
    pub user_input: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentCapabilities {
    pub tags: Vec<String>,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentProfile {
    pub name: String,
    pub model: String,
}

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

/// Agent 长期经验
#[derive(Debug, Clone, Component, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentExperience {
    pub entries: Vec<MemoryEntry>,
}

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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum AgentRequestKind {
    LlmCompletion,
    BrainDecision,
    ToolExecution { tool_name: String },
    Summarization,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentExecutionRequest {
    pub task_id: TaskId,
    pub agent_id: AgentId,
    pub request_kind: AgentRequestKind,
    pub prompt: String,
    pub system_prompt: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentExecutionResult {
    pub task_id: TaskId,
    pub agent_id: AgentId,
    pub request_kind: AgentRequestKind,
    pub result: Result<String, ExecutionError>,
}

#[derive(Debug, Clone, Component, Serialize, Deserialize)]
pub struct Task {
    pub id: TaskId,
    pub content: String,
    pub creator: AgentId,
    pub delegate: Option<AgentId>,
    pub status: TaskStatus,
    pub input_summary: String,
    pub result_summary: String,
    pub priority: u32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub retry_count: u32,
    pub max_retries: u32,
    pub next_retry_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    /// 是否支持多轮对话
    pub multi_turn: bool,
}

impl Task {
    /// 基于用户输入创建一个处于 Pending 状态的新任务（支持多轮对话）。
    pub fn from_user_input(content: impl Into<String>, max_retries: u32) -> Self {
        let content = content.into();
        let now = Utc::now();

        Self {
            id: Uuid::new_v4(),
            content: content.clone(),
            creator: Uuid::nil(),
            delegate: None,
            status: TaskStatus::Pending,
            input_summary: String::new(),
            result_summary: String::new(),
            priority: 0,
            created_at: now,
            updated_at: now,
            retry_count: 0,
            max_retries,
            next_retry_at: None,
            last_error: None,
            multi_turn: true,
        }
    }

    /// 基于用户输入创建一个处于 Ready 状态的新任务（用于测试或单轮场景）。
    pub fn from_user_input_ready(content: impl Into<String>, max_retries: u32) -> Self {
        let content = content.into();
        let now = Utc::now();

        Self {
            id: Uuid::new_v4(),
            content: content.clone(),
            creator: Uuid::nil(),
            delegate: None,
            status: TaskStatus::Ready,
            input_summary: content.clone(),
            result_summary: String::new(),
            priority: 0,
            created_at: now,
            updated_at: now,
            retry_count: 0,
            max_retries,
            next_retry_at: None,
            last_error: None,
            multi_turn: false,
        }
    }

    /// 将任务标记为分发等待状态。
    pub fn mark_waiting_for_agent(&mut self, agent_id: AgentId, now: DateTime<Utc>) {
        self.delegate = Some(agent_id);
        self.status = TaskStatus::Waiting(WaitingReason::Agent);
        self.updated_at = now;
    }

    /// 将任务标记为运行中。
    pub fn mark_running(&mut self, now: DateTime<Utc>) {
        self.status = TaskStatus::Running;
        self.updated_at = now;
    }

    /// 在成功完成后写回结果并清理重试状态。
    pub fn mark_done(&mut self, result: impl Into<String>, now: DateTime<Utc>) {
        self.result_summary = result.into();
        self.status = TaskStatus::Done;
        self.updated_at = now;
        self.next_retry_at = None;
        self.last_error = None;
    }

    /// 根据可重试错误更新任务回退信息。
    pub fn schedule_retry(&mut self, error: &ExecutionError, now: DateTime<Utc>) {
        self.retry_count += 1;
        self.next_retry_at = Some(
            now + ChronoDuration::from_std(error.retry_delay(self.retry_count))
                .unwrap_or_else(|_| ChronoDuration::seconds(1)),
        );
        self.last_error = Some(error.message().to_string());
        self.status = TaskStatus::Waiting(WaitingReason::RetryBackoff);
        self.updated_at = now;
    }

    /// 将任务标记为最终失败。
    pub fn mark_failed(&mut self, error: &ExecutionError, now: DateTime<Utc>) {
        self.last_error = Some(error.message().to_string());
        self.status = TaskStatus::Failed(error.to_failure_reason());
        self.updated_at = now;
    }

    /// 将任务重新置回 Ready 以进入下一次调度。
    pub fn mark_ready_for_retry(&mut self, now: DateTime<Utc>) {
        self.status = TaskStatus::Ready;
        self.next_retry_at = None;
        self.updated_at = now;
    }
}

#[derive(Debug, Clone, Error, Serialize, Deserialize, PartialEq, Eq)]
pub enum ExecutionError {
    #[error("request timed out: {0}")]
    Timeout(String),
    #[error("rate limited: {message}")]
    RateLimited {
        message: String,
        retry_after_secs: Option<u64>,
    },
    #[error("authentication failed: {0}")]
    Authentication(String),
    #[error("quota exhausted: {0}")]
    QuotaExhausted(String),
    #[error("transport error: {0}")]
    Transport(String),
    #[error("user cancelled: {0}")]
    UserCancelled(String),
    #[error("empty response from model")]
    EmptyResponse,
    #[error("unknown error: {0}")]
    Unknown(String),
}

/// Tool 执行错误
#[derive(Debug, Clone, Error, Serialize, Deserialize, PartialEq, Eq)]
pub enum ToolError {
    #[error("tool not found: {0}")]
    NotFound(String),
    #[error("permission denied: {0}")]
    PermissionDenied(String),
    #[error("execution failed: {0}")]
    ExecutionFailed(String),
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("timeout: {0}")]
    Timeout(String),
}

impl ExecutionError {
    /// 判断当前错误是否允许进入统一重试流程。
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::Timeout(_) | Self::RateLimited { .. } | Self::Transport(_) | Self::Unknown(_)
        )
    }

    /// 计算统一重试流程使用的指数退避时长。
    pub fn retry_delay(&self, retry_count: u32) -> Duration {
        match self {
            Self::RateLimited {
                retry_after_secs: Some(secs),
                ..
            } => Duration::from_secs(*secs),
            _ => {
                let factor = 2_u64.saturating_pow(retry_count.saturating_sub(1));
                Duration::from_secs((factor.max(1) * 2).min(30))
            }
        }
    }

    /// 将执行错误映射为结构化失败原因。
    pub fn to_failure_reason(&self) -> FailureReason {
        match self {
            Self::Timeout(_) => FailureReason::Timeout,
            Self::RateLimited { .. } => FailureReason::RateLimited,
            Self::Authentication(_) => FailureReason::Authentication,
            Self::QuotaExhausted(_) => FailureReason::QuotaExhausted,
            Self::UserCancelled(_) => FailureReason::UserCancelled,
            Self::Transport(_) | Self::EmptyResponse => FailureReason::AgentError,
            Self::Unknown(_) => FailureReason::Unknown,
        }
    }

    /// 提供统一的人类可读错误消息。
    pub fn message(&self) -> &str {
        match self {
            Self::Timeout(message)
            | Self::Authentication(message)
            | Self::QuotaExhausted(message)
            | Self::Transport(message)
            | Self::UserCancelled(message)
            | Self::Unknown(message) => message,
            Self::RateLimited { message, .. } => message,
            Self::EmptyResponse => "empty response from model",
        }
    }
}

pub trait AgentExecutor: Send + Sync {
    /// 执行一次 Agent 请求并返回异步结果。
    fn execute(&self, request: AgentExecutionRequest) -> ExecutorFuture;
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BrainDecisionOutput {
    pub selected_agent_name: String,
    pub delegate_prompt: String,
    pub reasoning: String,
}

#[derive(Debug, Clone, Error, Serialize, Deserialize, PartialEq, Eq)]
pub enum BrainDecisionError {
    #[error("brain decision parse failed: {0}")]
    ParseFailed(String),
    #[error("brain selected unknown agent: {0}")]
    UnknownAgent(String),
    #[error("brain returned empty response")]
    EmptyResponse,
}

#[derive(Debug, Clone, Component)]
pub struct AgentSpawnRequestMessage {
    pub parent_agent_id: AgentId,
    pub task_id: TaskId,
    pub name: String,
    pub model: String,
    pub tags: Vec<String>,
    pub description: String,
}

#[derive(Debug, Clone, Component)]
pub struct TaskTerminatedMessage {
    pub task_id: TaskId,
}

/// Tool 执行请求消息
#[derive(Debug, Clone, Component)]
pub struct ToolExecutionRequestMessage {
    pub request: AgentExecutionRequest,
    pub tool_name: String,
    pub tool_input: serde_json::Value,
    /// 确认请求 ID（当工具需要确认时设置）
    pub pending_confirmation_id: Option<Uuid>,
}

/// Tool 执行结果消息
#[derive(Debug, Clone, Component)]
pub struct ToolExecutionResultMessage {
    pub result: AgentExecutionResult,
    pub tool_name: String,
    pub tool_output: Result<serde_json::Value, ToolError>,
}

/// 摘要触发来源
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SummarizationTrigger {
    /// Token 阈值触发
    TokenThreshold,
    /// 用户 /summarize 指令
    UserCommand,
    /// 任务完成
    TaskComplete,
}

/// 摘要请求消息
#[derive(Debug, Clone, Component)]
pub struct SummarizationRequestMessage {
    /// 关联的任务 ID
    pub task_id: TaskId,
    /// 待压缩的内容
    pub content_to_summarize: String,
    /// 目标 token 数
    pub target_tokens: u32,
    /// 摘要触发来源
    pub trigger: SummarizationTrigger,
}

/// 摘要结果消息
#[derive(Debug, Clone, Component)]
pub struct SummarizationResultMessage {
    /// 关联的任务 ID
    pub task_id: TaskId,
    /// 生成的摘要
    pub summary: Result<String, ExecutionError>,
}

/// 确认模式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConfirmMode {
    /// 单次确认，仅对本次请求生效
    Once,
    /// 永久确认，修正 Agent 的长期权限配置
    Permanent,
}

/// 确认选项
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConfirmationOption {
    /// 选项标识
    pub id: String,
    /// 显示文本
    pub label: String,
    /// 确认模式
    pub mode: ConfirmMode,
}

impl ConfirmationOption {
    /// 创建 "允许一次" 选项
    pub fn allow_once() -> Self {
        Self {
            id: "allow_once".to_string(),
            label: "Allow once".to_string(),
            mode: ConfirmMode::Once,
        }
    }

    /// 创建 "永久允许" 选项
    pub fn allow_always() -> Self {
        Self {
            id: "allow_always".to_string(),
            label: "Allow always".to_string(),
            mode: ConfirmMode::Permanent,
        }
    }

    /// 创建 "拒绝" 选项
    pub fn deny() -> Self {
        Self {
            id: "deny".to_string(),
            label: "Deny".to_string(),
            mode: ConfirmMode::Once, // Deny 模式不影响 Permanent
        }
    }

    /// 判断是否为拒绝选项
    pub fn is_deny(&self) -> bool {
        self.id == "deny"
    }

    /// 获取默认选项列表
    pub fn default_options() -> Vec<Self> {
        vec![Self::allow_once(), Self::allow_always(), Self::deny()]
    }
}

/// Tool 确认请求消息
#[derive(Debug, Clone, Component)]
pub struct ToolConfirmationRequestMessage {
    pub request_id: Uuid,
    pub task_id: TaskId,
    pub agent_id: AgentId,
    pub tool_name: String,
    pub tool_input: serde_json::Value,
    pub options: Vec<ConfirmationOption>,
}

/// Tool 确认响应消息
#[derive(Debug, Clone, Component)]
pub struct ToolConfirmationResponseMessage {
    pub request_id: Uuid,
    pub selected_option: String,
}

/// 审批请求消息
#[derive(Debug, Clone, Component)]
pub struct ApprovalRequestMessage {
    pub request_id: Uuid,
    pub source_task_id: TaskId,
    pub approval_task_id: TaskId,
    pub parent_agent_id: AgentId,
    pub child_agent_id: AgentId,
    pub tool_name: String,
    pub tool_input: serde_json::Value,
    pub context: String,
}

/// 审批结果消息
#[derive(Debug, Clone, Component)]
pub struct ApprovalResultMessage {
    pub request_id: Uuid,
    pub source_task_id: TaskId,
    pub approval_task_id: TaskId,
    pub decision: ApprovalDecision,
    pub reasoning: String,
}

/// 审批决策
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalDecision {
    Approved,
    Rejected,
}

/// 用户指令
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserCommand {
    /// /btw - 创建子任务承接新话题
    NewTask { topic: String },
    /// /finish - 结束当前任务
    FinishCurrentTask,
    /// /summarize - 触发总结
    Summarize,
    /// /remember - 添加知识到 SpaceKnowledge
    Remember { content: String },
    /// 普通输入（非指令）
    PlainText(String),
}

impl UserCommand {
    /// 解析用户输入
    pub fn parse(input: &str) -> Self {
        let trimmed = input.trim();
        if trimmed.starts_with("/btw ") {
            Self::NewTask {
                topic: trimmed[4..].trim().to_string(),
            }
        } else if trimmed == "/btw" {
            Self::NewTask {
                topic: String::new(),
            }
        } else if trimmed == "/finish" {
            Self::FinishCurrentTask
        } else if trimmed == "/summarize" {
            Self::Summarize
        } else if let Some(stripped) = trimmed.strip_prefix("/remember ") {
            Self::Remember {
                content: stripped.trim().to_string(),
            }
        } else if trimmed == "/remember" {
            Self::Remember {
                content: String::new(),
            }
        } else {
            Self::PlainText(input.to_string())
        }
    }

    /// 判断是否是指令
    pub fn is_command(&self) -> bool {
        !matches!(self, Self::PlainText(_))
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct AgentConfig {
    pub agent: Vec<AgentEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AgentEntry {
    pub name: String,
    pub model: String,
    pub tags: Vec<String>,
    pub description: String,
    /// Tool 权限配置
    pub tools: Option<AgentToolsConfig>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waiting_reason_has_user_and_evaluator() {
        use WaitingReason::*;
        let _ = User;
        let _ = Evaluator;
    }
}
