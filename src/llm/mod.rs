mod brain_prompt;
mod factory;
mod genai;
mod provider;
mod registry;
mod summarization_prompt;

pub use brain_prompt::{brain_system_prompt, brain_user_prompt, parse_brain_decision};
pub use factory::create_executor_from_config;
pub use provider::{LlmProviderConfig, LlmProviderKind};
pub use registry::ExecutorRegistry;
pub use summarization_prompt::{summarization_system_prompt, summarization_user_prompt};
