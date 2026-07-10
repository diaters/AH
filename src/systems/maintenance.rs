use std::fs;

use crate::prelude::*;
use tracing::{debug, error, warn};
use uuid::Uuid;

use crate::{
    app::{Clock, HarnessSettings},
    domain::{
        Agent, AgentCapabilities, AgentExecutionRequest, AgentExecutionRequestMessage, AgentKind,
        AgentProfile, AgentSpawnRequestMessage, AgentStoppingHookPending, AgentToolPermissions,
        FailureReason, MessageDispatchedHookPending, SpaceToolRegistry, Task, TaskId,
        TaskTerminatedMessage, ToolPermission,
    },
};

/// Startup 系统：加载持久化 Agent
///
/// 先从配置文件加载，再合并插件贡献的 Agent。
pub(crate) fn load_agents_system(
    mut commands: Commands,
    settings: Res<HarnessSettings>,
    agents: Query<(Entity, &Agent)>,
    registry: Res<crate::llm::ExecutorRegistry>,
    plugin_registry: Option<Res<crate::user_plugins::registry::PluginRegistry>>,
) {
    load_persistent_agents(
        &mut commands,
        &settings,
        &agents,
        &registry,
        plugin_registry.as_deref(),
    );
}

/// 运行时系统：处理 Agent 创建和销毁
#[allow(clippy::too_many_arguments)]
pub(crate) fn agent_factory_system(
    mut commands: Commands,
    clock: Res<Clock>,
    registry: Res<SpaceToolRegistry>,
    agents: Query<(Entity, &Agent)>,
    mut tasks: Query<&mut Task>,
    spawn_requests: Query<(Entity, &AgentSpawnRequestMessage)>,
    terminated_messages: Query<(Entity, &TaskTerminatedMessage)>,
) {
    for (entity, request) in &spawn_requests {
        handle_spawn_request(
            &mut commands,
            &agents,
            &mut tasks,
            &clock,
            &registry,
            request,
        );
        commands.entity(entity).despawn();
    }

    for (entity, terminated) in &terminated_messages {
        handle_termination(&mut commands, &agents, &mut tasks, terminated.task_id);
        commands.entity(entity).despawn();
    }
}

fn load_persistent_agents(
    commands: &mut Commands,
    settings: &HarnessSettings,
    agents: &Query<(Entity, &Agent)>,
    registry: &crate::llm::ExecutorRegistry,
    plugin_registry: Option<&crate::user_plugins::registry::PluginRegistry>,
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

    // 收集插件贡献的 Agent 名称，一并检查重复
    let plugin_agent_entries = collect_plugin_agent_entries(plugin_registry);
    for (namespaced_name, _) in &plugin_agent_entries {
        if !seen_names.insert(namespaced_name.clone()) {
            panic!("duplicate agent name '{}' from plugin", namespaced_name);
        }
    }

    let existing_names: std::collections::HashSet<String> =
        agents.iter().map(|(_, a)| a.profile.name.clone()).collect();

    for entry in &config.agent {
        if existing_names.contains(&entry.name) {
            panic!("agent name '{}' already exists", entry.name);
        }
    }
    for (namespaced_name, _) in &plugin_agent_entries {
        if existing_names.contains(namespaced_name) {
            panic!("agent name '{}' already exists", namespaced_name);
        }
    }

    debug!(
        event = "AgentsConfigLoaded",
        config_path = %config_path,
        agent_count = config.agent.len(),
        agent_names = ?config.agent.iter().map(|a| &a.name).collect::<Vec<_>>(),
        "persistent agents loaded from config"
    );

    // 从配置文件加载
    for entry in &config.agent {
        spawn_persistent_agent_from_entry(commands, entry, registry);
    }

    // 合并插件贡献的 Agent
    for (_, entry) in &plugin_agent_entries {
        debug!(
            event = "PluginAgentSpawned",
            agent_name = %entry.name,
            "spawning plugin-contributed persistent agent"
        );
        spawn_persistent_agent_from_entry(commands, entry, registry);
    }

    if !plugin_agent_entries.is_empty() {
        debug!(
            event = "PluginAgentsMerged",
            count = plugin_agent_entries.len(),
            "plugin agents merged into persistent agent spawn"
        );
    }
}

/// 从配置条目生成持久化 Agent
fn spawn_persistent_agent_from_entry(
    commands: &mut Commands,
    entry: &crate::domain::AgentEntry,
    registry: &crate::llm::ExecutorRegistry,
) {
    let id = Uuid::new_v4();

    // 确定模型链
    let models = if !entry.models.is_empty() {
        entry.models.clone()
    } else if let Some(model) = &entry.model {
        // 向后兼容：从单 model 字段生成单元素链
        // 使用默认 provider（第一个注册的）
        let default_provider = registry
            .executors
            .keys()
            .next()
            .cloned()
            .unwrap_or_else(|| "default".to_string());

        vec![crate::domain::ModelChainEntry {
            provider: default_provider,
            model: model.clone(),
            fallback_cooldown_secs: None,
        }]
    } else {
        vec![]
    };

    let (profile_model, model_chain_state) = if !models.is_empty() {
        let first_model = models[0].model.clone();
        let state = crate::domain::ModelChainState::new(models, registry.default_cooldown_secs());
        (first_model, Some(state))
    } else {
        ("gpt-4.1-mini".to_string(), None) // fallback
    };

    debug!(
        event = "PersistentAgentSpawned",
        agent_id = %id,
        agent_name = %entry.name,
        agent_model = %profile_model,
        has_model_chain = model_chain_state.is_some(),
        "spawning persistent agent"
    );

    let tool_permissions = entry
        .tools
        .clone()
        .map(AgentToolPermissions::from)
        .unwrap_or_default();

    let mut entity_commands = commands.spawn(Agent {
        id,
        profile: AgentProfile {
            name: entry.name.clone(),
            model: profile_model,
        },
        capabilities: AgentCapabilities {
            tags: entry.tags.clone(),
            description: entry.description.clone(),
        },
        kind: AgentKind::Persistent,
        parent_id: None,
        bound_task_id: None,
        tool_permissions,
    });

    // 附加 ModelChainState Component
    if let Some(state) = model_chain_state {
        entity_commands.insert(state);
    }
}

/// 从 PluginRegistry 收集所有插件贡献的 AgentEntry，
/// 将 name 命名空间化为 `plugin_id:agent_name`。
fn collect_plugin_agent_entries(
    registry: Option<&crate::user_plugins::registry::PluginRegistry>,
) -> Vec<(String, crate::domain::AgentEntry)> {
    let Some(registry) = registry else {
        return Vec::new();
    };

    let mut entries = Vec::new();
    for plugin in registry.plugins() {
        for agent_contrib in &plugin.manifest.agents {
            let path = plugin.root_dir.join(&agent_contrib.profile);
            let Ok(content) = fs::read_to_string(&path) else {
                warn!(
                    event = "PluginAgentProfileNotFound",
                    plugin_id = %plugin.manifest.id,
                    path = %path.display(),
                    "plugin agent profile file not found, skipping"
                );
                continue;
            };
            let Ok(mut entry): Result<crate::domain::AgentEntry, _> = toml::from_str(&content)
            else {
                warn!(
                    event = "PluginAgentProfileParseError",
                    plugin_id = %plugin.manifest.id,
                    path = %path.display(),
                    "failed to parse plugin agent profile, skipping"
                );
                continue;
            };
            let namespaced_name = plugin.namespaced_agent_id(&entry.name);
            entry.name = namespaced_name.clone();
            entries.push((namespaced_name, entry));
        }
    }
    entries
}

fn handle_spawn_request(
    commands: &mut Commands,
    agents: &Query<(Entity, &Agent)>,
    tasks: &mut Query<&mut Task>,
    clock: &Clock,
    registry: &SpaceToolRegistry,
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

    // 过滤 tools：保留父 Agent 有 Allow 或 Confirm 权限的工具
    let allowed_tools: Vec<String> = request
        .tools
        .iter()
        .filter(|tool| {
            let perm = parent_agent.tool_permissions.get_permission(tool);
            !matches!(perm, crate::domain::ToolPermission::Deny)
        })
        .cloned()
        .collect();

    // 只在请求了工具但全部无效时才拒绝
    // 空工具列表是合法的，表示纯 LLM 对话任务
    if allowed_tools.is_empty() && !request.tools.is_empty() {
        warn!(
            event = "SpawnRequestRejected",
            parent_id = %request.parent_agent_id,
            task_id = %request.task_id,
            requested_tools = ?request.tools,
            reason = "all_requested_tools_denied",
            "spawn rejected: all requested tools are denied for parent agent"
        );
        let msg = format!(
            "Agent spawn rejected: all requested tools {:?} are denied for parent agent",
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
    });

    // 更新 Task 的 delegate 为实际执行的 task-scoped agent
    if let Some(mut task) = tasks.iter_mut().find(|t| t.id == request.task_id) {
        task.delegate = Some(id);
    }

    // 从 registry 构建子 Agent 的工具列表
    let child_tools: Vec<crate::domain::ToolDefinition> = registry
        .iter()
        .filter(|td| allowed_tools.contains(&td.name))
        .cloned()
        .collect();

    let execution_request = AgentExecutionRequest {
        task_id: request.task_id,
        agent_id: id,
        request_kind: crate::domain::AgentRequestKind::LlmCompletion,
        prompt: if request.task_prompt.is_empty() {
            request.description.clone()
        } else {
            request.task_prompt.clone()
        },
        system_prompt: request.task_system_prompt.clone(),
        tools: child_tools,
        conversation: None,
        work_item_id: None,
        model_override: None,
    };

    commands.spawn((
        AgentExecutionRequestMessage {
            request: execution_request,
        },
        MessageDispatchedHookPending,
    ));
}

fn handle_termination(
    commands: &mut Commands,
    agents: &Query<(Entity, &Agent)>,
    tasks: &mut Query<&mut Task>,
    _task_id: TaskId,
) {
    for (entity, agent) in agents {
        if agent.kind != AgentKind::TaskScoped {
            continue;
        }
        let Some(bound_task_id) = agent.bound_task_id else {
            continue;
        };
        let is_terminal = tasks
            .iter()
            .find(|t| t.id == bound_task_id)
            .is_some_and(|task| task.status.is_terminal());
        if is_terminal {
            debug!(
                event = "TaskScopedAgentStopping",
                agent_id = %agent.id,
                agent_name = %agent.profile.name,
                task_id = %bound_task_id,
                "marking task-scoped agent for stopping after task termination"
            );
            // 不直接 despawn，而是插入标记，由 agent_stopped_hook_system
            // 派发 OnAgentStopped hook 后再 despawn。
            commands.entity(entity).insert(AgentStoppingHookPending);
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
