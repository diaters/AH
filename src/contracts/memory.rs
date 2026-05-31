//! Memory 契约
//!
//! 定义记忆存储和治理相关的 trait 接口。

use uuid::Uuid;

use crate::domain::{AgentId, MemoryEntry, TaskId};

/// 记忆存储
///
/// 定义长期记忆的存储接口。
pub trait MemoryStore: Send + Sync + 'static {
    /// 获取 Agent 的所有记忆条目
    fn get_entries(&self, agent_id: AgentId) -> Vec<MemoryEntry>;

    /// 添加一条记忆条目
    fn add_entry(&mut self, agent_id: AgentId, entry: MemoryEntry);

    /// 删除一条记忆条目
    fn remove_entry(&mut self, agent_id: AgentId, entry_id: Uuid);

    /// 清空 Agent 的所有记忆
    fn clear(&mut self, agent_id: AgentId);
}

/// 记忆压缩上下文
#[derive(Debug, Clone)]
pub struct MemoryCompactionContext {
    pub task_id: TaskId,
    pub owner_agent_id: Option<AgentId>,
    pub content_to_compress: String,
    pub token_count: usize,
    pub trigger: CompressionTrigger,
}

impl MemoryCompactionContext {
    pub fn new(
        task_id: TaskId,
        owner_agent_id: Option<AgentId>,
        content_to_compress: String,
        token_count: usize,
        trigger: CompressionTrigger,
    ) -> Self {
        Self {
            task_id,
            owner_agent_id,
            content_to_compress,
            token_count,
            trigger,
        }
    }
}

/// 压缩触发原因
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompressionTrigger {
    /// Token 数量超过阈值
    TokenThreshold,
    /// 任务完成
    TaskComplete,
    /// 用户命令
    UserCommand,
}

/// 摘要结果
#[derive(Debug, Clone)]
pub struct SummaryResult {
    pub task_id: TaskId,
    pub content: String,
}

impl SummaryResult {
    pub fn new(task_id: TaskId, content: String) -> Self {
        Self { task_id, content }
    }
}

/// 写回决策
#[derive(Debug, Clone)]
pub enum WritebackDecision {
    /// 更新短期上下文
    UpdateShortTermContext,
    /// 添加到长期记忆
    AddLongTermMemory(MemoryEntry),
    /// 添加到共享知识库
    AddSharedKnowledge(MemoryEntry),
    /// 丢弃
    Drop,
}

/// 记忆治理协调器
///
/// 决定何时触发记忆压缩，并构建摘要请求。
pub trait MemoryCompactor: Send + Sync + 'static {
    /// 检查是否需要压缩
    fn should_compact(&self, context: &MemoryCompactionContext) -> bool;

    /// 计算压缩后的目标 token 数
    fn target_tokens(&self, context: &MemoryCompactionContext) -> usize;

    /// 保留的最近对话轮数
    fn preserve_recent_turns(&self) -> usize;
}

/// 压缩策略
#[derive(Debug, Clone)]
pub struct DefaultCompactionPolicy {
    /// 触发压缩的 token 阈值
    pub token_threshold: usize,
    /// 压缩后的目标 token 数
    pub target_tokens: usize,
    /// 保留的最近对话轮数
    pub preserve_recent_turns: usize,
}

impl Default for DefaultCompactionPolicy {
    fn default() -> Self {
        Self {
            token_threshold: 8000,
            target_tokens: 2000,
            preserve_recent_turns: 3,
        }
    }
}

impl MemoryCompactor for DefaultCompactionPolicy {
    fn should_compact(&self, context: &MemoryCompactionContext) -> bool {
        context.token_count >= self.token_threshold
            || matches!(context.trigger, CompressionTrigger::TaskComplete)
    }

    fn target_tokens(&self, _context: &MemoryCompactionContext) -> usize {
        self.target_tokens
    }

    fn preserve_recent_turns(&self) -> usize {
        self.preserve_recent_turns
    }
}

/// 经验沉淀策略
///
/// 决定摘要结果是否写回以及写回到哪里。
pub trait ContributionPolicy: Send + Sync + 'static {
    /// 根据摘要结果决定写回策略
    fn decide_writeback(&self, result: &SummaryResult) -> WritebackDecision;
}

/// 默认经验沉淀策略
#[derive(Debug, Clone, Copy, Default)]
pub struct DefaultContributionPolicy;

impl ContributionPolicy for DefaultContributionPolicy {
    fn decide_writeback(&self, _result: &SummaryResult) -> WritebackDecision {
        // 默认策略：丢弃（暂不自动沉淀）
        WritebackDecision::Drop
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compaction_policy_threshold() {
        let policy = DefaultCompactionPolicy::default();
        let context = MemoryCompactionContext::new(
            uuid::Uuid::nil(),
            None,
            "test".to_string(),
            10000,
            CompressionTrigger::TokenThreshold,
        );
        assert!(policy.should_compact(&context));
    }

    #[test]
    fn compaction_policy_task_complete() {
        let policy = DefaultCompactionPolicy::default();
        let context = MemoryCompactionContext::new(
            uuid::Uuid::nil(),
            None,
            "test".to_string(),
            100,
            CompressionTrigger::TaskComplete,
        );
        assert!(policy.should_compact(&context));
    }

    #[test]
    fn compaction_policy_below_threshold() {
        let policy = DefaultCompactionPolicy::default();
        let context = MemoryCompactionContext::new(
            uuid::Uuid::nil(),
            None,
            "test".to_string(),
            100,
            CompressionTrigger::TokenThreshold,
        );
        assert!(!policy.should_compact(&context));
    }
}
