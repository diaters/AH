//! 用户插件系统
//!
//! 提供 `.harness/plugins/<id>/` 下的用户扩展加载、hook 派发与 Host API。
//! 详见 `docs/superpowers/specs/2026-06-23-plugin-system-design.md`。

pub mod manifest;
pub mod loader;
pub mod registry;
pub mod hook_point;
pub mod dispatcher;
pub mod host_api;
pub mod tool_executor;
pub mod slash_command;
pub mod reload;

/// 核心 Host API 版本。manifest 的 `api_version` 必须与此相等才能加载。
pub const API_VERSION: u32 = 1;
