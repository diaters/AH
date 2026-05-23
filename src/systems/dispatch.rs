use bevy::prelude::*;
use tracing::debug;

use crate::{
    app::{Clock, HarnessSettings},
    domain::{
        Agent, AgentExecutionRequest, AgentExecutionRequestMessage, AgentKind, AgentRequestKind,
        EntryRole, LongTermMemory, ShortTermMemory, Task, TaskStatus,
    },
    llm::brain_system_prompt,
};

pub(crate) fn task_dispatch_system(
    clock: Res<Clock>,
    mut commands: Commands,
    mut tasks: Query<(&mut Task, Option<&ShortTermMemory>)>,
    agents: Query<(&Agent, Option<&LongTermMemory>)>,
) {
    for (mut task, short_term) in &mut tasks {
        // Pending 或 Ready 状态都可以被调度
        if task.status != TaskStatus::Ready && task.status != TaskStatus::Pending {
            continue;
        }

        // 收集候选 Agent 信息
        let candidates_info: Vec<_> = agents
            .iter()
            .filter(|(a, _)| a.kind == AgentKind::Persistent)
            .filter(|(a, _)| !a.capabilities.tags.contains(&"brain".to_string()))
            .map(|(a, ltm)| {
                (
                    a.profile.name.clone(),
                    match_score(a, &task.content),
                    ltm.map(|l| l.entries.len()).unwrap_or(0),
                )
            })
            .collect();

        let Some((agent, long_term)) = select_agent_with_memory(agents.iter(), &task.content)
        else {
            debug!(
                event = "NoAgentAvailable",
                task_id = %task.id,
                task_content = %task.content,
                task_status = ?task.status,
                candidates_count = candidates_info.len(),
                candidates = ?candidates_info,
                "no available agent for task dispatch"
            );
            continue;
        };

        // 构建带历史对话和长期记忆的 prompt
        let prompt = build_prompt_with_context(&task.content, short_term, long_term);
        let stm_entries = short_term.map(|s| s.entries.len()).unwrap_or(0);
        let stm_tokens = short_term.map(|s| s.estimated_tokens).unwrap_or(0);
        let ltm_entries = long_term.map(|l| l.entries.len()).unwrap_or(0);

        debug!(
            event = "AgentSelected",
            task_id = %task.id,
            task_content = %task.content,
            task_status = ?task.status,
            selected_agent = %agent.profile.name,
            selected_agent_id = %agent.id,
            selection_reason = "highest_score",
            candidates = ?candidates_info,
            stm_entries = stm_entries,
            stm_tokens = stm_tokens,
            stm_recent_entries = ?short_term.map(|s| s.entries.iter().rev().take(3).map(|e| (&e.role, &e.content)).collect::<Vec<_>>()),
            ltm_entries = ltm_entries,
            "agent selected for task"
        );

        debug!(
            event = "PromptBuilt",
            task_id = %task.id,
            agent_id = %agent.id,
            agent_name = %agent.profile.name,
            prompt_len = prompt.len(),
            prompt = %prompt,
            system_prompt = ?None::<String>,
            "execution request ready"
        );

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
        debug!(
            event = "BrainAgentNotFound",
            "no brain agent found, skipping brain dispatch"
        );
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

fn select_agent_with_memory<'a>(
    agents: impl Iterator<Item = (&'a Agent, Option<&'a LongTermMemory>)>,
    task_content: &str,
) -> Option<(&'a Agent, Option<&'a LongTermMemory>)> {
    let candidates: Vec<_> = agents
        .filter(|(a, _)| a.kind == AgentKind::Persistent)
        .filter(|(a, _)| !a.capabilities.tags.contains(&"brain".to_string()))
        .collect();

    let selected = candidates
        .iter()
        .max_by_key(|(a, _)| match_score(a, task_content));

    if let Some((agent, _ltm)) = selected {
        let score = match_score(agent, task_content);
        let all_scores: Vec<_> = candidates
            .iter()
            .map(|(a, _)| (a.profile.name.clone(), match_score(a, task_content)))
            .collect();
        debug!(
            event = "AgentScoring",
            selected_agent = %agent.profile.name,
            selected_score = score,
            all_candidates_scores = ?all_scores,
            task_content_preview = %task_content.chars().take(100).collect::<String>(),
            "agent scoring completed"
        );
    }

    selected.copied()
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

/// 构建带历史对话和长期记忆的 prompt
fn build_prompt_with_context(
    task_content: &str,
    short_term: Option<&ShortTermMemory>,
    long_term: Option<&LongTermMemory>,
) -> String {
    let mut parts = Vec::new();

    // 1. 长期记忆（Agent 专属经验）
    if let Some(ltm) = long_term
        && !ltm.entries.is_empty()
    {
        let memory_text: String = ltm
            .entries
            .iter()
            .map(|e| &e.content)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        parts.push(format!("[Agent memory]\n{}", memory_text));
    }

    // 2. 短期记忆（对话历史）
    if let Some(stm) = short_term
        && !stm.entries.is_empty()
    {
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

        parts.push(history.trim_end().to_string());
    }

    // 3. 当前请求（如果有上下文则添加前缀，否则直接返回）
    if parts.is_empty() {
        task_content.to_string()
    } else {
        parts.push(format!("[Current request]\n{}", task_content));
        parts.join("\n\n")
    }
}

/// 构建带历史对话的 prompt（Brain Agent 使用）
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
