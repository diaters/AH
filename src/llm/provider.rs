use std::env;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum LlmProviderKind {
    OpenAi,
    OpenAiCompatible,
}

impl LlmProviderKind {
    /// 从环境变量中的 provider 字符串解析 provider 类型。
    pub fn parse(raw: &str) -> Result<Self> {
        match raw.trim().to_lowercase().as_str() {
            "openai" => Ok(Self::OpenAi),
            "openai-compatible" | "openai_compatible" | "compatible" => Ok(Self::OpenAiCompatible),
            other => bail!(
                "unsupported HARNESS_LLM_PROVIDER value: {other}; expected openai or openai-compatible"
            ),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LlmProviderConfig {
    pub provider: LlmProviderKind,
    pub model: String,
    pub api_key: String,
    pub api_base: Option<String>,
    pub org_id: Option<String>,
    pub project_id: Option<String>,
}

impl LlmProviderConfig {
    /// 从环境变量加载 provider、模型和连接信息。
    pub fn from_env(default_model: &str) -> Result<Self> {
        let provider_raw =
            env::var("HARNESS_LLM_PROVIDER").unwrap_or_else(|_| "openai".to_string());
        let provider = LlmProviderKind::parse(&provider_raw)?;
        let model = env::var("HARNESS_MODEL").unwrap_or_else(|_| default_model.to_string());
        let api_key = read_first_env(&["HARNESS_LLM_API_KEY", "OPENAI_API_KEY"]).context(
            "missing HARNESS_LLM_API_KEY or OPENAI_API_KEY; please export a valid API key",
        )?;
        let api_base = read_first_env(&["HARNESS_LLM_API_BASE", "OPENAI_BASE_URL"]);
        let org_id = read_first_env(&["HARNESS_LLM_ORG_ID", "OPENAI_ORG_ID"]);
        let project_id = read_first_env(&["HARNESS_LLM_PROJECT_ID", "OPENAI_PROJECT_ID"]);

        let config = Self {
            provider,
            model,
            api_key,
            api_base,
            org_id,
            project_id,
        };

        config.validate()?;
        Ok(config)
    }

    /// 校验 provider 配置是否满足启动条件。
    pub fn validate(&self) -> Result<()> {
        if self.model.trim().is_empty() {
            bail!("HARNESS_MODEL must not be empty");
        }

        if self.api_key.trim().is_empty() {
            bail!("API key must not be empty");
        }

        if matches!(self.provider, LlmProviderKind::OpenAiCompatible)
            && self
                .api_base
                .as_deref()
                .is_none_or(|api_base| api_base.trim().is_empty())
        {
            bail!("HARNESS_LLM_API_BASE is required when HARNESS_LLM_PROVIDER=openai-compatible");
        }

        Ok(())
    }
}

/// 按优先级读取多个环境变量中的首个非空值。
fn read_first_env(keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        env::var(key)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    })
}

#[cfg(test)]
mod tests {
    use super::{LlmProviderConfig, LlmProviderKind};

    /// 验证 provider 字符串可以被稳定解析为内部枚举。
    #[test]
    fn parses_provider_aliases() {
        assert_eq!(
            LlmProviderKind::parse("openai").expect("openai should parse"),
            LlmProviderKind::OpenAi
        );
        assert_eq!(
            LlmProviderKind::parse("openai-compatible").expect("openai-compatible should parse"),
            LlmProviderKind::OpenAiCompatible
        );
        assert_eq!(
            LlmProviderKind::parse("compatible").expect("compatible should parse"),
            LlmProviderKind::OpenAiCompatible
        );
    }

    /// 验证 OpenAI 兼容 provider 缺失 base URL 时会被拒绝。
    #[test]
    fn rejects_compatible_provider_without_api_base() {
        let config = LlmProviderConfig {
            provider: LlmProviderKind::OpenAiCompatible,
            model: "test-model".to_string(),
            api_key: "test-key".to_string(),
            api_base: None,
            org_id: None,
            project_id: None,
        };

        let error = config
            .validate()
            .expect_err("compatible provider without api base should fail");

        assert!(
            error
                .to_string()
                .contains("HARNESS_LLM_API_BASE is required"),
            "unexpected error: {error}"
        );
    }

    /// 验证 OpenAI 兼容 provider 在完整配置下可以通过校验。
    #[test]
    fn accepts_compatible_provider_with_api_base() {
        let config = LlmProviderConfig {
            provider: LlmProviderKind::OpenAiCompatible,
            model: "test-model".to_string(),
            api_key: "test-key".to_string(),
            api_base: Some("https://example.com/v1".to_string()),
            org_id: None,
            project_id: None,
        };

        config
            .validate()
            .expect("compatible provider with api base should pass");
    }
}
