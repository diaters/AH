use std::sync::Arc;

use anyhow::Result;
use tracing::debug;

use crate::domain::AgentExecutor;

use super::{
    genai::GenaiExecutor,
    provider::{LlmProviderConfig, LlmProviderKind},
};

/// 基于 provider 配置创建可注入系统层的执行器实例。
pub fn create_executor_from_config(config: &LlmProviderConfig) -> Result<Arc<dyn AgentExecutor>> {
    match config.provider {
        LlmProviderKind::OpenAi
        | LlmProviderKind::Anthropic
        | LlmProviderKind::DeepSeek
        | LlmProviderKind::OpenAiCompatible => {
            debug!(provider = ?config.provider, model = %config.model, "creating executor from config");
            Ok(Arc::new(GenaiExecutor::new(config)?))
        }
    }
}
