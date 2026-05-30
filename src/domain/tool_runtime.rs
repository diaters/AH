//! Tool 调用运行时类型定义
//!
//! 定义 Tool 调用状态、对话消息等。

use bevy::prelude::Component;

use super::{AgentId, TaskId, ConversationMessage, ToolDefinition};

/// Tool 调用循环状态
#[derive(Debug, Clone, Component)]
pub struct ToolCallingState {
    pub task_id: TaskId,
    pub agent_id: AgentId,
    /// 仍在等待执行结果的 LLM Tool 调用 ID 列表
    pub pending_tool_call_ids: Vec<String>,
    /// 当前迭代次数
    pub iteration: u32,
    /// 最大迭代次数
    pub max_iterations: u32,
    /// 累积的结构化对话历史
    pub conversation: Vec<ConversationMessage>,
    /// Agent 可用的 Tool 定义（后续请求需要重新发送）
    pub tools: Vec<ToolDefinition>,
}
