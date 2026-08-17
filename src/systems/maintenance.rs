use std::fs;

use crate::ecs::{EntityIndex, spawn_agent};
use crate::prelude::*;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::{
    contracts::Clock,
    domain::{
        Agent, AgentCapabilities, AgentExecutionRequest, AgentExecutionRequestMessage, AgentKind,
        AgentProfile, AgentSpawnRequestMessage, AgentStoppingHookPending, AgentToolPermissions,
        FailureReason, MessageDispatchedHookPending, SpaceToolRegistry, Task, TaskId,
        TaskTerminatedMessage, ToolPermission,
    },
    systems::HarnessSettings,
};

/// Startup 系统：加载持久化 Agent
///
/// 先从配置文件加载，再合并插件贡献的 Agent。
pub(crate) fn load_agents_system(
    mut commands: Commands,
    settings: Res<HarnessSettings>,
    mut index: ResMut<EntityIndex>,
    agents: Query<(Entity, &Agent)>,
    registry: Res<crate::llm::ExecutorRegistry>,
    plugin_registry: Option<Res<crate::user_plugins::registry::PluginRegistry>>,
) {
    load_persistent_agents(
        &mut commands,
        &mut index,
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
    mut index: ResMut<EntityIndex>,
    agents: Query<(Entity, &Agent)>,
    mut tasks: Query<&mut Task>,
    spawn_requests: Query<(Entity, &AgentSpawnRequestMessage)>,
    terminated_messages: Query<(Entity, &TaskTerminatedMessage)>,
) {
    for (entity, request) in &spawn_requests {
        handle_spawn_request(
            &mut commands,
            &mut index,
            &agents,
            &mut tasks,
            &clock,
            &registry,
            request,
        );
        commands.entity(entity).despawn();
    }

    for (entity, terminated) in &terminated_messages {
        handle_termination(
            &mut commands,
            &agents,
            &mut tasks,
            &index,
            terminated.task_id,
        );
        commands.entity(entity).despawn();
    }
}

fn load_persistent_agents(
    commands: &mut Commands,
    index: &mut EntityIndex,
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

    // 预防性检查：profile-designer Agent 是否存在（孵化流程依赖此 Agent）
    let has_profile_designer = config
        .agent
        .iter()
        .any(|e| e.tags.iter().any(|t| t == "profile") || e.name == "profile-designer");
    if !has_profile_designer {
        warn!(
            event = "ProfileDesignerAgentMissing",
            "profile-designer Agent not found in agents.toml; \
             incubation flow will fail until a profile-designer Agent is configured"
        );
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
        spawn_persistent_agent_from_entry(commands, index, entry, registry);
    }

    // 合并插件贡献的 Agent
    for (_, entry) in &plugin_agent_entries {
        debug!(
            event = "PluginAgentSpawned",
            agent_name = %entry.name,
            "spawning plugin-contributed persistent agent"
        );
        spawn_persistent_agent_from_entry(commands, index, entry, registry);
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
    index: &mut EntityIndex,
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

    let entity = spawn_agent(
        commands,
        index,
        Agent {
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
            system_prompt: entry.system_prompt.clone(),
        },
    );

    // 附加 ModelChainState Component
    if let Some(state) = model_chain_state {
        commands.entity(entity).insert(state);
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
    index: &mut EntityIndex,
    agents: &Query<(Entity, &Agent)>,
    tasks: &mut Query<&mut Task>,
    clock: &Clock,
    registry: &SpaceToolRegistry,
    request: &AgentSpawnRequestMessage,
) {
    // UUID 寻址改用 EntityIndex O(1) 解析
    let Some((_, parent_agent)) = index
        .get_agent(&request.parent_agent_id)
        .and_then(|e| agents.get(e).ok())
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
            index,
            clock,
            request.task_id,
            "parent agent not found for spawn request",
        );
        return;
    };

    // 过滤 tools：保留父 Agent 非 Deny 的工具，并继承父的 effective_permission
    let parent_perms: Vec<(String, ToolPermission)> = request
        .tools
        .iter()
        .filter_map(|tool| {
            let (perm, _source) = parent_agent.effective_permission(tool, Some(registry));
            if perm == ToolPermission::Deny {
                return None;
            }
            Some((tool.clone(), perm))
        })
        .collect();

    let allowed_tools: Vec<String> = parent_perms.iter().map(|(t, _)| t.clone()).collect();

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
        mark_task_failed(tasks, index, clock, request.task_id, &msg);
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

    // 构建 tool_permissions: 子 Agent 默认拒绝，按工具逐个继承父的 effective_permission
    let tool_permissions = AgentToolPermissions {
        default_permission: ToolPermission::Deny,
        default_permission_explicit: true,
        overrides: parent_perms.into_iter().collect(),
    };

    // 权限审计（tracing log）：子 Agent 继承父权限的每个 override。
    // 不发 EngineEvent——spawn 路径无 frontend_registry 访问，且继承日志
    // 仅供可观测性消费，无需推前端。grant_permission 的日志已在
    // Agent::grant_permission 方法内（任务 2），此处仅记录继承映射。
    for (tool, perm) in &tool_permissions.overrides {
        info!(
            event = "PermissionInherit",
            agent_id = %id,
            tool_name = %tool,
            permission = ?perm,
            context = "SpawnInherit",
            "子 Agent 继承父权限"
        );
    }

    spawn_agent(
        commands,
        index,
        Agent {
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
            system_prompt: None,
        },
    );

    // 更新 Task 的 delegate 为实际执行的 task-scoped agent
    // UUID 寻址改用 EntityIndex O(1) 解析
    if let Some(mut task) = index
        .get_task(&request.task_id)
        .and_then(|e| tasks.get_mut(e).ok())
    {
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
    index: &EntityIndex,
    _task_id: TaskId,
) {
    for (entity, agent) in agents {
        if agent.kind != AgentKind::TaskScoped {
            continue;
        }
        let Some(bound_task_id) = agent.bound_task_id else {
            continue;
        };
        // UUID 寻址改用 EntityIndex O(1) 解析
        let is_terminal = index
            .get_task(&bound_task_id)
            .and_then(|e| tasks.get(e).ok())
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
    index: &EntityIndex,
    clock: &Clock,
    task_id: TaskId,
    error_message: &str,
) {
    // UUID 寻址改用 EntityIndex O(1) 解析
    if let Some(mut task) = index.get_task(&task_id).and_then(|e| tasks.get_mut(e).ok()) {
        task.last_error = Some(error_message.to_string());
        task.status = crate::domain::TaskStatus::Failed(FailureReason::AgentError);
        task.updated_at = clock.0;
    }
}

/// O7 启动期 required_tag 孤儿扫描。
///
/// 遍历 `SpaceToolRegistry` 中所有工具的 `required_tag`，若某工具声明了
/// `required_tag` 但当前所有 Agent 的 `capabilities.tags` 都不包含该 tag，
/// 则发出 `RequiredTagOrphan` warn。
///
/// 语义：该工具在当前 Agent 集合下不可被路由（无人能满足 required_tag），
/// 但不阻止启动——task-scoped Agent 可能在运行时被 spawn 并持有该 tag。
pub fn validate_required_tags(registry: &SpaceToolRegistry, agents: &[Agent]) {
    use std::collections::HashSet;
    let all_tags: HashSet<&str> = agents
        .iter()
        .flat_map(|a| a.capabilities.tags.iter().map(|s| s.as_str()))
        .collect();
    for tool_def in registry.iter() {
        if let Some(required) = &tool_def.required_tag
            && !all_tags.contains(required.as_str())
        {
            warn!(
                event = "RequiredTagOrphan",
                tool_name = %tool_def.name,
                required_tag = %required,
                "no agent currently holds the required_tag; tool will be unusable until \
                 an agent with this tag is loaded (e.g., task-scoped agent at runtime)"
            );
        }
    }
}

/// O7 启动期 required_tag 孤儿扫描 system。
///
/// 通过 `Local<bool>` 保证只在第一次 update 时运行一次，扫描启动期已加载的
/// 持久化 Agent 与 `SpaceToolRegistry` 中工具的 `required_tag` 匹配关系。
/// task-scoped Agent 在运行时 spawn 不经此扫描（它们由父 Agent 显式授权）。
pub(crate) fn validate_required_tags_system(
    mut ran: Local<bool>,
    agents: Query<&Agent>,
    tool_registry: Res<SpaceToolRegistry>,
) {
    if *ran {
        return;
    }
    *ran = true;
    let agent_list: Vec<Agent> = agents.iter().cloned().collect();
    validate_required_tags(&tool_registry, &agent_list);
}

#[cfg(test)]
mod o2_inheritance_tests {
    //! O2 子 Agent 权限继承单元测试
    //!
    //! 验证 `handle_spawn_request` 中 `parent_perms` 的 filter_map 逻辑：
    //! 父 Confirm → 子 Confirm（不再降为 Allow）；父 Allow → 子 Allow；
    //! 父 Deny → 工具不传入子 overrides。

    use super::*;
    use crate::domain::{ToolDefinition, ToolExecutorKind, ToolSchema};
    use std::collections::HashMap;

    /// 构造父 Agent：default + explicit + overrides 完全可控
    fn make_parent(
        default: ToolPermission,
        explicit: bool,
        overrides: HashMap<String, ToolPermission>,
    ) -> Agent {
        Agent {
            id: Uuid::nil(),
            profile: AgentProfile {
                name: "parent".to_string(),
                model: "m".to_string(),
            },
            capabilities: AgentCapabilities {
                tags: vec![],
                description: String::new(),
            },
            kind: AgentKind::Persistent,
            parent_id: None,
            bound_task_id: None,
            tool_permissions: AgentToolPermissions {
                default_permission: default,
                default_permission_explicit: explicit,
                overrides,
            },
            system_prompt: None,
        }
    }

    /// 构造只含一个工具的 SpaceToolRegistry
    fn make_registry_with(tool_name: &str, perm: ToolPermission) -> SpaceToolRegistry {
        let mut registry = SpaceToolRegistry::default();
        registry.register(ToolDefinition {
            name: tool_name.to_string(),
            description: "test".to_string(),
            parameters: ToolSchema::default(),
            default_permission: perm,
            executor: ToolExecutorKind::Builtin(tool_name.to_string()),
            required_tag: None,
        });
        registry
    }

    /// 复刻 `handle_spawn_request` 中构造 `parent_perms` 的 filter_map 逻辑，
    /// 让单元测试不依赖 Commands / World 即可验证权限继承语义。
    fn collect_parent_perms(
        parent: &Agent,
        registry: &SpaceToolRegistry,
        tools: &[String],
    ) -> Vec<(String, ToolPermission)> {
        tools
            .iter()
            .filter_map(|tool| {
                let (perm, _source) = parent.effective_permission(tool, Some(registry));
                if perm == ToolPermission::Deny {
                    return None;
                }
                Some((tool.clone(), perm))
            })
            .collect()
    }

    /// 父 Confirm → 子 Confirm（不再降为 Allow）
    #[test]
    fn child_inherits_confirm_from_parent_confirm() {
        let mut overrides = HashMap::new();
        overrides.insert("shell_exec".to_string(), ToolPermission::Confirm);
        let parent = make_parent(ToolPermission::Deny, true, overrides);
        let registry = make_registry_with("shell_exec", ToolPermission::Allow);

        let parent_perms = collect_parent_perms(&parent, &registry, &["shell_exec".to_string()]);

        assert_eq!(parent_perms.len(), 1, "Confirm 工具应保留");
        assert_eq!(parent_perms[0].0, "shell_exec");
        assert_eq!(
            parent_perms[0].1,
            ToolPermission::Confirm,
            "父 Confirm 必须原样继承为子 Confirm，不得降级为 Allow"
        );
    }

    /// 父 Allow → 子 Allow
    #[test]
    fn child_inherits_allow_from_parent_allow() {
        let mut overrides = HashMap::new();
        overrides.insert("shell_exec".to_string(), ToolPermission::Allow);
        let parent = make_parent(ToolPermission::Deny, true, overrides);
        let registry = make_registry_with("shell_exec", ToolPermission::Confirm);

        let parent_perms = collect_parent_perms(&parent, &registry, &["shell_exec".to_string()]);

        assert_eq!(parent_perms.len(), 1, "Allow 工具应保留");
        assert_eq!(
            parent_perms[0].1,
            ToolPermission::Allow,
            "父 Allow 必须原样继承为子 Allow"
        );
    }

    /// 父 Deny → 工具不传入子 overrides
    #[test]
    fn child_excludes_denied_tool() {
        let mut overrides = HashMap::new();
        overrides.insert("shell_exec".to_string(), ToolPermission::Deny);
        let parent = make_parent(ToolPermission::Deny, true, overrides);
        let registry = make_registry_with("shell_exec", ToolPermission::Allow);

        let parent_perms = collect_parent_perms(&parent, &registry, &["shell_exec".to_string()]);

        assert!(
            parent_perms.is_empty(),
            "父 Deny 的工具必须被排除，不得进入子 overrides"
        );
    }
}

#[cfg(test)]
mod required_tag_tests {
    //! O7 required_tag 启动期孤儿扫描测试
    //!
    //! 验证 `validate_required_tags` 在以下场景不 panic（warn 不 panic）：
    //! - 工具声明 required_tag 且 agent 持有该 tag
    //! - 工具无 required_tag 且 agents 列表为空

    use super::*;
    use crate::domain::{ToolDefinition, ToolExecutorKind, ToolSchema};

    fn make_tool_def_with_required_tag(name: &str, required_tag: Option<&str>) -> ToolDefinition {
        ToolDefinition {
            name: name.to_string(),
            description: "test".to_string(),
            parameters: ToolSchema::default(),
            default_permission: ToolPermission::Allow,
            executor: ToolExecutorKind::Builtin(name.to_string()),
            required_tag: required_tag.map(|s| s.to_string()),
        }
    }

    fn make_agent_with_tags(tags: Vec<String>) -> Agent {
        Agent {
            id: Uuid::nil(),
            profile: AgentProfile {
                name: "test".to_string(),
                model: "m".to_string(),
            },
            capabilities: AgentCapabilities {
                tags,
                description: String::new(),
            },
            kind: AgentKind::Persistent,
            parent_id: None,
            bound_task_id: None,
            tool_permissions: AgentToolPermissions::default(),
            system_prompt: None,
        }
    }

    /// 工具声明 required_tag="profile"，agent 持有 tag="profile"，不 warn
    #[test]
    fn validate_required_tags_no_warn_when_tag_held() {
        let mut registry = SpaceToolRegistry::default();
        registry.register(make_tool_def_with_required_tag(
            "profile_tool",
            Some("profile"),
        ));
        let agents = vec![make_agent_with_tags(vec!["profile".to_string()])];

        // 不应 panic
        validate_required_tags(&registry, &agents);
    }

    /// 工具无 required_tag，空 agents 列表，不 warn
    #[test]
    fn validate_required_tags_no_warn_when_no_required_tag() {
        let mut registry = SpaceToolRegistry::default();
        registry.register(make_tool_def_with_required_tag("plain_tool", None));
        let agents: Vec<Agent> = vec![];

        // 不应 panic
        validate_required_tags(&registry, &agents);
    }
}
