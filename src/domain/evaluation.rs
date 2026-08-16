use crate::prelude::Resource;
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
    let json_slice = extract_json_slice(content);
    serde_json::from_str(json_slice).map_err(|e| e.to_string())
}

/// 从 LLM 输出中提取 JSON 片段：容忍 markdown 代码块包裹，否则取原文。
fn extract_json_slice(content: &str) -> &str {
    if content.contains("```json") {
        content
            .split("```json")
            .nth(1)
            .and_then(|s| s.split("```").next())
            .map(str::trim)
            .unwrap_or(content)
    } else {
        content.trim()
    }
}

/// Judge 评估维度得分
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JudgeDimension {
    /// 维度名，如 "correctness" / "completeness"
    pub name: String,
    /// 维度得分 0.0 - 1.0
    pub score: f32,
    /// 该维度评分理由
    pub rationale: String,
}

/// Judge 裁决结果（LLM-as-Judge 输出的结构化形态）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JudgeVerdict {
    /// 各维度得分
    pub scores: Vec<JudgeDimension>,
    /// 综合裁决是否通过
    pub pass: bool,
    /// 裁决理由
    pub reasoning: String,
    /// 置信度 0.0 - 1.0，低于阈值时降级人工待审
    pub confidence: f32,
}

impl JudgeVerdict {
    /// 各维度平均分；无维度时返回 0.0
    pub fn overall_score(&self) -> f32 {
        if self.scores.is_empty() {
            return 0.0;
        }
        self.scores.iter().map(|d| d.score).sum::<f32>() / self.scores.len() as f32
    }
}

/// Judge rubric 配置（场景文件中的评估规格）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JudgeRubric {
    /// 评估维度名列表
    pub dimensions: Vec<String>,
    /// 综合分通过阈值
    pub threshold: f32,
    /// 采样次数（多数投票决定 pass/fail）
    pub samples: usize,
}

impl Default for JudgeRubric {
    fn default() -> Self {
        Self {
            dimensions: vec!["correctness".to_string()],
            threshold: 0.7,
            samples: 3,
        }
    }
}

/// Judge 采样投票结果
#[derive(Debug, Clone, PartialEq)]
pub struct JudgeVote {
    /// 通过票数
    pub pass_votes: usize,
    /// 总采样次数
    pub total: usize,
    /// 所有采样中最低置信度
    pub min_confidence: f32,
}

/// Judge 投票裁决：通过 / 失败 / 降级人工待审
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JudgeOutcome {
    Pass,
    Fail,
    NeedsHuman,
}

impl JudgeVote {
    /// 非确定性治理规则（设计 §6.2）：
    /// - 任一次采样 `confidence < 0.8` → 降级人工待审
    /// - 全票通过 → `Pass`；全票失败 → `Fail`
    /// - 票数分裂（如 2:1）→ 降级人工待审，不强行裁决
    pub fn outcome(&self) -> JudgeOutcome {
        if self.min_confidence < 0.8 {
            return JudgeOutcome::NeedsHuman;
        }
        if self.pass_votes == self.total {
            return JudgeOutcome::Pass;
        }
        if self.pass_votes == 0 {
            return JudgeOutcome::Fail;
        }
        JudgeOutcome::NeedsHuman
    }
}

/// 解析 Judge 输出（与 parse_evaluation_result 同构的鲁棒解析）
pub fn parse_judge_verdict(content: &str) -> Result<JudgeVerdict, String> {
    let json_slice = extract_json_slice(content);
    let verdict: JudgeVerdict = serde_json::from_str(json_slice).map_err(|e| e.to_string())?;
    if verdict.scores.is_empty() {
        return Err("judge verdict has no dimension scores".to_string());
    }
    Ok(verdict)
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
    fn off_track_policy_deserialize_all_variants() {
        let ac: OffTrackPolicy = serde_json::from_str("\"AutoCorrect\"").unwrap();
        assert_eq!(ac, OffTrackPolicy::AutoCorrect);
        let au: OffTrackPolicy = serde_json::from_str("\"AskUser\"").unwrap();
        assert_eq!(au, OffTrackPolicy::AskUser);
        let f: OffTrackPolicy = serde_json::from_str("\"Fail\"").unwrap();
        assert_eq!(f, OffTrackPolicy::Fail);
    }

    #[test]
    fn off_track_policy_deserialize_invalid_returns_err() {
        let result: Result<OffTrackPolicy, _> = serde_json::from_str("\"Unknown\"");
        assert!(result.is_err());
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

    // ============ Judge 数据结构与解析 ============

    fn sample_verdict_json() -> String {
        r#"{
            "scores": [
                {"name": "correctness", "score": 0.9, "rationale": "数字与目录一致"},
                {"name": "completeness", "score": 0.8, "rationale": "覆盖了全部要求"}
            ],
            "pass": true,
            "reasoning": "任务完成质量良好",
            "confidence": 0.92
        }"#
        .to_string()
    }

    #[test]
    fn parse_judge_verdict_from_plain_json() {
        let verdict = parse_judge_verdict(&sample_verdict_json()).unwrap();
        assert_eq!(verdict.scores.len(), 2);
        assert_eq!(verdict.scores[0].name, "correctness");
        assert!((verdict.scores[0].score - 0.9).abs() < 1e-6);
        assert!(verdict.pass);
        assert!((verdict.confidence - 0.92).abs() < 1e-6);
        assert!((verdict.overall_score() - 0.85).abs() < 1e-6);
    }

    #[test]
    fn parse_judge_verdict_from_markdown_code_block() {
        let wrapped = format!(
            "判断如下：\n```json\n{}\n```\n以上是判断。",
            sample_verdict_json()
        );
        let verdict = parse_judge_verdict(&wrapped).unwrap();
        assert!(verdict.pass);
        assert_eq!(verdict.scores.len(), 2);
    }

    #[test]
    fn parse_judge_verdict_rejects_invalid_json() {
        assert!(parse_judge_verdict("not json at all").is_err());
    }

    #[test]
    fn parse_judge_verdict_rejects_empty_scores() {
        let text = r#"{"scores":[],"pass":true,"reasoning":"r","confidence":0.9}"#;
        assert!(parse_judge_verdict(text).is_err());
    }

    #[test]
    fn judge_rubric_default_matches_design() {
        let rubric = JudgeRubric::default();
        assert_eq!(rubric.dimensions, vec!["correctness".to_string()]);
        assert!((rubric.threshold - 0.7).abs() < 1e-6);
        assert_eq!(rubric.samples, 3);
    }

    #[test]
    fn judge_vote_unanimous_pass() {
        let vote = JudgeVote {
            pass_votes: 3,
            total: 3,
            min_confidence: 0.9,
        };
        assert_eq!(vote.outcome(), JudgeOutcome::Pass);
    }

    #[test]
    fn judge_vote_unanimous_fail() {
        let vote = JudgeVote {
            pass_votes: 0,
            total: 3,
            min_confidence: 0.9,
        };
        assert_eq!(vote.outcome(), JudgeOutcome::Fail);
    }

    #[test]
    fn judge_vote_split_goes_to_human() {
        let vote = JudgeVote {
            pass_votes: 2,
            total: 3,
            min_confidence: 0.9,
        };
        assert_eq!(vote.outcome(), JudgeOutcome::NeedsHuman);
    }

    #[test]
    fn judge_vote_low_confidence_goes_to_human() {
        let vote = JudgeVote {
            pass_votes: 3,
            total: 3,
            min_confidence: 0.6,
        };
        assert_eq!(vote.outcome(), JudgeOutcome::NeedsHuman);
    }
}
