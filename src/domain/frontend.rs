use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{AgentId, TaskId};

/// 前端类型
#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub enum FrontendKind {
    Tui,
    Telegram,
    Web,
}

/// 标识一个前端通道中的具体用户
#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct ChannelId {
    pub frontend: FrontendKind,
    pub user_id: String,
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
    },
    /// 审批请求
    ApprovalRequest {
        target: EventTarget,
        request_id: Uuid,
        agent_name: String,
        tool_name: String,
        tool_input: serde_json::Value,
        options: Vec<ApprovalOption>,
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
        result: Option<String>,
    },
    /// 子任务批次进度
    BatchProgress {
        target: EventTarget,
        batch_id: Uuid,
        completed: usize,
        total: usize,
    },
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
