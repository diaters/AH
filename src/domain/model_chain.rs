use crate::llm::LlmProviderKind;
use crate::prelude::Component;
use serde::{Deserialize, Serialize};
use std::time::Instant;

/// providers.toml 中的 provider 配置条目
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderEntry {
    pub name: String,
    pub kind: LlmProviderKind,
    pub api_key_env: String,
    pub api_base: Option<String>,
}

/// providers.toml 顶层结构
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProvidersConfig {
    pub default_fallback_cooldown_secs: u64,
    #[serde(default)]
    pub default_provider: Option<String>,
    pub provider: Vec<ProviderEntry>,
}

/// agents.toml 中 [[agent.models]] 的条目
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelChainEntry {
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub fallback_cooldown_secs: Option<u64>,
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

    fn make_test_chain() -> Vec<ModelChainEntry> {
        vec![
            ModelChainEntry {
                provider: "openai".to_string(),
                model: "gpt-4.1-mini".to_string(),
                fallback_cooldown_secs: None,
            },
            ModelChainEntry {
                provider: "deepseek".to_string(),
                model: "deepseek-chat".to_string(),
                fallback_cooldown_secs: Some(120),
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
