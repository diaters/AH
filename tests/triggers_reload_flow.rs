//! reload_triggers_system 集成测试

use std::io::Write;

use harness::domain::{SignalTriggerRegistry, TriggersConfigPath};
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
fn reload_loads_initial_config_into_registry_and_state() {
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
    let state = app.world().resource::<SchedulerState>();
    assert!(state.static_routes().is_some());
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

#[test]
fn reload_with_invalid_cron_keeps_old_config() {
    let good = write_config(
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
cron = "0 9 * * 1-5"
approval_channel = { frontend = "telegram", user_id = "r" }
approval_context = "c"
prompt_template = "x"
"#,
    );
    let mut app = build_app(good.path().to_str().unwrap());
    app.update();
    assert_eq!(
        app.world()
            .resource::<SignalTriggerRegistry>()
            .timer_route_count(),
        1
    );

    // 改写为坏 cron
    let bad = write_config(
        r#"
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
    let mut path_res = app.world_mut().resource_mut::<TriggersConfigPath>();
    path_res.0 = Some(bad.path().to_str().unwrap().to_string());
    app.update();

    // 旧配置保留：timer 仍是 1 个
    let registry = app.world().resource::<SignalTriggerRegistry>();
    assert_eq!(registry.timer_route_count(), 1);
}

#[test]
fn reload_notifies_watcher_when_set() {
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
    let (tx, mut rx) = watch::channel(harness::triggers::SchedulerState::default());
    let mut app = build_app(f.path().to_str().unwrap());
    app.world_mut()
        .insert_resource(SchedulerStateWatcher(Some(tx)));
    app.update();
    assert!(rx.has_changed().unwrap());
    let updated = rx.borrow_and_update().clone();
    assert_eq!(updated.static_routes().unwrap().webhook.routes.len(), 1);
}
