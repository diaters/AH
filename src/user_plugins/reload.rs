//! /reload-plugins 实现
//!
//! 清除所有插件贡献（工具、技能、Agent 实体），重新扫描插件目录，
//! 重新集成所有贡献。

use bevy::prelude::World;

use crate::domain::{Agent, BuiltinToolExecutors, SpaceToolRegistry};
use crate::infrastructure::skills::PluginSkillContributions;
use crate::user_plugins::integrate::integrate_plugin_contributions;
use crate::user_plugins::loader::{DEFAULT_PLUGINS_DIR, load_plugins_from_dir};
use crate::user_plugins::registry::PluginRegistry;

/// 执行 /reload-plugins：清除所有插件贡献，重新扫描并集成。
///
/// 此函数需要 `&mut World`，通过 `ReloadPluginsMessage` 实体触发，
/// 由 `reload_plugins_system` 调用。
pub fn reload_plugins(world: &mut World) {
    tracing::info!(event = "PluginsReloading", "reload-plugins initiated");

    // 1) 收集当前已加载插件的 ID（用于按命名空间清理贡献）
    let stale_plugin_ids: Vec<String> = world
        .get_resource::<PluginRegistry>()
        .map(|r| {
            r.plugins()
                .iter()
                .map(|p| p.manifest.id.clone())
                .collect()
        })
        .unwrap_or_default();

    // 2) 清空 PluginRegistry
    if let Some(mut reg) = world.get_resource_mut::<PluginRegistry>() {
        reg.clear();
    }

    // 3) 移除插件贡献的工具
    if let Some(mut space) = world.get_resource_mut::<SpaceToolRegistry>() {
        let to_remove: Vec<String> = space
            .iter()
            .map(|t| t.name.clone())
            .filter(|name| stale_plugin_ids.iter().any(|pid| name.starts_with(&format!("{pid}:"))))
            .collect();
        for name in to_remove {
            space.remove(&name);
        }
    }
    if let Some(mut execs) = world.get_resource_mut::<BuiltinToolExecutors>() {
        let to_remove: Vec<String> = execs
            .iter_names()
            .filter(|n| stale_plugin_ids.iter().any(|pid| n.starts_with(&format!("{pid}:"))))
            .map(String::from)
            .collect();
        for n in to_remove {
            execs.remove(&n);
        }
    }

    // 4) 清空插件技能贡献
    if let Some(mut skills) = world.get_resource_mut::<PluginSkillContributions>() {
        skills.entries.clear();
    }

    // 5) Despawn 插件贡献的 Agent 实体（profile.name 以 "plugin_id:" 开头）
    let mut agents_to_despawn: Vec<bevy::prelude::Entity> = Vec::new();
    {
        let mut query = world.query::<(bevy::prelude::Entity, &Agent)>();
        for (entity, agent) in query.iter(world) {
            if stale_plugin_ids
                .iter()
                .any(|pid| agent.profile.name.starts_with(&format!("{pid}:")))
            {
                agents_to_despawn.push(entity);
            }
        }
    }
    for entity in agents_to_despawn {
        world.despawn(entity);
    }

    // 6) 重新扫描磁盘
    let plugins_dir = std::path::PathBuf::from(
        std::env::var("HARNESS_PLUGINS_DIR").unwrap_or_else(|_| DEFAULT_PLUGINS_DIR.to_string()),
    );
    let new_registry = load_plugins_from_dir(&plugins_dir);

    // 7) 重新集成贡献
    integrate_plugin_contributions(world, &new_registry);

    world.insert_resource(new_registry);
    tracing::info!(event = "PluginsReloaded", "reload-plugins complete");
}
