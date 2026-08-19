//! 任务创建 System
//!
//! 从消息创建任务实体，并派发 `on_task_created` hook。

use std::sync::{Arc, Mutex};

use crate::ecs::{EntityIndex, spawn_task};
use crate::prelude::*;
use crossbeam_channel::unbounded;
use tracing::{debug, info};

use crate::{
    domain::{
        CreateTaskMessage, DispatchHint, DispatchKind, DispatchStrategy, EntryMetadata, EntryRole,
        HookPoint, NewlyCreatedTask, PendingDispatch, ShortTermMemory, Task,
    },
    systems::HarnessSettings,
    user_plugins::{
        dispatcher::{
            HookDispatchInput, HookOutcome, PluginContext, SharedHookOutcome, dispatch_hook,
            flush_world_commands,
        },
        host_api::{
            approval::ApprovalContext,
            entity_query::WorldSnapshot,
            entity_write::{WorldCommand, WorldWriter},
            experience::ExperienceContext,
            message::MessageContext,
            plugin_resource::PluginRoots,
            skills_meta::SkillsSnapshot,
            temp_resource::TempResourceSlot,
            tool_control::ToolCallContext,
        },
        registry::PluginRegistry,
    },
};

/// 用户消息转任务 System
///
/// 将用户消息转换为任务实体。
pub fn user_message_to_task_system(
    mut commands: Commands,
    settings: Res<HarnessSettings>,
    mut index: ResMut<EntityIndex>,
    messages: Query<(Entity, &CreateTaskMessage)>,
) {
    for (entity, message) in &messages {
        // 创建多轮对话任务（Pending 状态）并附带 ShortTermMemory
        let mut stm = ShortTermMemory::default();
        stm.add_entry(EntryRole::User, &message.content, EntryMetadata::default());
        let stm_tokens = stm.estimated_tokens;

        let task = if message.origin_channel.is_some() {
            // 普通聊天任务：origin_channel 存在，使用 conversational 路由
            Task::from_user_input(
                message.content.clone(),
                settings.0.max_retries,
                message
                    .origin_channel
                    .clone()
                    .expect("origin_channel is Some"),
            )
        } else {
            // 事件触发任务：origin_channel 为 None，使用消息携带的 routing_policy
            Task::from_trigger(
                message.content.clone(),
                settings.0.max_retries,
                message.routing_policy.clone(),
            )
        };
        info!(
            event = "TaskCreated",
            task_id = %task.id,
            content = %message.content,
            "任务创建：{}",
            message.content
        );
        debug!(
            event = "TaskCreated",
            task_id = %task.id,
            content = %message.content,
            content_len = message.content.len(),
            multi_turn = task.multi_turn,
            max_retries = task.max_retries,
            stm_initial_entries = 1,
            stm_initial_tokens = stm_tokens,
            "new task spawned from user message"
        );

        spawn_task(
            &mut commands,
            &mut index,
            task,
            stm,
            NewlyCreatedTask,
            PendingDispatch {
                kind: DispatchKind::Task,
                hint: DispatchHint {
                    strategy: DispatchStrategy::BrainLlm,
                    preferred_agent_name: None,
                    required_skill_id: None,
                    agent_spawn_spec: None,
                },
            },
        );
        commands.entity(entity).despawn();
    }
}

/// `on_task_created` hook 派发 System
///
/// 在 `user_message_to_task_system` 之后运行。扫描带 `NewlyCreatedTask` 标记
/// 的 Task entity，逐个对 `WorldSnapshot` 快照派发 `HookPoint::OnTaskCreated`，
/// flush 累积的 `WorldCommand`，随后移除标记。
///
/// 与 `user_message_to_task_system` 共享 `Update` 同一帧执行（Transform 集合），
/// 标记组件保证即使 hook 在派发期间创建新 Task 也不会无限递归（新创建的 Task
/// 由 `WorldCommand::CreateTask` 产生，不带 `NewlyCreatedTask` 标记）。
pub fn on_task_created_hook_system(world: &mut World) {
    // 若没有 plugin registry，说明插件层未启用，直接跳过。
    if !world.contains_resource::<PluginRegistry>() {
        return;
    }

    // 先采集所有带标记的 Task entity 及其 Task clone，避免在派发 hook 期间
    // 借用 world（dispatch_hook 需要 &mut World）。
    let targets: Vec<(crate::prelude::Entity, Task)> = world
        .query_filtered::<(crate::prelude::Entity, &Task), With<NewlyCreatedTask>>()
        .iter(world)
        .map(|(e, t)| (e, t.clone()))
        .collect();

    if targets.is_empty() {
        return;
    }

    world.resource_scope(
        |world: &mut World, mut registry: bevy_ecs::change_detection::Mut<PluginRegistry>| {
            for (entity, task) in targets {
                dispatch_on_task_created(world, &mut registry, &task);
                // 派发后移除标记，避免重复派发。
                if let Ok(mut e) = world.get_entity_mut(entity) {
                    e.remove::<NewlyCreatedTask>();
                }
            }
        },
    );
}

/// 对单个 Task 派发 `on_task_created` hook 并 flush WorldCommand。
fn dispatch_on_task_created(world: &mut World, registry: &mut PluginRegistry, task: &Task) {
    let (writer_tx, writer_rx) = unbounded::<WorldCommand>();
    let (message_tx, _message_rx) = unbounded();
    let snap = WorldSnapshot::from_world(world);

    let input = HookDispatchInput {
        point: HookPoint::OnTaskCreated,
        world,
        registry,
        writer_tx: writer_tx.clone(),
        ctx_builder: Box::new(
            |plugin: &crate::user_plugins::registry::LoadedPlugin, world: &mut World| {
                let local_outcome: SharedHookOutcome = Arc::new(Mutex::new(HookOutcome::default()));
                PluginContext {
                    snapshot: snap.clone(),
                    writer: WorldWriter::new(writer_tx.clone()),
                    outcome: local_outcome,
                    plugin_roots: PluginRoots::single(plugin.root_dir.clone()),
                    approval: ApprovalContext {
                        current_request_id: None,
                    },
                    experience: ExperienceContext {
                        store: Arc::new(
                            world
                                .get_resource::<crate::domain::ExperienceStore>()
                                .cloned()
                                .unwrap_or_default(),
                        ),
                    },
                    skills: SkillsSnapshot::empty(),
                    message: MessageContext {
                        plugin_id: plugin.manifest.id.clone(),
                        tx: message_tx.clone(),
                    },
                    temp_resource: TempResourceSlot::new(),
                    tool: ToolCallContext::default(),
                }
            },
        ),
    };

    let _ = dispatch_hook(input);

    // flush 累积的 WorldCommand（create_task / set_task_metadata / ...）。
    // flush 内部对未实现变体做 debug 无操作处理。
    flush_world_commands(world, &writer_rx);

    debug!(
        event = "OnTaskCreatedHookDispatched",
        task_id = %task.id,
        "on_task_created hook dispatched for task"
    );
}
