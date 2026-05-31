//! 摘要相关类型定义
//!
//! 定义摘要触发来源等。

/// 摘要触发来源
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SummarizationTrigger {
    /// Token 阈值触发
    TokenThreshold,
    /// 用户 /summarize 指令
    UserCommand,
    /// 任务完成
    TaskComplete,
}
