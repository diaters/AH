use std::fs;

use bevy::prelude::*;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::{
    app::{Clock, HarnessSettings},
    domain::{
        Agent, AgentCapabilities, AgentExecutionRequest, AgentExecutionRequestMessage,
        AgentExperience, AgentKind, AgentProfile, AgentSpawnRequestMessage, AgentToolPermissions,
        FailureReason, Task, TaskId, TaskTerminatedMessage, ToolPermission,
    },
};

#[allow(clippy::too_many_arguments)]
pub(crate) fn agent_factory_system(
    mut commands: Commands,
    clock: Res<Clock>,
    settings: Res<HarnessSettings>,
    agents: Query<(Entity, &Agent)>,
    mut tasks: Query<&mut Task>,
    spawn_requests: Query<(Entity, &AgentSpawnRequestMessage)>,
    terminated_messages: Query<(Entity, &TaskTerminatedMessage)>,
    mut loaded: Local<bool>,
) {
    if !*loaded {
        load_persistent_agents(&mut commands, &settings, &agents);
        *loaded = true;
    }

    for (entity, request) in &spawn_requests {
        handle_spawn_request(&mut commands, &agents, &mut tasks, &clock, request);
        commands.entity(entity).despawn();
    }

    for (entity, terminated) in &terminated_messages {
        handle_termination(&mut commands, &agents, terminated.task_id);
        commands.entity(entity).despawn();
    }
}

fn load_persistent_agents(
    commands: &mut Commands,
    settings: &HarnessSettings,
    agents: &Query<(Entity, &Agent)>,
) {
    let config_path = &settings.0.agents_config_path;

    let content = match fs::read_to_string(config_path) {
        Ok(content) => content,
        Err(_) => {
            warn!(
                "agents config file '{}' not found, no persistent agents loaded",
                config_path
            );
            return;
        }
    };

    let config: crate::domain::AgentConfig = match toml::from_str(&content) {
        Ok(config) => config,
        Err(err) => {
            error!("failed to parse agents config: {err}");
            panic!("invalid agents config: {err}");
        }
    };

    let mut seen_names = std::collections::HashSet::new();
    for entry in &config.agent {
        if !seen_names.insert(entry.name.clone()) {
            panic!("duplicate agent name '{}' in config", entry.name);
        }
    }

    let existing_names: std::collections::HashSet<String> =
        agents.iter().map(|(_, a)| a.profile.name.clone()).collect();

    for entry in &config.agent {
        if existing_names.contains(&entry.name) {
            panic!("agent name '{}' already exists", entry.name);
        }
    }

    for entry in &config.agent {
        let id = Uuid::new_v4();
        info!(name = %entry.name, %id, "spawning persistent agent");

        let tool_permissions = if let Some(ref tools_config) = entry.tools {
            AgentToolPermissions {
                default_permission: tools_config
                    .default_permission
                    .unwrap_or(ToolPermission::Confirm),
                overrides: tools_config.overrides.clone(),
            }
        } else {
            AgentToolPermissions::default()
        };

        commands.spawn(Agent {
            id,
            profile: AgentProfile {
                name: entry.name.clone(),
                model: entry.model.clone(),
            },
            capabilities: AgentCapabilities {
                tags: entry.tags.clone(),
                description: entry.description.clone(),
            },
            kind: AgentKind::Persistent,
            parent_id: None,
            bound_task_id: None,
            tool_permissions,
            experience: AgentExperience::default(),
        });
    }
}

fn handle_spawn_request(
    commands: &mut Commands,
    agents: &Query<(Entity, &Agent)>,
    tasks: &mut Query<&mut Task>,
    clock: &Clock,
    request: &AgentSpawnRequestMessage,
) {
    let Some(parent_agent) = agents
        .iter()
        .find(|(_, a)| a.id == request.parent_agent_id)
        .map(|(_, a)| a)
    else {
        warn!(parent_id = %request.parent_agent_id, "parent agent not found for spawn request");
        mark_task_failed(
            tasks,
            clock,
            request.task_id,
            "parent agent not found for spawn request",
        );
        return;
    };

    if !validate_tags_subset(&parent_agent.capabilities.tags, &request.tags) {
        warn!(
            parent_tags = ?parent_agent.capabilities.tags,
            child_tags = ?request.tags,
            "spawn rejected: child tags exceed parent tags"
        );
        let msg = format!(
            "Agent spawn rejected: child tags {:?} exceed parent tags {:?}",
            request.tags, parent_agent.capabilities.tags
        );
        mark_task_failed(tasks, clock, request.task_id, &msg);
        return;
    }

    let id = Uuid::new_v4();
    info!(name = %request.name, %id, "spawning task-scoped agent");

    commands.spawn(Agent {
        id,
        profile: AgentProfile {
            name: request.name.clone(),
            model: request.model.clone(),
        },
        capabilities: AgentCapabilities {
            tags: request.tags.clone(),
            description: request.description.clone(),
        },
        kind: AgentKind::TaskScoped,
        parent_id: Some(request.parent_agent_id),
        bound_task_id: Some(request.task_id),
        tool_permissions: parent_agent.tool_permissions.clone(),
        experience: AgentExperience::default(),
    });

    let execution_request = AgentExecutionRequest {
        task_id: request.task_id,
        agent_id: id,
        request_kind: crate::domain::AgentRequestKind::LlmCompletion,
        prompt: String::new(),
        system_prompt: None,
    };

    commands.spawn(AgentExecutionRequestMessage {
        request: execution_request,
    });
}

fn handle_termination(commands: &mut Commands, agents: &Query<(Entity, &Agent)>, task_id: TaskId) {
    for (entity, agent) in agents.iter() {
        if agent.kind == AgentKind::TaskScoped && agent.bound_task_id == Some(task_id) {
            info!(name = %agent.profile.name, %task_id, "despawning task-scoped agent");
            commands.entity(entity).despawn();
        }
    }
}

pub(crate) fn validate_tags_subset(parent_tags: &[String], child_tags: &[String]) -> bool {
    child_tags.iter().all(|tag| parent_tags.contains(tag))
}

fn mark_task_failed(
    tasks: &mut Query<&mut Task>,
    clock: &Clock,
    task_id: TaskId,
    error_message: &str,
) {
    if let Some(mut task) = tasks.iter_mut().find(|t| t.id == task_id) {
        task.last_error = Some(error_message.to_string());
        task.status = crate::domain::TaskStatus::Failed(FailureReason::AgentError);
        task.updated_at = clock.0;
    }
}
