//! 契约层
//!
//! 定义模块间的稳定接口，支撑模块替换和测试。

mod dispatch;
mod execution;
mod memory;
mod tools;

pub use dispatch::{
    AgentCapabilitySummary, AgentSelector, AllMatchTagMatcher, AssignmentResult,
    BrainSelectionPolicy, DefaultDispatchPolicy, DispatchContext, DispatchPolicy, FirstBrainPolicy,
    FirstByTagPolicy, FirstSummarizerPolicy, SummarizerSelectionPolicy, TagBasedSelector,
    TagMatcher, TagSet,
};
pub use execution::{ExecutionBackend, ExecutionPolicy};
pub use memory::{
    CompressionTrigger, ContributionPolicy, DefaultCompactionPolicy, DefaultContributionPolicy,
    MemoryCompactionContext, MemoryCompactor, MemoryStore, SummaryResult, WritebackDecision,
};
pub use tools::{ApprovalRoute, DefaultToolApprovalPolicy, ToolApprovalPolicy, ToolCatalog};
