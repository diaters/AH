use bevy::prelude::Component;
use serde::{Deserialize, Serialize};

use super::{AgentId, LongTermMemoryEntry, TaskId};

/// 记忆贡献请求消息
#[derive(Debug, Clone, Component)]
pub struct MemoryContributionRequestMessage {
    pub contributor_id: AgentId,
    pub contributor_name: String,
    pub parent_id: AgentId,
    pub memories: Vec<LongTermMemoryEntry>,
    pub task_summary: TaskSummary,
}

/// 任务摘要
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSummary {
    pub task_id: TaskId,
    pub goal: String,
    pub outcome: String,
}

/// 贡献评估结果（LLM 返回）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContributionEvaluation {
    pub absorb: Vec<AbsorbedMemory>,
    pub discard: Vec<DiscardedMemory>,
}

/// 被吸收的记忆
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AbsorbedMemory {
    pub content: String,
    pub reason: String,
}

/// 被丢弃的记忆
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscardedMemory {
    pub content: String,
    pub reason: String,
}

/// 记忆吸收消息（内部使用）
#[derive(Debug, Clone, Component)]
pub struct MemoryAbsorptionMessage {
    pub parent_id: AgentId,
    pub absorbed: Vec<LongTermMemoryEntry>,
}
