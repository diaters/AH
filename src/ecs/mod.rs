//! ECS 关系建模改造相关基础设施（ADR-005）。
//!
//! 当前承载中心索引 `EntityIndex` 与 `RemovedComponents` 兜底清理系统。

pub mod entity_index;

pub use entity_index::*;
