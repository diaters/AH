//! triggers.toml 配置加载与校验集成测试

use std::cell::RefCell;
use std::fmt;
use std::io::Write;
use std::sync::Once;

use harness::triggers::{build_registry_from_config, load_triggers_config, validate_templates};
use tempfile::NamedTempFile;
use tracing::field::{Field, Visit};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::{Layer, Registry};

fn write_temp_config(content: &str) -> NamedTempFile {
    let mut f = NamedTempFile::new().expect("temp file");
    f.write_all(content.as_bytes()).expect("write");
    f
}

thread_local! {
    static CAPTURED: RefCell<Vec<(tracing::Level, String)>> = const { RefCell::new(Vec::new()) };
}

/// 全局 subscriber 的转发层：把 (级别, `event` 字段) 写入当前线程的缓冲区。
///
/// 用进程级全局 subscriber 而非 thread-local `set_default`，避免并行测试中
/// callsite 兴趣被未装 subscriber 的线程缓存为 never。
struct CaptureLayer;

#[derive(Default)]
struct EventNameVisitor(Option<String>);

impl Visit for EventNameVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "event" {
            self.0 = Some(value.to_string());
        }
    }

    fn record_debug(&mut self, _field: &Field, _value: &dyn fmt::Debug) {}
}

impl<S: tracing::Subscriber> Layer<S> for CaptureLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let mut visitor = EventNameVisitor::default();
        event.record(&mut visitor);
        if let Some(name) = visitor.0 {
            let level = *event.metadata().level();
            CAPTURED.with(|events| events.borrow_mut().push((level, name)));
        }
    }
}

/// 安装全局 subscriber（仅首次生效）并清空当前线程的捕获缓冲区。
fn init_capture() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        tracing::subscriber::set_global_default(Registry::default().with(CaptureLayer))
            .expect("global subscriber must be installed once");
    });
    CAPTURED.with(|events| events.borrow_mut().clear());
}

fn captured_warning(name: &str) -> bool {
    CAPTURED.with(|events| {
        events
            .borrow()
            .iter()
            .any(|(level, event)| *level == tracing::Level::WARN && event == name)
    })
}

#[test]
fn load_valid_config_from_file() {
    init_capture();
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
"#;
    let f = write_temp_config(toml);
    let config = load_triggers_config(f.path()).expect("load");
    assert!(config.webhook.enabled);
    assert_eq!(config.webhook.routes.len(), 1);
    assert!(!captured_warning("TriggersTimerSectionIgnored"));
}

#[test]
fn load_missing_file_returns_error() {
    let err = load_triggers_config(std::path::Path::new("/nonexistent/triggers.toml"))
        .expect_err("should error");
    let msg = format!("{err}");
    assert!(msg.contains("read triggers config"));
}

/// 旧配置残留的 `[timer]` 段：加载成功、只告警，webhook 路由照常生效。
#[test]
fn legacy_timer_section_is_ignored_with_warning() {
    init_capture();
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
    let config = load_triggers_config(f.path()).expect("legacy [timer] must not fail loading");
    assert!(
        captured_warning("TriggersTimerSectionIgnored"),
        "loading a legacy [timer] section must emit a warning"
    );
    validate_templates(&config).expect("templates valid");
    let registry = build_registry_from_config(&config).expect("registry");
    assert_eq!(registry.webhook_route_count(), 1);
    assert!(registry.webhook_route("a").is_some());
    assert!(registry.webhook_route("t1").is_none());
}
