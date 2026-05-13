use std::sync::Arc;

use anyhow::Result;

use crate::domain::AgentExecutor;

use super::{
    openai::OpenAiExecutor,
    provider::{LlmProviderConfig, LlmProviderKind},
};

/// 基于 provider 配置创建可注入系统层的执行器实例。
pub fn create_executor_from_config(config: &LlmProviderConfig) -> Result<Arc<dyn AgentExecutor>> {
    match config.provider {
        LlmProviderKind::OpenAi | LlmProviderKind::OpenAiCompatible => {
            Ok(Arc::new(OpenAiExecutor::new(config)?))
        }
    }
}
