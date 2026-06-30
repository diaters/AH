//! chat_with_agent 会话组件

use bevy::prelude::Component;
use uuid::Uuid;

/// 标记一个子任务为 chat_with_agent 对话型子任务，并保存每轮变化的状态。
#[derive(Component, Debug, Clone)]
pub struct ChatSession {
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
            parent_tool_call_id: "call_123".to_string(),
            current_batch_id: batch_id,
        };
        assert_eq!(session.parent_tool_call_id, "call_123");
        assert_eq!(session.current_batch_id, batch_id);
    }
}
