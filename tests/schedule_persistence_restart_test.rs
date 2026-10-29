//! 定时任务跨"重启"恢复的端到端测试。
//!
//! 第一个 App 创建定义并由 `schedule_persist_system` 落盘；第二个 App 模拟重启：
//! 从同一 `ScheduleStore` 加载定义，Startup 补跑系统为错过的 cron 生成补跑触发，
//! 路由后得到带 `[补跑]` 前缀与 `ScheduledRun` 标记的任务创建消息，`last_fired_at` 落盘。

use chrono::Utc;
use harness::domain::{
    CreateTaskMessage, ScheduleDefinition, ScheduleExpr, ScheduledRun, SignalTriggerRegistry,
};
use harness::infrastructure::schedules::ScheduleStore;
use harness::prelude::*;
use harness::systems::transform::{trigger_task_routing_system, user_message_to_task_system};
use harness::systems::{
    HarnessSettings, schedule_catch_up_startup_system, schedule_persist_system,
};
use harness::triggers::{SchedulerState, SchedulerStateWatcher, update_scheduler_state};

/// 只装定时任务相关资源与系统的 App（补跑 → 路由 → 建任务 → 持久化）。
fn app_with(store: &ScheduleStore, state: SchedulerState) -> App {
    let mut app = App::new();
    app.insert_resource(HarnessSettings::default_test());
    app.init_resource::<harness::ecs::EntityIndex>();
    app.insert_resource(SignalTriggerRegistry::default());
    app.insert_resource(state);
    app.insert_resource(SchedulerStateWatcher::default());
    app.insert_resource(store.clone());
    app.add_systems(Startup, schedule_catch_up_startup_system);
    app.add_systems(
        Update,
        (
            trigger_task_routing_system,
            user_message_to_task_system,
            schedule_persist_system,
        )
            .chain(),
    );
    app
}

#[test]
fn definitions_survive_restart_and_missed_cron_is_caught_up() {
    let dir = tempfile::tempdir().unwrap();
    let store = ScheduleStore::new(dir.path().join("schedules.json"));

    // 第一次运行：创建一个每小时整点的定义，假装上次触发在 3 小时前
    let mut first = app_with(&store, SchedulerState::default());
    first.update();
    let mut def = ScheduleDefinition::new(
        uuid::Uuid::new_v4(),
        Some("hourly-report".to_string()),
        "汇报进度".to_string(),
        ScheduleExpr::parse("cron:0 * * * *").unwrap(),
        None,
        Utc::now() - chrono::Duration::days(1),
    );
    def.health.last_fired_at = Some(Utc::now() - chrono::Duration::hours(3));
    let id = def.id;
    update_scheduler_state(first.world_mut(), |state| {
        state.dynamic_tasks_mut().push(def)
    });
    first.update();
    drop(first);

    // 重启：从磁盘加载
    let mut restored = SchedulerState::default();
    *restored.dynamic_tasks_mut() = store.load().unwrap();
    assert_eq!(restored.dynamic_tasks().len(), 1);
    let mut second = app_with(&store, restored);
    second.update();

    let created: Vec<(String, Option<ScheduledRun>)> = second
        .world_mut()
        .query::<(&CreateTaskMessage, Option<&ScheduledRun>)>()
        .iter(second.world())
        .map(|(m, r)| (m.content.clone(), r.copied()))
        .collect();
    // 消息可能已在同帧被转成 Task；两种形态都接受
    let tasks: Vec<(String, Option<ScheduledRun>)> = second
        .world_mut()
        .query::<(&harness::domain::Task, Option<&ScheduledRun>)>()
        .iter(second.world())
        .map(|(t, r)| (t.content.clone(), r.copied()))
        .collect();
    let runs: Vec<_> = created.into_iter().chain(tasks).collect();
    assert_eq!(runs.len(), 1, "exactly one catch-up run: {runs:?}");
    assert!(runs[0].0.starts_with("[补跑]"), "content: {}", runs[0].0);
    assert!(runs[0].0.contains("汇报进度\n\n<schedule-context"));
    let run = runs[0].1.expect("catch-up keeps its schedule ownership");
    assert_eq!(run.schedule_id, id);
    assert!(run.fired_at.is_some());
    assert!(runs[0].0.contains("trigger=\"catch_up\""));

    // 补跑记账也已落盘
    second.update();
    let persisted = store.load().unwrap();
    let last = persisted[0].health.last_fired_at.unwrap();
    assert_eq!(run.fired_at, Some(last));
    assert!(Utc::now() - last < chrono::Duration::minutes(1));
}
