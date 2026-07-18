//! Dispatch 模块
//!
//! 包含任务分发和 Agent 选择相关的 System。

mod agent_lifecycle_hook;
mod brain_dispatch;
mod brain_llm_builder;
mod dispatch_system;
mod memory_selection;
mod message_dispatched_hook;
mod prompt_builder;
mod subtask_dispatch_preparation;
mod workitem_lifecycle_hook;

pub(crate) use agent_lifecycle_hook::{agent_started_hook_system, agent_stopped_hook_system};
pub(crate) use brain_dispatch::parse_brain_skill_selection;
pub(crate) use brain_llm_builder::build_brain_execution_request;
pub(crate) use dispatch_system::dispatch_system;
pub(crate) use message_dispatched_hook::on_message_dispatched_hook_system;
pub(crate) use subtask_dispatch_preparation::subtask_dispatch_preparation_system;
pub(crate) use workitem_lifecycle_hook::workitem_lifecycle_hook_system;
