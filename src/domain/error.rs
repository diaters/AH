//! 执行错误类型
//!
//! 定义 LLM 执行和 Tool 执行的错误类型。

use serde::{Deserialize, Serialize};
use std::time::Duration;
use thiserror::Error;

/// 执行错误
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

/// 失败原因
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
