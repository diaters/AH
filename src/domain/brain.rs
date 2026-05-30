//! Brain 决策相关类型定义
//!
//! 定义 Brain 决策输出和错误类型。

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Brain 决策输出
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BrainDecisionOutput {
    pub selected_agent_name: String,
    pub delegate_prompt: String,
    pub reasoning: String,
}

/// Brain 决策错误
#[derive(Debug, Clone, Error, Serialize, Deserialize, PartialEq, Eq)]
pub enum BrainDecisionError {
    #[error("brain decision parse failed: {0}")]
    ParseFailed(String),
    #[error("brain selected unknown agent: {0}")]
    UnknownAgent(String),
    #[error("brain returned empty response")]
    EmptyResponse,
}
