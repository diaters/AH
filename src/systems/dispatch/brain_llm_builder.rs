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
};

use super::brain_dispatch::{
    AgentDescription, brain_user_prompt_from_descriptions, build_prompt_with_history,
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
pub fn build_agent_descriptions<'a>(
    agents: impl Iterator<Item = &'a Agent>,
) -> Vec<AgentDescription> {
    agents
        .filter(|a| a.kind == AgentKind::Persistent)
        .map(|agent| AgentDescription {
            name: agent.profile.name.clone(),
            model: agent.profile.model.clone(),
            tags: agent.capabilities.tags.clone(),
            description: agent.capabilities.description.clone(),
        })
        .collect()
}

/// 构建 Brain LLM 的工具列表（非 Deny）
#[allow(dead_code)] // 阶段 2.2 dispatch_system 接入后移除
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
/// 组合 Brain Agent 选择、prompt 构建、工具过滤，产出 `AgentExecutionRequestMessage`。
/// 调用方负责 spawn 返回的 request。
///
/// 返回 `None` 表示未找到 Brain Agent。
#[allow(dead_code)] // 阶段 2.2 dispatch_system 接入后移除
pub fn build_brain_execution_request(
    task: &Task,
    short_term: Option<&ShortTermMemory>,
    agents: &[&Agent],
    registry: &SpaceToolRegistry,
) -> Option<(AgentExecutionRequestMessage, MessageDispatchedHookPending)> {
    let brain_agent = find_brain_agent(agents)?;
    let all_agent_descriptions = build_agent_descriptions(agents.iter().copied());

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

    #[test]
    fn build_agent_descriptions_filters_persistent_only() {
        let persistent = make_persistent_agent("p", vec![]);
        let mut scoped = persistent.clone();
        scoped.kind = AgentKind::TaskScoped;
        scoped.profile.name = "s".to_string();

        let agents = [persistent, scoped];
        let descriptions = build_agent_descriptions(agents.iter());
        assert_eq!(descriptions.len(), 1);
        assert_eq!(descriptions[0].name, "p");
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
}
