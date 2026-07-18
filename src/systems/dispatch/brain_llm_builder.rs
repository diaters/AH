//! Brain LLM 调用构建辅助函数
//!
//! 从原 `brain_dispatch.rs` 提取 Brain LLM 调用逻辑，供 `dispatch_system` 复用。
//! 保留 Brain Agent 选择（FirstBrainPolicy）、prompt 构建、AgentExecutionRequest 构造。

use tracing::debug;

use crate::{
    contracts::{AgentCapabilitySummary, BrainSelectionPolicy, FirstBrainPolicy},
    domain::{
        Agent, AgentExecutionRequest, AgentExecutionRequestMessage, AgentKind, AgentRequestKind,
        MessageDispatchedHookPending, ShortTermMemory, SpaceToolRegistry, Task, ToolPermission,
    },
    infrastructure::skills::SkillRegistry,
};

use super::brain_dispatch::{
    AgentDescription, SkillSummary, brain_user_prompt_from_descriptions, build_prompt_with_history,
};

/// 查找 Brain Agent
///
/// 通过 Tag 查找所有带 "brain" 标签的 Persistent Agent，使用 FirstBrainPolicy 选择。
pub fn find_brain_agent<'a>(agents: &'a [&Agent]) -> Option<&'a Agent> {
    let brain_candidates: Vec<AgentCapabilitySummary> = agents
        .iter()
        .filter(|a| {
            a.kind == AgentKind::Persistent && a.capabilities.tags.contains(&"brain".to_string())
        })
        .map(|a| AgentCapabilitySummary::from_agent(a))
        .collect();

    let policy = FirstBrainPolicy;
    let brain_agent_id = policy.select_brain(&brain_candidates)?;
    agents.iter().find(|a| a.id == brain_agent_id).copied()
}

/// 构建所有 Persistent Agent 的描述列表（供 Brain LLM prompt 使用）
///
/// 每个 Agent 的 `skills` 字段从 `SkillRegistry` 取其名下所有 skill 的精简视图
/// （`name + description + owner_agent_name`，不含 `instructions`），依据 ADR-004 §2.1。
pub fn build_agent_descriptions<'a>(
    agents: impl Iterator<Item = &'a Agent>,
    skill_registry: &SkillRegistry,
) -> Vec<AgentDescription> {
    agents
        .filter(|a| a.kind == AgentKind::Persistent)
        .map(|agent| {
            let skills = skill_registry
                .list_by_owner(&agent.profile.name)
                .into_iter()
                .map(|entry| SkillSummary {
                    name: entry.name.clone(),
                    description: entry.description.clone(),
                    owner_agent_name: entry.owner_agent_name.clone(),
                })
                .collect();
            AgentDescription {
                name: agent.profile.name.clone(),
                model: agent.profile.model.clone(),
                tags: agent.capabilities.tags.clone(),
                description: agent.capabilities.description.clone(),
                skills,
            }
        })
        .collect()
}

/// 构建 Brain LLM 的工具列表（非 Deny）
pub fn build_brain_tools(
    registry: &SpaceToolRegistry,
    brain_agent: &Agent,
) -> Vec<crate::domain::ToolDefinition> {
    registry
        .iter()
        .filter(|tool_def| {
            !matches!(
                brain_agent.tool_permissions.get_permission(&tool_def.name),
                ToolPermission::Deny
            )
        })
        .cloned()
        .collect()
}

/// 构建 Brain LLM 执行请求
///
/// 组合 Brain Agent 选择、prompt 构建（含候选 Agent 名下 skills 清单）、工具过滤，
/// 产出 `AgentExecutionRequestMessage`。调用方负责 spawn 返回的 request。
///
/// 返回 `None` 表示未找到 Brain Agent。
pub fn build_brain_execution_request(
    task: &Task,
    short_term: Option<&ShortTermMemory>,
    agents: &[&Agent],
    registry: &SpaceToolRegistry,
    skill_registry: &SkillRegistry,
) -> Option<(AgentExecutionRequestMessage, MessageDispatchedHookPending)> {
    let brain_agent = find_brain_agent(agents)?;
    let all_agent_descriptions = build_agent_descriptions(agents.iter().copied(), skill_registry);

    let prompt_with_history = build_prompt_with_history(&task.content, short_term);
    let prompt = brain_user_prompt_from_descriptions(&prompt_with_history, &all_agent_descriptions);

    let tools = build_brain_tools(registry, brain_agent);

    debug!(
        event = "BrainLlmRequestBuilt",
        task_id = %task.id,
        brain_agent_id = %brain_agent.id,
        brain_agent_name = %brain_agent.profile.name,
        prompt_len = prompt.len(),
        tools_count = tools.len(),
        candidate_agents = all_agent_descriptions.len(),
        "built brain llm execution request"
    );

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

    Some((
        AgentExecutionRequestMessage { request },
        MessageDispatchedHookPending,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{AgentCapabilities, AgentKind, AgentProfile, AgentToolPermissions};
    use crate::infrastructure::skills::{SkillEntry, SkillId};
    use uuid::Uuid;

    fn make_persistent_agent(name: &str, tags: Vec<String>) -> Agent {
        Agent {
            id: Uuid::new_v4(),
            profile: AgentProfile {
                name: name.to_string(),
                model: "test-model".to_string(),
            },
            capabilities: AgentCapabilities {
                tags,
                description: "test".to_string(),
            },
            kind: AgentKind::Persistent,
            parent_id: None,
            bound_task_id: None,
            tool_permissions: AgentToolPermissions::default(),
            system_prompt: None,
        }
    }

    fn make_skill_entry(owner: &str, skill_name: &str, desc: &str) -> SkillEntry {
        SkillEntry {
            skill_id: SkillId::new(owner, skill_name),
            name: skill_name.to_string(),
            description: desc.to_string(),
            instructions: "instructions".to_string(),
            version: 1,
            owner_agent_name: owner.to_string(),
            self_updatable: true,
        }
    }

    #[test]
    fn build_agent_descriptions_filters_persistent_only() {
        let persistent = make_persistent_agent("p", vec![]);
        let mut scoped = persistent.clone();
        scoped.kind = AgentKind::TaskScoped;
        scoped.profile.name = "s".to_string();

        let agents = [persistent, scoped];
        let reg = SkillRegistry::default();
        let descriptions = build_agent_descriptions(agents.iter(), &reg);
        assert_eq!(descriptions.len(), 1);
        assert_eq!(descriptions[0].name, "p");
        assert!(descriptions[0].skills.is_empty());
    }

    #[test]
    fn find_brain_agent_returns_none_without_brain_tag() {
        let agent = make_persistent_agent("a", vec![]);
        let agents = [&agent];
        assert!(find_brain_agent(&agents).is_none());
    }

    #[test]
    fn find_brain_agent_returns_some_with_brain_tag() {
        let brain_agent = make_persistent_agent("brain", vec!["brain".to_string()]);
        let agents = [&brain_agent];
        let found = find_brain_agent(&agents);
        assert!(found.is_some());
        assert_eq!(found.unwrap().profile.name, "brain");
    }

    #[test]
    fn build_agent_descriptions_attaches_skills_by_owner() {
        let agent_a = make_persistent_agent("agent-a", vec![]);
        let agent_b = make_persistent_agent("agent-b", vec![]);

        let mut reg = SkillRegistry::default();
        reg.upsert(make_skill_entry("agent-a", "coding", "代码编写"));
        reg.upsert(make_skill_entry("agent-a", "review", "代码审查"));
        reg.upsert(make_skill_entry("agent-b", "summary", "摘要生成"));

        let agents = [agent_a, agent_b];
        let descriptions = build_agent_descriptions(agents.iter(), &reg);
        assert_eq!(descriptions.len(), 2);

        let a_desc = descriptions
            .iter()
            .find(|d| d.name == "agent-a")
            .expect("agent-a exists");
        assert_eq!(a_desc.skills.len(), 2);
        let skill_names: Vec<_> = a_desc.skills.iter().map(|s| s.name.as_str()).collect();
        assert!(skill_names.contains(&"coding"));
        assert!(skill_names.contains(&"review"));

        let b_desc = descriptions
            .iter()
            .find(|d| d.name == "agent-b")
            .expect("agent-b exists");
        assert_eq!(b_desc.skills.len(), 1);
        assert_eq!(b_desc.skills[0].name, "summary");
    }
}
