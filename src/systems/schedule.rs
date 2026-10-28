//! 动态定时任务的持久化与启动补跑系统
//!
//! `SchedulerState` 是动态定时任务定义的唯一账本；本模块在其变化时写穿到
//! `ScheduleStore`。写入只看 Bevy 变更检测，因此任何经 `update_scheduler_state*`
//! 入口的修改（创建、删除、修改、触发记账）都会被检测到并落盘，无需逐个调用点处理；
//! 晚于本系统发生的修改在下一帧写入。

use chrono::Utc;
use tracing::{info, warn};

use crate::domain::{
    CATCH_UP_WINDOW, CatchUp, ScheduleFire, SignalSource, TaskTrigger, TriggerTaskMessage,
    catch_up_decision,
};
use crate::infrastructure::schedules::ScheduleStore;
use crate::prelude::*;
use crate::triggers::SchedulerState;

/// 启动时为错过运行的 cron 定时任务各补跑一次（只补最近一次）。
///
/// 一次性任务不在这里处理：timer 启动后会立即触发已过期的一次性任务，补跑标注与过期
/// 删除由 `trigger_task_routing_system` 在路由时判定，避免重复触发。
pub fn schedule_catch_up_startup_system(mut commands: Commands, state: Res<SchedulerState>) {
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
}
