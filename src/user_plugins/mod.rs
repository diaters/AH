//! 用户插件系统
//!
//! 提供 `.harness/plugins/<id>/` 下的用户扩展加载、hook 派发与 Host API。
//! 详见 `docs/superpowers/specs/2026-06-23-plugin-system-design.md`。

pub mod dispatcher;
pub mod host_api;
pub mod integrate;
pub mod loader;
pub mod manifest;
pub mod registry;
pub mod reload;
pub mod slash_command;
pub mod tool_executor;

/// 核心 Host API 版本。manifest 的 `api_version` 必须与此相等才能加载。
pub const API_VERSION: u32 = 1;

use std::path::PathBuf;

use crate::prelude::World;
use crate::user_plugins::loader::{DEFAULT_PLUGINS_DIR, load_plugins_from_dir};

/// Startup 系统：扫描 `.harness/plugins/` 并把 registry 插入 world。
///
/// 只负责加载 registry 与插件技能贡献；插件工具的注册由
/// `systems::tools::register_plugin_tools_startup_system` 在本系统之后
/// 主动拉取完成（方向反转，user_plugins 不反向调用 systems）。
pub fn plugin_load_startup_system(world: &mut World) {
    let plugins_dir = PathBuf::from(
        std::env::var("HARNESS_PLUGINS_DIR").unwrap_or_else(|_| DEFAULT_PLUGINS_DIR.to_string()),
    );
    let registry = load_plugins_from_dir(&plugins_dir);
    let loaded: Vec<String> = registry
        .plugins()
        .iter()
        .map(|p| p.manifest.id.clone())
        .collect();
    let failed: Vec<String> = registry
        .failures()
        .iter()
        .map(|f| format!("{}: {}", f.plugin_id.as_deref().unwrap_or("?"), f.error))
        .collect();

    if loaded.is_empty() && failed.is_empty() {
        tracing::debug!(
            event = "PluginsEmpty",
            "no plugins found in {}",
            plugins_dir.display()
        );
    } else {
        tracing::info!(
            event = "PluginsLoadedSummary",
            loaded = ?loaded,
            failed = ?failed,
            "[plugins] summary"
        );
        eprintln!("[plugins] loaded: {}", loaded.join(", "));
        if !failed.is_empty() {
            eprintln!("[plugins] failed: {}", failed.join("; "));
        }
    }

    // 从已加载插件的 manifest.skills 中提取 skill 条目
    let skill_contributions = crate::infrastructure::skills::PluginSkillContributions {
        entries: registry
            .plugins()
            .iter()
            .flat_map(|plugin| {
                plugin.manifest.skills.iter().map(|skill| {
                    crate::infrastructure::skills::PluginSkillEntry {
                        plugin_id: plugin.manifest.id.clone(),
                        skill_id: skill.id.clone(),
                        path: plugin.root_dir.join(&skill.path),
                    }
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

    world.insert_resource(registry);
    world.insert_resource(skill_contributions);
}
