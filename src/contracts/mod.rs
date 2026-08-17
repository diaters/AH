//! 契约层
//!
//! 定义模块间的稳定接口，支撑模块替换和测试。

mod memory;
mod runtime;

pub use memory::MemoryStore;
pub use runtime::{AsyncRuntime, Clock, FrontendRegistry};
