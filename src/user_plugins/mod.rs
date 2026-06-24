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

use crate::user_plugins::loader::{DEFAULT_PLUGINS_DIR, load_plugins_from_dir};

/// Startup 系统：扫描 `.harness/plugins/` 并把 registry 插入 world。
pub fn plugin_load_startup_system(mut commands: Commands) {
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

    commands.insert_resource(registry);
}
