//! LLM provider 领域类型
//!
//! Provider 种类枚举与解析，供模型链与 llm 接入层共享。

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum LlmProviderKind {
    OpenAi,
    Anthropic,
    DeepSeek,
    OpenAiCompatible,
}

impl LlmProviderKind {
    /// 从环境变量中的 provider 字符串解析 provider 类型。
    pub fn parse(raw: &str) -> Result<Self> {
        match raw.trim().to_lowercase().as_str() {
            "openai" => Ok(Self::OpenAi),
            "anthropic" | "claude" => Ok(Self::Anthropic),
            "deepseek" => Ok(Self::DeepSeek),
            "openai-compatible" | "openai_compatible" | "compatible" => Ok(Self::OpenAiCompatible),
            other => bail!(
                "unsupported HARNESS_LLM_PROVIDER value: {other}; expected openai, anthropic, deepseek, or openai-compatible"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::LlmProviderKind;

    /// 验证 provider 字符串可以被稳定解析为内部枚举。
    #[test]
    fn parses_provider_aliases() {
        assert_eq!(
            LlmProviderKind::parse("openai").expect("openai should parse"),
            LlmProviderKind::OpenAi
        );
        assert_eq!(
            LlmProviderKind::parse("anthropic").expect("anthropic should parse"),
            LlmProviderKind::Anthropic
        );
        assert_eq!(
            LlmProviderKind::parse("claude").expect("claude should parse"),
            LlmProviderKind::Anthropic
        );
        assert_eq!(
            LlmProviderKind::parse("deepseek").expect("deepseek should parse"),
            LlmProviderKind::DeepSeek
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

    /// 验证不支持的 provider 字符串会被拒绝。
    #[test]
    fn rejects_unknown_provider() {
        let err = LlmProviderKind::parse("unknown").unwrap_err();
        assert!(err.to_string().contains("unsupported HARNESS_LLM_PROVIDER"));
    }
}
