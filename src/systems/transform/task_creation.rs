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
        HookPoint, NewlyCreatedTask, PendingDispatch, ScheduledRun, ShortTermMemory, Task,
        TaskInitiator, TrustedImages,
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
/// 将用户消息转换为任务实体。定时任务触发的消息携带的 `ScheduledRun` 一并拷到新 Task 上。
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn user_message_to_task_system(
    mut commands: Commands,
    settings: Res<HarnessSettings>,
    mut index: ResMut<EntityIndex>,
    messages: Query<(
        Entity,
        &CreateTaskMessage,
        Option<&ScheduledRun>,
        Option<&crate::domain::InputSequence>,
    )>,
    tasks: Query<&Task>,
    inflight_llm: Option<Res<crate::domain::InflightLlmRequests>>,
    pending: Query<&crate::domain::ToolRequestPending>,
    queued: Query<&crate::domain::ToolExecutionRequestMessage>,
    queued_llm: Query<&crate::domain::AgentExecutionRequestMessage>,
    backend: Option<Res<crate::systems::NativeProcessBackend>>,
    clock: Option<Res<crate::contracts::Clock>>,
) {
    let known: Vec<_> = tasks.iter().collect();
    let mut pending: std::collections::HashSet<_> = pending
        .iter()
        .map(|p| p.original_request.task_id)
        .chain(queued.iter().map(|request| request.request.task_id))
        .chain(
            queued_llm
                .iter()
                .filter(|request| request.request.work_item_id.is_none())
                .map(|request| request.request.task_id),
        )
        .collect();
    if let Some(inflight) = inflight_llm.as_ref() {
        pending.extend(
            known
                .iter()
                .filter(|task| inflight.in_flight(task.id) > 0)
                .map(|task| task.id),
        );
    }
    let mut reserved = std::collections::HashMap::new();
    let mut messages: Vec<_> = messages.iter().collect();
    messages.sort_by_key(|(entity, _, _, seq)| seq.map_or(entity.to_bits(), |seq| seq.0));
    for (entity, message, scheduled_run, sequence) in messages {
        if let Some(channel) = &message.origin_channel {
            if super::super::conversation::channel_is_closing(
                channel,
                &known,
                &pending,
                backend.as_deref(),
            ) {
                continue; // 保留创建消息，等旧 worker/会话真实结束。
            }
            if let Some(&(task_id, task_entity)) = reserved.get(channel) {
                tracing::debug!(event = "ConversationDuplicateCreateRedirected", %task_id, ?channel,
                    "same-frame duplicate creation becomes steer");
                TrustedImages::register(&mut commands, task_entity, &message.content);
                let mut delivery = commands.spawn(crate::domain::InboxDeliveryMessage {
                    task_id,
                    item: crate::domain::InboxItem::UserSteer {
                        content: message.content.clone(),
                        received_at: clock
                            .as_ref()
                            .map_or_else(chrono::Utc::now, |clock| clock.0),
                    },
                });
                if let Some(sequence) = sequence {
                    delivery.insert(*sequence);
                }
                commands.entity(entity).despawn();
                continue;
            }
            if known.iter().any(|task| {
                super::super::conversation::is_user_root(task, channel)
                    && !task.status.is_terminal()
            }) {
                // 插件和命令入口同样回到用户路由，由唯一任务接收正文。
                let mut input = commands.spawn(crate::domain::UserInputMessage {
                    content: message.content.clone(),
                    origin_channel: channel.clone(),
                    sender: message.initiator.clone(),
                });
                if let Some(sequence) = sequence {
                    input.insert(*sequence);
                }
                commands.entity(entity).despawn();
                continue;
            }
        }
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

        if let Some(channel) = &message.origin_channel {
            tracing::debug!(event = "ConversationSlotReserved", task_id = %task.id, ?channel,
                "user root reserves the conversation slot");
        }
        let task_id = task.id;
        let task_entity = spawn_task(
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
        if let Some(channel) = &message.origin_channel {
            reserved.insert(channel.clone(), (task_id, task_entity));
        }
        if let Some(run) = scheduled_run {
            commands.entity(task_entity).insert(*run);
        }
        if let Some(initiator) = &message.initiator {
            commands
                .entity(task_entity)
                .insert(TaskInitiator(initiator.clone()));
        }
        // 只有来自用户会话的输入可信；事件 / 定时任务内容中的图片标记不绑定
        if message.origin_channel.is_some() {
            TrustedImages::register(&mut commands, task_entity, &message.content);
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::TaskRoutingPolicy;

    fn app() -> App {
        let mut app = App::new();
        app.insert_resource(HarnessSettings::default_test());
        app.init_resource::<EntityIndex>();
        app.add_systems(Update, user_message_to_task_system);
        app
    }

    #[test]
    fn scheduled_run_marker_is_copied_onto_new_task() {
        let mut app = app();
        let schedule_id = uuid::Uuid::new_v4();
        app.world_mut().spawn((
            CreateTaskMessage {
                initiator: None,
                content: "scheduled".to_string(),
                origin_channel: None,
                routing_policy: TaskRoutingPolicy::scheduled_task(None, "scheduled task"),
            },
            ScheduledRun { schedule_id },
        ));
        app.world_mut().spawn(CreateTaskMessage {
            initiator: None,
            content: "plain".to_string(),
            origin_channel: None,
            routing_policy: TaskRoutingPolicy::event(None, None),
        });

        app.update();

        let mut query = app.world_mut().query::<(&Task, Option<&ScheduledRun>)>();
        let mut seen: Vec<(String, Option<ScheduledRun>)> = query
            .iter(app.world())
            .map(|(task, run)| (task.content.clone(), run.copied()))
            .collect();
        seen.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            seen,
            vec![
                ("plain".to_string(), None),
                ("scheduled".to_string(), Some(ScheduledRun { schedule_id })),
            ]
        );
    }

    #[test]
    fn only_conversation_input_registers_trusted_images() {
        let mut app = app();
        let channel = crate::domain::ChannelId {
            frontend: crate::domain::FrontendKind::QQ,
            user_id: "user:u".to_string(),
            thread_id: None,
        };
        app.world_mut().spawn(CreateTaskMessage {
            initiator: None,
            content: "看看 [IMAGE:/tmp/user.png]".to_string(),
            origin_channel: Some(channel.clone()),
            routing_policy: TaskRoutingPolicy::conversational(channel),
        });
        app.world_mut().spawn(CreateTaskMessage {
            initiator: None,
            content: "webhook 内容 [IMAGE:/etc/secret.png]".to_string(),
            origin_channel: None,
            routing_policy: TaskRoutingPolicy::event(None, None),
        });

        app.update();

        let mut query = app.world_mut().query::<(&Task, Option<&TrustedImages>)>();
        let mut seen: Vec<(bool, Option<Vec<String>>)> = query
            .iter(app.world())
            .map(|(task, trusted)| {
                (
                    task.origin_channel.is_some(),
                    trusted.map(|t| t.0.iter().cloned().collect()),
                )
            })
            .collect();
        seen.sort_by_key(|(conversational, _)| *conversational);
        assert_eq!(
            seen,
            vec![
                (false, None),
                (true, Some(vec!["/tmp/user.png".to_string()])),
            ]
        );
    }

    #[test]
    fn initiator_is_recorded_on_new_task() {
        let mut app = app();
        let channel = crate::domain::ChannelId {
            frontend: crate::domain::FrontendKind::QQ,
            user_id: "group:g".to_string(),
            thread_id: None,
        };
        app.world_mut().spawn(CreateTaskMessage {
            content: "from member".to_string(),
            origin_channel: Some(channel.clone()),
            routing_policy: TaskRoutingPolicy::conversational(channel),
            initiator: Some("member-a".to_string()),
        });
        app.world_mut().spawn(CreateTaskMessage {
            content: "event".to_string(),
            origin_channel: None,
            routing_policy: TaskRoutingPolicy::event(None, None),
            initiator: None,
        });

        app.update();

        let mut query = app.world_mut().query::<(&Task, Option<&TaskInitiator>)>();
        let mut seen: Vec<(String, Option<String>)> = query
            .iter(app.world())
            .map(|(task, initiator)| (task.content.clone(), initiator.map(|i| i.0.clone())))
            .collect();
        seen.sort();
        assert_eq!(
            seen,
            vec![
                ("event".to_string(), None),
                ("from member".to_string(), Some("member-a".to_string())),
            ]
        );
    }
}
