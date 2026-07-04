//! 用户插件系统
//!
//! 提供 `.harness/plugins/<id>/` 下的用户扩展加载、hook 派发与 Host API。
//! 详见 `docs/superpowers/specs/2026-06-23-plugin-system-design.md`。

pub mod dispatcher;
pub mod hook_point;
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

use crate::prelude::*;
use std::path::PathBuf;

use crate::user_plugins::loader::{DEFAULT_PLUGINS_DIR, load_plugins_from_dir};

/// Startup 系统：扫描 `.harness/plugins/` 并把 registry 插入 world。
///
/// 加载插件注册表后，委托 `integrate::integrate_plugin_contributions`
/// 将插件贡献的工具和技能注册到对应 Resource。
pub fn plugin_load_startup_system(
    mut commands: Commands,
    mut tool_registry: ResMut<crate::domain::SpaceToolRegistry>,
    mut tool_executors: ResMut<crate::domain::BuiltinToolExecutors>,
) {
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

    // 注册插件贡献的工具（Startup 阶段需要直接操作 ResMut，无法走 &mut World）
    crate::systems::tools::register_plugin_tools(
        &mut tool_registry,
        &mut tool_executors,
        &registry,
    );

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

    commands.insert_resource(registry);
    commands.insert_resource(skill_contributions);
}
