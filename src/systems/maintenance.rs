use std::fs;

use bevy::prelude::*;
use tracing::{debug, error, warn};
use uuid::Uuid;

use crate::{
    app::{Clock, HarnessSettings},
    domain::{
        Agent, AgentCapabilities, AgentExecutionRequest, AgentExecutionRequestMessage,
        AgentExperience, AgentKind, AgentProfile, AgentSpawnRequestMessage, AgentToolPermissions,
        FailureReason, Task, TaskId, TaskTerminatedMessage, ToolPermission,
    },
};

/// Startup 系统：加载持久化 Agent
pub(crate) fn load_agents_system(
    mut commands: Commands,
    settings: Res<HarnessSettings>,
    agents: Query<(Entity, &Agent)>,
) {
    load_persistent_agents(&mut commands, &settings, &agents);
}

/// 运行时系统：处理 Agent 创建和销毁
#[allow(clippy::too_many_arguments)]
pub(crate) fn agent_factory_system(
    mut commands: Commands,
    clock: Res<Clock>,
    agents: Query<(Entity, &Agent)>,
    mut tasks: Query<&mut Task>,
    spawn_requests: Query<(Entity, &AgentSpawnRequestMessage)>,
    terminated_messages: Query<(Entity, &TaskTerminatedMessage)>,
) {
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
                event = "AgentsConfigNotFound",
                config_path = %config_path,
                "agents config file not found, no persistent agents loaded"
            );
            return;
        }
    };

    let config: crate::domain::AgentConfig = match toml::from_str(&content) {
        Ok(config) => config,
        Err(err) => {
            error!(
                event = "AgentsConfigParseError",
                config_path = %config_path,
                error = %err,
                "failed to parse agents config"
            );
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

    debug!(
        event = "AgentsConfigLoaded",
        config_path = %config_path,
        agent_count = config.agent.len(),
        agent_names = ?config.agent.iter().map(|a| &a.name).collect::<Vec<_>>(),
        "persistent agents loaded from config"
    );

    for entry in &config.agent {
        let id = Uuid::new_v4();
        debug!(
            event = "PersistentAgentSpawned",
            agent_id = %id,
            agent_name = %entry.name,
            agent_model = %entry.model,
            agent_tags = ?entry.tags,
            "spawning persistent agent"
        );

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
    let Some((_, parent_agent)) = agents.iter().find(|(_, a)| a.id == request.parent_agent_id)
    else {
        warn!(
            event = "SpawnRequestFailed",
            parent_id = %request.parent_agent_id,
            task_id = %request.task_id,
            reason = "parent_agent_not_found",
            "parent agent not found for spawn request"
        );
        mark_task_failed(
            tasks,
            clock,
            request.task_id,
            "parent agent not found for spawn request",
        );
        return;
    };

    // 过滤 tools：仅保留父 Agent 拥有的权限
    let allowed_tools: Vec<String> = request
        .tools
        .iter()
        .filter(|tool| parent_agent.has_permission(tool))
        .cloned()
        .collect();

    if allowed_tools.is_empty() {
        warn!(
            event = "SpawnRequestRejected",
            parent_id = %request.parent_agent_id,
            task_id = %request.task_id,
            requested_tools = ?request.tools,
            reason = "no_valid_tools",
            "spawn rejected: no valid tools after filtering"
        );
        let msg = format!(
            "Agent spawn rejected: requested tools {:?} not available in parent agent",
            request.tools
        );
        mark_task_failed(tasks, clock, request.task_id, &msg);
        return;
    }

    // 使用请求中的 model，或继承父 Agent 的 model
    let model = request
        .model
        .clone()
        .unwrap_or_else(|| parent_agent.profile.model.clone());

    let id = Uuid::new_v4();
    debug!(
        event = "TaskScopedAgentSpawned",
        agent_id = %id,
        agent_name = %request.name,
        agent_model = %model,
        parent_agent_id = %request.parent_agent_id,
        task_id = %request.task_id,
        tools = ?allowed_tools,
        "spawning task-scoped agent"
    );

    // 构建 tool_permissions: 子 Agent 默认拒绝，仅显式允许的工具可用
    let tool_permissions = AgentToolPermissions {
        default_permission: ToolPermission::Deny,
        overrides: allowed_tools
            .iter()
            .map(|t| (t.clone(), ToolPermission::Allow))
            .collect(),
    };

    commands.spawn(Agent {
        id,
        profile: AgentProfile {
            name: request.name.clone(),
            model,
        },
        capabilities: AgentCapabilities {
            // TaskScoped Agent 不参与路由
            tags: vec![],
            description: request.description.clone(),
        },
        kind: AgentKind::TaskScoped,
        parent_id: Some(request.parent_agent_id),
        bound_task_id: Some(request.task_id),
        tool_permissions,
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
            debug!(
                event = "AgentDespawned",
                agent_id = %agent.id,
                agent_name = %agent.profile.name,
                task_id = %task_id,
                kind = ?agent.kind,
                "despawning task-scoped agent"
            );
            commands.entity(entity).despawn();
        }
    }
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
