//! 事件触发任务路由 System
//!
//! 消费 `TriggerTaskMessage`，根据 `SignalTriggerRegistry` 中注册的路由，
//! 把事件触发转换为 `CreateTaskMessage`，使事件任务进入与普通用户输入相同的任务创建链路。
//!
//! 对 `scheduled:` 前缀的 Timer 触发，从 `ScheduledTaskRegistry` 查找动态任务
//! 信息并生成 `CreateTaskMessage`；一次性任务触发后由 `cleanup_scheduled_task_if_once`
//! 清理，cron 任务保留在 registry 中以便反复触发。

use crate::prelude::*;
use tracing::{debug, info, warn};

use crate::domain::{
    CreateTaskMessage, SignalTriggerRegistry, TaskRoutingPolicy, TaskTrigger, TriggerTaskMessage,
};
use crate::triggers::{
    ScheduledTaskRegistry, SchedulerState, SchedulerStateWatcher,
    update_scheduler_state_with_watcher,
};

/// 将事件触发消息路由为 `CreateTaskMessage`。
///
/// - Webhook 触发走 `registry.route`，Timer 触发走 `registry.timer_route`
/// - `scheduled:` 前缀的 Timer 触发走 `ScheduledTaskRegistry` 动态分支
/// - 未注册的触发器会被丢弃并记录结构化日志
/// - `build_task_input` 失败的触发器会被丢弃
/// - 静态路由成功后产出 `CreateTaskMessage`，`origin_channel` 为 `None`，
///   `routing_policy` 使用 `TaskRoutingPolicy::event`，由路由配置提供审批通道与上下文
/// - 动态 scheduled task 成功后产出 `CreateTaskMessage`，`routing_policy`
///   使用 `TaskRoutingPolicy::scheduled_task`，由 `ScheduledTaskInfo` 构造
/// - 一次性 scheduled task 触发后从 registry 与 `SchedulerState.dynamic_tasks` 中清理，
///   清理经 `update_scheduler_state_with_watcher` 共享入口，让 `SchedulerStateWatcher`
///   收到通知（不变量 4 字面成立）
pub fn trigger_task_routing_system(
    mut commands: Commands,
    registry: Res<SignalTriggerRegistry>,
    mut scheduled_registry: ResMut<ScheduledTaskRegistry>,
    mut scheduler_state: ResMut<SchedulerState>,
    watcher: Res<SchedulerStateWatcher>,
    messages: Query<(Entity, &TriggerTaskMessage)>,
) {
    for (entity, message) in &messages {
        let trigger = &message.trigger;
        let kind = match trigger {
            TaskTrigger::Timer { kind } => kind.clone(),
            TaskTrigger::Webhook { kind, .. } => kind.clone(),
        };

        // Webhook 仍走 registry.route；Timer 改用 timer_route，让 scheduled: 动态任务分支生效
        let static_route = match trigger {
            TaskTrigger::Webhook { .. } => registry.route(trigger),
            TaskTrigger::Timer { .. } => registry.timer_route(&kind),
        };

        if let Some(route) = static_route {
            match route.build_task_input(trigger) {
                Ok(content) => {
                    let approval_context = route.build_approval_context(trigger);
                    debug!(
                        event = "SignalTriggerMatched",
                        source = %message.source.0,
                        trigger = ?message.trigger,
                        content_len = content.len(),
                        "signal trigger routed to CreateTaskMessage"
                    );
                    commands.spawn(CreateTaskMessage {
                        content,
                        origin_channel: None,
                        routing_policy: TaskRoutingPolicy::event(
                            route.approval_channel.clone(),
                            Some(approval_context),
                        ),
                    });
                }
                Err(_) => {
                    warn!(
                        event = "SignalTriggerPromptBuildFailed",
                        source = %message.source.0,
                        kind = %kind,
                        "dropping signal trigger after prompt build failure"
                    );
                }
            }
        } else if kind.starts_with("scheduled:") {
            // 先取出所需数据并结束不可变借用，以便后续可变借用清理 registry
            let task_input = scheduled_registry
                .get(&kind)
                .map(|info| (info.build_task_input(), info.build_routing_policy()));
            match task_input {
                Some((content, routing_policy)) => {
                    debug!(
                        event = "ScheduledTaskMatched",
                        source = %message.source.0,
                        kind = %kind,
                        content_len = content.len(),
                        "scheduled task routed to CreateTaskMessage"
                    );
                    commands.spawn(CreateTaskMessage {
                        content,
                        origin_channel: None,
                        routing_policy,
                    });
                    cleanup_scheduled_task_if_once(
                        &kind,
                        &mut scheduler_state,
                        &mut scheduled_registry,
                        &watcher,
                    );
                }
                None => {
                    warn!(
                        event = "ScheduledTaskNotFound",
                        source = %message.source.0,
                        kind = %kind,
                        "dropping scheduled task trigger without registry entry"
                    );
                }
            }
        } else {
            warn!(
                event = "SignalTriggerRouteMissing",
                source = %message.source.0,
                trigger = ?message.trigger,
                "dropping unregistered signal trigger"
            );
        }

        commands.entity(entity).despawn();
    }
}

/// 一次性 scheduled task 触发后从 registry 与 `SchedulerState.dynamic_tasks` 中清理。
///
/// cron 任务（`is_once == false`）保留在 registry 中以便反复触发。
///
/// 清理经 `update_scheduler_state_with_watcher` 共享入口，与 `commit_tool_effects_system`
/// / `reload_triggers_system` 走同一 `apply_and_notify` 实现，让 `SchedulerStateWatcher`
/// 在 once 任务清理后收到通知（不变量 4「双账本单一修改入口」字面成立）。
fn cleanup_scheduled_task_if_once(
    kind: &str,
    scheduler_state: &mut ResMut<SchedulerState>,
    scheduled_registry: &mut ResMut<ScheduledTaskRegistry>,
    watcher: &SchedulerStateWatcher,
) {
    // 先拷出 is_once 标记，避免不可变借用阻塞后续 remove
    let Some(is_once) = scheduled_registry.get(kind).map(|info| info.is_once) else {
        return;
    };
    if !is_once {
        return;
    }
    let kind_owned = kind.to_string();
    update_scheduler_state_with_watcher(
        scheduler_state,
        scheduled_registry,
        watcher,
        |state, registry| {
            registry.remove(&kind_owned);
            state.dynamic_tasks_mut().retain(|t| t.kind != kind_owned);
        },
    );
    info!(
        event = "DynamicTaskRemoved",
        kind = %kind,
        reason = "once scheduled task triggered",
        "dynamic once scheduled task removed after trigger"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{ChannelId, EventTaskRoute, FrontendKind, SignalSource, TaskTrigger};
    use crate::triggers::{
        DynamicScheduledTask, ScheduleSpec, ScheduledTaskInfo, SchedulerStateWatcher,
    };
    use chrono::Utc;
    use tokio::sync::watch;
    use uuid::Uuid;

    fn insert_scheduler_resources(app: &mut App) {
        app.insert_resource(ScheduledTaskRegistry::default());
        app.insert_resource(SchedulerState::default());
        app.insert_resource(SchedulerStateWatcher::default());
    }

    /// 与 `insert_scheduler_resources` 类似，但用真实的 `watch::channel` 连接
    /// `SchedulerStateWatcher`，返回 receiver 供测试断言 watch 是否被通知。
    fn insert_scheduler_resources_with_watch(app: &mut App) -> watch::Receiver<SchedulerState> {
        app.insert_resource(ScheduledTaskRegistry::default());
        app.insert_resource(SchedulerState::default());
        let (tx, rx) = watch::channel(SchedulerState::default());
        app.insert_resource(SchedulerStateWatcher(Some(tx)));
        rx
    }

    /// 构造一个 once 动态任务条目，用于在 `SchedulerState.dynamic_tasks` 预填。
    fn sample_once_dynamic_task(kind: &str) -> DynamicScheduledTask {
        DynamicScheduledTask {
            id: Uuid::new_v4(),
            kind: kind.to_string(),
            schedule: ScheduleSpec::Once(Utc::now() + chrono::Duration::minutes(5)),
            created_at: Utc::now(),
        }
    }

    #[test]
    fn registered_webhook_route_creates_create_task_message() {
        let mut app = App::new();
        let mut registry = SignalTriggerRegistry::default();
        registry.register_webhook(
            "github.issue_opened",
            EventTaskRoute {
                prompt_template: "请分析这个 issue".to_string(),
                approval_channel: Some(ChannelId {
                    frontend: FrontendKind::Telegram,
                    user_id: "reviewer".to_string(),
                    thread_id: None,
                }),
                approval_context: "GitHub issue opened".to_string(),
            },
        );
        app.insert_resource(registry);
        insert_scheduler_resources(&mut app);
        app.add_systems(Update, trigger_task_routing_system);
        app.world_mut().spawn(TriggerTaskMessage {
            source: SignalSource("external:test".to_string()),
            trigger: TaskTrigger::Webhook {
                kind: "github.issue_opened".to_string(),
                body: serde_json::json!({"title": "bug"}),
            },
        });
        app.update();
        let mut query = app.world_mut().query::<&CreateTaskMessage>();
        let messages: Vec<_> = query.iter(app.world()).collect();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content, "请分析这个 issue");
        assert_eq!(messages[0].origin_channel, None);
        assert_eq!(
            messages[0].routing_policy.approval_context.as_deref(),
            Some("GitHub issue opened")
        );
    }

    #[test]
    fn unregistered_timer_route_is_dropped() {
        let mut app = App::new();
        app.insert_resource(SignalTriggerRegistry::default());
        insert_scheduler_resources(&mut app);
        app.add_systems(Update, trigger_task_routing_system);
        app.world_mut().spawn(TriggerTaskMessage {
            source: SignalSource("scheduler:test".to_string()),
            trigger: TaskTrigger::Timer {
                kind: "nightly".to_string(),
            },
        });
        app.update();
        let mut query = app.world_mut().query::<&CreateTaskMessage>();
        assert_eq!(query.iter(app.world()).count(), 0);
    }

    #[test]
    fn scheduled_task_route_creates_create_task_message() {
        let mut app = App::new();
        app.insert_resource(SignalTriggerRegistry::default());
        insert_scheduler_resources(&mut app);
        app.add_systems(Update, trigger_task_routing_system);

        let id = Uuid::new_v4();
        let kind = format!("scheduled:{}", id);
        let channel = ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "chat".to_string(),
            thread_id: None,
        };

        let mut registry = ScheduledTaskRegistry::default();
        registry.insert(
            kind.clone(),
            ScheduledTaskInfo {
                content: "say hi".to_string(),
                output_channel: Some(channel.clone()),
                is_once: true,
            },
        );
        app.insert_resource(registry);

        app.world_mut().spawn(TriggerTaskMessage {
            source: SignalSource("scheduler:test".to_string()),
            trigger: TaskTrigger::Timer { kind: kind.clone() },
        });

        app.update();

        let messages: Vec<_> = app
            .world_mut()
            .query::<&CreateTaskMessage>()
            .iter(app.world())
            .collect();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content, "say hi");
        assert_eq!(
            messages[0].routing_policy.output_channel,
            Some(channel.clone())
        );
        assert_eq!(
            messages[0].routing_policy.approval_channel,
            Some(channel.clone())
        );
        assert!(
            app.world()
                .resource::<ScheduledTaskRegistry>()
                .get(&kind)
                .is_none()
        );
    }

    #[test]
    fn scheduled_cron_task_is_not_cleaned_up_after_trigger() {
        let mut app = App::new();
        app.insert_resource(SignalTriggerRegistry::default());
        insert_scheduler_resources(&mut app);
        app.add_systems(Update, trigger_task_routing_system);

        let id = Uuid::new_v4();
        let kind = format!("scheduled:{}", id);
        let channel = ChannelId {
            frontend: FrontendKind::Tui,
            user_id: "user".to_string(),
            thread_id: None,
        };

        let mut registry = ScheduledTaskRegistry::default();
        registry.insert(
            kind.clone(),
            ScheduledTaskInfo {
                content: "cron job".to_string(),
                output_channel: Some(channel),
                is_once: false,
            },
        );
        app.insert_resource(registry);

        app.world_mut().spawn(TriggerTaskMessage {
            source: SignalSource("scheduler:test".to_string()),
            trigger: TaskTrigger::Timer { kind: kind.clone() },
        });

        app.update();

        let messages: Vec<_> = app
            .world_mut()
            .query::<&CreateTaskMessage>()
            .iter(app.world())
            .collect();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content, "cron job");
        // cron 任务保留在 registry 中
        assert!(
            app.world()
                .resource::<ScheduledTaskRegistry>()
                .get(&kind)
                .is_some()
        );
    }

    /// t1: once 任务触发后 `SchedulerStateWatcher` 必须收到通知。
    ///
    /// 这是不变量 4「双账本单一修改入口」的直接契约：`cleanup_scheduled_task_if_once`
    /// 走 `update_scheduler_state_with_watcher` 共享入口，watch 必须在闭包返回后
    /// send 一次。若回归到直接 `registry.remove` + `state.retain`，watch 不通知，
    /// 本测试失败。
    #[test]
    fn once_task_trigger_notifies_scheduler_state_watch() {
        let mut app = App::new();
        app.insert_resource(SignalTriggerRegistry::default());
        let mut rx = insert_scheduler_resources_with_watch(&mut app);
        app.add_systems(Update, trigger_task_routing_system);

        let id = Uuid::new_v4();
        let kind = format!("scheduled:{}", id);
        let channel = ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "chat".to_string(),
            thread_id: None,
        };

        let mut registry = ScheduledTaskRegistry::default();
        registry.insert(
            kind.clone(),
            ScheduledTaskInfo {
                content: "once say hi".to_string(),
                output_channel: Some(channel),
                is_once: true,
            },
        );
        app.insert_resource(registry);
        // 预填 dynamic_tasks，让 t2 能同时验证双账本一致
        {
            let mut state = app.world_mut().resource_mut::<SchedulerState>();
            state
                .dynamic_tasks_mut()
                .push(sample_once_dynamic_task(&kind));
        }
        // 初始 receiver 值是 default，先标记为已读，便于后续判断是否「再次」发送
        rx.borrow_and_update();

        app.world_mut().spawn(TriggerTaskMessage {
            source: SignalSource("scheduler:test".to_string()),
            trigger: TaskTrigger::Timer { kind: kind.clone() },
        });

        app.update();

        // t1: watch 被通知
        assert!(
            rx.has_changed().unwrap(),
            "SchedulerStateWatcher 必须在 once 任务清理后收到通知"
        );
        // t2: 双账本一致——registry 与 dynamic_tasks 都不含该 kind
        assert!(
            app.world()
                .resource::<ScheduledTaskRegistry>()
                .get(&kind)
                .is_none(),
            "registry 中应已清理 once 任务"
        );
        let state = app.world().resource::<SchedulerState>();
        assert!(
            !state.dynamic_tasks().iter().any(|t| t.kind == kind),
            "dynamic_tasks 中应已清理 once 任务，与 registry 保持一致"
        );
    }

    /// t3: cron 任务触发后 `SchedulerStateWatcher` 不应收到通知。
    ///
    /// 反向断言，防止「为保险在 cron 路径也调 update_scheduler_state_with_watcher」
    /// 的退化——cron 任务保留在 registry 与 state 中无变化，watch 不应 send。
    #[test]
    fn cron_task_trigger_does_not_notify_scheduler_state_watch() {
        let mut app = App::new();
        app.insert_resource(SignalTriggerRegistry::default());
        let mut rx = insert_scheduler_resources_with_watch(&mut app);
        app.add_systems(Update, trigger_task_routing_system);

        let id = Uuid::new_v4();
        let kind = format!("scheduled:{}", id);
        let channel = ChannelId {
            frontend: FrontendKind::Tui,
            user_id: "user".to_string(),
            thread_id: None,
        };

        let mut registry = ScheduledTaskRegistry::default();
        registry.insert(
            kind.clone(),
            ScheduledTaskInfo {
                content: "cron job".to_string(),
                output_channel: Some(channel),
                is_once: false,
            },
        );
        app.insert_resource(registry);
        // 初始 receiver 值是 default，标记为已读
        rx.borrow_and_update();

        app.world_mut().spawn(TriggerTaskMessage {
            source: SignalSource("scheduler:test".to_string()),
            trigger: TaskTrigger::Timer { kind: kind.clone() },
        });

        app.update();

        // t3: watch 未被通知——cron 任务保留，state 无变化
        assert!(
            !rx.has_changed().unwrap(),
            "cron 任务触发不应通知 SchedulerStateWatcher（state 无变化）"
        );
        assert!(
            app.world()
                .resource::<ScheduledTaskRegistry>()
                .get(&kind)
                .is_some()
        );
    }

    #[test]
    fn scheduled_task_without_registry_entry_is_dropped() {
        let mut app = App::new();
        app.insert_resource(SignalTriggerRegistry::default());
        insert_scheduler_resources(&mut app);
        app.add_systems(Update, trigger_task_routing_system);

        let kind = format!("scheduled:{}", Uuid::new_v4());
        app.world_mut().spawn(TriggerTaskMessage {
            source: SignalSource("scheduler:test".to_string()),
            trigger: TaskTrigger::Timer { kind: kind.clone() },
        });

        app.update();

        let count = app
            .world_mut()
            .query::<&CreateTaskMessage>()
            .iter(app.world())
            .count();
        assert_eq!(count, 0);
    }
}
