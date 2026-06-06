//! 执行相关类型定义
//!
//! 定义 LLM 执行请求、响应、输出等。

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{AgentId, TaskId, ToolDefinition};

/// 执行请求类型
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum AgentRequestKind {
    LlmCompletion,
    BrainDecision,
    ToolExecution { tool_name: String },
    Summarization,
}

/// LLM 返回的 Tool 调用
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LlmToolCall {
    /// Tool 调用 ID（来自 LLM，如 "call_abc123"）
    pub id: String,
    /// LLM 请求调用的 Tool 名称
    pub name: String,
    /// LLM 传递的 JSON 参数字符串
    pub arguments: String,
}

/// LLM 执行输出
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentExecutionOutput {
    pub content: OutputContent,
    /// DeepSeek 等推理模型返回的思考内容，后续请求必须回传
    pub reasoning_content: Option<String>,
}

/// 输出内容
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum OutputContent {
    /// LLM 返回了文本响应
    Text(String),
    /// LLM 请求了一次或多次 Tool 调用
    ToolCalls(Vec<LlmToolCall>),
}

impl std::fmt::Display for AgentExecutionOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.content {
            OutputContent::Text(s) => write!(f, "{}", s),
            OutputContent::ToolCalls(calls) => {
                let names: Vec<&str> = calls.iter().map(|c| c.name.as_str()).collect();
                write!(f, "tool_calls: [{}]", names.join(", "))
            }
        }
    }
}

/// 结构化对话消息（用于 Tool 调用多轮对话）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ConversationMessage {
    System {
        content: String,
    },
    User {
        content: String,
    },
    Assistant {
        content: Option<String>,
        tool_calls: Vec<LlmToolCall>,
        /// DeepSeek 等推理模型的思考内容，后续请求必须回传
        reasoning_content: Option<String>,
    },
    Tool {
        tool_call_id: String,
        content: String,
    },
}

/// Agent 执行请求
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentExecutionRequest {
    pub task_id: TaskId,
    pub agent_id: AgentId,
    pub request_kind: AgentRequestKind,
    pub prompt: String,
    pub system_prompt: Option<String>,
    /// Agent 可用的 Tool 定义列表
    pub tools: Vec<ToolDefinition>,
    /// 结构化对话历史（后续请求使用，初始请求为 None）
    pub conversation: Option<Vec<ConversationMessage>>,
    /// 关联的 WorkItem ID；普通 Task 直发请求时为 None。
    pub work_item_id: Option<Uuid>,
}

/// Agent 执行结果
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentExecutionResult {
    pub task_id: TaskId,
    pub agent_id: AgentId,
    pub request_kind: AgentRequestKind,
    pub result: Result<AgentExecutionOutput, super::ExecutionError>,
    /// 原始 prompt（用于对话重建）
    pub prompt: String,
    /// 原始 system_prompt（用于对话重建）
    pub system_prompt: Option<String>,
    /// Agent 可用的 Tool 定义（用于 tool calling 循环重建）
    pub tools: Vec<ToolDefinition>,
    /// DeepSeek 等推理模型的思考内容，后续请求必须回传
    pub reasoning_content: Option<String>,
    /// 回传请求关联的 WorkItem ID。
    pub work_item_id: Option<Uuid>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_execution_request_carries_work_item_id() {
        let request = AgentExecutionRequest {
            task_id: uuid::Uuid::nil(),
            agent_id: uuid::Uuid::nil(),
            request_kind: AgentRequestKind::LlmCompletion,
            prompt: "test".to_string(),
            system_prompt: None,
            tools: vec![],
            conversation: None,
            work_item_id: Some(uuid::Uuid::new_v4()),
        };

        assert!(request.work_item_id.is_some());
    }
}
