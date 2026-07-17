//! Domain 层
//!
//! 定义项目的核心领域类型。

mod agent;
mod brain;
mod chat_session;
mod command;
mod confirmation;
mod contribution;
mod error;
mod evaluation;
mod execution;
mod frontend;
mod memory;
mod message;
mod model_chain;
mod session;
mod signal_trigger;
mod space;
mod summarization;
mod task;
mod task_experience;
mod tool_runtime;
mod work_item;
mod workflow;

use std::{future::Future, pin::Pin};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ============ 类型别名 ============

pub type TaskId = Uuid;
pub type AgentId = Uuid;
pub type ExecutorFuture =
    Pin<Box<dyn Future<Output = Result<AgentExecutionOutput, ExecutionError>> + Send>>;

// ============ 从子模块导出 ============

// agent
pub use agent::{
    Agent, AgentCapabilities, AgentKind, AgentProfile, AgentStoppingHookPending,
    AgentToolPermissions,
};

// chat_session
pub use chat_session::ChatSession;

// brain
pub use brain::{BrainDecisionError, BrainDecisionOutput};

// command
pub use command::UserCommand;

// confirmation
pub use confirmation::{ApprovalDecision, ConfirmationOption, ConfirmationSource, GrantMode};

// contribution
pub use contribution::{
    ExistingAgentProfile, ExperienceCandidate, ExperienceCandidatePayload,
    ExperienceCandidateStatus, ExperienceCandidateStatus as ExperienceStatus,
    ExperienceCollectionRequestMessage, ExperienceGovernanceDecision,
    ExperienceGovernanceRequestMessage, ExperienceInbox, ExperienceInboxStatus, ExperienceKindHint,
    ExperienceStore, ExperienceWritebackDestination, ExperienceWritebackRequestMessage,
    GeneratedProfile, IncubationProposal, IncubationProposalStatus, MAX_PROFILE_EXCEPTIONS,
    PendingExperienceHooks, ProfileGenerationCompletedMessage, ProfileGenerationContext,
    ProfileGenerationKind, ProfileGenerationRequestMessage, SkillFileRef, SkillFileRole,
    SkillUpdateCompletedMessage, SkillUpdateContext, SkillUpdateOperation, sanitize_tags,
};

// error
pub use error::{ExecutionError, FailureReason, ToolError};

// evaluation
pub use evaluation::{
    EvaluationDecision, EvaluationResult, EvaluationTrigger, OffTrackPolicy, TaskEvaluationConfig,
    parse_evaluation_result,
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
    EntryMetadata, EntryRole, ExecutableMemoryEntry, LongTermMemory, LongTermMemoryEntry,
    LtmEvictedHookPending, LtmWriteHookPending, MemoryEntry, MemoryImportance, MemorySnapshot,
    ShortTermMemory, ToolCall, estimate_tokens,
};

// message
pub use message::{
    AgentExecutionRequestMessage, AgentExecutionResultMessage, AgentSpawnRequestMessage,
    ApprovalRequestMessage, ApprovalRequestedHookPending, ApprovalResolvedHookPending,
    ApprovalResultMessage, ChatRoundReadyMessage, ChatRoundStartedMessage, ContinueTaskMessage,
    CreateTaskMessage, ExperienceCollectionCompletedMessage, ExternalInput, FinishTaskMessage,
    LlmResponseHookPending, MessageDispatchedHookPending, MessageReceivedHookPending,
    ModelChainStateUpdate, OutputKind, OutputMessage, PendingChannelSend, ReloadPluginsMessage,
    ReloadTriggersMessage, RetryReadyMessage, SessionExitedMessage, SessionOutputAppendedMessage,
    SessionStartedMessage, Signal, SignalPayload, SubTaskBatchCreatedMessage,
    SubTaskCompletedMessage, SummarizationRequestMessage, SystemOutputMessage,
    TaskTerminatedMessage, ToolConfirmationRequestMessage, ToolConfirmationResponseMessage,
    ToolExecutionRequestMessage, ToolExecutionResultMessage, TriggerTaskMessage, UserInputMessage,
    UserOutputMessage, WaitingReason,
};

// model_chain
pub use model_chain::{ModelChainEntry, ModelChainState, ProviderEntry, ProvidersConfig};

// signal_trigger
pub use signal_trigger::{EventTaskRoute, SignalSource, SignalTriggerRegistry, TaskTrigger};

// session
pub use session::{
    SessionBackendKind, SessionHandle, SessionHandleId, SessionInputRequest, SessionOutputSnapshot,
    SessionReadRequest, SessionStartRequest, SessionStatus, SessionSummary, ShellExecResult,
    ShellSessionResult,
};

// space
pub use space::{
    AgentToolsConfig, BuiltinTool, BuiltinToolExecutors, ExperienceCandidateSubmission,
    ExperienceConsolidationRequestMessage, KnowledgeSource, KnowledgeValidationStatus,
    PendingKnowledgeWriteHooks, SharedKnowledgeBase, SharedKnowledgeEntry, SpaceToolRegistry,
    ToolAction, ToolContext, ToolDefinition, ToolExecutorKind, ToolPermission, ToolSchema,
};

// summarization
pub use summarization::SummarizationTrigger;

// task
pub use task::{
    NewlyCreatedTask, Task, TaskRoutingPolicy, TaskStatus, ToolCalledHookPending,
    ToolReturnedHookPending, WaitingForSessionInfo, WaitingForTasksInfo,
};

// task_experience
pub use task_experience::{ExperienceKindFilter, TaskExperiencePolicy, TaskInjectedSkill};

// tool_runtime
pub use tool_runtime::ToolCallingState;

// work_item
pub use work_item::{
    WorkItem, WorkItemCompletedMessage, WorkItemContext, WorkItemCreatedMessage, WorkItemInput,
    WorkItemLifecycleHookPending, WorkItemOrigin, WorkItemStatus, WorkItemType,
    WorkItemWritebackTarget,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    pub agent: Vec<AgentEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentEntry {
    pub name: String,
    /// 向后兼容：单模型声明，自动生成单元素 models 链
    #[serde(default)]
    pub model: Option<String>,
    /// 有序模型链，第一个为最高优先级
    #[serde(default)]
    pub models: Vec<ModelChainEntry>,
    pub tags: Vec<String>,
    pub description: String,
    /// Tool 权限配置
    pub tools: Option<AgentToolsConfig>,
    /// Skill 路径列表
    #[serde(default)]
    pub skills: Option<Vec<String>>,
    /// Agent 级 system prompt：加载时注入 Agent 组件，WorkItem 执行时作为 system_prompt 传递给 LLM。
    /// 留空（None）时由 WorkItem 自身的 system_prompt 决定（保持向后兼容）。
    #[serde(default)]
    pub system_prompt: Option<String>,
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
            system_prompt: None,
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
            system_prompt: None,
        };

        assert!(!agent.has_permission("new_tool"));

        agent.grant_permission("new_tool".to_string());

        assert!(agent.has_permission("new_tool"));
    }

    #[test]
    fn agent_entry_backward_compat_single_model() {
        let toml_str = r#"
[[agent]]
name = "test-agent"
model = "gpt-4.1-mini"
tags = ["test"]
description = "test"
"#;
        let config: AgentConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.agent.len(), 1);
        assert_eq!(config.agent[0].model, Some("gpt-4.1-mini".to_string()));
        assert!(config.agent[0].models.is_empty());
    }

    #[test]
    fn agent_entry_with_models_chain() {
        let toml_str = r#"
[[agent]]
name = "test-agent"
tags = ["test"]
description = "test"

[[agent.models]]
provider = "openai"
model = "gpt-4.1-mini"

[[agent.models]]
provider = "deepseek"
model = "deepseek-chat"
"#;
        let config: AgentConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.agent.len(), 1);
        assert!(config.agent[0].model.is_none());
        assert_eq!(config.agent[0].models.len(), 2);
        assert_eq!(config.agent[0].models[0].provider, "openai");
        assert_eq!(config.agent[0].models[1].provider, "deepseek");
    }
}
