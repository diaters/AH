use bevy::prelude::*;

use crate::{
    app::{Clock, HarnessSettings},
    domain::{
        Agent, AgentExecutionRequest, AgentExecutionRequestMessage, AgentRequestKind,
        AgentStatus, Task, TaskStatus,
    },
    llm::brain_system_prompt,
};

/// 将 Ready 任务转换为 Agent 执行请求。
pub(crate) fn task_dispatch_system(
    clock: Res<Clock>,
    mut commands: Commands,
    mut tasks: Query<&mut Task>,
    mut agents: Query<&mut Agent>,
) {
    for mut task in &mut tasks {
        if task.status != TaskStatus::Ready {
            continue;
        }

        let Some(mut agent) = agents.iter_mut().find(|agent| agent.status == AgentStatus::Idle) else {
            continue;
        };

        let request = AgentExecutionRequest {
            task_id: task.id,
            agent_id: agent.id,
            request_kind: AgentRequestKind::LlmCompletion,
            prompt: task.content.clone(),
            system_prompt: None,
        };

        agent.status = AgentStatus::Busy;
        task.mark_waiting_for_agent(agent.id, clock.0);
        commands.spawn(AgentExecutionRequestMessage { request });
    }
}

/// 将 Ready 任务提交给 Brain Agent 进行调度决策。
pub(crate) fn brain_dispatch_system(
    clock: Res<Clock>,
    settings: Res<HarnessSettings>,
    mut commands: Commands,
    mut tasks: Query<&mut Task>,
    agents: Query<&Agent>,
) {
    let Some(brain_config) = &settings.0.brain else {
        return;
    };
    if !brain_config.enabled {
        return;
    }

    // 先收集 agent 快照，避免可变借用冲突
    let agent_snapshots: Vec<AgentSnapshot> = agents
        .iter()
        .filter(|agent| agent.profile.name == brain_config.agent_name && agent.status == AgentStatus::Idle)
        .map(|agent| AgentSnapshot {
            id: agent.id,
            name: agent.profile.name.clone(),
        })
        .collect();

    let all_agent_descriptions: Vec<AgentDescription> = agents
        .iter()
        .map(|agent| AgentDescription {
            name: agent.profile.name.clone(),
            model: agent.profile.model.clone(),
            tags: agent.capabilities.tags.clone(),
            description: agent.capabilities.description.clone(),
        })
        .collect();

    for mut task in &mut tasks {
        if task.status != TaskStatus::Ready {
            continue;
        }

        let Some(brain_snapshot) = agent_snapshots.first() else {
            continue;
        };

        let prompt = brain_user_prompt_from_descriptions(
            &task.content,
            &all_agent_descriptions,
            &brain_config.agent_name,
        );

        let request = AgentExecutionRequest {
            task_id: task.id,
            agent_id: brain_snapshot.id,
            request_kind: AgentRequestKind::BrainDecision,
            prompt,
            system_prompt: Some(brain_system_prompt()),
        };

        task.mark_waiting_for_brain(brain_snapshot.id, clock.0);
        commands.spawn(AgentExecutionRequestMessage { request });
    }
}

struct AgentSnapshot {
    id: crate::domain::AgentId,
    #[allow(dead_code)]
    name: String,
}

struct AgentDescription {
    name: String,
    model: String,
    tags: Vec<String>,
    description: String,
}

/// 从 Agent 描述快照构建 Brain user prompt，避免持有 Query 引用。
fn brain_user_prompt_from_descriptions(
    task_content: &str,
    agents: &[AgentDescription],
    brain_agent_name: &str,
) -> String {
    let agent_descriptions: Vec<String> = agents
        .iter()
        .filter(|agent| agent.name != brain_agent_name)
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
