use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{AgentId, TaskId};

/// 前端类型
#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FrontendKind {
    Tui,
    Telegram,
    Web,
    QQ,
    Feishu,
}

/// 标识一个前端通道中的具体用户
#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct ChannelId {
    pub frontend: FrontendKind,
    pub user_id: String,
    pub thread_id: Option<String>,
}

impl ChannelId {
    /// 返回用于注入 LLM prompt 的通道上下文字符串。
    pub fn to_prompt_context(&self) -> String {
        let channel_name = match self.frontend {
            FrontendKind::Tui => "tui",
            FrontendKind::Telegram => "telegram",
            FrontendKind::Web => "web",
            FrontendKind::QQ => "qq",
            FrontendKind::Feishu => "feishu",
        };
        let thread_hint = self
            .thread_id
            .as_deref()
            .map(|t| format!(", thread_id={t}"))
            .unwrap_or_default();
        format!(
            "[Current channel]\nchannel={channel_name}, chat_id={user_id}{thread_hint}\n\nWhen the user asks to send a file or message back, use the `channel_send` tool with channel='{channel_name}' and omit the target; include the file as [DOCUMENT:path] or [IMAGE:path] or [VIDEO:path].",
            user_id = self.user_id
        )
    }
}

/// 事件路由目标
#[derive(Debug, Clone)]
pub enum EventTarget {
    /// 广播：所有前端的所有用户
    Broadcast,
    /// 定向：发送给指定的一个或多个通道用户
    Directed(Vec<ChannelId>),
}

/// 消息角色
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageRole {
    User,
    Agent,
    System,
}

/// Agent 状态种类（用于前端展示）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentStatusKind {
    Idle,
    Running,
    WaitingApproval,
    WaitingTool,
}

/// Task 状态种类（用于前端展示）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStatusKind {
    Pending,
    Running,
    Waiting,
    Done,
    Failed,
}

/// 等待原因（前端展示用，精简版）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitingReasonKind {
    Agent,
    Tool,
    User,
    Retry,
    Other,
}

/// 审批选项
#[derive(Debug, Clone)]
pub struct ApprovalOption {
    pub id: String,
    pub label: String,
    pub description: String,
}

/// 引擎 → 前端事件
#[derive(Debug, Clone)]
pub enum EngineEvent {
    /// 用户可见文本（Agent 回复、系统消息）
    Text {
        target: EventTarget,
        role: MessageRole,
        content: String,
        task_id: Option<TaskId>,
    },
    /// 审批请求
    ApprovalRequest {
        target: EventTarget,
        request_id: Uuid,
        agent_name: String,
        tool_name: String,
        tool_input: serde_json::Value,
        options: Vec<ApprovalOption>,
        approval_context: Option<String>,
    },
    /// 审批结果
    ApprovalResult {
        target: EventTarget,
        request_id: Uuid,
        decision: String,
    },
    /// Agent 状态变化
    AgentStatusChanged {
        target: EventTarget,
        agent_id: AgentId,
        name: String,
        status: AgentStatusKind,
    },
    /// Task 状态变化
    TaskStatusChanged {
        target: EventTarget,
        task_id: TaskId,
        name: String,
        status: TaskStatusKind,
        old_status: Option<TaskStatusKind>,
        result: Option<String>,
        parent_id: Option<TaskId>,
        /// 任务来源的前端通道，事件任务为 None
        origin_channel: Option<ChannelId>,
        /// 被指派 agent 的名称，无 delegate 时为 None
        agent_name: Option<String>,
        /// 等待原因，仅当 status 为 Waiting 时有意义
        waiting_reason: Option<WaitingReasonKind>,
    },
    /// 子任务批次进度
    BatchProgress {
        target: EventTarget,
        batch_id: Uuid,
        completed: usize,
        total: usize,
    },
    /// 工具调用开始（不含结果）
    ToolCallStarted {
        target: EventTarget,
        task_id: TaskId,
        agent_name: String,
        tool_name: String,
        tool_input_summary: String,
    },
}

/// 生成工具调用的输入摘要（用于前端展示，避免长参数刷屏）
pub fn summarize_tool_input(tool_name: &str, tool_input: &serde_json::Value) -> String {
    match tool_name {
        "shell_exec" | "shell_start" => tool_input
            .get("command")
            .and_then(|v| v.as_str())
            .map(|s| {
                if s.chars().count() > 80 {
                    let truncated: String = s.chars().take(80).collect();
                    format!("{truncated}…")
                } else {
                    s.to_string()
                }
            })
            .unwrap_or_default(),
        "channel_send" => {
            let channel = tool_input
                .get("channel")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let content = tool_input
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let content_preview = if content.chars().count() > 50 {
                let truncated: String = content.chars().take(50).collect();
                format!("{truncated}…")
            } else {
                content.to_string()
            };
            format!("channel={channel} content={content_preview}")
        }
        "create_tasks" => tool_input
            .get("tasks")
            .and_then(|v| v.as_array())
            .map(|arr| format!("{} 个子任务", arr.len()))
            .unwrap_or_default(),
        "wait_tasks" => tool_input
            .get("task_ids")
            .and_then(|v| v.as_array())
            .map(|arr| format!("等待 {} 个任务", arr.len()))
            .unwrap_or_default(),
        _ => {
            let s = serde_json::to_string(tool_input).unwrap_or_default();
            if s.chars().count() > 100 {
                let truncated: String = s.chars().take(100).collect();
                format!("{truncated}…")
            } else {
                s
            }
        }
    }
}

impl EngineEvent {
    /// 获取事件的路由目标
    pub fn target(&self) -> &EventTarget {
        match self {
            Self::Text { target, .. } => target,
            Self::ApprovalRequest { target, .. } => target,
            Self::ApprovalResult { target, .. } => target,
            Self::AgentStatusChanged { target, .. } => target,
            Self::TaskStatusChanged { target, .. } => target,
            Self::BatchProgress { target, .. } => target,
            Self::ToolCallStarted { target, .. } => target,
        }
    }
}

/// 前端 → 引擎动作
#[derive(Debug, Clone)]
pub enum UserAction {
    /// 用户发送文本消息
    Text { channel: ChannelId, content: String },
    /// 用户响应审批请求
    Confirmation {
        channel: ChannelId,
        request_id: Uuid,
        option_id: String,
        /// 拒绝并反馈场景：用户评审反馈文本。
        feedback: Option<String>,
    },
}

/// 前端 trait — 引擎只依赖这个接口
pub trait Frontend: Send + Sync + 'static {
    /// 该前端的类型
    fn kind(&self) -> FrontendKind;

    /// 推送引擎事件，前端自行过滤路由目标
    fn push_event(&self, event: EngineEvent);

    /// 拉取待处理的用户动作（ECS 每帧调用）
    fn poll_actions(&self) -> Vec<UserAction>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarize_shell_exec_command() {
        let input = serde_json::json!({"command": "ls -la"});
        assert_eq!(summarize_tool_input("shell_exec", &input), "ls -la");
    }

    #[test]
    fn summarize_shell_exec_long_command_truncated() {
        let long_command = "a".repeat(100);
        let input = serde_json::json!({"command": long_command});
        let result = summarize_tool_input("shell_exec", &input);
        assert!(result.ends_with('…'));
        assert_eq!(result.chars().count(), 81);
    }

    #[test]
    fn summarize_channel_send() {
        let input = serde_json::json!({"channel": "qq", "content": "hello"});
        assert_eq!(
            summarize_tool_input("channel_send", &input),
            "channel=qq content=hello"
        );
    }

    #[test]
    fn summarize_create_tasks() {
        let input = serde_json::json!({"tasks": [{"goal": "a"}, {"goal": "b"}]});
        assert_eq!(summarize_tool_input("create_tasks", &input), "2 个子任务");
    }

    #[test]
    fn summarize_wait_tasks() {
        let input = serde_json::json!({"task_ids": ["id1", "id2", "id3"]});
        assert_eq!(summarize_tool_input("wait_tasks", &input), "等待 3 个任务");
    }

    #[test]
    fn summarize_unknown_tool_fallback_json() {
        let input = serde_json::json!({"key": "value"});
        let result = summarize_tool_input("unknown_tool", &input);
        assert!(result.contains("\"key\""));
    }

    #[test]
    fn summarize_missing_field_returns_empty() {
        let input = serde_json::json!({});
        assert_eq!(summarize_tool_input("shell_exec", &input), "");
    }

    #[test]
    fn summarize_shell_exec_chinese_command_truncated() {
        let long_command = "中".repeat(100);
        let input = serde_json::json!({"command": long_command});
        let result = summarize_tool_input("shell_exec", &input);
        assert!(result.ends_with('…'));
        assert_eq!(result.chars().count(), 81);
    }

    #[test]
    fn summarize_channel_send_long_content_truncated() {
        let long_content = "文".repeat(60);
        let input = serde_json::json!({"channel": "qq", "content": long_content});
        let result = summarize_tool_input("channel_send", &input);
        assert!(result.ends_with('…'));
        let content_part = result.split(" content=").nth(1).expect("应包含 content 段");
        assert_eq!(content_part.chars().count(), 51);
    }

    #[test]
    fn summarize_unknown_tool_long_json_truncated() {
        let long_value = "字".repeat(120);
        let input = serde_json::json!({"key": long_value});
        let result = summarize_tool_input("unknown_tool", &input);
        assert!(result.ends_with('…'));
    }
}
