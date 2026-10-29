//! commit_tool_effects_system 处理 ToolEffect::UpdateSchedule 的测试。
//!
//! 验证修改在 apply 时刻定位并落账：内容 / 调度 / 启停原地更新、watch 通知、
//! 找不到任务时回送 InvalidInput、`run_now` 生成带 `ScheduleFire::Manual` 的触发。

use bevy_ecs::prelude::*;
use chrono::Utc;
use uuid::Uuid;

use harness::domain::{
    ScheduleDefinition, ScheduleExpr, ScheduleFire, ScheduleSelector, TaskTrigger, ToolAsyncResult,
    ToolEffect, ToolEffectPending, ToolError, ToolResultReceiver, ToolResultSender,
    ToolWorkerPayload, TriggerTaskMessage,
};
use harness::systems::commit_tool_effects_system;
use harness::triggers::scheduled_task::{SchedulerState, SchedulerStateWatcher};

/// 世界里预置一条名为 `forum-watch` 的定义，返回 (world, kind, watch receiver)。
fn world_with_definition() -> (World, String, tokio::sync::watch::Receiver<SchedulerState>) {
    let mut world = World::new();
    let def = ScheduleDefinition::new(
        Uuid::new_v4(),
        Some("forum-watch".to_string()),
        "old prompt".to_string(),
        ScheduleExpr::parse("cron:*/10 * * * *").unwrap(),
        None,
        Utc::now(),
    );
    let kind = def.kind();
    let mut state = SchedulerState::default();
    state.dynamic_tasks_mut().push(def);
    world.insert_resource(state);
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<ToolAsyncResult>();
    world.insert_resource(ToolResultSender(tx));
    world.insert_resource(ToolResultReceiver(rx));
    let (watch_tx, watch_rx) = tokio::sync::watch::channel(SchedulerState::default());
    world.insert_resource(SchedulerStateWatcher(Some(watch_tx)));
    (world, kind, watch_rx)
}

fn update_effect(
    selector: ScheduleSelector,
    content: Option<&str>,
    schedule: Option<&str>,
    enabled: Option<bool>,
    run_now: bool,
) -> ToolEffect {
    ToolEffect::UpdateSchedule {
        selector,
        content: content.map(str::to_string),
        schedule: schedule.map(|s| ScheduleExpr::parse(s).unwrap()),
        enabled,
        run_now,
        notify: None,
        report_of: None,
    }
}

#[test]
fn invalid_report_target_rejects_the_whole_update_patch() {
    let (mut world, kind, _rx) = world_with_definition();
    let own_id = world
        .resource::<SchedulerState>()
        .find_by_kind(&kind)
        .unwrap()
        .id;
    for target in [own_id, Uuid::new_v4()] {
        let mut effect = update_effect(
            ScheduleSelector::Kind(kind.clone()),
            Some("must not be saved"),
            None,
            None,
            false,
        );
        if let ToolEffect::UpdateSchedule { report_of, .. } = &mut effect {
            *report_of = Some(Some(target));
        }
        assert!(matches!(
            commit(&mut world, effect),
            Err(ToolError::InvalidInput(_))
        ));
        assert_eq!(
            world
                .resource::<SchedulerState>()
                .find_by_kind(&kind)
                .unwrap()
                .content,
            "old prompt"
        );
    }
}

#[test]
fn report_reference_can_be_set_then_explicitly_cleared_with_notification_policy() {
    let (mut world, kind, _rx) = world_with_definition();
    let source = ScheduleDefinition::new(
        Uuid::new_v4(),
        Some("source".into()),
        "check".into(),
        ScheduleExpr::parse("cron:*/5 * * * *").unwrap(),
        None,
        Utc::now(),
    );
    let source_id = source.id;
    harness::triggers::scheduled_task::update_scheduler_state(&mut world, |state| {
        state.dynamic_tasks_mut().push(source)
    });
    let mut effect = update_effect(
        ScheduleSelector::Kind(kind.clone()),
        None,
        None,
        None,
        false,
    );
    if let ToolEffect::UpdateSchedule {
        notify, report_of, ..
    } = &mut effect
    {
        *notify = Some(harness::domain::NotifyPolicy::Explicit);
        *report_of = Some(Some(source_id));
    }
    commit(&mut world, effect).unwrap();
    let def = world
        .resource::<SchedulerState>()
        .find_by_kind(&kind)
        .unwrap();
    assert_eq!(def.notify, harness::domain::NotifyPolicy::Explicit);
    assert_eq!(def.report_of, Some(source_id));
    let mut effect = update_effect(
        ScheduleSelector::Kind(kind.clone()),
        None,
        None,
        None,
        false,
    );
    if let ToolEffect::UpdateSchedule { report_of, .. } = &mut effect {
        *report_of = Some(None);
    }
    commit(&mut world, effect).unwrap();
    let def = world
        .resource::<SchedulerState>()
        .find_by_kind(&kind)
        .unwrap();
    assert_eq!(def.report_of, None);
    assert_eq!(def.notify, harness::domain::NotifyPolicy::Explicit);
}

fn commit(world: &mut World, effect: ToolEffect) -> Result<serde_json::Value, ToolError> {
    world.spawn(ToolEffectPending {
        tool_call_id: "call-update".to_string(),
        effect,
    });
    commit_tool_effects_system(world);
    let result = world
        .resource_mut::<ToolResultReceiver>()
        .0
        .try_recv()
        .expect("one result");
    match result.payload {
        ToolWorkerPayload::Completed(r) => r,
        other => panic!("expected Completed, got {:?}", other),
    }
}

fn manual_triggers(world: &mut World) -> Vec<String> {
    world
        .query::<(&TriggerTaskMessage, &ScheduleFire)>()
        .iter(world)
        .filter(|(_, fire)| **fire == ScheduleFire::Manual)
        .map(|(m, _)| match &m.trigger {
            TaskTrigger::Timer { kind } => kind.clone(),
            other => panic!("unexpected trigger {other:?}"),
        })
        .collect()
}

#[test]
fn update_by_name_changes_content_and_schedule() {
    let (mut world, kind, mut watch_rx) = world_with_definition();
    watch_rx.borrow_and_update();

    let v = commit(
        &mut world,
        update_effect(
            ScheduleSelector::Name("forum-watch".into()),
            Some("new prompt"),
            Some("cron:0 8 * * *"),
            None,
            false,
        ),
    )
    .unwrap();

    assert_eq!(v["status"], "updated");
    assert_eq!(v["kind"], kind);
    assert_eq!(v["schedule"], "cron:0 8 * * *");
    assert!(v["next_trigger"].is_string());
    let state = world.resource::<SchedulerState>();
    let def = state.find_by_kind(&kind).unwrap();
    assert_eq!(def.content, "new prompt");
    assert_eq!(def.schedule.raw(), "cron:0 8 * * *");
    assert!(
        watch_rx.has_changed().unwrap(),
        "timer must be told to reschedule"
    );
    assert!(manual_triggers(&mut world).is_empty());
}

#[test]
fn pause_and_resume_by_kind() {
    let (mut world, kind, _watch_rx) = world_with_definition();

    let paused = commit(
        &mut world,
        update_effect(
            ScheduleSelector::Kind(kind.clone()),
            None,
            None,
            Some(false),
            false,
        ),
    )
    .unwrap();
    assert_eq!(paused["enabled"], false);
    assert!(
        paused["next_trigger"].is_null(),
        "paused task has no next trigger"
    );
    assert!(
        !world
            .resource::<SchedulerState>()
            .find_by_kind(&kind)
            .unwrap()
            .enabled
    );

    let resumed = commit(
        &mut world,
        update_effect(
            ScheduleSelector::Kind(kind.clone()),
            None,
            None,
            Some(true),
            false,
        ),
    )
    .unwrap();
    assert_eq!(resumed["enabled"], true);
    assert!(
        world
            .resource::<SchedulerState>()
            .find_by_kind(&kind)
            .unwrap()
            .enabled
    );
}

#[test]
fn run_now_spawns_manual_trigger() {
    let (mut world, kind, _watch_rx) = world_with_definition();

    let v = commit(
        &mut world,
        update_effect(
            ScheduleSelector::Name("forum-watch".into()),
            None,
            None,
            None,
            true,
        ),
    )
    .unwrap();

    assert_eq!(v["run_now"], true);
    assert_eq!(manual_triggers(&mut world), vec![kind]);
}

#[test]
fn unknown_task_reports_invalid_input_and_changes_nothing() {
    let (mut world, kind, _watch_rx) = world_with_definition();

    let err = commit(
        &mut world,
        update_effect(
            ScheduleSelector::Name("ghost".into()),
            Some("x"),
            None,
            None,
            true,
        ),
    )
    .unwrap_err();

    match err {
        ToolError::InvalidInput(msg) => assert!(msg.contains("ghost"), "message: {msg}"),
        other => panic!("expected InvalidInput, got {other:?}"),
    }
    assert_eq!(
        world
            .resource::<SchedulerState>()
            .find_by_kind(&kind)
            .unwrap()
            .content,
        "old prompt"
    );
    assert!(manual_triggers(&mut world).is_empty());
}
