//! triggers.toml 配置加载与校验集成测试

use std::io::Write;

use harness::triggers::{
    build_registry_from_config, build_schedules, load_triggers_config, validate_templates,
};
use tempfile::NamedTempFile;

fn write_temp_config(content: &str) -> NamedTempFile {
    let mut f = NamedTempFile::new().expect("temp file");
    f.write_all(content.as_bytes()).expect("write");
    f
}

#[test]
fn load_valid_config_from_file() {
    let toml = r#"
[webhook]
enabled = true
listen_addr = "127.0.0.1:8080"
auth_token = "tok"

[[webhook.routes]]
kind = "test.kind"
approval_channel = { frontend = "telegram", user_id = "r" }
approval_context = "ctx"
prompt_template = "{{kind}}: {{body}}"

[timer]
enabled = false
"#;
    let f = write_temp_config(toml);
    let config = load_triggers_config(f.path()).expect("load");
    assert!(config.webhook.enabled);
    assert!(!config.timer.enabled);
    assert_eq!(config.webhook.routes.len(), 1);
}

#[test]
fn load_missing_file_returns_error() {
    let err = load_triggers_config(std::path::Path::new("/nonexistent/triggers.toml"))
        .expect_err("should error");
    let msg = format!("{err}");
    assert!(msg.contains("read triggers config"));
}

#[test]
fn validate_then_build_registry_then_schedules() {
    let toml = r#"
[webhook]
enabled = true

[[webhook.routes]]
kind = "a"
approval_channel = { frontend = "telegram", user_id = "r" }
approval_context = "ca"
prompt_template = "{{kind}}"

[timer]
enabled = true

[[timer.routes]]
kind = "t1"
cron = "0 9 * * 1-5"
approval_channel = { frontend = "telegram", user_id = "r" }
approval_context = "ct"
prompt_template = "t"
"#;
    let f = write_temp_config(toml);
    let config = load_triggers_config(f.path()).unwrap();
    validate_templates(&config).expect("templates valid");
    let registry = build_registry_from_config(&config).expect("registry");
    assert_eq!(registry.webhook_route_count(), 1);
    assert_eq!(registry.timer_route_count(), 1);
    let schedules = build_schedules(&config.timer).expect("schedules");
    assert_eq!(schedules.len(), 1);
}
