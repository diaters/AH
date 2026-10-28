use harness::domain::{
    BuiltinTool, OwnedToolContext, ScheduleDefinition, ScheduleExpr, SchedulerStateSnapshot,
    ToolWorkerOutput,
};
use harness::systems::tools::builtin::scheduled::list::ListScheduledTasksTool;
use std::sync::Arc;

fn definition(name: &str, schedule: &str) -> ScheduleDefinition {
    ScheduleDefinition::new(
        uuid::Uuid::new_v4(),
        Some(name.to_string()),
        format!("{name} 的内容"),
        ScheduleExpr::parse(schedule).unwrap(),
        None,
        chrono::Utc::now(),
    )
}

fn ctx_with(defs: Vec<ScheduleDefinition>) -> OwnedToolContext {
    OwnedToolContext {
        scheduler_state: Some(Arc::new(SchedulerStateSnapshot {
            dynamic_tasks: defs,
        })),
        tool_inflight_timeout_secs: 300,
        ..Default::default()
    }
}

fn run_list(ctx: OwnedToolContext) -> serde_json::Value {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let output = rt
        .block_on(ListScheduledTasksTool.run_async(serde_json::json!({}), ctx))
        .unwrap();
    let ToolWorkerOutput::Value(v) = output else {
        panic!("expected Value")
    };
    v
}

#[test]
fn list_reports_definition_fields() {
    let daily = definition("daily", "cron:0 9 * * *");
    let kind = daily.kind();
    let v = run_list(ctx_with(vec![daily]));

    let tasks = v["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0]["kind"], kind);
    assert_eq!(tasks[0]["name"], "daily");
    assert_eq!(tasks[0]["content"], "daily 的内容");
    assert_eq!(tasks[0]["schedule"], "cron:0 9 * * *");
    assert_eq!(tasks[0]["is_once"], false);
    assert_eq!(tasks[0]["enabled"], true);
    assert!(tasks[0]["last_fired_at"].is_null());
    // cron 任务必须算出下次触发时间
    assert!(tasks[0]["next_fire_time"].is_string());
}

#[test]
fn list_paused_task_has_no_next_fire_time() {
    let mut paused = definition("paused", "cron:0 9 * * *");
    paused.enabled = false;
    let v = run_list(ctx_with(vec![paused]));

    let task = &v["tasks"][0];
    assert_eq!(task["enabled"], false);
    assert!(task.get("next_fire_time").is_none());
}

#[test]
fn list_empty_ledger_returns_empty_array() {
    let v = run_list(ctx_with(vec![]));
    assert_eq!(v["tasks"].as_array().unwrap().len(), 0);
    assert_eq!(v["count"], 0);
}

#[test]
fn list_missing_snapshot_returns_internal_state_error() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let result = rt.block_on(ListScheduledTasksTool.run_async(
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
    // 用一个明确过去的 Once 时间（一天前）
    let past = chrono::Utc::now() - chrono::Duration::days(1);
    let once = definition("past-once", &format!("once:{}", past.to_rfc3339()));
    let v = run_list(ctx_with(vec![once]));

    let tasks = v["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0]["is_once"], true);
    // 关键断言：Once 任务的 next_fire_time 等于原始 at（即使已过期）
    let reported =
        chrono::DateTime::parse_from_rfc3339(tasks[0]["next_fire_time"].as_str().unwrap()).unwrap();
    assert_eq!(reported.timestamp(), past.timestamp());
}
