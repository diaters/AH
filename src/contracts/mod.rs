//! 契约层
//!
//! 定义模块间的稳定接口，支撑模块替换和测试。

mod experience;
mod memory;
mod runtime;
mod wake;

pub use experience::{ExperienceLibrary, SharedExperienceLibrary};
pub use memory::MemoryStore;
pub use runtime::{AsyncRuntime, BufferedIngress, Clock, FrontendRegistry};
pub use wake::{
    ExecutionResultSender, ModelChainStateUpdateSender, RuntimeDeadlines, RuntimeWake,
    ToolResultSender, WakeSender,
};
