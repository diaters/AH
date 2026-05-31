//! Execution 契约
//!
//! 定义执行后端和执行策略相关的 trait 接口。

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use crate::domain::{AgentExecutionOutput, AgentExecutionRequest, ExecutionError};

/// 执行后端
///
/// 定义 LLM 执行的抽象接口，支持不同的执行后端实现。
pub trait ExecutionBackend: Send + Sync + 'static {
    /// 执行请求，返回异步结果
    fn execute(&self, request: AgentExecutionRequest) -> ExecutionFuture;
}

/// 执行 Future 类型别名
pub type ExecutionFuture =
    Pin<Box<dyn Future<Output = Result<AgentExecutionOutput, ExecutionError>> + Send>>;

/// 执行策略
///
/// 定义执行的重试、超时等策略配置。
pub trait ExecutionPolicy: Send + Sync + 'static {
    /// 最大重试次数
    fn max_retries(&self) -> u32;

    /// 根据重试次数计算重试延迟
    fn retry_delay(&self, retry_count: u32) -> Duration;

    /// 执行超时时间
    fn timeout(&self) -> Duration;
}

/// 默认执行策略
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct DefaultExecutionPolicy {
    pub max_retries: u32,
    pub base_retry_delay: Duration,
    pub max_retry_delay: Duration,
    pub timeout: Duration,
}

impl Default for DefaultExecutionPolicy {
    fn default() -> Self {
        Self {
            max_retries: 3,
            base_retry_delay: Duration::from_secs(1),
            max_retry_delay: Duration::from_secs(60),
            timeout: Duration::from_secs(300),
        }
    }
}

impl ExecutionPolicy for DefaultExecutionPolicy {
    fn max_retries(&self) -> u32 {
        self.max_retries
    }

    fn retry_delay(&self, retry_count: u32) -> Duration {
        // 指数退避：base * 2^retry，但不超过 max
        let delay = self.base_retry_delay.as_secs() * 2u64.pow(retry_count);
        Duration::from_secs(delay.min(self.max_retry_delay.as_secs()))
    }

    fn timeout(&self) -> Duration {
        self.timeout
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_execution_policy_values() {
        let policy = DefaultExecutionPolicy::default();
        assert_eq!(policy.max_retries(), 3);
        assert_eq!(policy.timeout(), Duration::from_secs(300));
    }

    #[test]
    fn retry_delay_exponential_backoff() {
        let policy = DefaultExecutionPolicy::default();
        assert_eq!(policy.retry_delay(0), Duration::from_secs(1));
        assert_eq!(policy.retry_delay(1), Duration::from_secs(2));
        assert_eq!(policy.retry_delay(2), Duration::from_secs(4));
    }

    #[test]
    fn retry_delay_capped_at_max() {
        let policy = DefaultExecutionPolicy {
            max_retry_delay: Duration::from_secs(10),
            ..Default::default()
        };
        // 2^10 = 1024, but capped at 10
        assert_eq!(policy.retry_delay(10), Duration::from_secs(10));
    }
}
