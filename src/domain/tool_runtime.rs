//! Tool 调用运行时类型定义
//!
//! 定义 Tool 调用状态、对话消息等。

use crate::prelude::Component;

use super::{AgentId, AgentRequestKind, ConversationMessage, TaskId, ToolDefinition};

/// 工具循环的收尾要求。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinalizeRequest {
    /// 收尾原因（写入合成结果与最终结果标注）。
    pub reason: String,
    /// 已因收尾要求拦截的轮次。
    pub rounds: u32,
}

impl FinalizeRequest {
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            rounds: 0,
        }
    }
}

/// Tool 调用循环状态
#[derive(Debug, Clone, Component)]
pub struct ToolCallingState {
    pub task_id: TaskId,
    pub agent_id: AgentId,
    /// 仍在等待执行结果的 LLM Tool 调用 ID 列表
    pub pending_tool_call_ids: Vec<String>,
    /// 当前迭代次数
    pub iteration: u32,
    /// 轮次预算：`None` 表示不受轮次预算约束（批次子任务，由 watchdog 监督进展）。
    pub max_iterations: Option<u32>,
    /// 收尾要求（watchdog finalize 或安全熔断）：之后的工具调用不再执行，
    /// 改为要求 LLM 总结交付；超过宽限轮次仍不收尾则判失败。
    pub finalize: Option<FinalizeRequest>,
    /// 累积的结构化对话历史
    pub conversation: Vec<ConversationMessage>,
    /// Agent 可用的 Tool 定义（后续请求需要重新发送）
    pub tools: Vec<ToolDefinition>,
    /// 原始请求类型（follow-up 请求需要保留）
    pub request_kind: AgentRequestKind,
    /// 关联的 WorkItem ID（仅治理型 WorkItem 使用）
    pub work_item_id: Option<uuid::Uuid>,
}
