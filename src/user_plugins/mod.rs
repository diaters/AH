//! 用户插件系统
//!
//! 提供 `.harness/plugins/<id>/` 下的用户扩展加载、hook 派发与 Host API。
//! 详见 `docs/superpowers/specs/2026-06-23-plugin-system-design.md`。

pub mod dispatcher;
pub mod hook_point;
pub mod host_api;
pub mod loader;
pub mod manifest;
pub mod registry;
pub mod reload;
pub mod slash_command;
pub mod tool_executor;

/// 核心 Host API 版本。manifest 的 `api_version` 必须与此相等才能加载。
pub const API_VERSION: u32 = 1;

use bevy::prelude::*;
use std::path::PathBuf;

use crate::infrastructure::skills::{PluginSkillContributions, PluginSkillEntry};
use crate::user_plugins::loader::{DEFAULT_PLUGINS_DIR, load_plugins_from_dir};

/// Startup 系统：扫描 `.harness/plugins/` 并把 registry 插入 world。
///
/// 同时从 PluginRegistry 中提取所有已声明 skill 的路径，
/// 构建并插入 `PluginSkillContributions` 资源。
/// 最后，将插件贡献的 Tool 以 `plugin_id:tool_id` 命名空间注册到
/// SpaceToolRegistry 和 BuiltinToolExecutors。
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

    // 从已加载插件的 manifest.skills 中提取 skill 条目
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

    // 注册插件贡献的 Tool（在同一个系统内完成，避免 Startup 阶段命令延迟问题）
    crate::systems::tools::register_plugin_tools(
        &mut tool_registry,
        &mut tool_executors,
        &registry,
    );

    commands.insert_resource(registry);
    commands.insert_resource(skill_contributions);
}
