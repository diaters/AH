//! Dispatch 模块
//!
//! 包含任务分发和 Agent 选择相关的 System。

mod agent_lifecycle_hook;
mod agent_selection;
mod brain_dispatch;
mod memory_selection;
mod message_dispatched_hook;
mod task_dispatch;
mod workitem_dispatch;
mod workitem_lifecycle_hook;

pub(crate) use agent_lifecycle_hook::{agent_started_hook_system, agent_stopped_hook_system};
pub use brain_dispatch::brain_dispatch_system;
pub(crate) use message_dispatched_hook::on_message_dispatched_hook_system;
pub use task_dispatch::task_dispatch_system;
pub(crate) use workitem_dispatch::workitem_dispatch_system;
pub(crate) use workitem_lifecycle_hook::workitem_lifecycle_hook_system;
