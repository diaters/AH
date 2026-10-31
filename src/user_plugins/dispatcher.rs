//! Hook 派发器。
//!
//! 按 `PluginRegistry.plugins()` 字母序逐插件执行订阅 AST。
//! 每个脚本在独立线程中运行，受 `HOOK_TIMEOUT=200ms` 限制；一帧内的 hook 累计耗时记入
//! `HookFrameBudget`，超过 `HOOK_FRAME_BUDGET` 时由 `hook_budget_check_system` 告警。

use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;
use crossbeam_channel::Sender;
use tracing::{debug, warn};

use crate::domain::HookPoint;
use crate::domain::{ChannelId, DispatchHint, DispatchKind, FrontendKind, PendingDispatch, Task};
use crate::prelude::{ResMut, Resource, World};
use crate::user_plugins::host_api;
use crate::user_plugins::host_api::approval::ApprovalContext;
use crate::user_plugins::host_api::entity_query::WorldSnapshot;
use crate::user_plugins::host_api::entity_write::{WorldCommand, WorldWriter};
use crate::user_plugins::host_api::message::MessageContext;
use crate::user_plugins::host_api::plugin_resource::PluginRoots;
use crate::user_plugins::host_api::skills_meta::SkillsSnapshot;
use crate::user_plugins::host_api::temp_resource::TempResourceSlot;
use crate::user_plugins::host_api::tool_control::ToolCallContext;
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
    pub skills: SkillsSnapshot,
    pub message: MessageContext,
    pub temp_resource: TempResourceSlot,
    /// 当前工具调用上下文：仅 `on_tool_called` 前置 hook 派发时由调用方填充，
    /// 其余 hook 为默认空值（`tool_call_name()` 返回空串）。
    pub tool: ToolCallContext,
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
    pub ctx_builder: Box<dyn Fn(&LoadedPlugin) -> PluginContext + 'a>,
}

/// hook 单脚本超时：hook 在 ECS 线程上同步等待，超时过长会让整个主循环卡顿。
const HOOK_TIMEOUT: Duration = Duration::from_millis(200);

/// 一帧内 hook 累计耗时的告警阈值。
pub const HOOK_FRAME_BUDGET: Duration = Duration::from_millis(50);

/// 一帧内 hook 派发的累计耗时：`dispatch_hook` 累加，`hook_budget_check_system` 每帧检查并清零。
#[derive(Resource, Debug, Default)]
pub struct HookFrameBudget {
    pub spent: Duration,
}

/// 每帧末检查 hook 累计耗时，超过 `HOOK_FRAME_BUDGET` 时告警，然后清零。
pub fn hook_budget_check_system(mut budget: ResMut<HookFrameBudget>) {
    if budget.spent > HOOK_FRAME_BUDGET {
        warn!(
            event = "HookBudgetExceeded",
            spent_ms = budget.spent.as_millis() as u64,
            budget_ms = HOOK_FRAME_BUDGET.as_millis() as u64,
            "plugin hooks blocked the ECS thread longer than the per-frame budget"
        );
    }
    budget.spent = Duration::ZERO;
}

/// 派发入口。按 registry.plugins() 字母序逐插件执行订阅 AST。
///
/// 返回累积的 `HookOutcome`：多次 deny 取最后一个；replaced_result 取最后一次 set。
pub fn dispatch_hook(input: HookDispatchInput<'_>) -> HookOutcome {
    let started = Instant::now();
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
        let ctx = (input.ctx_builder)(&plugin);

        for ast in asts {
            run_one_ast(&plugin, ast, &ctx, input.point, &outcome);
        }
    }

    if let Some(mut budget) = input.world.get_resource_mut::<HookFrameBudget>() {
        budget.spent += started.elapsed();
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
    let mut engine = crate::user_plugins::loader::new_sandboxed_engine();
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
            // 执行 AST 的顶层语句（而非具名函数）。Rhai 的 `call_fn` 仅调用脚本内
            // 定义的具名函数，对空名 `""` 会返回 "Function not found"。用 `run_ast`
            // 执行脚本顶层并丢弃返回值。
            let r = engine.run_ast(&ast_clone);
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
                "hook script exceeded timeout, ignored"
            );
        }
    }
    // 注：超时线程在后台继续运行直到脚本退出；引擎的操作数上限保证脚本
    // （含死循环）有限步内终止，泄漏有界。
    let _ = handle;
}

/// replay hook 期间累积的 `WorldCommand` 到 `world`。
///
/// 在系统（system）调用 `dispatch_hook` 之后调用此函数，按到达顺序逐条应用
/// 插件通过 host API 写出的指令。失败（如 task_id 不存在）以 `debug!` 记录，
/// 不中断 flush。
pub fn flush_world_commands(world: &mut World, rx: &Receiver<WorldCommand>) {
    while let Ok(cmd) = rx.try_recv() {
        apply_world_command(world, cmd);
    }
}

/// 应用单条 `WorldCommand`。
///
/// `SetTaskMetadata` / `SetTaskTag` 因 Task 暂无对应字段，当前为记录日志的
/// 安全占位（不写回），其余变体均已实现。
fn apply_world_command(world: &mut World, cmd: WorldCommand) {
    match cmd {
        WorldCommand::CreateTask { title, parent } => {
            let channel = ChannelId {
                frontend: FrontendKind::Tui,
                user_id: "plugin".to_string(),
                thread_id: None,
            };
            if let Some(parent_id) = parent {
                let mut task = Task::from_user_input(title, 0, channel);
                task.parent_task_id = Some(parent_id.into());
                let task_id = task.id;
                let entity = world
                    .spawn((
                        task,
                        crate::domain::ShortTermMemory::default(),
                        PendingDispatch {
                            kind: DispatchKind::Task,
                            hint: DispatchHint::main_agent(),
                        },
                    ))
                    .id();
                if let Some(mut index) = world.get_resource_mut::<crate::ecs::EntityIndex>() {
                    index.tasks.insert(task_id, entity);
                }
            } else {
                world.spawn(crate::domain::CreateTaskMessage {
                    content: title,
                    origin_channel: Some(channel.clone()),
                    routing_policy: crate::domain::TaskRoutingPolicy::conversational(channel),
                    initiator: None,
                });
            }
        }
        WorldCommand::SetTaskMetadata {
            task_id,
            key,
            value,
        } => {
            // Task 暂无 metadata 字段（spec 期望此能力但未实施）。
            // 先以 `debug!` 记录并不写回，确保 hook 脚本可安全调用。
            debug!(
                event = "WorldCommandSetTaskMetadataDeferred",
                task_id = %task_id,
                key = %key,
                value = %value,
                "SetTaskMetadata deferred: Task.metadata 字段尚未添加"
            );
        }
        WorldCommand::SetTaskTag {
            task_id,
            key,
            value,
        } => {
            // Task 暂无 tags 字段，先以 `debug!` 记录并不写回。
            debug!(
                event = "WorldCommandSetTaskTagDeferred",
                task_id = %task_id,
                key = %key,
                value = %value,
                "SetTaskTag deferred: Task.tags 字段尚未添加"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prelude::Schedule;

    fn plugin_dir_with_hook(script: &str) -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().join("budget");
        std::fs::create_dir_all(root.join("hooks")).unwrap();
        std::fs::write(
            root.join("manifest.toml"),
            "id = \"budget\"\napi_version = 1\n[[hooks]]\nevent = \"on_task_created\"\nscript = \"hooks/h.rhai\"\n",
        )
        .unwrap();
        std::fs::write(root.join("hooks/h.rhai"), script).unwrap();
        dir
    }

    fn dispatch(world: &mut World, registry: &mut PluginRegistry) {
        let (writer_tx, _writer_rx) = crossbeam_channel::unbounded();
        let (message_tx, _message_rx) = crossbeam_channel::unbounded();
        let tx = writer_tx.clone();
        dispatch_hook(HookDispatchInput {
            point: HookPoint::OnTaskCreated,
            world,
            registry,
            writer_tx,
            ctx_builder: Box::new(move |plugin: &LoadedPlugin| PluginContext {
                snapshot: WorldSnapshot::empty(),
                writer: WorldWriter::new(tx.clone()),
                outcome: Arc::new(Mutex::new(HookOutcome::default())),
                plugin_roots: PluginRoots::single(plugin.root_dir.clone()),
                approval: ApprovalContext {
                    current_request_id: None,
                },
                skills: SkillsSnapshot::empty(),
                message: MessageContext {
                    plugin_id: plugin.manifest.id.clone(),
                    tx: message_tx.clone(),
                },
                temp_resource: TempResourceSlot::new(),
                tool: ToolCallContext::default(),
            }),
        });
    }

    /// hook 派发耗时累加到本帧预算，供 `hook_budget_check_system` 检查。
    #[test]
    fn dispatch_records_time_into_frame_budget() {
        let dir = plugin_dir_with_hook("let x = 0; for i in 0..20000 { x += i; }");
        let mut registry = crate::user_plugins::loader::load_plugins_from_dir(dir.path());
        assert_eq!(registry.plugins().len(), 1);
        let mut world = World::new();
        world.init_resource::<HookFrameBudget>();

        dispatch(&mut world, &mut registry);

        assert!(world.resource::<HookFrameBudget>().spent > Duration::ZERO);
    }

    #[test]
    fn budget_check_resets_each_frame() {
        let mut world = World::new();
        world.insert_resource(HookFrameBudget {
            spent: HOOK_FRAME_BUDGET * 3,
        });
        let mut schedule = Schedule::default();
        schedule.add_systems(hook_budget_check_system);
        schedule.run(&mut world);
        assert_eq!(world.resource::<HookFrameBudget>().spent, Duration::ZERO);
    }

    #[test]
    fn hook_timeout_is_short_enough_for_the_ecs_thread() {
        assert!(HOOK_TIMEOUT <= Duration::from_millis(200));
    }
}
