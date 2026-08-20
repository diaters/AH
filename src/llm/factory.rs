use std::sync::Arc;

use anyhow::Result;
use tracing::debug;

use crate::domain::AgentExecutor;

use super::{genai::GenaiExecutor, provider::LlmProviderConfig};

/// 基于 provider 配置创建可注入系统层的执行器实例。
///
/// 所有 `LlmProviderKind` 变体共用 `GenaiExecutor` 单一实现，
/// provider 差异在 `GenaiExecutor::new` 内部处理。
pub fn create_executor_from_config(config: &LlmProviderConfig) -> Result<Arc<dyn AgentExecutor>> {
    debug!(provider = ?config.provider, model = ?config.model, "creating executor from config");
    Ok(Arc::new(GenaiExecutor::new(config)?))
}
