use harness::domain::{
    BuiltinTool, DynamicScheduledTaskSnapshot, OwnedToolContext, ScheduledTaskInfoSnapshot,
    ScheduledTaskRegistrySnapshot, SchedulerStateSnapshot, ToolWorkerOutput,
};
use harness::systems::tools::builtin::scheduled::list::ListScheduledTasksTool;
use harness::triggers::scheduled_task::ScheduleSpec;
use std::sync::Arc;

fn ctx_with_two_ledgers() -> OwnedToolContext {
    let state = SchedulerStateSnapshot {
        dynamic_tasks: vec![
            DynamicScheduledTaskSnapshot {
                id: uuid::Uuid::new_v4(),
                kind: "daily".into(),
                schedule: ScheduleSpec::Cron(Box::new(
                    "0 0 9 * * * *".parse::<cron::Schedule>().unwrap(),
                )),
                created_at: chrono::Utc::now(),
            },
            DynamicScheduledTaskSnapshot {
                id: uuid::Uuid::new_v4(),
                kind: "orphan".into(), // registry 里没有 → 过滤
                schedule: ScheduleSpec::Once(chrono::Utc::now()),
                created_at: chrono::Utc::now(),
            },
        ],
    };
    let mut registry = ScheduledTaskRegistrySnapshot::default();
    registry.tasks.insert(
        "daily".into(),
        ScheduledTaskInfoSnapshot {
            content: "每日报告".into(),
            output_channel: None,
            is_once: false,
        },
    );
    OwnedToolContext {
        scheduler_state: Some(Arc::new(state)),
        registry: Some(Arc::new(registry)),
        tool_inflight_timeout_secs: 300,
        ..Default::default()
    }
}

#[test]
fn list_joins_ledgers_and_filters_orphans() {
    let tool = ListScheduledTasksTool;
    let rt = tokio::runtime::Runtime::new().unwrap();
    let output = rt
        .block_on(tool.run_async(serde_json::json!({}), ctx_with_two_ledgers()))
        .unwrap();
    let ToolWorkerOutput::Value(v) = output else {
        panic!("expected Value")
    };

    let tasks = v["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0]["kind"], "daily");
    assert_eq!(tasks[0]["content"], "每日报告");
    assert_eq!(tasks[0]["is_once"], false);
    // cron 任务必须算出下次触发时间
    assert!(tasks[0]["next_fire_time"].is_string());
}

#[test]
fn list_empty_ledgers_returns_empty_array() {
    let tool = ListScheduledTasksTool;
    let rt = tokio::runtime::Runtime::new().unwrap();
    let output = rt
        .block_on(tool.run_async(
            serde_json::json!({}),
            OwnedToolContext {
                scheduler_state: Some(Arc::new(SchedulerStateSnapshot::default())),
                registry: Some(Arc::new(ScheduledTaskRegistrySnapshot::default())),
                tool_inflight_timeout_secs: 300,
                ..Default::default()
            },
        ))
        .unwrap();
    let ToolWorkerOutput::Value(v) = output else {
        panic!("expected Value")
    };
    assert_eq!(v["tasks"].as_array().unwrap().len(), 0);
    assert_eq!(v["count"], 0);
}

#[test]
fn list_missing_snapshot_returns_internal_state_error() {
    let tool = ListScheduledTasksTool;
    let rt = tokio::runtime::Runtime::new().unwrap();
    let result = rt.block_on(tool.run_async(
        serde_json::json!({}),
        OwnedToolContext::empty_for_test(300), // 无快照
    ));
    assert!(matches!(
        result,
        Err(harness::domain::ToolError::InternalState(_))
    ));
}

#[test]
fn list_once_task_shows_original_next_fire_time_even_if_past() {
    let tool = ListScheduledTasksTool;
    let rt = tokio::runtime::Runtime::new().unwrap();

    // 用一个明确过去的 Once 时间（一天前）
    let past = chrono::Utc::now() - chrono::Duration::days(1);
    let state = SchedulerStateSnapshot {
        dynamic_tasks: vec![DynamicScheduledTaskSnapshot {
            id: uuid::Uuid::new_v4(),
            kind: "past-once".into(),
            schedule: ScheduleSpec::Once(past),
            created_at: past,
        }],
    };
    let mut registry = ScheduledTaskRegistrySnapshot::default();
    registry.tasks.insert(
        "past-once".into(),
        ScheduledTaskInfoSnapshot {
            content: "过去的一次性任务".into(),
            output_channel: None,
            is_once: true,
        },
    );
    let ctx = OwnedToolContext {
        scheduler_state: Some(Arc::new(state)),
        registry: Some(Arc::new(registry)),
        tool_inflight_timeout_secs: 300,
        ..Default::default()
    };

    let output = rt
        .block_on(tool.run_async(serde_json::json!({}), ctx))
        .unwrap();
    let ToolWorkerOutput::Value(v) = output else {
        panic!("expected Value")
    };

    let tasks = v["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0]["kind"], "past-once");
    assert_eq!(tasks[0]["is_once"], true);
    // 关键断言：Once 任务的 next_fire_time 等于原始 at（即使已过期）
    assert_eq!(tasks[0]["next_fire_time"], past.to_rfc3339());
}
