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

    /// 判断错误是否应触发模型降级。
    /// 仅 429（限流）和 402（配额耗尽）触发降级。
    /// 401/403（认证/权限错误）不降级，因为同一环境下降级无效。
    pub fn is_fallback_eligible(&self) -> bool {
        matches!(self, Self::RateLimited { .. } | Self::QuotaExhausted(_))
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
    /// 框架内部状态不一致——非 LLM 输入错误。
    ///
    /// 当 orchestrator 在处理工具调用时发现必备的运行时上下文缺失
    /// （如 `work_item_id` 缺失、`SkillUpdateContext` 未注册等）时使用。
    /// 与 `InvalidInput` 区分：`InvalidInput` 表示 LLM 提交的参数有问题，
    /// 而 `InternalState` 表示框架自身的状态机不一致，LLM 重试同样参数
    /// 也无法解决。
    #[error("framework state error (not an input error): {0}")]
    InternalState(String),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_fallback_eligible_for_429_and_402() {
        let rate_limited = ExecutionError::RateLimited {
            message: "too many requests".to_string(),
            retry_after_secs: Some(60),
        };
        assert!(rate_limited.is_fallback_eligible());

        let quota_exhausted = ExecutionError::QuotaExhausted("insufficient quota".to_string());
        assert!(quota_exhausted.is_fallback_eligible());
    }

    #[test]
    fn is_fallback_eligible_false_for_other_errors() {
        let auth = ExecutionError::Authentication("invalid key".to_string());
        assert!(!auth.is_fallback_eligible());

        let timeout = ExecutionError::Timeout("timed out".to_string());
        assert!(!timeout.is_fallback_eligible());

        let transport = ExecutionError::Transport("network error".to_string());
        assert!(!transport.is_fallback_eligible());
    }
}
