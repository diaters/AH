//! 事件触发任务路由 System
//!
//! 消费 `TriggerTaskMessage`，根据 `SignalTriggerRegistry` 中注册的路由，
//! 把事件触发转换为 `CreateTaskMessage`，使事件任务进入与普通用户输入相同的任务创建链路。
//!
//! 对 `scheduled:` 前缀的 Timer 触发，从 `SchedulerState` 查找动态定时任务定义
//! 并生成 `CreateTaskMessage`（见 [`route_scheduled_trigger`]）：暂停与重叠时跳过，
//! 补跑时在任务内容前标注原定时间并通知用户，触发后记录 `last_fired_at`；
//! 一次性任务在运行及真实通知回执完成并持久记账后清理，cron 定义保留。

use std::collections::HashSet;

use chrono::{DateTime, Duration, Local, Utc};
use uuid::Uuid;

use crate::prelude::*;
use tracing::{debug, info, warn};

use crate::contracts::FrontendRegistry;
use crate::domain::{
    CATCH_UP_WINDOW, CatchUp, CreateTaskMessage, EngineEvent, EventTarget, MessageRole,
    ScheduleDefinition, ScheduleFire, ScheduledRun, SignalTriggerRegistry, Task, TaskRoutingPolicy,
    TaskTrigger, TriggerTaskMessage, catch_up_decision,
};
use crate::triggers::{SchedulerState, SchedulerStateWatcher, update_scheduler_state_with_watcher};

/// 一次性任务晚于原定时间超过该时长才视为补跑（避免把正常的调度延迟标成补跑）。
const ONCE_CATCH_UP_GRACE: Duration = Duration::seconds(60);

/// 将事件触发消息路由为 `CreateTaskMessage`。
///
/// - Webhook 触发走 `registry.route`，Timer 触发走 `registry.timer_route`
/// - `scheduled:` 前缀的 Timer 触发走 `SchedulerState` 动态定义分支（Webhook 不进入该分支）
/// - 未注册的触发器会被丢弃并记录结构化日志
/// - `build_task_input` 失败的触发器会被丢弃
/// - 静态路由成功后产出 `CreateTaskMessage`，`origin_channel` 为 `None`，
///   `routing_policy` 使用 `TaskRoutingPolicy::event`，由路由配置提供审批通道与上下文
/// - 动态 scheduled task 成功后产出 `CreateTaskMessage`，`routing_policy`
///   使用 `TaskRoutingPolicy::scheduled_task`，由 `ScheduleDefinition` 构造
/// - 动态定时任务的状态写入（`last_fired_at`、过期一次性任务清理）经
///   `update_scheduler_state_with_watcher` 共享入口，让 `SchedulerStateWatcher` 收到通知
#[allow(clippy::too_many_arguments)]
pub fn trigger_task_routing_system(
    mut commands: Commands,
    registry: Res<SignalTriggerRegistry>,
    mut scheduler_state: ResMut<SchedulerState>,
    watcher: Res<SchedulerStateWatcher>,
    frontends: Option<Res<FrontendRegistry>>,
    store: Option<Res<crate::infrastructure::schedules::ScheduleStore>>,
    messages: Query<(Entity, &TriggerTaskMessage, Option<&ScheduleFire>)>,
    runs: Query<(
        &Task,
        &ScheduledRun,
        Option<&crate::systems::ScheduleRunRecorded>,
    )>,
    mut index: Option<ResMut<crate::ecs::EntityIndex>>,
    pending_runs: Query<&ScheduledRun, With<CreateTaskMessage>>,
) {
    // 仍在运行的定时任务：未终态的 Task，加上本帧刚生成、尚未变成 Task 的 CreateTaskMessage
    let mut active_runs: HashSet<Uuid> = pending_runs.iter().map(|r| r.schedule_id).collect();
    active_runs.extend(
        runs.iter()
            .filter(|(task, _, recorded)| {
                !task.status().is_terminal() || (store.is_some() && recorded.is_none())
            })
            .map(|(_, run, _)| run.schedule_id),
    );
    for (entity, message, fire) in &messages {
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
                        initiator: None,
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
        } else if matches!(trigger, TaskTrigger::Timer { .. }) && kind.starts_with("scheduled:") {
            // 动态定时任务只接受 timer 触发；webhook 路径不能借 kind 前缀触发定时任务
            route_scheduled_trigger(
                &mut commands,
                &kind,
                fire.copied(),
                &mut scheduler_state,
                &watcher,
                frontends.as_deref(),
                &mut active_runs,
                store.as_deref(),
                index.as_deref_mut(),
            );
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

/// 路由一次动态定时任务触发。
///
/// `active_runs` 是仍在运行（含本帧刚创建）的定时任务 id 集合，用于重叠判定；
/// 本次成功触发后把 id 加入其中。
#[allow(clippy::too_many_arguments)]
fn route_scheduled_trigger(
    commands: &mut Commands,
    kind: &str,
    fire: Option<ScheduleFire>,
    scheduler_state: &mut ResMut<SchedulerState>,
    watcher: &SchedulerStateWatcher,
    frontends: Option<&FrontendRegistry>,
    active_runs: &mut HashSet<Uuid>,
    store: Option<&crate::infrastructure::schedules::ScheduleStore>,
    index: Option<&mut crate::ecs::EntityIndex>,
) {
    let Some(def) = scheduler_state.find_by_kind(kind).cloned() else {
        warn!(
            event = "ScheduledTaskNotFound",
            kind = %kind,
            "dropping scheduled task trigger without definition"
        );
        return;
    };
    let now = Utc::now();

    // 暂停的定时任务只接受手动运行
    if !def.enabled && fire != Some(ScheduleFire::Manual) {
        info!(event = "ScheduleSkipped", kind = %kind, reason = "paused", "scheduled task skipped");
        return;
    }
    if active_runs.contains(&def.id) {
        queue_skipped(
            commands,
            &def,
            now,
            crate::domain::RunOutcome::SkippedOverlap,
            None,
        );
        info!(
            event = "ScheduleSkipped",
            kind = %kind,
            reason = "overlap",
            "previous run still in progress, scheduled task skipped"
        );
        return;
    }

    // 一次性任务的补跑在这里判定：重启后 timer 会立即触发已过期的一次性任务
    let fire = match (fire, catch_up_decision(&def, now, CATCH_UP_WINDOW)) {
        (None, CatchUp::Fire { missed_at })
            if def.schedule.is_once() && now - missed_at > ONCE_CATCH_UP_GRACE =>
        {
            Some(ScheduleFire::CatchUp { missed_at })
        }
        (None, CatchUp::Expired { missed_at }) if def.schedule.is_once() => {
            if store.is_some() {
                queue_skipped(
                    commands,
                    &def,
                    missed_at,
                    crate::domain::RunOutcome::Missed,
                    Some(def.clone()),
                );
                // 阻止 timer 在账本写失败期间反复生成过期触发。
                update_scheduler_state_with_watcher(scheduler_state, watcher, |state| {
                    if let Some(def) = state.find_by_kind_mut(kind) {
                        def.health.last_fired_at = Some(now);
                    }
                });
            } else {
                remove_definition(scheduler_state, watcher, def.id);
                notify(
                    frontends,
                    &def,
                    format!(
                        "定时任务「{}」原定于 {} 运行，已超过补跑期限，未执行并已删除。",
                        def.name,
                        format_local(missed_at)
                    ),
                );
            }
            return;
        }
        (fire, _) => fire,
    };

    let mut content = def.content.clone();
    if let Some(ScheduleFire::CatchUp { missed_at }) = fire {
        content = format!(
            "[补跑] 本次运行原定于 {}，因 harness 未运行而错过。\n\n{content}",
            format_local(missed_at)
        );
        notify(
            frontends,
            &def,
            format!(
                "定时任务「{}」错过了原定 {} 的运行，现在补跑一次。",
                def.name,
                format_local(missed_at)
            ),
        );
    }
    debug!(
        event = "ScheduledTaskMatched",
        kind = %kind,
        fire = ?fire,
        content_len = content.len(),
        "scheduled task routed to CreateTaskMessage"
    );
    let run = ScheduledRun {
        schedule_id: def.id,
        fired_at: Some(now),
        notify: def.notify,
    };
    let context = store
        .map(|store| build_context(&def, fire, now, store, scheduler_state))
        .transpose();
    match context {
        Ok(context) => {
            if let Some(context) = context {
                content.push_str(&context);
            }
            commands.spawn((
                CreateTaskMessage {
                    content,
                    origin_channel: None,
                    routing_policy: TaskRoutingPolicy::scheduled_task(
                        def.output_channel.clone(),
                        "scheduled task",
                    ),
                    initiator: None,
                },
                run,
            ));
        }
        Err(error) => {
            // 可信统计或状态目录不可用时不让 LLM 把缺失证据描述成无变化。
            let mut task = Task::from_trigger(
                content,
                0,
                TaskRoutingPolicy::scheduled_task(def.output_channel.clone(), "scheduled task"),
            );
            task.mark_failed_reason(
                crate::domain::FailureReason::Unknown,
                format!("schedule context unavailable: {error}"),
                now,
            );
            let id = task.id;
            let entity = commands.spawn((task, run)).id();
            if let Some(index) = index {
                index.tasks.insert(id, entity);
            }
        }
    }
    active_runs.insert(def.id);

    // 一次性任务也只记账不删除：运行进入终态后由 `once_schedule_completion_system` 删除，
    // timer 依据 `last_fired_at` 不再调度已触发的一次性任务
    update_scheduler_state_with_watcher(scheduler_state, watcher, |state| {
        if let Some(d) = state
            .dynamic_tasks_mut()
            .iter_mut()
            .find(|d| d.id == def.id)
        {
            d.health.last_fired_at = Some(now);
        }
    });
}

fn queue_skipped(
    commands: &mut Commands,
    def: &ScheduleDefinition,
    fired_at: DateTime<Utc>,
    outcome: crate::domain::RunOutcome,
    expired: Option<ScheduleDefinition>,
) {
    commands.spawn(crate::systems::schedule::PendingScheduleRecord {
        record: crate::domain::ScheduleRunRecord {
            task_id: None,
            schedule_id: def.id,
            fired_at,
            finished_at: Utc::now(),
            outcome,
            error: None,
            result_summary: format!("{outcome:?}"),
            notification_count: 0,
        },
        expired,
    });
}

fn build_context(
    def: &ScheduleDefinition,
    fire: Option<ScheduleFire>,
    now: DateTime<Utc>,
    store: &crate::infrastructure::schedules::ScheduleStore,
    state: &SchedulerState,
) -> Result<String, String> {
    let state_dir = store.state_dir(def.id);
    std::fs::create_dir_all(&state_dir)
        .map_err(|error| format!("{}: {error}", state_dir.display()))?;
    let summary = store
        .run_summary(def.id, now - Duration::hours(24))
        .map_err(|error| error.to_string())?;
    let trigger = match fire {
        Some(ScheduleFire::Manual) => "manual",
        Some(ScheduleFire::CatchUp { .. }) => "catch_up",
        None => "scheduled",
    };
    let policy = match def.notify {
        crate::domain::NotifyPolicy::Reply => "reply",
        crate::domain::NotifyPolicy::Explicit => "explicit",
    };
    // 属性值序列化，避免用户名称破坏 harness 上下文边界。
    let name = serde_json::to_string(&def.name).map_err(|error| error.to_string())?;
    let mut context = format!(
        "\n\n<schedule-context name={name} trigger=\"{trigger}\" fired_at=\"{now}\">\nstate_dir: {}\nnotify: {policy}\nrecent_runs: total={} notified={} silent={} failed={} skipped={} missed={}\n",
        state_dir.display(),
        summary.total,
        summary.notified,
        summary.total - summary.notified - summary.failed - summary.skipped - summary.missed,
        summary.failed,
        summary.skipped,
        summary.missed
    );
    if let Some(reference) = def.report_of {
        let observed = state
            .dynamic_tasks()
            .iter()
            .find(|def| def.id == reference)
            .ok_or_else(|| format!("report_of schedule missing: {reference}"))?;
        let summary = store
            .run_summary(reference, now - Duration::hours(24))
            .map_err(|error| error.to_string())?;
        context.push_str(&format!("report_of: {reference}\nreport_state_dir: {}\nlast_success_at: {}\nconsecutive_failures: {}\nalerting: {}\nreport_recent_runs: total={} notified={} failed={} skipped={} missed={}\n",
            store.state_dir(reference).display(), observed.health.last_success_at.map(|time| time.to_rfc3339()).unwrap_or_else(|| "never".into()), observed.health.consecutive_failures, observed.health.alerting,
            summary.total, summary.notified, summary.failed, summary.skipped, summary.missed));
    }
    context.push_str("</schedule-context>");
    Ok(context)
}

fn remove_definition(
    scheduler_state: &mut ResMut<SchedulerState>,
    watcher: &SchedulerStateWatcher,
    id: Uuid,
) {
    update_scheduler_state_with_watcher(scheduler_state, watcher, |state| {
        state.dynamic_tasks_mut().retain(|d| d.id != id);
    });
}

/// 向定时任务的输出通道推送一条系统消息（无输出通道或无前端时只记日志）。
pub(crate) fn notify(
    frontends: Option<&FrontendRegistry>,
    def: &ScheduleDefinition,
    content: String,
) {
    let (Some(frontends), Some(channel)) = (frontends, def.output_channel.clone()) else {
        debug!(
            event = "ScheduleNoticeDropped",
            name = %def.name,
            "no frontend or output channel for scheduled task notice"
        );
        return;
    };
    let event = EngineEvent::Text {
        target: EventTarget::Directed(vec![channel]),
        role: MessageRole::System,
        content,
        task_id: None,
    };
    for frontend in &frontends.frontends {
        frontend.push_event(event.clone());
    }
}

pub(crate) fn format_local(t: DateTime<Utc>) -> String {
    t.with_timezone(&Local).format("%Y-%m-%d %H:%M").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{ChannelId, EventTaskRoute, FrontendKind, SignalSource, TaskTrigger};
    use crate::domain::{ScheduleDefinition, ScheduleExpr};
    use crate::triggers::SchedulerStateWatcher;
    use chrono::Utc;
    use tokio::sync::watch;
    use uuid::Uuid;

    fn insert_scheduler_resources(app: &mut App) {
        app.insert_resource(SchedulerState::default());
        app.insert_resource(SchedulerStateWatcher::default());
    }

    /// 与 `insert_scheduler_resources` 类似，但用真实的 `watch::channel` 连接
    /// `SchedulerStateWatcher`，返回 receiver 供测试断言 watch 是否被通知。
    fn insert_scheduler_resources_with_watch(app: &mut App) -> watch::Receiver<SchedulerState> {
        app.insert_resource(SchedulerState::default());
        let (tx, rx) = watch::channel(SchedulerState::default());
        app.insert_resource(SchedulerStateWatcher(Some(tx)));
        rx
    }

    /// 构造一条动态定时任务定义并写入 `SchedulerState`，返回其 kind。
    fn insert_definition(
        app: &mut App,
        content: &str,
        schedule: &str,
        output_channel: Option<ChannelId>,
    ) -> String {
        let def = ScheduleDefinition::new(
            Uuid::new_v4(),
            None,
            content.to_string(),
            ScheduleExpr::parse(schedule).unwrap(),
            output_channel,
            Utc::now(),
        );
        let kind = def.kind();
        app.world_mut()
            .resource_mut::<SchedulerState>()
            .dynamic_tasks_mut()
            .push(def);
        kind
    }

    fn once_in_future() -> String {
        format!(
            "once:{}",
            (Utc::now() + chrono::Duration::minutes(5)).to_rfc3339()
        )
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
            messages[0].routing_policy.approval_context(),
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

        let channel = ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "chat".to_string(),
            thread_id: None,
        };
        let kind = insert_definition(&mut app, "say hi", &once_in_future(), Some(channel.clone()));

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
        assert_eq!(messages[0].routing_policy.output_channel(), Some(&channel));
        assert_eq!(
            messages[0].routing_policy.approval_channel(),
            Some(&channel)
        );
        let def = app
            .world()
            .resource::<SchedulerState>()
            .find_by_kind(&kind)
            .cloned()
            .expect("once 任务在运行结束前保留定义");
        assert!(
            def.health.last_fired_at.is_some(),
            "触发时记录 last_fired_at"
        );
    }

    #[test]
    fn scheduled_cron_task_is_not_cleaned_up_after_trigger() {
        let mut app = App::new();
        app.insert_resource(SignalTriggerRegistry::default());
        insert_scheduler_resources(&mut app);
        app.add_systems(Update, trigger_task_routing_system);

        let channel = ChannelId {
            frontend: FrontendKind::Tui,
            user_id: "user".to_string(),
            thread_id: None,
        };
        let kind = insert_definition(&mut app, "cron job", "cron:0 8 * * *", Some(channel));

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
        assert!(
            app.world()
                .resource::<SchedulerState>()
                .find_by_kind(&kind)
                .is_some(),
            "cron 任务应保留"
        );
    }

    /// once 任务触发后 `SchedulerStateWatcher` 必须收到通知（触发记账经统一修改入口），
    /// timer 据 `last_fired_at` 不再重复调度它。
    #[test]
    fn once_task_trigger_notifies_scheduler_state_watch() {
        let mut app = App::new();
        app.insert_resource(SignalTriggerRegistry::default());
        let mut rx = insert_scheduler_resources_with_watch(&mut app);
        app.add_systems(Update, trigger_task_routing_system);

        let kind = insert_definition(&mut app, "once say hi", &once_in_future(), None);
        // 初始 receiver 值是 default，先标记为已读，便于后续判断是否「再次」发送
        rx.borrow_and_update();

        app.world_mut().spawn(TriggerTaskMessage {
            source: SignalSource("scheduler:test".to_string()),
            trigger: TaskTrigger::Timer { kind: kind.clone() },
        });

        app.update();

        assert!(
            rx.has_changed().unwrap(),
            "SchedulerStateWatcher 必须在 once 任务触发记账后收到通知"
        );
        assert!(
            rx.borrow_and_update()
                .find_by_kind(&kind)
                .is_some_and(|d| d.health.last_fired_at.is_some()),
            "广播出的状态中 once 任务应已标记为已触发"
        );
    }

    #[test]
    fn scheduled_task_without_definition_is_dropped() {
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

    // ============ 暂停 / 重叠 / 补跑 / 记账 ============

    use std::sync::{Arc, Mutex};

    use crate::domain::{Frontend, ScheduleFire, ScheduledRun, Task, UserAction};

    struct RecordingFrontend(Arc<Mutex<Vec<EngineEvent>>>);
    impl Frontend for RecordingFrontend {
        fn kind(&self) -> FrontendKind {
            FrontendKind::Tui
        }
        fn push_event(&self, event: EngineEvent) {
            self.0.lock().unwrap().push(event);
        }
        fn poll_actions(&self) -> Vec<UserAction> {
            vec![]
        }
    }

    fn tui_channel() -> ChannelId {
        ChannelId {
            frontend: FrontendKind::Tui,
            user_id: "user".to_string(),
            thread_id: None,
        }
    }

    /// 带记录前端的路由 App，返回 (app, 前端收到的事件)。
    fn routing_app() -> (App, Arc<Mutex<Vec<EngineEvent>>>) {
        let mut app = App::new();
        app.insert_resource(SignalTriggerRegistry::default());
        insert_scheduler_resources(&mut app);
        let events = Arc::new(Mutex::new(Vec::new()));
        app.insert_resource(FrontendRegistry {
            frontends: vec![Box::new(RecordingFrontend(events.clone()))],
        });
        app.add_systems(Update, trigger_task_routing_system);
        (app, events)
    }

    fn fire(app: &mut App, kind: &str, marker: Option<ScheduleFire>) {
        let mut entity = app.world_mut().spawn(TriggerTaskMessage {
            source: SignalSource("scheduler:test".to_string()),
            trigger: TaskTrigger::Timer {
                kind: kind.to_string(),
            },
        });
        if let Some(marker) = marker {
            entity.insert(marker);
        }
    }

    fn created(app: &mut App) -> Vec<(String, Option<ScheduledRun>)> {
        app.world_mut()
            .query::<(&CreateTaskMessage, Option<&ScheduledRun>)>()
            .iter(app.world())
            .map(|(m, r)| (m.content.clone(), r.copied()))
            .collect()
    }

    fn definition_of(app: &App, kind: &str) -> Option<ScheduleDefinition> {
        app.world()
            .resource::<SchedulerState>()
            .find_by_kind(kind)
            .cloned()
    }

    fn texts(events: &Arc<Mutex<Vec<EngineEvent>>>) -> Vec<String> {
        events
            .lock()
            .unwrap()
            .iter()
            .filter_map(|e| match e {
                EngineEvent::Text { content, .. } => Some(content.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn webhook_with_scheduled_kind_does_not_fire_dynamic_task() {
        let (mut app, _events) = routing_app();
        let kind = insert_definition(&mut app, "cron job", "cron:0 8 * * *", None);

        app.world_mut().spawn(TriggerTaskMessage {
            source: SignalSource("webhook".to_string()),
            trigger: TaskTrigger::Webhook {
                kind: kind.clone(),
                body: serde_json::json!({}),
            },
        });
        app.update();

        assert!(
            created(&mut app).is_empty(),
            "webhook must not fire scheduled tasks"
        );
        assert!(
            definition_of(&app, &kind)
                .unwrap()
                .health
                .last_fired_at
                .is_none()
        );
    }

    #[test]
    fn cron_trigger_marks_run_and_records_last_fired_at() {
        let (mut app, _events) = routing_app();
        let kind = insert_definition(&mut app, "cron job", "cron:0 8 * * *", None);
        let id = definition_of(&app, &kind).unwrap().id;
        let before = Utc::now();

        fire(&mut app, &kind, None);
        app.update();

        let runs = created(&mut app);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].0, "cron job");
        assert_eq!(runs[0].1.unwrap().schedule_id, id);
        assert!(runs[0].1.unwrap().fired_at.unwrap() >= before);
        let last = definition_of(&app, &kind)
            .unwrap()
            .health
            .last_fired_at
            .expect("last_fired_at recorded");
        assert!(last >= before);
    }

    #[test]
    fn paused_definition_is_skipped_but_manual_run_proceeds() {
        let (mut app, _events) = routing_app();
        let kind = insert_definition(&mut app, "paused job", "cron:0 8 * * *", None);
        app.world_mut()
            .resource_mut::<SchedulerState>()
            .find_by_kind_mut(&kind)
            .unwrap()
            .enabled = false;

        fire(&mut app, &kind, None);
        app.update();
        assert!(created(&mut app).is_empty(), "paused schedule must not run");
        assert!(
            definition_of(&app, &kind)
                .unwrap()
                .health
                .last_fired_at
                .is_none()
        );

        fire(&mut app, &kind, Some(ScheduleFire::Manual));
        app.update();
        assert_eq!(created(&mut app).len(), 1, "manual run ignores pause");
    }

    #[test]
    fn overlapping_trigger_is_skipped_while_previous_run_is_active() {
        let (mut app, _events) = routing_app();
        let kind = insert_definition(&mut app, "slow job", "cron:*/10 * * * *", None);
        let id = definition_of(&app, &kind).unwrap().id;
        let task = Task::from_trigger(
            "slow job".to_string(),
            3,
            TaskRoutingPolicy::scheduled_task(None, "scheduled task"),
        );
        let task_entity = app.world_mut().spawn((task, ScheduledRun::new(id))).id();

        fire(&mut app, &kind, None);
        app.update();
        assert!(
            created(&mut app).is_empty(),
            "active run blocks new trigger"
        );

        app.world_mut()
            .get_mut::<Task>(task_entity)
            .unwrap()
            .mark_done("done", Utc::now());
        fire(&mut app, &kind, None);
        app.update();
        assert_eq!(created(&mut app).len(), 1, "terminal run no longer blocks");
    }

    #[test]
    fn duplicate_triggers_in_same_frame_create_one_run() {
        let (mut app, _events) = routing_app();
        let kind = insert_definition(&mut app, "job", "cron:0 8 * * *", None);

        fire(&mut app, &kind, None);
        fire(&mut app, &kind, Some(ScheduleFire::Manual));
        app.update();

        assert_eq!(created(&mut app).len(), 1);
    }

    #[test]
    fn catch_up_fire_prefixes_content_and_notifies_user() {
        let (mut app, events) = routing_app();
        let kind = insert_definition(
            &mut app,
            "daily report",
            "cron:0 8 * * *",
            Some(tui_channel()),
        );
        let missed_at = Utc::now() - chrono::Duration::hours(3);

        fire(&mut app, &kind, Some(ScheduleFire::CatchUp { missed_at }));
        app.update();

        let runs = created(&mut app);
        assert_eq!(runs.len(), 1);
        assert!(runs[0].0.starts_with("[补跑]"), "content: {}", runs[0].0);
        assert!(runs[0].0.ends_with("daily report"));
        let notices = texts(&events);
        assert_eq!(notices.len(), 1);
        assert!(notices[0].contains("补跑"), "notice: {}", notices[0]);
    }

    #[test]
    fn overdue_once_beyond_grace_runs_as_catch_up() {
        let (mut app, events) = routing_app();
        let at = (Utc::now() - chrono::Duration::hours(2)).to_rfc3339();
        let kind = insert_definition(
            &mut app,
            "once job",
            &format!("once:{at}"),
            Some(tui_channel()),
        );

        fire(&mut app, &kind, None);
        app.update();

        let runs = created(&mut app);
        assert_eq!(runs.len(), 1);
        assert!(runs[0].0.starts_with("[补跑]"));
        assert_eq!(texts(&events).len(), 1);
        assert!(
            definition_of(&app, &kind).is_some_and(|d| d.health.last_fired_at.is_some()),
            "once kept (marked fired) until the run reaches a terminal state"
        );
    }

    #[test]
    fn once_beyond_catch_up_window_is_removed_without_running() {
        let (mut app, events) = routing_app();
        let at = (Utc::now() - chrono::Duration::days(8)).to_rfc3339();
        let kind = insert_definition(
            &mut app,
            "stale once",
            &format!("once:{at}"),
            Some(tui_channel()),
        );

        fire(&mut app, &kind, None);
        app.update();

        assert!(created(&mut app).is_empty());
        assert!(definition_of(&app, &kind).is_none());
        let notices = texts(&events);
        assert_eq!(notices.len(), 1);
        assert!(notices[0].contains("未执行"), "notice: {}", notices[0]);
    }

    #[test]
    fn dynamic_context_includes_state_path_policy_and_verified_report_health() {
        use crate::domain::{NotifyPolicy, RunOutcome, ScheduleRunRecord};
        use crate::infrastructure::schedules::ScheduleStore;
        let directory = tempfile::tempdir().unwrap();
        let store = ScheduleStore::new(directory.path().join("schedules.json"));
        let (mut app, _) = routing_app();
        app.insert_resource(store.clone());
        let observed_kind = insert_definition(&mut app, "forum-watch", "cron:*/10 * * * *", None);
        let observed_id = definition_of(&app, &observed_kind).unwrap().id;
        {
            let mut state = app.world_mut().resource_mut::<SchedulerState>();
            let observed = state.find_by_kind_mut(&observed_kind).unwrap();
            observed.health.last_success_at = Some(Utc::now() - Duration::hours(1));
            observed.health.consecutive_failures = 2;
            observed.health.alerting = true;
        }
        store
            .append_run(&ScheduleRunRecord {
                task_id: None,
                schedule_id: observed_id,
                fired_at: Utc::now() - Duration::minutes(20),
                finished_at: Utc::now() - Duration::minutes(10),
                outcome: RunOutcome::Failed,
                error: Some("observed failure".into()),
                result_summary: String::new(),
                notification_count: 0,
            })
            .unwrap();
        let kind = insert_definition(&mut app, "daily report", "cron:0 8 * * *", None);
        let id = definition_of(&app, &kind).unwrap().id;
        {
            let mut state = app.world_mut().resource_mut::<SchedulerState>();
            let def = state.find_by_kind_mut(&kind).unwrap();
            def.notify = NotifyPolicy::Explicit;
            def.report_of = Some(observed_id);
        }
        fire(&mut app, &kind, Some(ScheduleFire::Manual));
        app.update();
        let runs = created(&mut app);
        assert_eq!(runs.len(), 1);
        let content = &runs[0].0;
        assert!(
            content.contains("<schedule-context"),
            "scheduled runs need durable runtime context"
        );
        assert!(content.contains("notify: explicit"));
        assert!(content.contains("trigger=\"manual\""));
        assert!(content.contains(&store.state_dir(id).display().to_string()));
        assert!(store.state_dir(id).is_dir());
        assert!(content.contains(&observed_id.to_string()));
        assert!(content.contains("consecutive_failures: 2"));
        assert!(content.contains("alerting: true"));
        assert!(content.contains("failed=1"));
        assert_eq!(runs[0].1.unwrap().notify, NotifyPolicy::Explicit);
    }

    #[test]
    fn overlap_and_expired_once_are_recorded_without_counting_as_failed_runs() {
        use crate::domain::RunOutcome;
        use crate::infrastructure::schedules::ScheduleStore;
        let directory = tempfile::tempdir().unwrap();
        let store = ScheduleStore::new(directory.path().join("schedules.json"));
        let (mut app, _) = routing_app();
        app.insert_resource(store.clone());
        app.add_systems(
            Update,
            crate::systems::schedule::schedule_run_accounting_system
                .after(trigger_task_routing_system),
        );
        let active_kind = insert_definition(&mut app, "active", "cron:*/10 * * * *", None);
        let active_id = definition_of(&app, &active_kind).unwrap().id;
        app.world_mut().spawn((
            Task::from_trigger(
                "active",
                0,
                TaskRoutingPolicy::scheduled_task(None, "scheduled task"),
            ),
            ScheduledRun::new(active_id),
        ));
        let at = (Utc::now() - Duration::days(8)).to_rfc3339();
        let expired_kind = insert_definition(&mut app, "expired", &format!("once:{at}"), None);
        let expired_id = definition_of(&app, &expired_kind).unwrap().id;
        fire(&mut app, &active_kind, None);
        fire(&mut app, &expired_kind, None);
        app.update();
        app.update();
        for (id, expected) in [
            (active_id, RunOutcome::SkippedOverlap),
            (expired_id, RunOutcome::Missed),
        ] {
            let records =
                std::fs::read_to_string(store.state_dir(id).parent().unwrap().join("runs.jsonl"))
                    .unwrap_or_default();
            let records: Vec<crate::domain::ScheduleRunRecord> = records
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            assert_eq!(
                records.len(),
                1,
                "each unexecuted trigger needs a durable ledger result"
            );
            assert_eq!(records[0].outcome, expected);
        }
        assert_eq!(
            definition_of(&app, &active_kind)
                .unwrap()
                .health
                .consecutive_failures,
            0
        );
    }
    #[test]
    fn corrupt_runtime_statistics_create_failed_run_instead_of_silent_success() {
        let dir = tempfile::tempdir().unwrap();
        let store =
            crate::infrastructure::schedules::ScheduleStore::new(dir.path().join("schedules.json"));
        let (mut app, _) = routing_app();
        app.insert_resource(store.clone());
        let kind = insert_definition(&mut app, "check", "cron:*/5 * * * *", None);
        let id = definition_of(&app, &kind).unwrap().id;
        let ledger = store.state_dir(id).parent().unwrap().join("runs.jsonl");
        std::fs::create_dir_all(ledger.parent().unwrap()).unwrap();
        std::fs::write(&ledger, "invalid JSON").unwrap();
        fire(&mut app, &kind, None);
        app.update();
        assert!(
            created(&mut app).is_empty(),
            "LLM cannot run on fictitious zero statistics"
        );
        let mut query = app.world_mut().query::<(&Task, &ScheduledRun)>();
        let (task, run) = query.single(app.world()).unwrap();
        assert!(matches!(
            task.status(),
            crate::domain::TaskStatus::Failed(_)
        ));
        assert_eq!(run.schedule_id, id);
        assert!(task.last_error.as_deref().unwrap().contains("line 1"));
        assert_eq!(std::fs::read_to_string(&ledger).unwrap(), "invalid JSON");
    }

    #[test]
    fn expired_once_keeps_definition_until_skipped_ledger_write_can_retry() {
        let dir = tempfile::tempdir().unwrap();
        let store =
            crate::infrastructure::schedules::ScheduleStore::new(dir.path().join("schedules.json"));
        let (mut app, events) = routing_app();
        app.insert_resource(store.clone());
        app.add_systems(
            Update,
            crate::systems::schedule_run_accounting_system.after(trigger_task_routing_system),
        );
        let at = (Utc::now() - Duration::days(8)).to_rfc3339();
        let kind = insert_definition(
            &mut app,
            "stale",
            &format!("once:{at}"),
            Some(tui_channel()),
        );
        let id = definition_of(&app, &kind).unwrap().id;
        let path = store.state_dir(id).parent().unwrap().join("runs.jsonl");
        std::fs::create_dir_all(&path).unwrap();
        fire(&mut app, &kind, None);
        app.update();
        app.update();
        assert!(definition_of(&app, &kind).is_some());
        assert_eq!(
            texts(&events)
                .iter()
                .filter(|text| text.contains("存储失败"))
                .count(),
            1
        );
        std::fs::remove_dir(&path).unwrap();
        app.update();
        app.update();
        assert!(definition_of(&app, &kind).is_none());
        assert_eq!(
            store
                .run_summary(id, chrono::DateTime::<Utc>::MIN_UTC)
                .unwrap()
                .missed,
            1
        );
        assert!(store.load().unwrap().is_empty());
    }
    #[test]
    fn expired_record_retry_does_not_remove_a_rescheduled_definition() {
        let dir = tempfile::tempdir().unwrap();
        let store =
            crate::infrastructure::schedules::ScheduleStore::new(dir.path().join("schedules.json"));
        let (mut app, _) = routing_app();
        app.insert_resource(store.clone());
        app.add_systems(
            Update,
            crate::systems::schedule_run_accounting_system.after(trigger_task_routing_system),
        );
        let at = (Utc::now() - Duration::days(8)).to_rfc3339();
        let kind = insert_definition(&mut app, "rescheduled", &format!("once:{at}"), None);
        let id = definition_of(&app, &kind).unwrap().id;
        let path = store.state_dir(id).parent().unwrap().join("runs.jsonl");
        std::fs::create_dir_all(&path).unwrap();
        fire(&mut app, &kind, None);
        app.update();
        let future = format!("once:{}", (Utc::now() + Duration::days(1)).to_rfc3339());
        crate::triggers::update_scheduler_state(app.world_mut(), |state| {
            let def = state.find_by_kind_mut(&kind).unwrap();
            def.schedule = ScheduleExpr::parse(&future).unwrap();
            def.health.last_fired_at = None;
        });
        std::fs::remove_dir(&path).unwrap();
        app.update();
        app.update();
        assert_eq!(definition_of(&app, &kind).unwrap().schedule.raw(), future);
        assert_eq!(
            store
                .run_summary(id, chrono::DateTime::<Utc>::MIN_UTC)
                .unwrap()
                .missed,
            1
        );
    }
}
