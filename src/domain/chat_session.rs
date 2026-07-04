//! chat_with_agent 会话组件

use crate::prelude::Component;
use uuid::Uuid;

/// 标记一个子任务为 chat_with_agent 对话型子任务，并保存每轮变化的状态。
#[derive(Component, Debug, Clone)]
pub struct ChatSession {
    /// 目标对话 Agent 名称（创建时设置，不变）
    pub child_agent_name: String,
    /// 本轮父任务的 tool_call_id（每轮更新）
    pub parent_tool_call_id: String,
    /// 本轮父任务等待用的 batch_id（每轮更新）
    pub current_batch_id: Uuid,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_session_stores_round_state() {
        let batch_id = Uuid::new_v4();
        let session = ChatSession {
            child_agent_name: "reviewer".to_string(),
            parent_tool_call_id: "call_123".to_string(),
            current_batch_id: batch_id,
        };
        assert_eq!(session.child_agent_name, "reviewer");
        assert_eq!(session.parent_tool_call_id, "call_123");
        assert_eq!(session.current_batch_id, batch_id);
    }
}
