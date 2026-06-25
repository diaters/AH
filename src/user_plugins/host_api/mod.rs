//! 注册到 Rhai Engine 的 Host API 表面。
//!
//! 每个子模块导出一个 `register(Engine, ctx)` 函数，由
//! `register_all` 在派发前一次性注册。

pub mod approval;
pub mod entity_query;
pub mod entity_write;
pub mod experience;
pub mod log;
pub mod message;
pub mod plugin_resource;
pub mod skills_meta;
pub mod state;
pub mod temp_resource;
pub mod tool_control;

use rhai::Engine;

use crate::user_plugins::dispatcher::PluginContext;

/// 用给定 PluginContext 在 engine 上注册全部 v1 host API。
///
/// dispatcher 在派发每个 hook 脚本前调用此函数构造独立 Engine 实例。
pub fn register_all(engine: &mut Engine, ctx: &PluginContext) {
    log::register(engine);
    state::register(engine);
    entity_query::register(engine, ctx.snapshot.clone());
    entity_write::register(engine, ctx.writer.clone());
    tool_control::register(engine, ctx.outcome.clone());
    plugin_resource::register(engine, ctx.plugin_roots.clone());
    approval::register(engine, ctx.approval.clone());
    experience::register(engine, ctx.experience.clone());
    skills_meta::register(engine, ctx.skills.clone());
    message::register(engine, ctx.message.clone());
    temp_resource::register(engine, ctx.temp_resource.clone());

    // 注册 Vec<String> 的基础方法，供 entity query 返回值使用。
    // 沙箱引擎不加载 Rhai stdlib，因此需要显式注册。
    engine.register_fn("len", |v: &mut Vec<String>| -> i64 { v.len() as i64 });
    engine.register_fn("push", |v: &mut Vec<String>, item: &str| v.push(item.to_string()));
    engine.register_indexer_get(|v: &mut Vec<String>, idx: i64| -> String {
        v.get(idx as usize).cloned().unwrap_or_default()
    });
}
