//! Brain 决策辅助类型与 prompt 构建
//!
//! 顶层任务通过 `PendingDispatch(BrainLlm)` 流入 `dispatch_system`，
//! Brain LLM 请求由 `brain_llm_builder::build_brain_execution_request` 构造。
//!
//! 本模块提供：
//! - `AgentDescription` / `SkillSummary`：候选 Agent 与其名下 skill 的精简视图
//! - `brain_user_prompt_from_descriptions`：Brain LLM user prompt 构建
//! - `build_prompt_with_history`：注入短期记忆历史
//! - `parse_brain_skill_selection` / `sanitize_brain_output`：Brain LLM 输出解析
//! - `BrainSkillSelectionError`：解析错误类型

use serde::Deserialize;
use thiserror::Error;
use tracing::warn;

use crate::domain::{EntryRole, ShortTermMemory};

/// Brain 选 skill 流程的统一错误类型（ADR-004 §2.3）
///
/// 当前仅 `parse_brain_skill_selection` 使用 `InvalidJson` 变体；
/// `AgentNotInCandidates` 与 `SkillNotOwned` 预留给调用方
/// （`brain_decision_system` 后续接入更严格校验时使用）。
#[derive(Debug, Error)]
pub enum BrainSkillSelectionError {
    #[error("invalid brain skill selection JSON: {0}")]
    InvalidJson(#[from] serde_json::Error),
    #[allow(dead_code)] // 预留给调用方（brain_decision_system 接入更严格校验时使用）
    #[error("agent not in candidates: {0}")]
    AgentNotInCandidates(String),
    #[allow(dead_code)] // 预留给调用方（brain_decision_system 接入更严格校验时使用）
    #[error("skill not owned by agent: agent={agent}, skill={skill}")]
    SkillNotOwned { agent: String, skill: String },
}

pub(crate) struct AgentDescription {
    pub name: String,
    pub model: String,
    pub tags: Vec<String>,
    pub description: String,
    /// 该 Agent 名下所有 skill 的精简视图（不含 instructions）
    pub skills: Vec<SkillSummary>,
}

/// Skill 精简视图，供 Brain LLM 在候选清单中决定是否注入 skill。
///
/// 依据 ADR-004 §2.1：Brain 可见字段为 `name + description + owner_agent_name`，
/// 不暴露 `instructions` / `version` / `self_updatable`。
#[derive(Clone)]
pub(crate) struct SkillSummary {
    pub name: String,
    pub description: String,
    pub owner_agent_name: String,
}

pub(crate) fn brain_user_prompt_from_descriptions(
    task_content: &str,
    agents: &[AgentDescription],
) -> String {
    let agent_descriptions: Vec<String> = agents
        .iter()
        .filter(|agent| !agent.tags.contains(&"brain".to_string()))
        .map(|agent| {
            let skills_block = if agent.skills.is_empty() {
                "    (no skills)".to_string()
            } else {
                agent
                    .skills
                    .iter()
                    .map(|s| {
                        format!(
                            "    - name: \"{}\"\n      description: \"{}\"\n      owner_agent_name: \"{}\"",
                            s.name, s.description, s.owner_agent_name,
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            format!(
                "- name: \"{}\"\n  model: \"{}\"\n  tags: {:?}\n  description: \"{}\"\n  skills:\n{}",
                agent.name, agent.model, agent.tags, agent.description, skills_block,
            )
        })
        .collect();

    format!(
        r#"Task content: "{}"

Available agents:
{}"#,
        task_content,
        agent_descriptions.join("\n"),
    )
}

/// 构建带历史对话的 prompt（Brain Agent 使用）
pub(crate) fn build_prompt_with_history(
    task_content: &str,
    short_term: Option<&ShortTermMemory>,
) -> String {
    let Some(stm) = short_term else {
        return task_content.to_string();
    };

    if stm.entries.is_empty() {
        return task_content.to_string();
    }

    // 构建历史对话
    let mut history = String::new();

    // 添加摘要前缀（如果有）
    if let Some(summary) = &stm.summary_prefix {
        history.push_str(&format!("[Previous context summary]\n{}\n\n", summary));
    }

    // 添加对话历史
    history.push_str("[Conversation history]\n");
    for entry in &stm.entries {
        let role = match entry.role {
            EntryRole::User => "User",
            EntryRole::Assistant => "Assistant",
            EntryRole::Summary => "System note",
            EntryRole::Archive => continue,
        };
        history.push_str(&format!("{}: {}\n", role, entry.content));
    }

    // 组合成完整 prompt
    format!(
        "{}\n\n[Current request]\n{}",
        history.trim_end(),
        task_content
    )
}

#[derive(Deserialize)]
struct BrainSkillSelection {
    agent_name: String,
    skill_name: Option<serde_json::Value>,
}

/// 解析 brain LLM 的 skill 选择输出
///
/// 输入 JSON 格式：`{"agent_name": "agent-a", "skill_name": "coding"}`
///
/// 容错策略：
/// - `skill_name` 字段缺失或为 null：返回 None
/// - `skill_name` 为字符串 "None"（不区分大小写）或空字符串（trim 后）：返回 None
/// - `skill_name` 为非字符串类型（数字、布尔、对象、数组）：记录 warn 日志并返回 None
/// - `agent_name` 字段缺失或 JSON 格式错误：返回 [`BrainSkillSelectionError::InvalidJson`]
///
/// 输入清洗：调用 [`sanitize_brain_output`] 剥离 LLM 常见的 markdown 代码块包裹、
/// BOM 与不可见字符。
pub(crate) fn parse_brain_skill_selection(
    raw: &str,
) -> Result<(String, Option<String>), BrainSkillSelectionError> {
    // 清洗 LLM 输出：剥离 markdown 包裹/BOM/不可见字符
    let cleaned = sanitize_brain_output(raw);
    let parsed: BrainSkillSelection = serde_json::from_str(&cleaned)?;
    let skill = match parsed.skill_name {
        None => None,
        Some(serde_json::Value::String(s)) => {
            let trimmed = s.trim();
            if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("none") {
                None
            } else {
                Some(trimmed.to_string())
            }
        }
        Some(other) => {
            warn!(
                event = "SkillNameParseFailed",
                error_type = "NonStringSkillName",
                error = ?other,
                "skill_name is not a string, treating as None"
            );
            None
        }
    };
    Ok((parsed.agent_name, skill))
}

/// 清洗 brain LLM 输出：剥离 markdown 代码块包裹、BOM 与不可见字符。
fn sanitize_brain_output(raw: &str) -> String {
    let mut s = raw.trim().to_string();

    // 剥离 BOM (U+FEFF)
    if let Some(stripped) = s.strip_prefix('\u{feff}') {
        s = stripped.to_string();
    }

    // 剥离不可见字符 (U+200B / U+200C / U+200D / U+2060)
    for inv in ['\u{200b}', '\u{200c}', '\u{200d}', '\u{2060}'] {
        s = s.replace(inv, "");
    }

    // 剥离 ```json ... ``` 代码块包裹
    let trimmed = s.trim();
    if let (Some(start), Some(end)) = (trimmed.find("```json"), trimmed.rfind("```"))
        && end > start
    {
        let json_start = start + 7;
        return trimmed[json_start..end].trim().to_string();
    }

    s
}

#[cfg(test)]
mod tests {
    use super::*;

    mod skill_selection_parse_tests {
        use super::*;

        #[test]
        fn parse_standard_json() {
            let json = r#"{"agent_name": "agent-a", "skill_name": "coding"}"#;
            let result = parse_brain_skill_selection(json);
            assert!(result.is_ok());
            let (agent, skill) = result.unwrap();
            assert_eq!(agent, "agent-a");
            assert_eq!(skill, Some("coding".to_string()));
        }

        #[test]
        fn parse_null_skill_name() {
            let json = r#"{"agent_name": "agent-a", "skill_name": null}"#;
            let result = parse_brain_skill_selection(json);
            assert!(result.is_ok());
            let (_, skill) = result.unwrap();
            assert_eq!(skill, None);
        }

        #[test]
        fn parse_string_none_skill_name() {
            let json = r#"{"agent_name": "agent-a", "skill_name": "None"}"#;
            let result = parse_brain_skill_selection(json);
            assert!(result.is_ok());
            let (_, skill) = result.unwrap();
            assert_eq!(skill, None);
        }

        #[test]
        fn parse_empty_string_skill_name() {
            let json = r#"{"agent_name": "agent-a", "skill_name": ""}"#;
            let result = parse_brain_skill_selection(json);
            assert!(result.is_ok());
            let (_, skill) = result.unwrap();
            assert_eq!(skill, None);
        }

        #[test]
        fn parse_extra_fields_ignored() {
            let json = r#"{"agent_name": "agent-a", "skill_name": "coding", "reason": "because"}"#;
            let result = parse_brain_skill_selection(json);
            assert!(result.is_ok());
        }

        #[test]
        fn parse_invalid_json_errors() {
            let json = "not a json";
            let result = parse_brain_skill_selection(json);
            assert!(result.is_err());
        }

        #[test]
        fn parse_missing_agent_name_errors() {
            let json = r#"{"skill_name": "coding"}"#;
            let result = parse_brain_skill_selection(json);
            assert!(result.is_err());
        }

        #[test]
        fn parse_non_string_skill_name_returns_none() {
            // 数字类型 skill_name 应被容错为 None
            let json = r#"{"agent_name": "agent-a", "skill_name": 123}"#;
            let result = parse_brain_skill_selection(json);
            assert!(result.is_ok());
            let (agent, skill) = result.unwrap();
            assert_eq!(agent, "agent-a");
            assert_eq!(skill, None);

            // 布尔类型 skill_name 应被容错为 None
            let json = r#"{"agent_name": "agent-a", "skill_name": true}"#;
            let result = parse_brain_skill_selection(json);
            assert!(result.is_ok());
            let (_, skill) = result.unwrap();
            assert_eq!(skill, None);
        }

        #[test]
        fn parse_strips_markdown_json_block() {
            // LLM 常见输出格式：```json ... ``` 代码块包裹
            let json = "```json\n{\"agent_name\":\"a\",\"skill_name\":\"coding\"}\n```";
            let result = parse_brain_skill_selection(json);
            assert!(result.is_ok());
            let (agent, skill) = result.unwrap();
            assert_eq!(agent, "a");
            assert_eq!(skill, Some("coding".to_string()));
        }

        #[test]
        fn parse_strips_bom_and_zero_width_chars() {
            // BOM (U+FEFF) + 零宽空格 (U+200B) 等不可见字符应被清洗
            let json = "\u{feff}\u{200b}{\"agent_name\":\"a\",\"skill_name\":\"coding\"}";
            let result = parse_brain_skill_selection(json);
            assert!(result.is_ok());
            let (agent, skill) = result.unwrap();
            assert_eq!(agent, "a");
            assert_eq!(skill, Some("coding".to_string()));
        }

        #[test]
        fn parse_invalid_json_returns_invalid_json_variant() {
            // 验证 typed error：JSON 解析失败应返回 InvalidJson 变体
            let result = parse_brain_skill_selection("not a json");
            assert!(matches!(
                result,
                Err(BrainSkillSelectionError::InvalidJson(_))
            ));
        }

        #[test]
        fn parse_missing_agent_name_returns_invalid_json_variant() {
            // agent_name 字段缺失时 serde_json 解析失败，应返回 InvalidJson 变体
            let result = parse_brain_skill_selection(r#"{"skill_name": "coding"}"#);
            assert!(matches!(
                result,
                Err(BrainSkillSelectionError::InvalidJson(_))
            ));
        }
    }
}
