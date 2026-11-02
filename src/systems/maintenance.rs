use std::fs;

use crate::ecs::{EntityIndex, spawn_agent};
use crate::prelude::*;
use tracing::{debug, error, warn};

use crate::{
    domain::{Agent, AgentCapabilities, AgentProfile, AgentToolPermissions, SpaceToolRegistry},
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

fn load_persistent_agents(
    commands: &mut Commands,
    index: &mut EntityIndex,
    settings: &HarnessSettings,
    agents: &Query<(Entity, &Agent)>,
    registry: &crate::llm::ExecutorRegistry,
    plugin_registry: Option<&crate::user_plugins::registry::PluginRegistry>,
) {
    let config_path = &settings.0.agents_config_path;

    let config = match crate::infrastructure::agents_config::load_agent_config(
        std::path::Path::new(config_path),
    ) {
        Ok(Some(config)) => config,
        Ok(None) => {
            warn!(
                event = "AgentsConfigNotFound",
                config_path = %config_path,
                "agents config file not found, no persistent agents loaded"
            );
            return;
        }
        Err(err) => {
            // 启动期 validate_agent_config 已做 fail-fast；此处为防御兜底
            error!(
                event = "AgentsConfigParseError",
                config_path = %config_path,
                error = %err,
                "failed to parse agents config, skipping persistent agent load"
            );
            return;
        }
    };

    let mut seen_names = std::collections::HashSet::new();
    for entry in &config.agent {
        if !seen_names.insert(entry.name.clone()) {
            // 启动期校验已拦截；此处为防御兜底，跳过而非 panic
            error!(
                event = "AgentsConfigDuplicateName",
                config_path = %config_path,
                agent_name = %entry.name,
                "duplicate agent name in config, skipping persistent agent load"
            );
            return;
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
    let id = crate::domain::AgentId::new();

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
            vision: false,
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

/// O7 启动期 required_tag 孤儿扫描。
///
/// 遍历 `SpaceToolRegistry` 中所有工具的 `required_tag`，若某工具声明了
/// `required_tag` 但当前所有 Agent 的 `capabilities.tags` 都不包含该 tag，
/// 则发出 `RequiredTagOrphan` warn。
///
/// 语义：该工具在当前 Agent 集合下不可被路由（无人能满足 required_tag），
/// 但不阻止启动。
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
                 an agent with this tag is configured"
            );
        }
    }
}

/// O7 启动期 required_tag 孤儿扫描 system。
///
/// 通过 `Local<bool>` 保证只在第一次 update 时运行一次，扫描启动期已加载的
/// 持久化 Agent 与 `SpaceToolRegistry` 中工具的 `required_tag` 匹配关系。
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
mod required_tag_tests {
    //! O7 required_tag 启动期孤儿扫描测试
    //!
    //! 验证 `validate_required_tags` 在以下场景不 panic（warn 不 panic）：
    //! - 工具声明 required_tag 且 agent 持有该 tag
    //! - 工具无 required_tag 且 agents 列表为空

    use super::*;
    use crate::domain::{ToolDefinition, ToolExecutorKind, ToolPermission, ToolSchema};

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
            id: crate::domain::AgentId::nil(),
            profile: AgentProfile {
                name: "test".to_string(),
                model: "m".to_string(),
            },
            capabilities: AgentCapabilities {
                tags,
                description: String::new(),
            },
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
