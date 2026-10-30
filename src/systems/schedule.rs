//! 动态定时任务的持久化与启动补跑系统
//!
//! `SchedulerState` 是动态定时任务定义的唯一账本；本模块在其变化时写穿到
//! `ScheduleStore`。写入只看 Bevy 变更检测，因此任何经 `update_scheduler_state*`
//! 入口的修改（创建、删除、修改、触发记账）都会被检测到并落盘，无需逐个调用点处理；
//! 晚于本系统发生的修改在下一帧写入。

use crate::systems::demand::{DeadlineKey, DemandSchedule};
use chrono::Utc;
use tracing::{info, warn};

use crate::contracts::FrontendRegistry;
use crate::domain::{
    CATCH_UP_WINDOW, CatchUp, ScheduleFire, ScheduledRun, SignalSource, Task, TaskTrigger,
    TriggerTaskMessage, catch_up_decision,
};
use crate::infrastructure::schedules::ScheduleStore;
use crate::prelude::*;
use crate::systems::transform::trigger_task::{format_local, notify};
use crate::triggers::{SchedulerState, SchedulerStateWatcher, update_scheduler_state_with_watcher};

/// 启动时为错过运行的 cron 定时任务各补跑一次（只补最近一次），并处理上次被中断的一次性任务。
///
/// 未触发的一次性任务不在这里处理：timer 启动后会立即触发已过期的一次性任务，补跑标注与
/// 过期删除由 `trigger_task_routing_system` 在路由时判定，避免重复触发。
///
/// 已触发（`last_fired_at` 已设置）却仍在账本中的一次性任务，说明上次运行未进入终态就停机了：
/// 不自动重跑（避免重复副作用），转为暂停并提醒用户用 `run_now` 重新运行或删除。
pub fn schedule_catch_up_startup_system(
    mut commands: Commands,
    mut state: ResMut<SchedulerState>,
    watcher: Option<Res<SchedulerStateWatcher>>,
    frontends: Option<Res<FrontendRegistry>>,
) {
    let interrupted: Vec<_> = state
        .dynamic_tasks()
        .iter()
        .filter(|d| d.enabled && d.schedule.is_once() && d.health.last_fired_at.is_some())
        .cloned()
        .collect();
    if !interrupted.is_empty() {
        let ids: Vec<_> = interrupted.iter().map(|d| d.id).collect();
        let pause = |state: &mut SchedulerState| {
            for d in state.dynamic_tasks_mut() {
                if ids.contains(&d.id) {
                    d.enabled = false;
                }
            }
        };
        match watcher.as_deref() {
            Some(watcher) => update_scheduler_state_with_watcher(&mut state, watcher, pause),
            None => pause(&mut state),
        }
        for def in &interrupted {
            warn!(
                event = "OnceScheduleInterrupted",
                name = %def.name,
                "one-shot scheduled run was interrupted before completion, paused"
            );
            let fired_at = def
                .health
                .last_fired_at
                .map(format_local)
                .unwrap_or_default();
            notify(
                frontends.as_deref(),
                def,
                format!(
                    "一次性定时任务「{}」于 {fired_at} 开始运行后 harness 停止，运行未完成。\
                     已暂停该任务，可用 update_scheduled_task 的 run_now 重新运行，或删除它。",
                    def.name
                ),
            );
        }
    }

    let now = Utc::now();
    for def in state.dynamic_tasks() {
        if !def.enabled || def.schedule.is_once() {
            continue;
        }
        match catch_up_decision(def, now, CATCH_UP_WINDOW) {
            CatchUp::None => {}
            CatchUp::Fire { missed_at } => {
                info!(
                    event = "ScheduleCatchUp",
                    name = %def.name,
                    missed_at = %missed_at,
                    "firing one catch-up run for missed cron schedule"
                );
                commands.spawn((
                    TriggerTaskMessage {
                        source: SignalSource("schedule:catch_up".to_string()),
                        trigger: TaskTrigger::Timer { kind: def.kind() },
                    },
                    ScheduleFire::CatchUp { missed_at },
                ));
            }
            CatchUp::Expired { missed_at } => {
                commands.spawn(PendingScheduleRecord {
                    record: crate::domain::ScheduleRunRecord {
                        task_id: None,
                        schedule_id: def.id,
                        fired_at: missed_at,
                        finished_at: now,
                        outcome: crate::domain::RunOutcome::Missed,
                        error: None,
                        result_summary: "outside catch-up window".into(),
                        notification_count: 0,
                    },
                    expired: None,
                });
                info!(
                    event = "ScheduleCatchUpExpired",
                    name = %def.name,
                    missed_at = %missed_at,
                    "missed cron runs are beyond catch-up window, skipped"
                );
            }
        }
    }
}

/// 定时运行的账本与健康状态已经持久提交。
#[derive(Component)]
pub struct ScheduleRunRecorded;

/// 未执行触发也经同一重试链路持久化；一次性过期定义在账本落盘后才删除。
#[derive(Component)]
pub struct PendingScheduleRecord {
    pub record: crate::domain::ScheduleRunRecord,
    pub expired: Option<crate::domain::ScheduleDefinition>,
}

/// 等待所有输出派发与平台回执后提交账本，再持久提交健康状态；成功后才发布通知。
#[allow(clippy::too_many_arguments)]
pub fn schedule_run_accounting_system(
    mut commands: Commands,
    runs: Query<(Entity, &Task, &ScheduledRun), Without<ScheduleRunRecorded>>,
    execution_failures: Query<&crate::domain::ScheduledRunFailure>,
    skipped: Query<(Entity, &PendingScheduleRecord)>,
    outputs: Query<&crate::domain::UserOutputMessage>,
    sends: Query<&crate::domain::PendingChannelSend>,
    mut state: ResMut<SchedulerState>,
    watcher: Option<Res<SchedulerStateWatcher>>,
    store: Option<Res<ScheduleStore>>,
    mut deliveries: Option<ResMut<crate::systems::ScheduleDeliveryTracker>>,
    settings: Option<Res<crate::systems::HarnessSettings>>,
    frontends: Option<Res<FrontendRegistry>>,
    mut storage_alerted: Local<std::collections::HashSet<Entity>>,
    mut schedule: Option<ResMut<DemandSchedule>>,
    mut completed_at: Local<
        std::collections::HashMap<crate::domain::TaskId, chrono::DateTime<Utc>>,
    >,
) {
    use crate::domain::{RunOutcome, ScheduleRunRecord, TaskStatus};
    if let Some(tracker) = deliveries.as_deref_mut() {
        tracker.poll_receipts();
    }
    for (entity, pending) in &skipped {
        let retry_key = DeadlineKey::Accounting(entity);
        if schedule.as_deref().is_some_and(|s| s.blocked(retry_key)) {
            continue;
        }
        // 重试期间用户可能已经重新调度；旧的过期事件不能删除新定义。
        let expired = pending.expired.as_ref().filter(|expired| {
            state.dynamic_tasks().iter().any(|current| {
                current.id == expired.id && current.schedule.raw() == expired.schedule.raw()
            })
        });
        if let Some(store) = store.as_deref() {
            if let Err(error) = store.append_run(&pending.record) {
                warn!(event = "ScheduleAccountingFailed", schedule_id = %pending.record.schedule_id, %error, "skipped run ledger will retry");
                if let Some(schedule) = schedule.as_deref_mut() {
                    schedule.retry(retry_key);
                }
                if storage_alerted.insert(entity)
                    && let Some(def) = state
                        .dynamic_tasks()
                        .iter()
                        .find(|def| def.id == pending.record.schedule_id)
                {
                    notify(
                        frontends.as_deref(),
                        def,
                        format!("定时任务「{}」运行记录存储失败，将重试：{error}", def.name),
                    );
                }
                continue;
            }
            if expired.is_some() {
                let mut defs = state.dynamic_tasks().to_vec();
                defs.retain(|def| def.id != pending.record.schedule_id);
                if let Err(error) = store.save(&defs) {
                    warn!(event = "ScheduleAccountingFailed", %error, "expired definition removal will retry");
                    if let Some(schedule) = schedule.as_deref_mut() {
                        schedule.retry(retry_key);
                    }
                    continue;
                }
            }
        }
        if let Some(def) = expired {
            let apply =
                |state: &mut SchedulerState| state.dynamic_tasks_mut().retain(|d| d.id != def.id);
            if let Some(watcher) = watcher.as_deref() {
                update_scheduler_state_with_watcher(&mut state, watcher, apply);
            } else {
                apply(&mut state);
            }
            notify(
                frontends.as_deref(),
                def,
                format!(
                    "定时任务「{}」原定于 {} 运行，已超过补跑期限，未执行并已删除。",
                    def.name,
                    format_local(pending.record.fired_at)
                ),
            );
        }
        if let Some(schedule) = schedule.as_deref_mut() {
            schedule.clear(retry_key);
        }
        storage_alerted.remove(&entity);
        commands.entity(entity).despawn();
    }
    let threshold = settings
        .as_deref()
        .map_or(3, |settings| settings.0.schedule_failure_alert_threshold);
    for (entity, task, run) in &runs {
        let retry_key = DeadlineKey::Accounting(entity);
        if schedule.as_deref().is_some_and(|s| s.blocked(retry_key)) {
            continue;
        }
        if !task.status().is_terminal()
            || outputs.iter().any(|output| output.task_id == task.id)
            || sends.iter().any(|send| send.task_id == task.id)
            || deliveries
                .as_deref()
                .is_some_and(|tracker| tracker.pending_for(task.id) > 0)
        {
            continue;
        }
        let delivery_error = deliveries
            .as_deref()
            .and_then(|tracker| tracker.failure_for(task.id));
        let execution_error = execution_failures.get(entity).ok().map(|e| e.0.as_str());
        let count = deliveries
            .as_deref()
            .map_or(0, |tracker| tracker.notified_for(task.id));
        let failed = !matches!(task.status(), TaskStatus::Done)
            || delivery_error.is_some()
            || execution_error.is_some();
        let record = ScheduleRunRecord {
            task_id: Some(task.id),
            schedule_id: run.schedule_id,
            fired_at: run.fired_at.unwrap_or(task.created_at),
            finished_at: *completed_at.entry(task.id).or_insert_with(Utc::now),
            outcome: if failed {
                RunOutcome::Failed
            } else if count > 0 {
                RunOutcome::Notified
            } else {
                RunOutcome::Silent
            },
            error: if failed {
                Some(
                    delivery_error
                        .or(execution_error)
                        .map(str::to_owned)
                        .or_else(|| task.last_error.clone())
                        .unwrap_or_else(|| format!("{:?}", task.status())),
                )
            } else {
                None
            },
            result_summary: task.result_summary.chars().take(2048).collect(),
            notification_count: count,
        };
        if let Some(store) = store.as_deref()
            && let Err(error) = store.append_run(&record)
        {
            warn!(event = "ScheduleAccountingFailed", task_id = %task.id, %error, "terminal run ledger will retry");
            if let Some(schedule) = schedule.as_deref_mut() {
                schedule.retry(retry_key);
            }
            if storage_alerted.insert(entity)
                && let Some(def) = state
                    .dynamic_tasks()
                    .iter()
                    .find(|def| def.id == run.schedule_id)
            {
                notify(
                    frontends.as_deref(),
                    def,
                    format!("定时任务「{}」运行记录存储失败，将重试：{error}", def.name),
                );
            }
            continue;
        }
        let mut defs = state.dynamic_tasks().to_vec();
        let mut notice = None;
        if let Some(def) = defs.iter_mut().find(|def| def.id == run.schedule_id) {
            let was_alerting = def.health.alerting;
            if failed {
                def.health.consecutive_failures = def.health.consecutive_failures.saturating_add(1);
                def.health.alerting |= def.health.consecutive_failures >= threshold;
            } else {
                def.health.last_success_at = Some(record.finished_at);
                def.health.consecutive_failures = 0;
                def.health.alerting = false;
            }
            notice = Some((
                def.clone(),
                !was_alerting && def.health.alerting,
                was_alerting && !def.health.alerting,
            ));
            if let Some(store) = store.as_deref()
                && let Err(error) = store.save(&defs)
            {
                warn!(event = "ScheduleAccountingFailed", task_id = %task.id, %error, "health commit will retry without incrementing twice");
                if let Some(schedule) = schedule.as_deref_mut() {
                    schedule.retry(retry_key);
                }
                if storage_alerted.insert(entity) {
                    notify(
                        frontends.as_deref(),
                        &notice.as_ref().unwrap().0,
                        format!("定时任务健康状态存储失败，将重试：{error}"),
                    );
                }
                continue;
            }
            let apply = |state: &mut SchedulerState| *state.dynamic_tasks_mut() = defs;
            if let Some(watcher) = watcher.as_deref() {
                update_scheduler_state_with_watcher(&mut state, watcher, apply);
            } else {
                apply(&mut state);
            }
        }
        if storage_alerted.remove(&entity)
            && let Some((def, _, _)) = &notice
        {
            notify(
                frontends.as_deref(),
                def,
                format!("定时任务「{}」运行记录存储恢复正常。", def.name),
            );
        }
        if let Some(schedule) = schedule.as_deref_mut() {
            schedule.clear(retry_key);
        }
        completed_at.remove(&task.id);
        commands.entity(entity).insert(ScheduleRunRecorded);
        if let Some(tracker) = deliveries.as_deref_mut() {
            tracker.forget(task.id);
        }
        info!(event = "ScheduleRunRecorded", task_id = %task.id, schedule_id = %run.schedule_id, outcome = ?record.outcome, notification_count = count, "scheduled run accounting committed");
        if let Some((def, alert, recovered)) = notice {
            if failed {
                notify(
                    frontends.as_deref(),
                    &def,
                    format!(
                        "定时任务「{}」运行失败：{}",
                        def.name,
                        record.error.as_deref().unwrap_or("unknown")
                    ),
                );
            }
            if alert {
                info!(event = "ScheduleFailureAlert", schedule_id = %def.id, task_id = %task.id, consecutive_failures = def.health.consecutive_failures, "scheduled task entered alerting state");
                notify(
                    frontends.as_deref(),
                    &def,
                    format!(
                        "定时任务「{}」连续失败 {} 次，进入告警状态。",
                        def.name, def.health.consecutive_failures
                    ),
                );
            }
            if recovered {
                info!(event = "ScheduleRecovered", schedule_id = %def.id, task_id = %task.id, "scheduled task recovered");
                notify(
                    frontends.as_deref(),
                    &def,
                    format!("定时任务「{}」恢复正常。", def.name),
                );
            }
        }
    }
}

/// 一次性定时任务的运行进入终态后删除其定义。
///
/// 触发时只记 `last_fired_at`（timer 据此不再调度），定义保留到运行结束：运行失败或进程
/// 中途停止时定义仍在，重启后由 `schedule_catch_up_startup_system` 转为暂停并提醒。
pub fn once_schedule_completion_system(
    runs: Query<(&Task, &ScheduledRun), With<ScheduleRunRecorded>>,
    mut state: ResMut<SchedulerState>,
    watcher: Res<SchedulerStateWatcher>,
) {
    let finished: Vec<_> = runs
        .iter()
        .filter(|(task, _)| task.status().is_terminal())
        .filter_map(|(_, run)| {
            state
                .dynamic_tasks()
                .iter()
                .find(|d| d.id == run.schedule_id && d.schedule.is_once())
                .map(|d| (d.id, d.kind()))
        })
        .collect();
    if finished.is_empty() {
        return;
    }
    update_scheduler_state_with_watcher(&mut state, &watcher, |state| {
        state
            .dynamic_tasks_mut()
            .retain(|d| !finished.iter().any(|(id, _)| *id == d.id));
    });
    for (_, kind) in &finished {
        info!(
            event = "DynamicTaskRemoved",
            kind = %kind,
            reason = "once scheduled run finished",
            "dynamic once scheduled task removed after its run finished"
        );
    }
}

/// `SchedulerState` 变化时把动态定时任务定义写入 `ScheduleStore`。
///
/// 未插入 `ScheduleStore`（测试默认）时不落盘。写失败保留重试标记，
/// 后续帧即使状态未再变化也会重试。
pub fn schedule_persist_system(
    state: Res<SchedulerState>,
    store: Option<Res<ScheduleStore>>,
    mut retry: Local<bool>,
    mut schedule: Option<ResMut<DemandSchedule>>,
) {
    let Some(store) = store else {
        return;
    };
    if !state.is_changed() && !*retry {
        return;
    }
    if schedule
        .as_deref()
        .is_some_and(|s| s.blocked(DeadlineKey::Persist))
    {
        return;
    }
    *retry = false;
    if let Err(e) = store.save(state.dynamic_tasks()) {
        *retry = true;
        if let Some(schedule) = schedule.as_deref_mut() {
            schedule.retry(DeadlineKey::Persist);
        }
        warn!(
            event = "ScheduleStoreSaveFailed",
            path = %store.path().display(),
            error = %e,
            "failed to persist scheduled task definitions"
        );
    } else if let Some(schedule) = schedule.as_deref_mut() {
        schedule.clear(DeadlineKey::Persist);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{ScheduleDefinition, ScheduleExpr};
    use crate::triggers::{SchedulerStateWatcher, update_scheduler_state};
    use chrono::Utc;
    use uuid::Uuid;

    fn definition(name: &str) -> ScheduleDefinition {
        ScheduleDefinition::new(
            Uuid::new_v4(),
            Some(name.to_string()),
            "x".to_string(),
            ScheduleExpr::parse("cron:0 8 * * *").unwrap(),
            None,
            Utc::now(),
        )
    }

    fn catch_up_messages(app: &mut App) -> Vec<(String, ScheduleFire)> {
        app.world_mut()
            .query::<(&TriggerTaskMessage, &ScheduleFire)>()
            .iter(app.world())
            .map(|(m, f)| match &m.trigger {
                TaskTrigger::Timer { kind } => (kind.clone(), *f),
                other => panic!("unexpected trigger {other:?}"),
            })
            .collect()
    }

    fn startup_app(defs: Vec<ScheduleDefinition>) -> App {
        let mut state = SchedulerState::default();
        *state.dynamic_tasks_mut() = defs;
        let mut app = App::new();
        app.insert_resource(state);
        app.add_systems(Startup, schedule_catch_up_startup_system);
        app.update();
        app
    }

    #[test]
    fn startup_fires_one_catch_up_for_missed_cron() {
        let mut missed = definition("hourly");
        missed.schedule = ScheduleExpr::parse("cron:0 * * * *").unwrap();
        missed.health.last_fired_at = Some(Utc::now() - chrono::Duration::hours(5));
        let kind = missed.kind();

        let mut app = startup_app(vec![missed]);

        let messages = catch_up_messages(&mut app);
        assert_eq!(messages.len(), 1, "only the latest missed run is fired");
        assert_eq!(messages[0].0, kind);
        assert!(matches!(messages[0].1, ScheduleFire::CatchUp { .. }));
    }

    #[test]
    fn startup_skips_paused_up_to_date_and_once_definitions() {
        let mut paused = definition("paused");
        paused.schedule = ScheduleExpr::parse("cron:0 * * * *").unwrap();
        paused.health.last_fired_at = Some(Utc::now() - chrono::Duration::hours(5));
        paused.enabled = false;

        let mut up_to_date = definition("up-to-date");
        up_to_date.schedule = ScheduleExpr::parse("cron:0 0 1 1 *").unwrap();
        up_to_date.health.last_fired_at = Some(Utc::now());

        let mut once = definition("once");
        let past = (Utc::now() - chrono::Duration::hours(1)).to_rfc3339();
        once.schedule = ScheduleExpr::parse(&format!("once:{past}")).unwrap();

        let mut app = startup_app(vec![paused, up_to_date, once]);
        assert!(catch_up_messages(&mut app).is_empty());
    }

    fn app_with_store(dir: &tempfile::TempDir) -> (App, ScheduleStore) {
        let store = ScheduleStore::new(dir.path().join("schedules.json"));
        let mut app = App::new();
        app.insert_resource(SchedulerState::default());
        app.insert_resource(SchedulerStateWatcher::default());
        app.insert_resource(store.clone());
        app.add_systems(Update, schedule_persist_system);
        (app, store)
    }

    #[test]
    fn persists_definitions_when_state_changes() {
        let dir = tempfile::tempdir().unwrap();
        let (mut app, store) = app_with_store(&dir);
        app.update();

        update_scheduler_state(app.world_mut(), |state| {
            state.dynamic_tasks_mut().push(definition("forum-watch"));
        });
        app.update();

        let loaded = store.load().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "forum-watch");
    }

    #[test]
    fn does_not_rewrite_when_state_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let (mut app, store) = app_with_store(&dir);
        app.update();
        assert!(store.path().exists(), "initial state is written once");

        std::fs::remove_file(store.path()).unwrap();
        app.update();
        assert!(
            !store.path().exists(),
            "unchanged state must not be written again"
        );
    }

    #[test]
    fn without_store_resource_nothing_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("schedules.json");
        let mut app = App::new();
        app.insert_resource(SchedulerState::default());
        app.add_systems(Update, schedule_persist_system);
        update_scheduler_state(app.world_mut(), |state| {
            state.dynamic_tasks_mut().push(definition("x"));
        });
        app.update();
        assert!(!path.exists());
    }

    fn once_definition(name: &str) -> ScheduleDefinition {
        let at = (Utc::now() - chrono::Duration::minutes(10)).to_rfc3339();
        let mut def = definition(name);
        def.schedule = ScheduleExpr::parse(&format!("once:{at}")).unwrap();
        def
    }

    /// 已触发却仍在账本中的一次性任务：上次运行被中断，转为暂停且不自动重跑。
    #[test]
    fn startup_pauses_interrupted_once_without_rerun() {
        let mut interrupted = once_definition("interrupted");
        interrupted.health.last_fired_at = Some(Utc::now() - chrono::Duration::minutes(5));
        let pending = once_definition("pending");

        let mut app = startup_app(vec![interrupted.clone(), pending.clone()]);

        let state = app.world().resource::<SchedulerState>();
        let enabled = |id| {
            state
                .dynamic_tasks()
                .iter()
                .find(|d| d.id == id)
                .unwrap()
                .enabled
        };
        assert!(!enabled(interrupted.id), "interrupted once must be paused");
        assert!(
            enabled(pending.id),
            "unfired once stays enabled for the timer"
        );
        assert!(catch_up_messages(&mut app).is_empty());
    }

    fn completion_app(defs: Vec<ScheduleDefinition>) -> App {
        let mut state = SchedulerState::default();
        *state.dynamic_tasks_mut() = defs;
        let mut app = App::new();
        app.insert_resource(state);
        app.insert_resource(SchedulerStateWatcher::default());
        app.add_systems(Update, once_schedule_completion_system);
        app
    }

    fn run_task(status: crate::domain::TaskStatus) -> Task {
        let mut task = Task::from_trigger(
            "run".to_string(),
            3,
            crate::domain::TaskRoutingPolicy::scheduled_task(None, "scheduled task"),
        );
        task.status = status;
        task
    }

    #[test]
    fn once_definition_removed_only_after_run_finishes() {
        let once = once_definition("once");
        let cron = definition("cron");
        let mut app = completion_app(vec![once.clone(), cron.clone()]);
        let running = app
            .world_mut()
            .spawn((
                run_task(crate::domain::TaskStatus::Running),
                ScheduledRun::new(once.id),
            ))
            .id();
        app.world_mut().spawn((
            run_task(crate::domain::TaskStatus::Done),
            ScheduledRun::new(cron.id),
        ));

        app.update();
        let ids = |app: &App| -> Vec<_> {
            app.world()
                .resource::<SchedulerState>()
                .dynamic_tasks()
                .iter()
                .map(|d| d.id)
                .collect()
        };
        assert_eq!(
            ids(&app),
            vec![once.id, cron.id],
            "running once and cron are kept"
        );

        app.world_mut().get_mut::<Task>(running).unwrap().status =
            crate::domain::TaskStatus::Failed(crate::domain::FailureReason::Unknown);
        app.world_mut()
            .entity_mut(running)
            .insert(ScheduleRunRecorded);
        app.update();
        assert_eq!(ids(&app), vec![cron.id], "once removed after its run fails");
    }

    fn accounting_app(
        dir: &tempfile::TempDir,
        def: ScheduleDefinition,
    ) -> (
        App,
        ScheduleStore,
        std::sync::Arc<std::sync::Mutex<Vec<crate::domain::EngineEvent>>>,
    ) {
        use crate::domain::{EngineEvent, Frontend, FrontendKind, UserAction};
        use std::sync::{Arc, Mutex};
        struct RecordingFrontend(Arc<Mutex<Vec<EngineEvent>>>);
        impl Frontend for RecordingFrontend {
            fn kind(&self) -> FrontendKind {
                FrontendKind::Tui
            }
            fn push_event(&self, event: EngineEvent) {
                self.0.lock().unwrap().push(event);
            }
            fn poll_actions(&self) -> Vec<UserAction> {
                Vec::new()
            }
        }
        let (mut app, store) = app_with_store(dir);
        app.world_mut()
            .resource_mut::<SchedulerState>()
            .dynamic_tasks_mut()
            .push(def);
        app.insert_resource(crate::systems::HarnessSettings::default_test());
        let events = Arc::new(Mutex::new(Vec::new()));
        app.insert_resource(FrontendRegistry {
            frontends: vec![Box::new(RecordingFrontend(events.clone()))],
        });
        app.add_systems(
            Update,
            schedule_run_accounting_system.before(schedule_persist_system),
        );
        (app, store, events)
    }

    fn accounting_task(app: &mut App, schedule_id: uuid::Uuid, failed: bool) -> Entity {
        let mut task = Task::from_trigger(
            "scheduled work",
            0,
            crate::domain::TaskRoutingPolicy::scheduled_task(None, "scheduled task"),
        );
        if failed {
            task.mark_failed_reason(
                crate::domain::FailureReason::Unknown,
                "runtime accounting test failure",
                Utc::now(),
            );
        } else {
            task.mark_done("successful unchanged run", Utc::now());
        }
        app.world_mut()
            .spawn((task, ScheduledRun::new(schedule_id)))
            .id()
    }

    fn ledger_records(
        store: &ScheduleStore,
        id: uuid::Uuid,
    ) -> Vec<crate::domain::ScheduleRunRecord> {
        let path = store.state_dir(id).parent().unwrap().join("runs.jsonl");
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn accounting_definition() -> ScheduleDefinition {
        let mut def = definition("accounted-monitor");
        def.notify = crate::domain::NotifyPolicy::Explicit;
        def.output_channel = Some(crate::domain::ChannelId {
            frontend: crate::domain::FrontendKind::Tui,
            user_id: "accounting".into(),
            thread_id: None,
        });
        def
    }

    #[test]
    fn terminal_run_is_written_once_and_updates_success_health() {
        let directory = tempfile::tempdir().unwrap();
        let def = accounting_definition();
        let id = def.id;
        let (mut app, store, events) = accounting_app(&directory, def);
        let entity = accounting_task(&mut app, id, false);
        app.update();
        app.update();
        let records = ledger_records(&store, id);
        assert_eq!(
            records.len(),
            1,
            "terminal records are durable and idempotent across frames"
        );
        assert_eq!(records[0].outcome, crate::domain::RunOutcome::Silent);
        assert_eq!(records[0].notification_count, 0);
        assert!(app.world().get::<ScheduleRunRecorded>(entity).is_some());
        assert!(store.load().unwrap()[0].health.last_success_at.is_some());
        assert!(
            events.lock().unwrap().is_empty(),
            "successful explicit runs stay silent"
        );
    }

    #[test]
    fn denied_scheduled_action_cannot_be_accounted_as_silent_success() {
        use crate::domain::{
            AgentExecutionResult, AgentId, AgentRequestKind, ExecutionError, ShortTermMemory,
            ToolError, ToolExecutionResultMessage,
        };
        let directory = tempfile::tempdir().unwrap();
        let def = accounting_definition();
        let id = def.id;
        let (mut app, store, _) = accounting_app(&directory, def);
        app.init_resource::<crate::contracts::Clock>();
        app.insert_resource(crate::systems::HarnessSettings::default_test());
        app.add_systems(
            Update,
            crate::systems::tools::tool_result_system.before(schedule_run_accounting_system),
        );
        let entity = accounting_task(&mut app, id, false);
        let task_id = app.world().get::<Task>(entity).unwrap().id;
        app.world_mut()
            .entity_mut(entity)
            .insert(ShortTermMemory::default());
        app.world_mut().spawn(ToolExecutionResultMessage {
            result: AgentExecutionResult {
                task_id,
                agent_id: AgentId::nil(),
                request_kind: AgentRequestKind::LlmCompletion,
                result: Err(ExecutionError::Unknown(String::new())),
                prompt: String::new(),
                system_prompt: None,
                tools: vec![],
                reasoning_content: None,
                work_item_id: None,
                conversation: None,
                turn_generation: None,
            },
            tool_name: "shell_exec".into(),
            tool_output: Err(ToolError::PermissionDenied("unattended".into())),
            tool_call_id: None,
            processed: false,
            original_tool_output: None,
        });
        app.update();
        let records = ledger_records(&store, id);
        assert_eq!(records[0].outcome, crate::domain::RunOutcome::Failed);
        assert!(records[0].error.as_ref().unwrap().contains("shell_exec"));
        let defs = store.load().unwrap();
        assert_eq!(defs[0].health.consecutive_failures, 1);
        assert!(defs[0].health.last_success_at.is_none());
    }

    #[test]
    fn repeated_failures_alert_once_and_next_success_recovers_once() {
        let directory = tempfile::tempdir().unwrap();
        let def = accounting_definition();
        let id = def.id;
        let (mut app, store, events) = accounting_app(&directory, def);
        for _ in 0..4 {
            accounting_task(&mut app, id, true);
            app.update();
        }
        let health = &store.load().unwrap()[0].health;
        assert_eq!(health.consecutive_failures, 4);
        assert!(health.alerting);
        let texts = || {
            events
                .lock()
                .unwrap()
                .iter()
                .filter_map(|event| match event {
                    crate::domain::EngineEvent::Text { content, .. } => Some(content.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            texts()
                .iter()
                .filter(|text| text.contains("连续失败"))
                .count(),
            1,
            "threshold alert is emitted once"
        );
        assert_eq!(
            texts()
                .iter()
                .filter(|text| text.contains("运行失败"))
                .count(),
            4,
            "every failed run is visible despite explicit policy"
        );
        accounting_task(&mut app, id, false);
        app.update();
        app.update();
        let health = &store.load().unwrap()[0].health;
        assert_eq!(health.consecutive_failures, 0);
        assert!(!health.alerting);
        assert_eq!(
            texts()
                .iter()
                .filter(|text| text.contains("恢复正常"))
                .count(),
            1
        );
        assert_eq!(ledger_records(&store, id).len(), 5);
    }

    #[test]
    fn definition_save_failure_retries_without_duplicate_ledger_or_failure_increment() {
        let directory = tempfile::tempdir().unwrap();
        let def = accounting_definition();
        let id = def.id;
        let (mut app, store, _) = accounting_app(&directory, def);
        std::fs::create_dir_all(store.path()).unwrap();
        let entity = accounting_task(&mut app, id, true);
        app.update();
        app.update();
        assert!(app.world().get::<ScheduleRunRecorded>(entity).is_none());
        assert_eq!(
            app.world().resource::<SchedulerState>().dynamic_tasks()[0]
                .health
                .consecutive_failures,
            0
        );
        assert_eq!(
            ledger_records(&store, id).len(),
            1,
            "append retry is idempotent before a definition save succeeds"
        );
        std::fs::remove_dir(store.path()).unwrap();
        app.update();
        app.update();
        assert!(app.world().get::<ScheduleRunRecorded>(entity).is_some());
        assert_eq!(store.load().unwrap()[0].health.consecutive_failures, 1);
        assert_eq!(ledger_records(&store, id).len(), 1);
    }

    #[test]
    fn completed_once_definition_waits_for_durable_run_record() {
        let def = once_definition("record-before-delete");
        let id = def.id;
        let mut app = completion_app(vec![def]);
        let entity = accounting_task(&mut app, id, false);
        app.update();
        assert_eq!(
            app.world()
                .resource::<SchedulerState>()
                .dynamic_tasks()
                .len(),
            1,
            "terminal task alone cannot delete a definition before accounting and receipts settle"
        );
        app.world_mut()
            .entity_mut(entity)
            .insert(ScheduleRunRecorded);
        app.update();
        assert!(
            app.world()
                .resource::<SchedulerState>()
                .dynamic_tasks()
                .is_empty()
        );
    }
    #[test]
    fn ledger_io_failure_keeps_task_pending_then_retries_exactly_once() {
        let directory = tempfile::tempdir().unwrap();
        let def = accounting_definition();
        let id = def.id;
        let (mut app, store, _) = accounting_app(&directory, def);
        let ledger = store.state_dir(id).parent().unwrap().join("runs.jsonl");
        std::fs::create_dir_all(&ledger).unwrap();
        let entity = accounting_task(&mut app, id, true);
        app.update();
        app.update();
        assert!(app.world().get::<ScheduleRunRecorded>(entity).is_none());
        assert_eq!(
            app.world().resource::<SchedulerState>().dynamic_tasks()[0]
                .health
                .consecutive_failures,
            0
        );
        std::fs::remove_dir(&ledger).unwrap();
        app.update();
        app.update();
        assert!(app.world().get::<ScheduleRunRecorded>(entity).is_some());
        assert_eq!(ledger_records(&store, id).len(), 1);
        assert_eq!(store.load().unwrap()[0].health.consecutive_failures, 1);
    }

    #[test]
    fn definition_persistence_retries_even_without_a_new_state_change() {
        let directory = tempfile::tempdir().unwrap();
        let (mut app, store) = app_with_store(&directory);
        std::fs::create_dir_all(store.path()).unwrap();
        app.update();
        app.update();
        std::fs::remove_dir(store.path()).unwrap();
        app.update();
        assert!(store.path().is_file());
    }
}
