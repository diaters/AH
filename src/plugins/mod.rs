//! Plugin 模块
//!
//! 提供 Bevy Plugin 架构的模块化装配。

mod default_runtime;
mod dispatch;
mod execution;
mod frontend;
mod memory;
mod task_runtime;
mod tools;

pub use default_runtime::DefaultRuntimePluginGroup;
pub use dispatch::DispatchPlugin;
pub use execution::ExecutionPlugin;
pub use frontend::FrontendPlugin;
pub use memory::MemoryPlugin;
pub use task_runtime::TaskRuntimePlugin;
pub use tools::ToolRuntimePlugin;
