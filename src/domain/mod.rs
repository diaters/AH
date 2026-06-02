//! Domain 层
//!
//! 定义项目的核心领域类型。

mod agent;
mod brain;
mod command;
mod confirmation;
mod contribution;
mod error;
mod evaluation;
mod execution;
mod frontend;
mod memory;
mod message;
mod space;
mod summarization;
mod task;
mod tool_runtime;
mod work_item;
mod workflow;

use std::{future::Future, pin::Pin};

use serde::Deserialize;
use uuid::Uuid;

// ============ 类型别名 ============

pub type TaskId = Uuid;
pub type AgentId = Uuid;
pub type ExecutorFuture =
    Pin<Box<dyn Future<Output = Result<AgentExecutionOutput, ExecutionError>> + Send>>;

// ============ 从子模块导出 ============

// agent
pub use agent::{
    Agent, AgentCapabilities, AgentExperience, AgentKind, AgentProfile, AgentToolPermissions,
};

// brain
pub use brain::{BrainDecisionError, BrainDecisionOutput};

// command
pub use command::UserCommand;

// confirmation
pub use confirmation::{ApprovalDecision, ConfirmationOption, ConfirmationSource, GrantMode};

// contribution
pub use contribution::{
    AbsorbedMemory, ContributionEvaluation, DiscardedMemory, MemoryAbsorptionMessage,
    MemoryContributionRequestMessage, TaskSummary,
};

// error
pub use error::{ExecutionError, FailureReason, ToolError};

// evaluation
pub use evaluation::{
    EvaluationDecision, EvaluationRequestMessage, EvaluationResult, EvaluationResultMessage,
    EvaluationTrigger, OffTrackPolicy, TaskEvaluationConfig,
};

// execution
pub use execution::{
    AgentExecutionOutput, AgentExecutionRequest, AgentExecutionResult, AgentRequestKind,
    ConversationMessage, LlmToolCall, OutputContent,
};

// frontend
pub use frontend::{
    AgentStatusKind, ApprovalOption, ChannelId, EngineEvent, EventTarget, Frontend, FrontendKind,
    MessageRole, TaskStatusKind, UserAction,
};

// memory
pub use memory::{
    EntryMetadata, EntryRole, LongTermMemory, MemoryEntry, ShortTermMemory, ToolCall,
    estimate_tokens,
};

// message
pub use message::{
    AgentExecutionRequestMessage, AgentExecutionResultMessage, AgentSpawnRequestMessage,
    ApprovalRequestMessage, ApprovalResultMessage, ContinueTaskMessage, CreateTaskMessage,
    ExternalInput, FinishTaskMessage, OutputKind, OutputMessage, RetryReadyMessage, Signal,
    SignalPayload, SignalType, SubTaskBatchCreatedMessage, SubTaskCompletedMessage,
    SummarizationRequestMessage, SummarizationResultMessage, SystemOutputMessage,
    TaskTerminatedMessage, ToolConfirmationRequestMessage, ToolConfirmationResponseMessage,
    ToolExecutionRequestMessage, ToolExecutionResultMessage, UserInputMessage, UserOutputMessage,
    WaitingReason,
};

// space
pub use space::{
    AgentToolsConfig, BuiltinTool, BuiltinToolExecutors, PersistentAgentConfig, SpaceAgentRegistry,
    SpaceKnowledge, SpacePreferences, SpaceRuntimeContext, SpaceToolRegistry, SystemStatus,
    ToolAction, ToolContext, ToolDefinition, ToolExecutorKind, ToolPermission, ToolSchema,
};

// summarization
pub use summarization::SummarizationTrigger;

// task
pub use task::{Task, TaskStatus, WaitingForTasksInfo};

// tool_runtime
pub use tool_runtime::ToolCallingState;

// work_item
pub use work_item::{
    WorkItem, WorkItemCompletedMessage, WorkItemContext, WorkItemCreatedMessage, WorkItemInput,
    WorkItemOrigin, WorkItemStatus, WorkItemType, WorkItemWritebackTarget,
};

// workflow
pub use workflow::{
    BatchTaskState, BatchTaskStatus, SubTaskBatchState, SubTaskConfig, SubTaskDefinition,
};

// ============ AgentExecutor trait ============

pub trait AgentExecutor: Send + Sync {
    /// 执行一次 Agent 请求并返回异步结果。
    fn execute(&self, request: AgentExecutionRequest) -> ExecutorFuture;
}

// ============ 配置类型（保留在 mod.rs） ============

#[derive(Debug, Clone, Deserialize)]
pub struct AgentConfig {
    pub agent: Vec<AgentEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AgentEntry {
    pub name: String,
    pub model: String,
    pub tags: Vec<String>,
    pub description: String,
    /// Tool 权限配置
    pub tools: Option<AgentToolsConfig>,
}

// ============ 测试 ============

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waiting_reason_has_user_and_evaluator() {
        use WaitingReason::*;
        let _ = User;
        let _ = Evaluator;
    }

    #[test]
    fn agent_has_permission_returns_true_for_allow() {
        let mut perms = AgentToolPermissions::default();
        perms
            .overrides
            .insert("test_tool".to_string(), ToolPermission::Allow);

        let agent = Agent {
            id: Uuid::nil(),
            profile: AgentProfile {
                name: "test".to_string(),
                model: "test-model".to_string(),
            },
            capabilities: AgentCapabilities {
                tags: vec![],
                description: "test".to_string(),
            },
            kind: AgentKind::Persistent,
            parent_id: None,
            bound_task_id: None,
            tool_permissions: perms,
            experience: AgentExperience::default(),
        };

        assert!(agent.has_permission("test_tool"));
        assert!(!agent.has_permission("other_tool"));
    }

    #[test]
    fn agent_grant_permission_updates_overrides() {
        let mut agent = Agent {
            id: Uuid::nil(),
            profile: AgentProfile {
                name: "test".to_string(),
                model: "test-model".to_string(),
            },
            capabilities: AgentCapabilities {
                tags: vec![],
                description: "test".to_string(),
            },
            kind: AgentKind::Persistent,
            parent_id: None,
            bound_task_id: None,
            tool_permissions: AgentToolPermissions::default(),
            experience: AgentExperience::default(),
        };

        assert!(!agent.has_permission("new_tool"));

        agent.grant_permission("new_tool".to_string());

        assert!(agent.has_permission("new_tool"));
    }
}
