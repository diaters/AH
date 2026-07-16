use crate::domain::{Agent, BrainDecisionError, BrainDecisionOutput, Task};

/// 构建 Brain 决策的 system prompt。
///
/// 此函数保留供测试使用；生产环境中 system_prompt 已移至 agents.toml。
#[allow(dead_code)]
pub fn brain_system_prompt() -> String {
    r#"You are the Brain Agent, a global dispatcher responsible for deciding which agent should handle a given task.

You must respond with a valid JSON object matching this exact schema:
{
  "selected_agent_name": "<name of the selected agent>",
  "delegate_prompt": "<the prompt to send to the selected agent>",
  "reasoning": "<brief explanation of why this agent was selected>"
}

Rules:
- Select the most appropriate agent based on the task content and agent capabilities.
- The delegate_prompt should contain the full task description and any context needed.
- If no agent is suitable, select the default agent.
- Respond ONLY with the JSON object, no additional text."#
        .to_string()
}

/// 构建 Brain 决策的 user prompt，包含 Task 内容和所有可用 Agent 的能力描述。
pub fn brain_user_prompt(task: &Task, agents: &[&Agent], brain_agent_name: &str) -> String {
    let agent_descriptions: Vec<String> = agents
        .iter()
        .filter(|agent| agent.profile.name != brain_agent_name)
        .map(|agent| {
            format!(
                "- name: \"{}\"\n  model: \"{}\"\n  tags: {:?}\n  description: \"{}\"",
                agent.profile.name,
                agent.profile.model,
                agent.capabilities.tags,
                agent.capabilities.description,
            )
        })
        .collect();

    format!(
        r#"Task content: "{}"

Available agents:
{}

Select the best agent for this task and provide a delegate prompt."#,
        task.content,
        agent_descriptions.join("\n"),
    )
}

/// 从 Brain LLM 返回的原始文本中解析结构化决策。
pub fn parse_brain_decision(raw: &str) -> Result<BrainDecisionOutput, BrainDecisionError> {
    if raw.trim().is_empty() {
        return Err(BrainDecisionError::EmptyResponse);
    }

    let json_str = extract_json_block(raw);
    let json_str = sanitize_json_like_input(json_str);

    serde_json::from_str::<BrainDecisionOutput>(&json_str)
        .map_err(|e| BrainDecisionError::ParseFailed(e.to_string()))
}

/// 从可能包含 markdown code block 的文本中提取 JSON。
fn extract_json_block(raw: &str) -> &str {
    let trimmed = raw.trim();

    if let (Some(start), Some(end)) = (trimmed.find("```json"), trimmed.rfind("```")) {
        let json_start = start + 7;
        return trimmed[json_start..end].trim();
    }

    if let (Some(start), Some(end)) = (trimmed.find("```"), trimmed.rfind("```"))
        && start != end
    {
        let json_start = start + 3;
        return trimmed[json_start..end].trim();
    }

    trimmed
}

/// 清理 JSON 文本前缀中的不可见字符（如 BOM/零宽字符），降低解析失败率。
fn sanitize_json_like_input(raw: &str) -> String {
    let mut s = raw.trim().to_string();

    if let Some(stripped) = s.strip_prefix('\u{feff}') {
        s = stripped.to_string();
    }

    loop {
        let next = s
            .strip_prefix('\u{200b}')
            .or_else(|| s.strip_prefix('\u{200c}'))
            .or_else(|| s.strip_prefix('\u{200d}'))
            .or_else(|| s.strip_prefix('\u{2060}'));

        if let Some(stripped) = next {
            s = stripped.to_string();
            continue;
        }

        break;
    }

    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{AgentCapabilities, AgentKind, AgentProfile, ChannelId, FrontendKind};
    use uuid::Uuid;

    fn test_task(content: &str) -> Task {
        Task::from_user_input(
            content,
            3,
            ChannelId {
                frontend: FrontendKind::Tui,
                user_id: "default".to_string(),
                thread_id: None,
            },
        )
    }

    fn test_agent(name: &str) -> Agent {
        use crate::domain::AgentToolPermissions;
        Agent {
            id: Uuid::new_v4(),
            profile: AgentProfile {
                name: name.to_string(),
                model: "test-model".to_string(),
            },
            capabilities: AgentCapabilities {
                tags: vec!["test".to_string()],
                description: format!("{name} agent for testing"),
            },
            kind: AgentKind::Persistent,
            parent_id: None,
            bound_task_id: None,
            tool_permissions: AgentToolPermissions::default(),
            system_prompt: None,
        }
    }

    #[test]
    fn brain_system_prompt_contains_json_schema() {
        let prompt = brain_system_prompt();
        assert!(prompt.contains("selected_agent_name"));
        assert!(prompt.contains("delegate_prompt"));
        assert!(prompt.contains("reasoning"));
    }

    #[test]
    fn brain_user_prompt_includes_task_and_agents() {
        let task = test_task("hello");
        let agent = test_agent("worker");
        let agents = &[&agent];

        let prompt = brain_user_prompt(&task, agents, "brain");

        assert!(prompt.contains("hello"));
        assert!(prompt.contains("worker"));
    }

    #[test]
    fn brain_user_prompt_excludes_brain_agent() {
        let task = test_task("hello");
        let worker = test_agent("worker");
        let brain = test_agent("brain");
        let agents = &[&worker, &brain];

        let prompt = brain_user_prompt(&task, agents, "brain");

        assert!(prompt.contains("worker"));
        assert!(!prompt.contains("brain"));
    }

    #[test]
    fn parse_valid_json() {
        let raw =
            r#"{"selected_agent_name":"worker","delegate_prompt":"do it","reasoning":"test"}"#;
        let output = parse_brain_decision(raw).expect("should parse");

        assert_eq!(output.selected_agent_name, "worker");
        assert_eq!(output.delegate_prompt, "do it");
        assert_eq!(output.reasoning, "test");
    }

    #[test]
    fn parse_json_in_markdown_code_block() {
        let raw = "```json\n{\"selected_agent_name\":\"worker\",\"delegate_prompt\":\"do it\",\"reasoning\":\"test\"}\n```";
        let output = parse_brain_decision(raw).expect("should parse");

        assert_eq!(output.selected_agent_name, "worker");
    }

    /// 解析 JSON 时应忽略 UTF-8 BOM（U+FEFF）前缀。
    #[test]
    fn parse_json_with_bom_prefix() {
        let json =
            r#"{"selected_agent_name":"worker","delegate_prompt":"do it","reasoning":"test"}"#;
        let raw = format!("\u{feff}{json}");
        let output = parse_brain_decision(&raw).expect("should parse");

        assert_eq!(output.selected_agent_name, "worker");
    }

    /// 解析 JSON 时应忽略零宽字符（例如 U+200B）前缀。
    #[test]
    fn parse_json_with_zero_width_prefix() {
        let json =
            r#"{"selected_agent_name":"worker","delegate_prompt":"do it","reasoning":"test"}"#;
        let raw = format!("\u{200b}{json}");
        let output = parse_brain_decision(&raw).expect("should parse");

        assert_eq!(output.selected_agent_name, "worker");
    }

    /// 解析 markdown code block 中的 JSON 时应忽略 BOM（U+FEFF）前缀。
    #[test]
    fn parse_json_in_markdown_code_block_with_bom_prefix() {
        let json =
            r#"{"selected_agent_name":"worker","delegate_prompt":"do it","reasoning":"test"}"#;
        let raw = format!("```json\n\u{feff}{json}\n```");
        let output = parse_brain_decision(&raw).expect("should parse");

        assert_eq!(output.selected_agent_name, "worker");
    }

    #[test]
    fn parse_invalid_json_returns_error() {
        let raw = "not json at all";
        let result = parse_brain_decision(raw);

        assert!(matches!(result, Err(BrainDecisionError::ParseFailed(_))));
    }

    #[test]
    fn parse_empty_returns_error() {
        let result = parse_brain_decision("");
        assert!(matches!(result, Err(BrainDecisionError::EmptyResponse)));
    }
}
