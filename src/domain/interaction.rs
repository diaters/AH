//! 结构化交互保留目标身份，不把时效控制降级成会话文本。
use super::{ChannelId, TaskId};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TaskInteraction {
    Stop {
        task_id: TaskId,
        expected_revision: String,
    },
    Finish {
        task_id: TaskId,
        expected_revision: String,
    },
    Answer {
        task_id: TaskId,
        question_id: String,
        answer: String,
    },
}

#[derive(Debug, Clone, crate::prelude::Component)]
pub struct InteractionRequest {
    pub channel: ChannelId,
    pub command_id: Uuid,
    pub interaction: TaskInteraction,
}
