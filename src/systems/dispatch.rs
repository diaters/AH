use bevy::prelude::*;

use crate::{
    app::{Clock, HarnessSettings},
    domain::{
        Agent, AgentExecutionRequest, AgentExecutionRequestMessage, AgentKind, AgentRequestKind,
        EntryRole, ShortTermMemory, Task, TaskStatus,
    },
    llm::brain_system_prompt,
};

pub(crate) fn task_dispatch_system(
    clock: Res<Clock>,
    mut commands: Commands,
    mut tasks: Query<(&mut Task, Option<&ShortTermMemory>)>,
    agents: Query<&Agent>,
) {
    for (mut task, short_term) in &mut tasks {
        // Pending 或 Ready 状态都可以被调度
        if task.status != TaskStatus::Ready && task.status != TaskStatus::Pending {
            continue;
        }

        let Some(agent) = select_agent(agents.iter(), &task.content) else {
            continue;
        };

        // 构建带历史对话的 prompt
        let prompt = build_prompt_with_history(&task.content, short_term);

        let request = AgentExecutionRequest {
            task_id: task.id,
            agent_id: agent.id,
            request_kind: AgentRequestKind::LlmCompletion,
            prompt,
            system_prompt: None,
        };

        task.mark_waiting_for_agent(agent.id, clock.0);
        commands.spawn(AgentExecutionRequestMessage { request });
    }
}

pub(crate) fn brain_dispatch_system(
    clock: Res<Clock>,
    settings: Res<HarnessSettings>,
    mut commands: Commands,
    mut tasks: Query<(&mut Task, Option<&ShortTermMemory>)>,
    agents: Query<&Agent>,
) {
    let Some(brain_config) = &settings.0.brain else {
        return;
    };
    if !brain_config.enabled {
        return;
    }

    let brain_agent = agents.iter().find(|a| {
        a.kind == AgentKind::Persistent && a.capabilities.tags.contains(&"brain".to_string())
    });

    let Some(brain_agent) = brain_agent else {
        return;
    };

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

    for (mut task, short_term) in &mut tasks {
        // Pending 或 Ready 状态都可以被调度
        if task.status != TaskStatus::Ready && task.status != TaskStatus::Pending {
            continue;
        }

        // 构建带历史对话的 prompt
        let prompt_with_history = build_prompt_with_history(&task.content, short_term);
        let prompt =
            brain_user_prompt_from_descriptions(&prompt_with_history, &all_agent_descriptions);

        let request = AgentExecutionRequest {
            task_id: task.id,
            agent_id: brain_agent.id,
            request_kind: AgentRequestKind::BrainDecision,
            prompt,
            system_prompt: Some(brain_system_prompt()),
        };

        task.mark_waiting_for_agent(brain_agent.id, clock.0);
        commands.spawn(AgentExecutionRequestMessage { request });
    }
}

struct AgentDescription {
    name: String,
    model: String,
    tags: Vec<String>,
    description: String,
}

fn brain_user_prompt_from_descriptions(task_content: &str, agents: &[AgentDescription]) -> String {
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

Select the best agent for this task and provide a delegate prompt."#,
        task_content,
        agent_descriptions.join("\n"),
    )
}

fn select_agent<'a>(
    agents: impl Iterator<Item = &'a Agent>,
    task_content: &str,
) -> Option<&'a Agent> {
    agents
        .filter(|a| a.kind == AgentKind::Persistent)
        .filter(|a| !a.capabilities.tags.contains(&"brain".to_string()))
        .max_by_key(|a| match_score(a, task_content))
}

fn match_score(agent: &Agent, task_content: &str) -> usize {
    let lower = task_content.to_lowercase();
    agent
        .capabilities
        .tags
        .iter()
        .filter(|tag| lower.contains(&tag.to_lowercase()))
        .count()
}

/// 构建带历史对话的 prompt
fn build_prompt_with_history(task_content: &str, short_term: Option<&ShortTermMemory>) -> String {
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
            _ => continue,
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
