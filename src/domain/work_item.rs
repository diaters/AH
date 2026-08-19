//! WorkItem 统一工作单元
//!
//! 定义统一的工作单元类型，支持执行、摘要、评估等多种工作类型。

use crate::prelude::*;
use uuid::Uuid;

use crate::domain::HookPoint;
use crate::domain::{AgentId, ConversationMessage, SummarizationTrigger, TaskId, ToolDefinition};

/// 工作项类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WorkItemType {
    /// 执行工作项
    Execution,
    /// 摘要工作项
    Summarization,
    /// 评估工作项
    Evaluation,
    /// 经验收集工作项
    ExperienceCollection,
    /// profile 生成工作项（孵化场景生成新 profile，更新场景评估并生成更新后 profile）
    ProfileGeneration,
    /// skill 更新工作项：由 skill-updater Agent 消费，产出 SkillUpdateOperation 列表
    SkillUpdate,
    /// skill 创建工作项：由 skill-creator Agent 消费，根据用户意图生成新 skill
    SkillCreation,
}

impl WorkItemType {
    /// 返回此 WorkItem 类型对应的 Agent tag。
    ///
    /// `dispatch_system` 通过此方法查找匹配的 Persistent Agent。
    /// 集中管理 tag 映射，避免散落硬编码。
    pub fn required_tag(&self) -> &'static str {
        match self {
            WorkItemType::Evaluation => "evaluation",
            WorkItemType::Summarization => "summarization",
            WorkItemType::ExperienceCollection => "collect",
            WorkItemType::SkillUpdate => "skill-updater",
            WorkItemType::SkillCreation => "skill-creator",
            WorkItemType::ProfileGeneration => "profile",
            WorkItemType::Execution => "execution",
        }
    }
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

/// 标记 WorkItem 状态变更后尚未派发生命周期 hook 的 entity。
///
/// 由 `dispatch_system` / `llm_response_system` 在调用
/// `work_item.start()` / `.complete()` / `.fail()` 后附带，由 companion 系统
/// `workitem_lifecycle_hook_system` 派发对应 hook 后移除。内含的 `HookPoint`
/// 指示应派发的 hook 点（`OnWorkItemStarted` / `OnWorkItemCompleted` / `OnWorkItemFailed`）。
#[derive(Component, Debug, Clone)]
pub struct WorkItemLifecycleHookPending(pub HookPoint);

/// 工作项来源
#[derive(Debug, Clone, PartialEq)]
pub enum WorkItemOrigin {
    /// 用户任务
    UserTask,
    /// 记忆压缩
    MemoryCompaction,
    /// 评估
    Evaluation,
    /// 经验收集
    ExperienceCollection,
}

/// 工作项写回目标
#[derive(Debug, Clone, PartialEq)]
pub enum WorkItemWritebackTarget {
    /// 任务结果
    TaskResult,
    /// 短期上下文
    ShortTermContext,
    /// 长期记忆
    LongTermMemory,
    /// 经验收件箱
    ExperienceInbox,
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
/// 代表一个待执行的工作单元，可以来自用户任务或记忆压缩。
#[derive(Debug, Clone, Component)]
pub struct WorkItem {
    /// 唯一标识
    pub id: Uuid,
    /// 关联的任务 ID
    pub task_id: TaskId,
    /// 父任务 ID（经验收集用于溯源）
    pub parent_task_id: Option<TaskId>,
    /// 工作类型
    pub work_type: WorkItemType,
    /// 输入
    pub input: WorkItemInput,
    /// 状态（收窄为 crate 内可见：读取经 `status()`，转换经 `assign`/`start`/`complete`/`fail`）
    pub(crate) status: WorkItemStatus,
    /// 分配的 Agent（收窄为 crate 内可见：读取经 `assigned_agent()`，写入经 `assign`）
    pub(crate) assigned_agent: Option<AgentId>,
    /// 原任务治理者（仅经验收集等场景使用）
    pub governing_agent_id: Option<AgentId>,
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
        origin: WorkItemOrigin,
        writeback_target: WorkItemWritebackTarget,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            task_id,
            parent_task_id: None,
            work_type,
            input,
            status: WorkItemStatus::Pending,
            assigned_agent: None,
            governing_agent_id: None,
            origin,
            writeback_target,
        }
    }

    /// 创建执行工作项
    pub fn execution(task_id: TaskId, prompt: String) -> Self {
        Self::new(
            task_id,
            WorkItemType::Execution,
            WorkItemInput::new(prompt),
            WorkItemOrigin::UserTask,
            WorkItemWritebackTarget::TaskResult,
        )
    }

    /// 创建摘要工作项
    pub fn summarization(
        task_id: TaskId,
        content: String,
        target_tokens: usize,
        _trigger: SummarizationTrigger,
    ) -> Self {
        let input = WorkItemInput::new(format!(
            "请对以下内容进行摘要，目标约 {} tokens:\n\n{}",
            target_tokens, content
        ));
        Self::new(
            task_id,
            WorkItemType::Summarization,
            input,
            WorkItemOrigin::MemoryCompaction,
            WorkItemWritebackTarget::ShortTermContext,
        )
    }

    /// 创建评估工作项
    pub fn evaluation(task_id: TaskId, prompt: String, reasoning_hint: Option<String>) -> Self {
        let full_prompt = if let Some(hint) = reasoning_hint {
            format!("{}\n\n评估提示: {}", prompt, hint)
        } else {
            prompt
        };
        let input = WorkItemInput::new(full_prompt);
        Self::new(
            task_id,
            WorkItemType::Evaluation,
            input,
            WorkItemOrigin::Evaluation,
            WorkItemWritebackTarget::TaskResult,
        )
    }

    /// 创建经验收集工作项
    pub fn experience_collection(
        task_id: TaskId,
        prompt: String,
        parent_task_id: Option<TaskId>,
        conversation: Vec<ConversationMessage>,
        tools: Vec<ToolDefinition>,
        governing_agent_id: AgentId,
    ) -> Self {
        let context = WorkItemContext {
            conversation: Some(conversation),
            tools,
            system_prompt: None,
        };
        let input = WorkItemInput { prompt, context };
        let mut wi = Self::new(
            task_id,
            WorkItemType::ExperienceCollection,
            input,
            WorkItemOrigin::ExperienceCollection,
            WorkItemWritebackTarget::ExperienceInbox,
        );
        wi.parent_task_id = parent_task_id;
        wi.governing_agent_id = Some(governing_agent_id);
        wi
    }

    /// 创建 profile 生成工作项
    ///
    /// `kind` 决定 system_prompt 内容（孵化 vs 更新）。
    /// 实际的 kind 与 exception_count 通过 ExperienceStore 临时字段传递到 completion 阶段，
    /// 因 WorkItem 结构不承载 profile 生成元数据。
    pub fn profile_generation(
        task_id: TaskId,
        prompt: String,
        conversation: Vec<ConversationMessage>,
        tools: Vec<ToolDefinition>,
        governing_agent_id: AgentId,
        _kind: crate::domain::ProfileGenerationKind,
    ) -> Self {
        let context = WorkItemContext {
            conversation: Some(conversation),
            tools,
            system_prompt: None,
        };
        let input = WorkItemInput { prompt, context };
        let mut wi = Self::new(
            task_id,
            WorkItemType::ProfileGeneration,
            input,
            WorkItemOrigin::ExperienceCollection,
            WorkItemWritebackTarget::ExperienceInbox,
        );
        wi.governing_agent_id = Some(governing_agent_id);
        wi
    }

    /// 创建 skill 更新工作项
    ///
    /// 具体的 `SkillUpdateContext` 由调用方作为独立 Component 注入到同一 entity，
    /// 不存储在 WorkItem 中。
    pub fn skill_update(
        task_id: TaskId,
        prompt: String,
        conversation: Vec<ConversationMessage>,
        tools: Vec<ToolDefinition>,
        governing_agent_id: AgentId,
    ) -> Self {
        let context = WorkItemContext {
            conversation: Some(conversation),
            tools,
            system_prompt: None,
        };
        let input = WorkItemInput { prompt, context };
        let mut wi = Self::new(
            task_id,
            WorkItemType::SkillUpdate,
            input,
            WorkItemOrigin::ExperienceCollection,
            WorkItemWritebackTarget::ExperienceInbox,
        );
        wi.governing_agent_id = Some(governing_agent_id);
        wi
    }

    /// 创建 skill 创建工作项
    ///
    /// 具体的 `SkillCreationContext` 由调用方作为独立 Component 注入到同一 entity，
    /// 不存储在 WorkItem 中。multi_turn = true：skill-creator 需要多轮工具调用。
    pub fn skill_creation(
        task_id: TaskId,
        prompt: String,
        conversation: Vec<ConversationMessage>,
        tools: Vec<ToolDefinition>,
        governing_agent_id: AgentId,
    ) -> Self {
        let context = WorkItemContext {
            conversation: Some(conversation),
            tools,
            system_prompt: None,
        };
        let input = WorkItemInput { prompt, context };
        let mut wi = Self::new(
            task_id,
            WorkItemType::SkillCreation,
            input,
            WorkItemOrigin::ExperienceCollection,
            WorkItemWritebackTarget::ExperienceInbox,
        );
        wi.governing_agent_id = Some(governing_agent_id);
        wi
    }

    /// 标记为已分配
    pub fn assign(&mut self, agent_id: AgentId) {
        self.assigned_agent = Some(agent_id);
        self.status = WorkItemStatus::Assigned;
    }

    /// 只读状态访问器。
    pub fn status(&self) -> WorkItemStatus {
        self.status
    }

    /// 只读分配 Agent 访问器。
    pub fn assigned_agent(&self) -> Option<AgentId> {
        self.assigned_agent
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
        let task_id = crate::domain::TaskId::nil();
        let work_item = WorkItem::execution(task_id, "test prompt".to_string());
        assert_eq!(work_item.work_type, WorkItemType::Execution);
        assert_eq!(work_item.status, WorkItemStatus::Pending);
        assert!(work_item.is_pending());
    }

    #[test]
    fn work_item_state_transitions() {
        let task_id = crate::domain::TaskId::nil();
        let mut work_item = WorkItem::execution(task_id, "test".to_string());

        assert!(work_item.is_pending());
        work_item.assign(crate::domain::AgentId::new());
        assert_eq!(work_item.status, WorkItemStatus::Assigned);

        work_item.start();
        assert_eq!(work_item.status, WorkItemStatus::Running);

        work_item.complete();
        assert!(work_item.is_completed());
        assert!(work_item.is_terminal());
    }

    #[test]
    fn work_item_summarization() {
        let task_id = crate::domain::TaskId::nil();
        let work_item = WorkItem::summarization(
            task_id,
            "content to summarize".to_string(),
            500,
            SummarizationTrigger::TaskComplete,
        );
        assert_eq!(work_item.work_type, WorkItemType::Summarization);
        assert_eq!(work_item.work_type.required_tag(), "summarization");
        // System prompt is now provided by agents.toml, not WorkItem
        assert!(work_item.input.context.system_prompt.is_none());
        // Verify prompt contains target_tokens value
        assert!(work_item.input.prompt.contains("500"));
    }

    #[test]
    fn work_item_evaluation_creation() {
        let task_id = crate::domain::TaskId::nil();
        let work_item = WorkItem::evaluation(
            task_id,
            "请评估当前任务状态".to_string(),
            Some("检查任务是否偏离目标".to_string()),
        );

        assert_eq!(work_item.work_type, WorkItemType::Evaluation);
        assert_eq!(work_item.origin, WorkItemOrigin::Evaluation);
        assert_eq!(
            work_item.writeback_target,
            WorkItemWritebackTarget::TaskResult
        );
        assert_eq!(work_item.work_type.required_tag(), "evaluation");

        // Verify the prompt contains reasoning hint
        assert!(work_item.input.prompt.contains("请评估当前任务状态"));
        assert!(work_item.input.prompt.contains("检查任务是否偏离目标"));
        assert!(work_item.input.prompt.contains("评估提示"));

        // Verify the system prompt is now provided by agents.toml
        assert!(work_item.input.context.system_prompt.is_none());
    }

    #[test]
    fn work_item_evaluation_without_reasoning_hint() {
        let task_id = crate::domain::TaskId::nil();
        let work_item = WorkItem::evaluation(task_id, "请评估当前任务状态".to_string(), None);

        // Verify the prompt is unchanged when no reasoning hint is provided
        assert_eq!(work_item.input.prompt, "请评估当前任务状态");

        // Verify the system prompt is now provided by agents.toml (not WorkItem)
        assert!(work_item.input.context.system_prompt.is_none());
    }

    #[test]
    fn work_item_experience_collection_creation() {
        use crate::domain::{ToolExecutorKind, ToolPermission, ToolSchema};

        let task_id = crate::domain::TaskId::nil();
        let parent_task_id = crate::domain::TaskId::new();
        let tool = ToolDefinition {
            name: "submit_experience_candidate".to_string(),
            description: "submit experience candidate".to_string(),
            parameters: ToolSchema::default(),
            default_permission: ToolPermission::Allow,
            executor: ToolExecutorKind::Builtin("submit_experience_candidate".to_string()),
            required_tag: None,
        };
        let work_item = WorkItem::experience_collection(
            task_id,
            "summarize what we learned".to_string(),
            Some(parent_task_id),
            vec![ConversationMessage::User {
                content: "user goal".to_string(),
            }],
            vec![tool],
            crate::domain::AgentId::new(),
        );

        assert_eq!(work_item.work_type, WorkItemType::ExperienceCollection);
        assert_eq!(work_item.origin, WorkItemOrigin::ExperienceCollection);
        assert_eq!(
            work_item.writeback_target,
            WorkItemWritebackTarget::ExperienceInbox
        );
        assert_eq!(work_item.work_type.required_tag(), "collect");
        // System prompt is now provided by agents.toml, not WorkItem
        assert!(work_item.input.context.system_prompt.is_none());
        assert_eq!(work_item.input.context.tools.len(), 1);
        assert_eq!(
            work_item.input.context.tools[0].name,
            "submit_experience_candidate"
        );
        assert!(work_item.input.context.conversation.is_some());
        assert_eq!(work_item.parent_task_id, Some(parent_task_id));
    }

    #[test]
    fn required_tag_evaluation() {
        assert_eq!(WorkItemType::Evaluation.required_tag(), "evaluation");
    }

    #[test]
    fn required_tag_summarization() {
        assert_eq!(WorkItemType::Summarization.required_tag(), "summarization");
    }

    #[test]
    fn required_tag_experience_collection() {
        assert_eq!(WorkItemType::ExperienceCollection.required_tag(), "collect");
    }

    #[test]
    fn required_tag_skill_update() {
        assert_eq!(WorkItemType::SkillUpdate.required_tag(), "skill-updater");
    }

    #[test]
    fn required_tag_profile_generation() {
        assert_eq!(WorkItemType::ProfileGeneration.required_tag(), "profile");
    }

    #[test]
    fn required_tag_execution() {
        assert_eq!(WorkItemType::Execution.required_tag(), "execution");
    }

    #[test]
    fn required_tag_skill_creation() {
        assert_eq!(WorkItemType::SkillCreation.required_tag(), "skill-creator");
    }
}
