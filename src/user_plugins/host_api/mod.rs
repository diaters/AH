//! 注册到 Rhai Engine 的 Host API 表面。
//!
//! 每个子模块导出一个 `register(Engine, ctx)` 函数，由
//! `register_all` 在派发前一次性注册。

use rhai::Engine;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub mod approval;
pub mod entity_query;
pub mod entity_write;
pub mod experience;
pub mod log;
pub mod plugin_resource;
pub mod state;
pub mod tool_control;

use crate::user_plugins::dispatcher::{HookOutcome, SharedHookOutcome};
use entity_query::WorldSnapshot;
use entity_write::WorldWriter;

/// 把所有 host API 注册到给定 Engine 上。
///
/// 每次派发 hook 时，dispatcher 会为本插件构造一个独立的 Engine 实例，
/// 调用此函数后再注入插件上下文（plugin_id、ctx），最后执行 AST。
pub fn register_all(engine: &mut Engine) {
    let (tx, _rx) = crossbeam_channel::unbounded();
    approval::register(
        engine,
        approval::ApprovalContext {
            current_request_id: None,
            tx: tx.clone(),
        },
    );
    entity_query::register(engine, WorldSnapshot::empty());
    entity_write::register(engine, WorldWriter::new(tx.clone()));
    experience::register(
        engine,
        experience::ExperienceContext {
            store: Arc::new(crate::domain::ExperienceStore::default()),
            tx: tx.clone(),
        },
    );
    log::register(engine);
    plugin_resource::register(engine, plugin_resource::PluginRoots::single(PathBuf::new()));
    state::register(engine);
    let outcome: SharedHookOutcome = Arc::new(Mutex::new(HookOutcome::default()));
    tool_control::register(engine, outcome);
}
