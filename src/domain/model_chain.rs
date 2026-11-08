use crate::domain::LlmProviderKind;
use crate::prelude::Component;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Instant;

/// providers.toml 中的 provider 配置条目
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProviderEntry {
    pub name: String,
    pub kind: LlmProviderKind,
    pub api_key_env: String,
    pub api_base: Option<String>,
    /// 按模型名配置的单价（元 / 百万 token），用于用量统计折算费用；未配置的模型只统计 token
    #[serde(default)]
    pub pricing: HashMap<String, ModelPrice>,
}

/// 模型单价（元 / 百万 token）。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct ModelPrice {
    pub input: f64,
    pub output: f64,
    /// 缓存命中的输入单价；未配置时按普通输入计
    #[serde(default)]
    pub cached_input: Option<f64>,
}

impl ModelPrice {
    /// 按单价折算一次调用的费用（元）。缓存命中部分按 cached_input 计价。
    pub fn cost(&self, usage: &super::TokenUsage) -> f64 {
        let cached = usage.cached_input_tokens.min(usage.input_tokens);
        let uncached = usage.input_tokens - cached;
        (uncached as f64 * self.input
            + cached as f64 * self.cached_input.unwrap_or(self.input)
            + usage.output_tokens as f64 * self.output)
            / 1_000_000.0
    }
}

/// providers.toml 顶层结构
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProvidersConfig {
    pub default_fallback_cooldown_secs: u64,
    #[serde(default)]
    pub default_provider: Option<String>,
    pub provider: Vec<ProviderEntry>,
    /// 语音转写服务；未配置时语音消息只以 `[VOICE:path]` 标记进入任务
    #[serde(default)]
    pub transcription: Option<TranscriptionConfig>,
}

/// providers.toml 中的 `[transcription]` 段：OpenAI 兼容的 `/audio/transcriptions` 接口。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TranscriptionConfig {
    pub api_key_env: String,
    /// 接口根地址（不含 `/audio/transcriptions`），缺省为 OpenAI 官方地址
    #[serde(default)]
    pub api_base: Option<String>,
    pub model: String,
    /// ISO-639-1 语言代码提示（如 `zh`）；缺省由服务自动识别
    #[serde(default)]
    pub language: Option<String>,
}

/// agents.toml 中 [[agent.models]] 的条目
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelChainEntry {
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub fallback_cooldown_secs: Option<u64>,
    /// 模型是否接受图片输入；默认 false，图片以 `[IMAGE:...]` 文本标记发送
    #[serde(default)]
    pub vision: bool,
}

/// Agent 运行时的模型链状态（Bevy Component）
#[derive(Debug, Clone, Component)]
pub struct ModelChainState {
    pub chain: Vec<ModelChainEntry>,
    pub active_index: usize,
    pub cooldown_until: Option<Instant>,
    pub default_cooldown_secs: u64,
}

impl ModelChainState {
    pub fn new(chain: Vec<ModelChainEntry>, default_cooldown_secs: u64) -> Self {
        Self {
            chain,
            active_index: 0,
            cooldown_until: None,
            default_cooldown_secs,
        }
    }

    pub fn current_entry(&self) -> &ModelChainEntry {
        &self.chain[self.active_index]
    }

    pub fn step_fallback(&mut self, cooldown_secs: u64) -> bool {
        if self.active_index + 1 >= self.chain.len() {
            return false;
        }
        self.active_index += 1;
        self.cooldown_until = Some(Instant::now() + std::time::Duration::from_secs(cooldown_secs));
        true
    }

    pub fn reset_if_cooldown_expired(&mut self, now: Instant) -> bool {
        if let Some(until) = self.cooldown_until
            && now >= until
        {
            self.active_index = 0;
            self.cooldown_until = None;
            return true;
        }
        false
    }

    pub fn current_provider(&self) -> &str {
        &self.current_entry().provider
    }

    pub fn current_model(&self) -> &str {
        &self.current_entry().model
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn providers_example_parses_with_pricing() {
        let config: ProvidersConfig =
            toml::from_str(include_str!("../../providers.toml.example")).unwrap();
        let deepseek = config
            .provider
            .iter()
            .find(|p| p.name == "deepseek")
            .unwrap();
        let price = deepseek.pricing["deepseek-chat"];
        assert_eq!(
            (price.input, price.output, price.cached_input),
            (2.0, 8.0, Some(0.5))
        );
        let openai = config.provider.iter().find(|p| p.name == "openai").unwrap();
        assert!(openai.pricing.is_empty());
        let transcription = config.transcription.unwrap();
        assert_eq!(
            (
                transcription.api_key_env.as_str(),
                transcription.model.as_str()
            ),
            ("OPENAI_API_KEY", "whisper-1")
        );
        assert_eq!(transcription.api_base, None);
    }

    fn make_test_chain() -> Vec<ModelChainEntry> {
        vec![
            ModelChainEntry {
                provider: "openai".to_string(),
                model: "gpt-4.1-mini".to_string(),
                fallback_cooldown_secs: None,
                vision: false,
            },
            ModelChainEntry {
                provider: "deepseek".to_string(),
                model: "deepseek-chat".to_string(),
                fallback_cooldown_secs: Some(120),
                vision: false,
            },
        ]
    }

    #[test]
    fn new_initializes_active_index_to_zero() {
        let chain = make_test_chain();
        let state = ModelChainState::new(chain.clone(), 60);

        assert_eq!(state.active_index, 0);
        assert_eq!(state.chain.len(), 2);
        assert!(state.cooldown_until.is_none());
    }

    #[test]
    fn current_entry_returns_first_by_default() {
        let chain = make_test_chain();
        let state = ModelChainState::new(chain, 60);

        let entry = state.current_entry();
        assert_eq!(entry.provider, "openai");
        assert_eq!(entry.model, "gpt-4.1-mini");
    }

    #[test]
    fn step_fallback_moves_to_next_priority() {
        let chain = make_test_chain();
        let mut state = ModelChainState::new(chain, 60);

        let result = state.step_fallback(90);
        assert!(result);
        assert_eq!(state.active_index, 1);
        assert!(state.cooldown_until.is_some());
    }

    #[test]
    fn step_fallback_returns_false_when_exhausted() {
        let chain = make_test_chain();
        let mut state = ModelChainState::new(chain, 60);

        state.step_fallback(90);
        let result = state.step_fallback(90);
        assert!(!result);
        assert_eq!(state.active_index, 1);
    }

    #[test]
    fn reset_if_cooldown_expired_resets_to_first_priority() {
        let chain = make_test_chain();
        let mut state = ModelChainState::new(chain, 60);

        state.step_fallback(90);
        // 冷却期未过
        assert!(!state.reset_if_cooldown_expired(Instant::now()));
        assert_eq!(state.active_index, 1);

        // 模拟冷却期已过（设置一个过去的时刻）
        state.cooldown_until = Some(Instant::now() - std::time::Duration::from_secs(1));
        assert!(state.reset_if_cooldown_expired(Instant::now()));
        assert_eq!(state.active_index, 0);
        assert!(state.cooldown_until.is_none());
    }

    #[test]
    fn current_provider_and_model_helpers() {
        let chain = make_test_chain();
        let state = ModelChainState::new(chain, 60);

        assert_eq!(state.current_provider(), "openai");
        assert_eq!(state.current_model(), "gpt-4.1-mini");
    }
}
