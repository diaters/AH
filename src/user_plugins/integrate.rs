//! 插件贡献集成
//!
//! 提供可复用的函数，将 `PluginRegistry` 中的插件贡献注册到 World 的各个 Resource：
//! - SpaceToolRegistry + BuiltinToolExecutors（插件工具）
//! - PluginSkillContributions（插件技能）
//!
//! 启动阶段和 /reload-plugins 均调用此模块，避免重复逻辑。

use crate::prelude::World;

use crate::infrastructure::skills::{PluginSkillContributions, PluginSkillEntry};
use crate::systems::tools::register_plugin_tools;
use crate::user_plugins::registry::PluginRegistry;

/// 将 `PluginRegistry` 中的插件贡献注册到 World 的各 Resource。
///
/// 包括：
/// - 插件工具 → SpaceToolRegistry + BuiltinToolExecutors
/// - 插件技能 → PluginSkillContributions
///
/// 插件贡献的 Agent 不在此处处理——Agent 需要 model 配置，
/// 仅在启动阶段通过 `load_agents_system` 加载。
pub fn integrate_plugin_contributions(world: &mut World, registry: &PluginRegistry) {
    // 注册插件贡献的工具：逐个 resource_scope 避免同时 &mut World。
    world.resource_scope(
        |world: &mut World,
         mut space: bevy_ecs::change_detection::Mut<crate::domain::SpaceToolRegistry>| {
            if let Some(mut execs) = world.get_resource_mut::<crate::domain::BuiltinToolExecutors>()
            {
                register_plugin_tools(&mut space, &mut execs, registry);
            }
        },
    );

    // 注册插件贡献的技能
    let skill_contributions = PluginSkillContributions {
        entries: registry
            .plugins()
            .iter()
            .flat_map(|plugin| {
                plugin.manifest.skills.iter().map(|skill| PluginSkillEntry {
                    plugin_id: plugin.manifest.id.clone(),
                    skill_id: skill.id.clone(),
                    path: plugin.root_dir.join(&skill.path),
                })
            })
            .collect(),
    };

    if !skill_contributions.entries.is_empty() {
        tracing::info!(
            event = "PluginSkillsRegistered",
            count = skill_contributions.entries.len(),
            "plugin skill contributions registered"
        );
    }

    // 合并到已有的 PluginSkillContributions 资源（reload 场景下资源已存在）
    if let Some(mut existing) = world.get_resource_mut::<PluginSkillContributions>() {
        existing.entries.extend(skill_contributions.entries);
    } else {
        world.insert_resource(skill_contributions);
    }
}
