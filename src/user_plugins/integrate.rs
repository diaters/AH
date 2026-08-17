//! 插件贡献集成
//!
//! 提供可复用的函数，将 `PluginRegistry` 中的插件技能贡献注册到 World 的
//! `PluginSkillContributions` Resource。
//!
//! 插件工具的注册由 `systems::tools` 侧主动拉取完成（方向反转，
//! user_plugins 不反向调用 systems）；启动阶段与 /reload-plugins 的
//! 工具注册都走 `systems::tools::register_plugin_tools_in_world`。

use crate::prelude::World;

use crate::infrastructure::skills::{PluginSkillContributions, PluginSkillEntry};
use crate::user_plugins::registry::PluginRegistry;

/// 将 `PluginRegistry` 中的插件技能贡献注册到 World 的 Resource。
///
/// 插件贡献的 Agent 不在此处处理——Agent 需要 model 配置，
/// 仅在启动阶段通过 `load_agents_system` 加载。
pub fn integrate_plugin_contributions(world: &mut World, registry: &PluginRegistry) {
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
