//! WorkItem 统一工作单元
//!
//! 定义统一的工作单元类型，支持规划、执行、摘要等多种工作类型。

use bevy::prelude::*;
use uuid::Uuid;

use crate::{
    contracts::TagSet,
    domain::{AgentId, ConversationMessage, TaskId, ToolDefinition},
};

/// 工作项类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WorkItemType {
    /// 规划工作项
    Planning,
    /// 执行工作项
    Execution,
    /// 摘要工作项
    Summarization,
    /// 评估工作项
    Evaluation,
}

/// 工作项状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WorkItemStatus {
    /// 待分配
    Pending,
    /// 已分配
    Assigned,
    /// 执行中
    Running,
    /// 已完成
    Completed,
    /// 失败
    Failed,
}

/// 工作项来源
#[derive(Debug, Clone)]
pub enum WorkItemOrigin {
    /// 用户任务
    UserTask,
    /// 规划产物
    PlanArtifact,
    /// 记忆压缩
    MemoryCompaction,
    /// 评估
    Evaluation,
}

/// 工作项写回目标
#[derive(Debug, Clone)]
pub enum WorkItemWritebackTarget {
    /// 任务结果
    TaskResult,
    /// 规划产物
    PlanArtifact,
    /// 短期上下文
    ShortTermContext,
    /// 长期记忆
    LongTermMemory,
}

/// 工作项上下文
#[derive(Debug, Clone, Default)]
pub struct WorkItemContext {
    /// 对话历史
    pub conversation: Option<Vec<ConversationMessage>>,
    /// 可用工具
    pub tools: Vec<ToolDefinition>,
    /// 系统提示词
    pub system_prompt: Option<String>,
}

/// 工作项输入
#[derive(Debug, Clone)]
pub struct WorkItemInput {
    /// 提示词
    pub prompt: String,
    /// 上下文
    pub context: WorkItemContext,
}

impl WorkItemInput {
    pub fn new(prompt: String) -> Self {
        Self {
            prompt,
            context: WorkItemContext::default(),
        }
    }

    pub fn with_context(mut self, context: WorkItemContext) -> Self {
        self.context = context;
        self
    }

    pub fn with_tools(mut self, tools: Vec<ToolDefinition>) -> Self {
        self.context.tools = tools;
        self
    }

    pub fn with_system_prompt(mut self, system_prompt: String) -> Self {
        self.context.system_prompt = Some(system_prompt);
        self
    }
}

/// 统一工作单元
///
/// 代表一个待执行的工作单元，可以来自用户任务、规划产物或记忆压缩。
#[derive(Debug, Clone, Component)]
pub struct WorkItem {
    /// 唯一标识
    pub id: Uuid,
    /// 关联的任务 ID
    pub task_id: TaskId,
    /// 工作类型
    pub work_type: WorkItemType,
    /// 输入
    pub input: WorkItemInput,
    /// 标签集合
    pub tags: TagSet,
    /// 状态
    pub status: WorkItemStatus,
    /// 分配的 Agent
    pub assigned_agent: Option<AgentId>,
    /// 来源
    pub origin: WorkItemOrigin,
    /// 写回目标
    pub writeback_target: WorkItemWritebackTarget,
}

impl WorkItem {
    /// 创建新的工作项
    pub fn new(
        task_id: TaskId,
        work_type: WorkItemType,
        input: WorkItemInput,
        tags: TagSet,
        origin: WorkItemOrigin,
        writeback_target: WorkItemWritebackTarget,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            task_id,
            work_type,
            input,
            tags,
            status: WorkItemStatus::Pending,
            assigned_agent: None,
            origin,
            writeback_target,
        }
    }

    /// 创建执行工作项
    pub fn execution(task_id: TaskId, prompt: String, tags: TagSet) -> Self {
        Self::new(
            task_id,
            WorkItemType::Execution,
            WorkItemInput::new(prompt),
            tags,
            WorkItemOrigin::UserTask,
            WorkItemWritebackTarget::TaskResult,
        )
    }

    /// 创建摘要工作项
    pub fn summarization(task_id: TaskId, content: String, target_tokens: usize) -> Self {
        let tags = TagSet::from_tags(["summarization"]);
        let input = WorkItemInput::new(format!(
            "请对以下内容进行摘要，目标约 {} tokens:\n\n{}",
            target_tokens, content
        ));
        Self::new(
            task_id,
            WorkItemType::Summarization,
            input,
            tags,
            WorkItemOrigin::MemoryCompaction,
            WorkItemWritebackTarget::ShortTermContext,
        )
    }

    /// 标记为已分配
    pub fn assign(&mut self, agent_id: AgentId) {
        self.assigned_agent = Some(agent_id);
        self.status = WorkItemStatus::Assigned;
    }

    /// 标记为执行中
    pub fn start(&mut self) {
        self.status = WorkItemStatus::Running;
    }

    /// 标记为已完成
    pub fn complete(&mut self) {
        self.status = WorkItemStatus::Completed;
    }

    /// 标记为失败
    pub fn fail(&mut self) {
        self.status = WorkItemStatus::Failed;
    }

    /// 是否待分配
    pub fn is_pending(&self) -> bool {
        self.status == WorkItemStatus::Pending
    }

    /// 是否已完成
    pub fn is_completed(&self) -> bool {
        self.status == WorkItemStatus::Completed
    }

    /// 是否失败
    pub fn is_failed(&self) -> bool {
        self.status == WorkItemStatus::Failed
    }

    /// 是否终态
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.status,
            WorkItemStatus::Completed | WorkItemStatus::Failed
        )
    }
}

/// 工作项创建消息
#[derive(Debug, Clone, Event)]
pub struct WorkItemCreatedMessage {
    pub work_item_id: Uuid,
    pub task_id: TaskId,
    pub work_type: WorkItemType,
}

/// 工作项完成消息
#[derive(Debug, Clone, Event)]
pub struct WorkItemCompletedMessage {
    pub work_item_id: Uuid,
    pub task_id: TaskId,
    pub work_type: WorkItemType,
    pub success: bool,
    pub result: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn work_item_execution_creation() {
        let task_id = Uuid::nil();
        let work_item = WorkItem::execution(
            task_id,
            "test prompt".to_string(),
            TagSet::from_tags(["llm"]),
        );
        assert_eq!(work_item.work_type, WorkItemType::Execution);
        assert_eq!(work_item.status, WorkItemStatus::Pending);
        assert!(work_item.is_pending());
    }

    #[test]
    fn work_item_state_transitions() {
        let task_id = Uuid::nil();
        let mut work_item = WorkItem::execution(task_id, "test".to_string(), TagSet::empty());

        assert!(work_item.is_pending());
        work_item.assign(Uuid::new_v4());
        assert_eq!(work_item.status, WorkItemStatus::Assigned);

        work_item.start();
        assert_eq!(work_item.status, WorkItemStatus::Running);

        work_item.complete();
        assert!(work_item.is_completed());
        assert!(work_item.is_terminal());
    }

    #[test]
    fn work_item_summarization() {
        let task_id = Uuid::nil();
        let work_item = WorkItem::summarization(task_id, "content to summarize".to_string(), 500);
        assert_eq!(work_item.work_type, WorkItemType::Summarization);
        assert!(work_item.tags.tags.contains(&"summarization".to_string()));
    }
}
