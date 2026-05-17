use bevy::prelude::{Component, Resource};
use serde::{Deserialize, Serialize};

use super::{AgentId, TaskId};

/// 评估触发条件
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvaluationTrigger {
    AgentRequested,
    TurnLimitReached,
    UserRequested,
}

/// 评估请求消息
#[derive(Debug, Clone, Component)]
pub struct EvaluationRequestMessage {
    pub task_id: TaskId,
    pub trigger: EvaluationTrigger,
    pub agent_id: AgentId,
}

/// 评估结果消息
#[derive(Debug, Clone, Component)]
pub struct EvaluationResultMessage {
    pub task_id: TaskId,
    pub result: EvaluationResult,
}

/// 评估结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvaluationResult {
    pub decision: EvaluationDecision,
    pub reasoning: String,
    pub suggested_action: Option<String>,
}

/// 评估决策
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum EvaluationDecision {
    Continue,
    Complete,
    Failed,
    OffTrack,
}

/// 任务评估配置
#[derive(Debug, Clone, Resource)]
pub struct TaskEvaluationConfig {
    pub enabled: bool,
    pub max_turns: Option<u32>,
    pub evaluator_agent_name: String,
    pub offtrack_policy: OffTrackPolicy,
}

impl Default for TaskEvaluationConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_turns: None,
            evaluator_agent_name: "evaluator".to_string(),
            offtrack_policy: OffTrackPolicy::AskUser,
        }
    }
}

/// 偏离处理策略
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OffTrackPolicy {
    AutoCorrect,
    AskUser,
    Fail,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evaluation_trigger_variants_exist() {
        let _ = EvaluationTrigger::AgentRequested;
        let _ = EvaluationTrigger::TurnLimitReached;
        let _ = EvaluationTrigger::UserRequested;
    }

    #[test]
    fn evaluation_decision_variants_exist() {
        let _ = EvaluationDecision::Continue;
        let _ = EvaluationDecision::Complete;
        let _ = EvaluationDecision::Failed;
        let _ = EvaluationDecision::OffTrack;
    }

    #[test]
    fn task_evaluation_config_default() {
        let config = TaskEvaluationConfig::default();
        assert!(!config.enabled);
        assert!(config.max_turns.is_none());
        assert_eq!(config.evaluator_agent_name, "evaluator");
        assert_eq!(config.offtrack_policy, OffTrackPolicy::AskUser);
    }

    #[test]
    fn off_track_policy_variants_exist() {
        let _ = OffTrackPolicy::AutoCorrect;
        let _ = OffTrackPolicy::AskUser;
        let _ = OffTrackPolicy::Fail;
    }
}
