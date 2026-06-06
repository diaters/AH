use bevy::prelude::Resource;
use serde::{Deserialize, Serialize};

/// 评估触发条件
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvaluationTrigger {
    AgentRequested,
    TurnLimitReached,
    UserRequested,
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
#[derive(Debug, Clone, Resource, Deserialize)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum OffTrackPolicy {
    AutoCorrect,
    AskUser,
    Fail,
}

/// 解析评估结果 JSON
///
/// 支持直接 JSON 或 markdown 代码块包裹的 JSON。
pub fn parse_evaluation_result(content: &str) -> Result<EvaluationResult, String> {
    let json_slice = if content.contains("```json") {
        content
            .split("```json")
            .nth(1)
            .and_then(|s| s.split("```").next())
            .map(str::trim)
            .unwrap_or(content)
    } else {
        content.trim()
    };

    serde_json::from_str(json_slice).map_err(|e| e.to_string())
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

    #[test]
    fn parse_evaluation_json_from_text() {
        let text =
            r#"{"decision":"Continue","reasoning":"still progressing","suggested_action":null}"#;
        let parsed = parse_evaluation_result(text).unwrap();
        assert_eq!(parsed.decision, EvaluationDecision::Continue);
        assert_eq!(parsed.reasoning, "still progressing");
        assert!(parsed.suggested_action.is_none());
    }

    #[test]
    fn parse_evaluation_json_from_markdown_code_block() {
        let text = r#"Some text before
```json
{"decision":"Complete","reasoning":"task done","suggested_action":"next step"}
```
Some text after"#;
        let parsed = parse_evaluation_result(text).unwrap();
        assert_eq!(parsed.decision, EvaluationDecision::Complete);
        assert_eq!(parsed.reasoning, "task done");
        assert_eq!(parsed.suggested_action, Some("next step".to_string()));
    }

    #[test]
    fn parse_evaluation_json_handles_invalid_json() {
        let text = "not valid json";
        let result = parse_evaluation_result(text);
        assert!(result.is_err());
    }

    #[test]
    fn parse_evaluation_json_handles_all_decisions() {
        for (decision_str, decision) in [
            ("Continue", EvaluationDecision::Continue),
            ("Complete", EvaluationDecision::Complete),
            ("Failed", EvaluationDecision::Failed),
            ("OffTrack", EvaluationDecision::OffTrack),
        ] {
            let text = format!(r#"{{"decision":"{}","reasoning":"test"}}"#, decision_str);
            let parsed = parse_evaluation_result(&text).unwrap();
            assert_eq!(parsed.decision, decision);
        }
    }
}
