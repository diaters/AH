//! 注册到 Rhai Engine 的 Host API 表面。
//!
//! 每个子模块导出一个 `register(Engine)` 函数，由
//! `register_all` 在派发前一次性注册。

use rhai::Engine;

pub mod approval;
pub mod entity_query;
pub mod entity_write;
pub mod experience;
pub mod log;
pub mod plugin_resource;
pub mod state;
pub mod tool_control;

/// 把所有 host API 注册到给定 Engine 上。
///
/// 每次派发 hook 时，dispatcher 会为本插件构造一个独立的 Engine 实例，
/// 调用此函数后再注入插件上下文（plugin_id、ctx），最后执行 AST。
pub fn register_all(engine: &mut Engine) {
    approval::register(engine);
    entity_query::register(engine);
    entity_write::register(engine);
    experience::register(engine);
    log::register(engine);
    plugin_resource::register(engine);
    state::register(engine);
    tool_control::register(engine);
}
