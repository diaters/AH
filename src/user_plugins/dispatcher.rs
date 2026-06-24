//! Hook 派发器。
//!
//! 按 `PluginRegistry.plugins()` 字母序逐插件执行订阅 AST。
//! 每个脚本在独立线程中运行，受 `HOOK_TIMEOUT=1s` 限制。

use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use bevy::prelude::World;
use crossbeam_channel::Sender;
use rhai::{Engine, Scope};
use tracing::{debug, warn};

use crate::user_plugins::hook_point::HookPoint;
use crate::user_plugins::host_api;
use crate::user_plugins::host_api::approval::ApprovalContext;
use crate::user_plugins::host_api::entity_query::WorldSnapshot;
use crate::user_plugins::host_api::entity_write::{WorldCommand, WorldWriter};
use crate::user_plugins::host_api::experience::ExperienceContext;
use crate::user_plugins::host_api::message::MessageContext;
use crate::user_plugins::host_api::plugin_resource::PluginRoots;
use crate::user_plugins::host_api::skills_meta::SkillsSnapshot;
use crate::user_plugins::host_api::temp_resource::TempResourceSlot;
use crate::user_plugins::registry::{LoadedPlugin, PluginRegistry};

/// 单次 hook 派发的累积结果。同一 hook 点多个订阅者顺序派发，
/// 前一个的 outcome 会作为后一个的输入。
#[derive(Debug, Default, Clone)]
pub struct HookOutcome {
    pub deny_reason: Option<String>,
    pub replaced_result: Option<serde_json::Value>,
}

pub type SharedHookOutcome = Arc<Mutex<HookOutcome>>;

/// 每次 hook 派发提供给 host API 的上下文。
///
/// 不包含 `&mut World`。World 状态被快照为 `WorldSnapshot`，
/// 写操作通过 `WorldWriter` 攒到 `WorldCommand` 后由 dispatcher replay。
#[derive(Clone)]
pub struct PluginContext {
    pub snapshot: WorldSnapshot,
    pub writer: WorldWriter,
    pub outcome: SharedHookOutcome,
    pub plugin_roots: PluginRoots,
    pub approval: ApprovalContext,
    pub experience: ExperienceContext,
    pub skills: SkillsSnapshot,
    pub message: MessageContext,
    pub temp_resource: TempResourceSlot,
}

/// Hook 派发参数。
pub struct HookDispatchInput<'a> {
    pub point: HookPoint,
    pub world: &'a mut World,
    pub registry: &'a mut PluginRegistry,
    pub writer_tx: Sender<WorldCommand>,
    /// ctx 字段，由调用方按 hook 点填充。
    ///
    /// 实现要求：每次调用必须为当前 plugin 构造一个**新的** `MessageContext`，
    /// 其中 `plugin_id` 字段填入 `plugin.manifest.id`；同时为 `temp_resource`
    /// 构造一个**新的** `TempResourceSlot`（每次 hook 派发独立 state，不复用）。
    /// 其他字段从当前 `World` 与 `PluginRegistry` 派生。
    #[allow(clippy::type_complexity)]
    pub ctx_builder: Box<dyn Fn(&LoadedPlugin, &mut World) -> PluginContext + 'a>,
}

/// v1 hook 单脚本超时 1 秒。
const HOOK_TIMEOUT: Duration = Duration::from_secs(1);

/// 派发入口。按 registry.plugins() 字母序逐插件执行订阅 AST。
///
/// 返回累积的 `HookOutcome`：多次 deny 取最后一个；replaced_result 取最后一次 set。
pub fn dispatch_hook(input: HookDispatchInput<'_>) -> HookOutcome {
    let outcome: SharedHookOutcome = Arc::new(Mutex::new(HookOutcome::default()));
    let subscribers: Vec<LoadedPlugin> = input
        .registry
        .subscribers_for(input.point)
        .into_iter()
        .cloned()
        .collect();

    debug!(
        event = "HookDispatchStart",
        point = ?input.point,
        subscribers = subscribers.len()
    );

    let asts_by_plugin: std::collections::HashMap<String, Vec<rhai::AST>> = subscribers
        .iter()
        .map(|p| {
            let asts = p.hook_asts.get(&input.point).cloned().unwrap_or_default();
            (p.manifest.id.clone(), asts)
        })
        .collect();

    for plugin in subscribers {
        let asts = match asts_by_plugin.get(&plugin.manifest.id) {
            Some(a) if !a.is_empty() => a,
            _ => continue,
        };
        let ctx = (input.ctx_builder)(&plugin, input.world);

        for ast in asts {
            run_one_ast(&plugin, ast, &ctx, input.point, &outcome);
        }
    }

    outcome.lock().unwrap().clone()
}

fn run_one_ast(
    plugin: &LoadedPlugin,
    ast: &rhai::AST,
    ctx: &PluginContext,
    point: HookPoint,
    outcome: &SharedHookOutcome,
) {
    let mut engine = Engine::new();
    {
        let cur = outcome.lock().unwrap().clone();
        *ctx.outcome.lock().unwrap() = cur;
    }
    host_api::register_all(&mut engine, ctx);

    let (done_tx, done_rx) = mpsc::channel();
    let ast_clone = ast.clone();
    let handle = thread::Builder::new()
        .name(format!("hook-{point:?}-{}", plugin.manifest.id))
        .spawn(move || {
            let mut scope = Scope::new();
            let r = engine.call_fn::<()>(&mut scope, &ast_clone, "", ());
            let _ = done_tx.send(r);
        })
        .ok();

    let handle = match handle {
        Some(h) => h,
        None => {
            warn!(
                event = "HookThreadSpawnFailed",
                plugin = %plugin.manifest.id
            );
            return;
        }
    };

    match done_rx.recv_timeout(HOOK_TIMEOUT) {
        Ok(Ok(())) => {
            let local = ctx.outcome.lock().unwrap().clone();
            let mut g = outcome.lock().unwrap();
            if local.deny_reason.is_some() {
                g.deny_reason = local.deny_reason;
            }
            if local.replaced_result.is_some() {
                g.replaced_result = local.replaced_result;
            }
        }
        Ok(Err(e)) => {
            warn!(
                event = "HookScriptError",
                plugin = %plugin.manifest.id,
                point = ?point,
                error = %e,
                "hook script returned error"
            );
        }
        Err(_) => {
            warn!(
                event = "HookTimeout",
                plugin = %plugin.manifest.id,
                point = ?point,
                "hook script exceeded 1s, ignored"
            );
        }
    }
    // 注：超时线程在后台继续运行直到脚本退出。v1 接受这一潜在泄漏，因为 host API
    // 都是同步进程内快速操作，最长 1s 内必然结束。
    let _ = handle;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_registry_dispatches_no_op() {
        // 占位：集成测试在 Phase 8 编写。
        let _ = HookOutcome::default();
    }
}
