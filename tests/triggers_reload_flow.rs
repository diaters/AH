//! reload_triggers_system 集成测试

use std::io::Write;

use harness::domain::{
    ScheduleDefinition, ScheduleExpr, SignalTriggerRegistry, TriggersConfigPath,
};
use harness::prelude::*;
use harness::triggers::{SchedulerState, SchedulerStateWatcher, reload_triggers_system};
use tempfile::NamedTempFile;
use tokio::sync::watch;

fn write_config(content: &str) -> NamedTempFile {
    let mut f = NamedTempFile::new().unwrap();
    f.write_all(content.as_bytes()).unwrap();
    f
}

fn build_app(path: &str) -> App {
    let mut app = App::new();
    app.insert_resource(TriggersConfigPath(Some(path.to_string())));
    app.insert_resource(SignalTriggerRegistry::default());
    app.insert_resource(SchedulerStateWatcher::default());
    app.insert_resource(SchedulerState::default());
    app.add_systems(Update, reload_triggers_system);
    app
}

#[test]
fn reload_loads_initial_config_into_registry() {
    let f = write_config(
        r#"
[webhook]
enabled = true

[[webhook.routes]]
kind = "k1"
approval_channel = { frontend = "telegram", user_id = "r" }
approval_context = "c1"
prompt_template = "{{kind}}"
"#,
    );
    let mut app = build_app(f.path().to_str().unwrap());
    app.update();
    let registry = app.world().resource::<SignalTriggerRegistry>();
    assert_eq!(registry.webhook_route_count(), 1);
}

#[test]
fn reload_with_invalid_template_keeps_old_config() {
    let good = write_config(
        r#"
[webhook]
enabled = true

[[webhook.routes]]
kind = "good"
approval_channel = { frontend = "telegram", user_id = "r" }
approval_context = "c"
prompt_template = "{{kind}}"
"#,
    );
    let mut app = build_app(good.path().to_str().unwrap());
    app.update();
    assert_eq!(
        app.world()
            .resource::<SignalTriggerRegistry>()
            .webhook_route_count(),
        1
    );

    // 改写为无效模板
    let bad = write_config(
        r#"
[webhook]
enabled = true

[[webhook.routes]]
kind = "bad"
approval_channel = { frontend = "telegram", user_id = "r" }
approval_context = "c"
prompt_template = "{{body_json.a.b}}"
"#,
    );
    // 把 path 指向坏文件
    let mut path_res = app.world_mut().resource_mut::<TriggersConfigPath>();
    path_res.0 = Some(bad.path().to_str().unwrap().to_string());
    app.update();

    // 应保留旧配置（registry 仍是 1 个路由，但 kind 应仍是 "good"）
    let registry = app.world().resource::<SignalTriggerRegistry>();
    assert_eq!(registry.webhook_route_count(), 1);
    assert!(registry.webhook_route("good").is_some());
    assert!(registry.webhook_route("bad").is_none());
}

/// 旧配置残留的 `[timer]` 段（即使 cron 非法）不阻止 webhook 路由热加载。
#[test]
fn reload_ignores_legacy_timer_section() {
    let f = write_config(
        r#"
[webhook]
enabled = true

[[webhook.routes]]
kind = "k"
approval_channel = { frontend = "telegram", user_id = "r" }
approval_context = "c"
prompt_template = "{{kind}}"

[timer]
enabled = true

[[timer.routes]]
kind = "t"
cron = "not a cron"
approval_channel = { frontend = "telegram", user_id = "r" }
approval_context = "c"
prompt_template = "x"
"#,
    );
    let mut app = build_app(f.path().to_str().unwrap());
    app.update();
    let registry = app.world().resource::<SignalTriggerRegistry>();
    assert_eq!(registry.webhook_route_count(), 1);
    assert!(registry.webhook_route("k").is_some());
    assert!(registry.webhook_route("t").is_none());
}

/// reload 只替换 webhook 路由表，不改动动态定时任务，也不通知 timer scheduler。
#[test]
fn reload_leaves_scheduler_state_untouched() {
    let f = write_config(
        r#"
[webhook]
enabled = true

[[webhook.routes]]
kind = "k"
approval_channel = { frontend = "telegram", user_id = "r" }
approval_context = "c"
prompt_template = "{{kind}}"
"#,
    );
    let def = ScheduleDefinition::new(
        uuid::Uuid::new_v4(),
        None,
        "x".to_string(),
        ScheduleExpr::parse("cron:0 8 * * *").unwrap(),
        None,
        chrono::Utc::now(),
    );
    let (tx, rx) = watch::channel(SchedulerState::default());
    let mut app = build_app(f.path().to_str().unwrap());
    app.world_mut()
        .resource_mut::<SchedulerState>()
        .dynamic_tasks_mut()
        .push(def);
    app.world_mut()
        .insert_resource(SchedulerStateWatcher(Some(tx)));
    app.update();
    assert_eq!(
        app.world()
            .resource::<SignalTriggerRegistry>()
            .webhook_route_count(),
        1
    );
    assert_eq!(
        app.world()
            .resource::<SchedulerState>()
            .dynamic_tasks()
            .len(),
        1
    );
    assert!(!rx.has_changed().unwrap());
}
