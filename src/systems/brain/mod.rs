//! Brain 知识域
//!
//! 集中 Brain 决策的完整知识：候选 Agent 描述与 prompt 构建（`brain_dispatch`）、
//! Brain LLM 请求构造（`brain_llm_builder`）、Brain 输出解析与派发决策
//! （`brain_decision`）。此前三者分散在 `dispatch/` 与 `transform/`，
//! 现按知识域归位为单一目录。

mod brain_decision;
mod brain_dispatch;
mod brain_llm_builder;

pub(crate) use brain_decision::brain_decision_system;
pub(crate) use brain_llm_builder::{build_brain_execution_request, find_brain_agent};
