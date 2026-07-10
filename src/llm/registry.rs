use crate::domain::ProvidersConfig;
use crate::llm::genai::GenaiExecutor;
use crate::prelude::Resource;
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{debug, info};

/// 全局 executor 注册表，按 provider name 索引
#[derive(Resource, Clone)]
pub struct ExecutorRegistry {
    pub(crate) executors: HashMap<String, Arc<dyn crate::domain::AgentExecutor>>,
    default_fallback_cooldown_secs: u64,
}

impl ExecutorRegistry {
    /// 从 ProvidersConfig 构建注册表
    pub fn from_config(config: &ProvidersConfig) -> Result<Self> {
        let mut executors = HashMap::new();

        for entry in &config.provider {
            let llm_config = crate::llm::LlmProviderConfig {
                provider: entry.kind.clone(),
                model: "placeholder".to_string(), // 模型由请求覆盖
                api_key: std::env::var(&entry.api_key_env).ok(),
                api_base: entry.api_base.clone(),
            };

            let executor = GenaiExecutor::new(&llm_config).with_context(|| {
                format!("failed to create executor for provider '{}'", entry.name)
            })?;

            executors.insert(
                entry.name.clone(),
                Arc::new(executor) as Arc<dyn crate::domain::AgentExecutor>,
            );

            debug!(
                provider = %entry.name,
                kind = ?entry.kind,
                "executor registered"
            );
        }

        info!(
            provider_count = executors.len(),
            default_cooldown_secs = config.default_fallback_cooldown_secs,
            "executor registry initialized"
        );

        Ok(Self {
            executors,
            default_fallback_cooldown_secs: config.default_fallback_cooldown_secs,
        })
    }

    /// 查找指定 provider 的 executor
    pub fn get(&self, provider_name: &str) -> Option<Arc<dyn crate::domain::AgentExecutor>> {
        self.executors.get(provider_name).cloned()
    }

    /// 从环境变量构建单 provider 注册表（向后兼容）
    pub fn from_env() -> Result<Self> {
        let config = crate::llm::LlmProviderConfig::from_env("gpt-4.1-mini")?;
        let executor = GenaiExecutor::new(&config)?;

        let provider_name =
            std::env::var("HARNESS_LLM_PROVIDER").unwrap_or_else(|_| "default".to_string());

        let mut executors = HashMap::new();
        executors.insert(
            provider_name.clone(),
            Arc::new(executor) as Arc<dyn crate::domain::AgentExecutor>,
        );

        info!(
            provider = %provider_name,
            "single-provider registry initialized (backward compat)"
        );

        Ok(Self {
            executors,
            default_fallback_cooldown_secs: 60,
        })
    }

    /// 全局默认冷却期
    pub fn default_cooldown_secs(&self) -> u64 {
        self.default_fallback_cooldown_secs
    }

    /// Create a single-provider registry from an existing executor
    pub fn from_single_executor(
        executor: Arc<dyn crate::domain::AgentExecutor>,
        provider_name: &str,
    ) -> Self {
        let mut executors = HashMap::new();
        executors.insert(provider_name.to_string(), executor);

        Self {
            executors,
            default_fallback_cooldown_secs: 60,
        }
    }
}
