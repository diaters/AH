//! 共享测试模块。
//!
//! 集成测试通过 `mod common;` 引入，子模块提供可复用的测试 harness。

// 各测试 binary 按需引用子模块，未引用的项不应产生 dead_code 噪音警告。
#[allow(dead_code)]
pub mod async_tool_bridge;
#[allow(dead_code)]
pub mod mock_executor;
