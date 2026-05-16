use std::{future::Future, pin::Pin, time::Duration};

use bevy::prelude::Component;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

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
    Brain,
    User,
    RetryBackoff,
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
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OutputMessage {
    pub content: String,
}

impl OutputMessage {
    /// 构造发往外部线程的输出消息。
    pub fn new(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
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

#[derive(Debug, Clone, Component)]
pub struct Agent {
    pub id: AgentId,
    pub profile: AgentProfile,
    pub capabilities: AgentCapabilities,
    pub kind: AgentKind,
    pub parent_id: Option<AgentId>,
    pub bound_task_id: Option<TaskId>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum AgentRequestKind {
    LlmCompletion,
    BrainDecision,
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
}

impl Task {
    /// 基于用户输入创建一个处于 Ready 状态的新任务。
    pub fn from_user_input(content: impl Into<String>, max_retries: u32) -> Self {
        let content = content.into();
        let now = Utc::now();

        Self {
            id: Uuid::new_v4(),
            content: content.clone(),
            creator: Uuid::nil(),
            delegate: None,
            status: TaskStatus::Ready,
            input_summary: content,
            result_summary: String::new(),
            priority: 0,
            created_at: now,
            updated_at: now,
            retry_count: 0,
            max_retries,
            next_retry_at: None,
            last_error: None,
        }
    }

    /// 将任务标记为分发等待状态。
    pub fn mark_waiting_for_agent(&mut self, agent_id: AgentId, now: DateTime<Utc>) {
        self.delegate = Some(agent_id);
        self.status = TaskStatus::Waiting(WaitingReason::Agent);
        self.updated_at = now;
    }

    /// 将任务标记为等待 Brain 决策状态。
    pub fn mark_waiting_for_brain(&mut self, agent_id: AgentId, now: DateTime<Utc>) {
        self.delegate = Some(agent_id);
        self.status = TaskStatus::Waiting(WaitingReason::Brain);
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

impl ExecutionError {
    /// 判断当前错误是否允许进入统一重试流程。
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::Timeout(_)
                | Self::RateLimited { .. }
                | Self::Transport(_)
                | Self::Unknown(_)
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
}
