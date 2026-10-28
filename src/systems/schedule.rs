//! 动态定时任务的持久化与启动补跑系统
//!
//! `SchedulerState` 是动态定时任务定义的唯一账本；本模块在其变化时写穿到
//! `ScheduleStore`。写入只看 Bevy 变更检测，因此任何经 `update_scheduler_state*`
//! 入口的修改（创建、删除、修改、触发记账）都会被检测到并落盘，无需逐个调用点处理；
//! 晚于本系统发生的修改在下一帧写入。

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

/// 一次性定时任务的运行进入终态后删除其定义。
///
/// 触发时只记 `last_fired_at`（timer 据此不再调度），定义保留到运行结束：运行失败或进程
/// 中途停止时定义仍在，重启后由 `schedule_catch_up_startup_system` 转为暂停并提醒。
pub fn once_schedule_completion_system(
    runs: Query<(&Task, &ScheduledRun), Changed<Task>>,
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
/// 未插入 `ScheduleStore`（测试默认）时不落盘。写失败只记 `warn`，内存状态保持不变，
/// 下一次变化时会再次尝试写入。
pub fn schedule_persist_system(state: Res<SchedulerState>, store: Option<Res<ScheduleStore>>) {
    let Some(store) = store else {
        return;
    };
    if !state.is_changed() {
        return;
    }
    if let Err(e) = store.save(state.dynamic_tasks()) {
        warn!(
            event = "ScheduleStoreSaveFailed",
            path = %store.path().display(),
            error = %e,
            "failed to persist scheduled task definitions"
        );
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
                ScheduledRun {
                    schedule_id: once.id,
                },
            ))
            .id();
        app.world_mut().spawn((
            run_task(crate::domain::TaskStatus::Done),
            ScheduledRun {
                schedule_id: cron.id,
            },
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
        app.update();
        assert_eq!(ids(&app), vec![cron.id], "once removed after its run fails");
    }
}
