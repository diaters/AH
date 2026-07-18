//! Brain 分发 System
//!
//! 使用 Brain Agent 进行任务分发决策。
//!
//! ## Brain Agent 选择规则
//!
//! 通过 Tag 查找所有带 "brain" 标签的 Agent，选择配置中最前的那个。
//! 这允许灵活配置多个 Brain Agent（如不同模型），同时保持确定性选择。

use crate::prelude::*;
use serde::Deserialize;
use thiserror::Error;
use tracing::{debug, trace, warn};

use crate::{
    app::{Clock, HarnessSettings},
    contracts::{AgentCapabilitySummary, BrainSelectionPolicy, FirstBrainPolicy},
    domain::{
        Agent, AgentExecutionRequest, AgentExecutionRequestMessage, AgentKind, AgentRequestKind,
        EntryRole, MessageDispatchedHookPending, PendingDispatch, ShortTermMemory,
        SpaceToolRegistry, Task, TaskStatus, ToolPermission,
    },
};

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
}

pub(crate) fn brain_user_prompt_from_descriptions(
    task_content: &str,
    agents: &[AgentDescription],
) -> String {
    let agent_descriptions: Vec<String> = agents
        .iter()
        .filter(|agent| !agent.tags.contains(&"brain".to_string()))
        .map(|agent| {
            format!(
                "- name: \"{}\"\n  model: \"{}\"\n  tags: {:?}\n  description: \"{}\"",
                agent.name, agent.model, agent.tags, agent.description,
            )
        })
        .collect();

    format!(
        r#"Task content: "{}"

Available agents:
{}

Select the best agent for this task and optionally a skill.

Return your decision as JSON:
{{"agent_name": "<selected_agent_name>", "skill_name": "<skill_name_or_null>"}}

- agent_name: must be one of the available agents listed above
- skill_name: optional, the name of a skill to inject; use null if no skill is needed"#,
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

/// Brain 分发 System
///
/// 使用 Brain Agent 进行任务分发决策。
///
/// ## Brain Agent 选择
///
/// 通过 Tag 查找所有带 "brain" 标签的 Agent，选择配置中最前的那个。
pub fn brain_dispatch_system(
    clock: Res<Clock>,
    settings: Res<HarnessSettings>,
    mut commands: Commands,
    mut tasks: Query<(
        &mut Task,
        Option<&ShortTermMemory>,
        Option<&PendingDispatch>,
    )>,
    agents: Query<&Agent>,
    registry: Res<SpaceToolRegistry>,
) {
    let Some(brain_config) = &settings.0.brain else {
        return;
    };
    if !brain_config.enabled {
        return;
    }

    // 通过 Tag 查找所有带 "brain" 标签的 Agent，选择配置中最前的
    let brain_candidates: Vec<AgentCapabilitySummary> = agents
        .iter()
        .filter(|a| {
            a.kind == AgentKind::Persistent && a.capabilities.tags.contains(&"brain".to_string())
        })
        .map(AgentCapabilitySummary::from_agent)
        .collect();

    let brain_policy = FirstBrainPolicy;
    let Some(brain_agent_id) = brain_policy.select_brain(&brain_candidates) else {
        debug!(
            event = "BrainAgentNotFound",
            "no brain agent found with 'brain' tag, skipping brain dispatch"
        );
        return;
    };

    let brain_agent = agents.iter().find(|a| a.id == brain_agent_id).unwrap();

    let all_agent_descriptions: Vec<AgentDescription> = agents
        .iter()
        .filter(|a| a.kind == AgentKind::Persistent)
        .map(|agent| AgentDescription {
            name: agent.profile.name.clone(),
            model: agent.profile.model.clone(),
            tags: agent.capabilities.tags.clone(),
            description: agent.capabilities.description.clone(),
        })
        .collect();

    for (mut task, short_term, pending_dispatch) in &mut tasks {
        // 阶段 3：带 PendingDispatch 的 Task 由 dispatch_system 处理，跳过
        if pending_dispatch.is_some() {
            continue;
        }

        // Pending 或 Ready 状态都可以被调度
        if task.status != TaskStatus::Ready && task.status != TaskStatus::Pending {
            continue;
        }

        if task.delegate.is_some() {
            trace!(
                event = "BrainDispatchSkipped",
                task_id = %task.id,
                task_status = ?task.status,
                delegate = ?task.delegate,
                "task already has delegate, skipping brain dispatch"
            );
            continue;
        }

        // 构建带历史对话的 prompt
        let prompt_with_history = build_prompt_with_history(&task.content, short_term);
        let prompt =
            brain_user_prompt_from_descriptions(&prompt_with_history, &all_agent_descriptions);

        let stm_entries = short_term.map(|s| s.entries.len()).unwrap_or(0);
        let stm_tokens = short_term.map(|s| s.estimated_tokens).unwrap_or(0);

        debug!(
            event = "BrainDispatch",
            task_id = %task.id,
            task_content = %task.content,
            task_status = ?task.status,
            brain_agent_id = %brain_agent.id,
            brain_agent_name = %brain_agent.profile.name,
            prompt_len = prompt.len(),
            stm_entries = stm_entries,
            stm_tokens = stm_tokens,
            available_agents = ?all_agent_descriptions.iter().map(|a| &a.name).collect::<Vec<_>>(),
            "brain dispatching task"
        );

        // 构建 Brain Agent 可用的工具列表（非 Deny）
        let tools: Vec<_> = registry
            .iter()
            .filter(|tool_def| {
                !matches!(
                    brain_agent.tool_permissions.get_permission(&tool_def.name),
                    ToolPermission::Deny
                )
            })
            .cloned()
            .collect();

        let request = AgentExecutionRequest {
            task_id: task.id,
            agent_id: brain_agent.id,
            request_kind: AgentRequestKind::BrainDecision,
            prompt,
            system_prompt: brain_agent.system_prompt.clone(),
            tools,
            conversation: None,
            work_item_id: None,
            model_override: None,
        };

        task.mark_waiting_for_agent(brain_agent.id, clock.0);
        commands.spawn((
            AgentExecutionRequestMessage { request },
            MessageDispatchedHookPending,
        ));
    }
}

#[allow(dead_code)] // 后续 brain_dispatch 改造任务接入
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
/// BOM 与不可见字符，逻辑与 `crate::llm::brain_prompt` 中的私有函数对齐，
/// 但不跨模块复用以避免引入不必要的耦合。
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
///
/// 与 `crate::llm::brain_prompt::sanitize_json_like_input` +
/// `extract_json_block` 逻辑对齐，因后者为私有函数且跨模块复用价值有限，
/// 此处保留本地实现供 brain_dispatch 使用。
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
    use crate::{
        app::{BrainConfig, HarnessConfig},
        domain::{AgentCapabilities, AgentProfile, ChannelId, FrontendKind},
    };
    use uuid::Uuid;

    /// 构建用于测试的 BrainDispatch App（包含必要 Resource 与 System）。
    fn build_test_app() -> App {
        let mut app = App::new();
        app.insert_resource(Clock::default());
        app.insert_resource(HarnessSettings(HarnessConfig {
            brain: Some(BrainConfig { enabled: true }),
            ..Default::default()
        }));
        app.insert_resource(SpaceToolRegistry::default());
        app.add_systems(Update, brain_dispatch_system);
        app
    }

    /// 当 Task 已存在 delegate 时（即便状态为 Ready），BrainDispatch 不应再次派发请求。
    #[test]
    fn brain_dispatch_skips_ready_task_with_existing_delegate() {
        let mut app = build_test_app();

        let brain_agent_id = Uuid::new_v4();
        app.world_mut().spawn(Agent {
            id: brain_agent_id,
            profile: AgentProfile {
                name: "brain".to_string(),
                model: "test-model".to_string(),
            },
            capabilities: AgentCapabilities {
                tags: vec!["brain".to_string()],
                description: "brain agent".to_string(),
            },
            kind: AgentKind::Persistent,
            parent_id: None,
            bound_task_id: None,
            tool_permissions: Default::default(),
            system_prompt: None,
        });

        let channel = ChannelId {
            frontend: FrontendKind::Tui,
            user_id: "test-user".to_string(),
            thread_id: None,
        };
        let mut task = Task::from_user_input_ready("do something", 0, channel);
        task.delegate = Some(Uuid::new_v4());
        let task_id = task.id;
        app.world_mut().spawn(task);

        app.update();

        let request_count = {
            let world = app.world_mut();
            let mut query = world.query::<&AgentExecutionRequestMessage>();
            query.iter(world).count()
        };
        assert_eq!(
            request_count, 0,
            "ready task with delegate should not spawn AgentExecutionRequestMessage"
        );

        let task_after = {
            let world = app.world_mut();
            let mut query = world.query::<&Task>();
            query
                .iter(world)
                .find(|t| t.id == task_id)
                .expect("task should still exist")
                .clone()
        };
        assert_eq!(task_after.status, TaskStatus::Ready);
        assert!(task_after.delegate.is_some());
    }

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
