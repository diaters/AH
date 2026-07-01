//! 消息类型定义
//!
//! 定义 ECS 中使用的各种消息组件。

use bevy::prelude::{Component, Entity};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{AgentExecutionRequest, AgentExecutionResult, AgentId, SummarizationTrigger, TaskId};

// ============ 信号与输入 ============

/// 信号类型
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SignalType {
    UserInput,
    RetryWakeup,
    SystemWakeup,
}

/// 等待原因
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum WaitingReason {
    Agent,
    User,      // 等待用户输入
    Evaluator, // 等待评估器判定
    RetryBackoff,
    Approval,      // 等待审批
    Summarization, // 等待摘要完成
    ToolExecution, // 等待工具执行结果
    /// 等待一批子任务全部完成（create_tasks 工具调用后）
    SubTaskBatch {
        batch_id: Uuid,
    },
    /// 等待 shell 会话完成
    Session {
        handle_id: Uuid,
    },
    /// chat_with_agent 子任务等待父 Agent 下一轮调用
    ChatAgent,
}

/// 信号载荷
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SignalPayload {
    UserInput(String),
    RetryWakeup(TaskId),
    SystemWakeup,
}

/// 信号组件
#[derive(Debug, Clone, Component)]
pub struct Signal {
    pub kind: SignalType,
    pub payload: SignalPayload,
    pub origin_channel: super::ChannelId,
}

impl Signal {
    /// 构造用户输入信号（默认 Tui 通道）。
    pub fn user_input(content: impl Into<String>) -> Self {
        Self::user_input_with_channel(
            content,
            super::ChannelId {
                frontend: super::FrontendKind::Tui,
                user_id: "default".to_string(),
                thread_id: None,
            },
        )
    }

    pub fn user_input_with_channel(
        content: impl Into<String>,
        origin_channel: super::ChannelId,
    ) -> Self {
        Self {
            kind: SignalType::UserInput,
            payload: SignalPayload::UserInput(content.into()),
            origin_channel,
        }
    }

    /// 构造重试唤醒信号（origin_channel 使用 Tui 默认）。
    pub fn retry_wakeup(task_id: TaskId) -> Self {
        Self {
            kind: SignalType::RetryWakeup,
            payload: SignalPayload::RetryWakeup(task_id),
            origin_channel: super::ChannelId {
                frontend: super::FrontendKind::Tui,
                user_id: "default".to_string(),
                thread_id: None,
            },
        }
    }
}

/// 外部输入
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ExternalInput {
    TextWithChannel {
        channel: super::ChannelId,
        content: String,
    },
    Shutdown,
    /// Tool 确认响应
    Confirmation {
        request_id: Uuid,
        option: String,
    },
}

/// 用户输入消息
#[derive(Debug, Clone, Component)]
pub struct UserInputMessage {
    pub content: String,
    pub origin_channel: super::ChannelId,
}

/// 重试就绪消息
#[derive(Debug, Clone, Component)]
pub struct RetryReadyMessage {
    pub task_id: TaskId,
}

// ============ 执行请求/响应 ============

/// 标记刚派发、尚未触发 `on_message_dispatched` 观察 hook 的 `AgentExecutionRequestMessage`。
///
/// 由 `AgentExecutionRequestMessage` 的所有 spawn 点附带，由 companion 系统
/// `on_message_dispatched_hook_system` 派发 hook 后移除。
#[derive(Component, Debug, Clone, Default)]
pub struct MessageDispatchedHookPending;

/// 标记刚到达、尚未触发 `on_message_received` 观察 hook 的外部输入 entity。
///
/// 由 `input_ingress_system` 在 spawn `Signal::user_input` 或
/// `ToolConfirmationResponseMessage` 时附带，由 companion 系统
/// `on_message_received_hook_system` 派发 hook 后移除。
#[derive(Component, Debug, Clone, Default)]
pub struct MessageReceivedHookPending;

/// 标记刚接收、尚未触发 `on_llm_response` 观察 hook 的 `AgentExecutionResultMessage`。
///
/// 由 `ingest_execution_results_system` 在 spawn `AgentExecutionResultMessage` 时附带，
/// 由 companion 系统 `on_llm_response_hook_system` 派发 hook 后移除。
#[derive(Component, Debug, Clone, Default)]
pub struct LlmResponseHookPending;

/// 标记刚创建、尚未触发 `on_approval_requested` 观察 hook 的 `ApprovalRequestMessage`。
///
/// 由 `tool_dispatch_system` 在 spawn `ApprovalRequestMessage` 时附带，
/// 由 companion 系统 `on_approval_requested_hook_system` 派发 hook 后移除。
#[derive(Component, Debug, Clone, Default)]
pub struct ApprovalRequestedHookPending;

/// 标记刚产生、尚未触发 `on_approval_resolved` 观察 hook 的 `ApprovalResultMessage`。
///
/// 由 `approval_dispatch_system` 在 spawn `ApprovalResultMessage` 时附带，
/// 由 companion 系统 `on_approval_resolved_hook_system` 派发 hook 后移除。
#[derive(Component, Debug, Clone, Default)]
pub struct ApprovalResolvedHookPending;

/// Agent 执行请求消息
#[derive(Debug, Clone, Component)]
pub struct AgentExecutionRequestMessage {
    pub request: AgentExecutionRequest,
}

/// Agent 执行结果消息
#[derive(Debug, Clone, Component)]
pub struct AgentExecutionResultMessage {
    pub result: AgentExecutionResult,
}

/// 用户输出消息
#[derive(Debug, Clone, Component)]
pub struct UserOutputMessage {
    pub task_id: TaskId,
    pub content: String,
}

/// 系统输出消息
///
/// 用于向用户发送系统通知，不会进入 task 的 STM 上下文。
/// 例如：摘要完成通知、错误提示等。
#[derive(Debug, Clone, Component)]
pub struct SystemOutputMessage {
    /// 关联的任务 ID，用于路由到正确的 channel
    pub task_id: TaskId,
    /// 通知内容
    pub content: String,
}

// ============ 任务管理 ============

/// 创建新任务消息
#[derive(Debug, Clone, Component)]
pub struct CreateTaskMessage {
    pub content: String,
    pub origin_channel: super::ChannelId,
}

/// 继续现有任务消息
#[derive(Debug, Clone, Component)]
pub struct ContinueTaskMessage {
    pub task_id: TaskId,
    pub user_input: String,
}

/// 任务终止消息
#[derive(Debug, Clone, Component)]
pub struct TaskTerminatedMessage {
    pub task_id: TaskId,
}

/// /finish 命令触发的任务完成消息
#[derive(Debug, Clone, Component)]
pub struct FinishTaskMessage {
    pub task_id: TaskId,
}

// ============ Tool 执行 ============

/// Tool 执行请求消息
#[derive(Debug, Clone, Component)]
pub struct ToolExecutionRequestMessage {
    pub request: AgentExecutionRequest,
    pub tool_name: String,
    pub tool_input: serde_json::Value,
    /// 确认请求 ID（当工具需要确认时设置）
    pub pending_confirmation_id: Option<Uuid>,
    /// LLM Tool 调用 ID（用于结果匹配，非 LLM 发起的为 None）
    pub tool_call_id: Option<String>,
    /// 确认请求的选项列表（用于匹配用户响应，避免硬编码 default_options）
    pub pending_confirmation_options: Option<Vec<super::ConfirmationOption>>,
}

/// Tool 执行结果消息
#[derive(Debug, Clone, Component)]
pub struct ToolExecutionResultMessage {
    pub result: AgentExecutionResult,
    pub tool_name: String,
    pub tool_output: Result<serde_json::Value, super::ToolError>,
    /// LLM Tool 调用 ID（从请求传递到结果，用于匹配）
    pub tool_call_id: Option<String>,
    /// 是否已被 tool_result_system 处理过，防止重复记录日志和 STM
    pub processed: bool,
    /// 审计字段：插件通过 `tool_set_result` 替换 `tool_output` 时，
    /// 原始输出值保留在此。仅当 `on_tool_returned` hook 触发过替换时为 `Some`。
    pub original_tool_output: Option<serde_json::Value>,
}

// ============ Session 生命周期 ============

/// Session 启动消息
#[derive(Debug, Clone, Component)]
pub struct SessionStartedMessage {
    pub handle_id: Uuid,
}

/// Session 退出消息
#[derive(Debug, Clone, Component)]
pub struct SessionExitedMessage {
    pub handle_id: Uuid,
}

/// Session 输出追加消息
#[derive(Debug, Clone, Component)]
pub struct SessionOutputAppendedMessage {
    pub handle_id: Uuid,
    pub content: String,
}

// ============ 审批与确认 ============

/// Tool 确认请求消息
#[derive(Debug, Clone, Component)]
pub struct ToolConfirmationRequestMessage {
    pub request_id: Uuid,
    pub task_id: TaskId,
    pub agent_id: AgentId,
    pub tool_name: String,
    pub tool_input: serde_json::Value,
    pub options: Vec<super::ConfirmationOption>,
    /// 审批来源
    pub source: super::ConfirmationSource,
    /// 父 Agent ID（当 source == ParentAgent 时）
    pub parent_agent_id: Option<AgentId>,
}

/// Tool 确认响应消息
#[derive(Debug, Clone, Component)]
pub struct ToolConfirmationResponseMessage {
    pub request_id: Uuid,
    pub selected_option: String,
}

/// 审批请求消息
#[derive(Debug, Clone, Component)]
pub struct ApprovalRequestMessage {
    pub request_id: Uuid,
    pub source_task_id: TaskId,
    pub approval_task_id: TaskId,
    pub parent_agent_id: AgentId,
    pub child_agent_id: AgentId,
    pub tool_name: String,
    pub tool_input: serde_json::Value,
    pub context: String,
}

/// 审批结果消息
#[derive(Debug, Clone, Component)]
pub struct ApprovalResultMessage {
    pub request_id: Uuid,
    pub source_task_id: TaskId,
    pub approval_task_id: TaskId,
    pub decision: super::ApprovalDecision,
    pub reasoning: String,
    /// 授权模式
    pub grant_mode: super::GrantMode,
}

// ============ Agent 管理 ============

/// Agent 创建请求消息
#[derive(Debug, Clone, Component)]
pub struct AgentSpawnRequestMessage {
    pub parent_agent_id: AgentId,
    pub task_id: TaskId,
    pub name: String,
    /// 可选，None 时继承父 Agent 的 model
    pub model: Option<String>,
    pub description: String,
    /// 初始 Tool 权限列表（每个 Tool 设为 Allow）
    pub tools: Vec<String>,
    /// 子任务的 prompt 内容
    pub task_prompt: String,
    /// 子任务的 system prompt（可选）
    pub task_system_prompt: Option<String>,
}

// ============ 子任务批次 ============

/// create_tasks 工具调用后产出，触发父 Task 阻塞 + Brain 分发
#[derive(Debug, Clone, Component)]
pub struct SubTaskBatchCreatedMessage {
    pub parent_task_id: TaskId,
    pub batch_id: Uuid,
    pub parent_tool_call_id: String,
    pub tasks: Vec<super::SubTaskDefinition>,
}

/// 单个子任务完成时产出，用于更新 BatchState 并检查是否全部完成
#[derive(Debug, Clone, Component)]
pub struct SubTaskCompletedMessage {
    pub parent_task_id: TaskId,
    pub batch_id: Uuid,
    pub child_task_id: TaskId,
    pub child_task_name: String,
    pub result_summary: String,
    pub success: bool,
}

/// chat_with_agent 新一轮开始，触发父 Task 阻塞
#[derive(Debug, Clone, Component)]
pub struct ChatRoundStartedMessage {
    pub parent_task_id: TaskId,
    pub child_task_id: TaskId,
    pub batch_id: Uuid,
    pub parent_tool_call_id: String,
}

/// chat_with_agent 子任务本轮回复就绪
#[derive(Debug, Clone, Component)]
pub struct ChatRoundReadyMessage {
    pub child_task_id: TaskId,
    pub parent_task_id: TaskId,
    pub parent_agent_id: AgentId,
    pub batch_id: Uuid,
    pub parent_tool_call_id: String,
    pub response: String,
    /// 目标对话 Agent 名称（用于工具返回值中的 `agent` 字段）
    pub child_agent_name: String,
}

// ============ 摘要 ============

/// 摘要请求消息
#[derive(Debug, Clone, Component)]
pub struct SummarizationRequestMessage {
    /// 关联的任务 ID
    pub task_id: TaskId,
    /// 待压缩的内容
    pub content_to_summarize: String,
    /// 目标 token 数
    pub target_tokens: u32,
    /// 摘要触发来源
    pub trigger: SummarizationTrigger,
}

// ============ 输出类型 ============

/// 输出类型
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum OutputKind {
    /// 普通文本输出
    #[default]
    Text,
    /// Tool 确认请求
    ConfirmationRequest {
        request_id: Uuid,
        title: String,
        options: Vec<super::ConfirmationOption>,
    },
}

/// 输出消息
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OutputMessage {
    pub content: String,
    pub kind: OutputKind,
}

impl OutputMessage {
    /// 构造普通文本输出消息。
    pub fn new(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            kind: OutputKind::Text,
        }
    }

    /// 构造确认请求输出消息。
    pub fn confirmation_request(
        request_id: Uuid,
        title: impl Into<String>,
        options: Vec<super::ConfirmationOption>,
    ) -> Self {
        Self {
            content: String::new(),
            kind: OutputKind::ConfirmationRequest {
                request_id,
                title: title.into(),
                options,
            },
        }
    }
}

/// 经验收集完成消息：WorkItem 完成后触发汇聚与治理。
#[derive(Debug, Clone, Component)]
pub struct ExperienceCollectionCompletedMessage {
    pub task_id: TaskId,
    pub parent_task_id: Option<TaskId>,
    pub agent_id: AgentId,
    /// 原任务治理者，由请求链路显式传递。
    pub governing_agent_id: AgentId,
}

/// /reload-plugins 触发的重载请求消息。
///
/// `command_parse_system` 使用 Commands 无法直接获取 `&mut World`，
/// 因此 spawn 此消息实体，由 `reload_plugins_system` 消费后执行重载。
#[derive(Debug, Clone, Component)]
pub struct ReloadPluginsMessage;

/// 待发送的通道消息，由 channel_send_dispatch_system 消费。
#[derive(Debug, Clone, Component)]
pub struct PendingChannelSend {
    pub channel: String,
    /// 显式指定的目标；为 None 时由 dispatch 系统回退到 task 的 origin_channel。
    pub recipient: Option<String>,
    pub content: String,
    pub attachments: Vec<crate::channels::ChannelAttachment>,
    pub tool_call_id: Option<String>,
    pub task_id: TaskId,
    pub agent_id: AgentId,
    pub request_entity: Entity,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{ChannelId, FrontendKind};

    #[test]
    fn signal_user_input_carries_default_channel() {
        let signal = Signal::user_input("hi");
        assert_eq!(signal.origin_channel.frontend, FrontendKind::Tui);
    }

    #[test]
    fn signal_user_input_with_channel_preserves_channel() {
        let channel = ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "u1".to_string(),
            thread_id: None,
        };
        let signal = Signal::user_input_with_channel("hi", channel.clone());
        assert_eq!(signal.origin_channel, channel);
    }
}
