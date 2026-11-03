//! triggers.toml 配置反序列化与校验
//!
//! 配置结构：
//! ```toml
//! [webhook]
//! enabled = true
//! listen_addr = "127.0.0.1:8080"
//! auth_token = "secret"
//!
//! [[webhook.routes]]
//! kind = "github.issue_opened"
//! approval_channel = { frontend = "telegram", user_id = "reviewer" }
//! approval_context = "GitHub issue opened"
//! prompt_template = "分析: {{body_json.title}}"
//! ```
//!
//! 定时任务统一由 `schedule_task` 工具创建；旧配置残留的 `[timer]` 段被忽略并告警。

use std::path::Path;

use anyhow::{Context, Result};
use bevy_ecs::prelude::Resource;
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::domain::validate_template;
use crate::domain::{ChannelId, EventTaskRoute, SignalTriggerRegistry};

/// triggers.toml 顶层结构
#[derive(Debug, Clone, Default, Serialize, Deserialize, Resource)]
pub struct TriggerConfig {
    #[serde(default)]
    pub webhook: WebhookConfig,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WebhookConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_listen_addr")]
    pub listen_addr: String,
    #[serde(default)]
    pub auth_token: String,
    #[serde(default)]
    pub routes: Vec<WebhookRouteConfig>,
}

fn default_listen_addr() -> String {
    "127.0.0.1:8080".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookRouteConfig {
    pub kind: String,
    pub approval_channel: ChannelId,
    pub approval_context: String,
    pub prompt_template: String,
}

/// 从文件加载 TriggerConfig。
///
/// 调用方需在 `triggers_config_path` 为 `Some` 时调用。
pub fn load_triggers_config(path: &Path) -> Result<TriggerConfig> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("read triggers config: {}", path.display()))?;
    parse_triggers_config(&text, path)
}

/// 解析配置文本。旧版 `[timer]` 静态 cron 路由已移除：该段被忽略并告警，不阻止启动。
fn parse_triggers_config(text: &str, path: &Path) -> Result<TriggerConfig> {
    let mut table: toml::Table = toml::from_str(text).context("parse triggers config")?;
    if table.remove("timer").is_some() {
        warn!(
            event = "TriggersTimerSectionIgnored",
            path = %path.display(),
            "[timer] section in triggers config is no longer supported and was ignored; \
             use the schedule_task tool for scheduled tasks"
        );
    }
    toml::Value::Table(table)
        .try_into()
        .context("parse triggers config")
}

/// 校验所有 route 的 prompt_template 语法。
pub fn validate_templates(config: &TriggerConfig) -> Result<()> {
    for route in &config.webhook.routes {
        validate_template(&route.prompt_template).with_context(|| {
            format!(
                "invalid prompt_template for webhook route kind='{}'",
                route.kind
            )
        })?;
    }
    Ok(())
}

/// 从配置构建 SignalTriggerRegistry。
///
/// 重复 kind 采用警告 + last-write-wins 语义。
pub fn build_registry_from_config(config: &TriggerConfig) -> Result<SignalTriggerRegistry> {
    let mut registry = SignalTriggerRegistry::default();
    for route in &config.webhook.routes {
        let event_route = EventTaskRoute {
            prompt_template: route.prompt_template.clone(),
            approval_channel: Some(route.approval_channel.clone()),
            approval_context: route.approval_context.clone(),
        };
        if registry.webhook_route(&route.kind).is_some() {
            warn!(
                event = "TriggerRouteKindDuplicate",
                kind = %route.kind,
                scope = "webhook",
                "duplicate webhook route kind, last-write-wins"
            );
        }
        registry.register_webhook(route.kind.clone(), event_route);
    }
    Ok(registry)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::FrontendKind;

    fn sample_toml() -> &'static str {
        r#"
[webhook]
enabled = true
listen_addr = "127.0.0.1:9090"
auth_token = "secret"

[[webhook.routes]]
kind = "github.issue_opened"
approval_channel = { frontend = "telegram", user_id = "reviewer" }
approval_context = "GitHub issue opened"
prompt_template = "分析: {{body_json.title}}"
"#
    }

    fn parse(text: &str) -> Result<TriggerConfig> {
        parse_triggers_config(text, Path::new("triggers.toml"))
    }

    #[test]
    fn parse_valid_config() {
        let config = parse(sample_toml()).unwrap();
        assert!(config.webhook.enabled);
        assert_eq!(config.webhook.listen_addr, "127.0.0.1:9090");
        assert_eq!(config.webhook.routes.len(), 1);
        assert_eq!(config.webhook.routes[0].kind, "github.issue_opened");
        assert_eq!(
            config.webhook.routes[0].approval_channel.frontend,
            FrontendKind::Telegram
        );
    }

    #[test]
    fn legacy_timer_section_is_ignored() {
        let text = format!(
            "{}\n{}",
            sample_toml(),
            r#"
[timer]
enabled = true

[[timer.routes]]
kind = "daily_summary"
cron = "not a cron"
approval_channel = { frontend = "telegram", user_id = "reviewer" }
approval_context = "daily summary"
prompt_template = "{{body_json.a.b}}"
"#
        );
        let config = parse(&text).expect("legacy [timer] must not fail parsing");
        assert_eq!(config.webhook.routes.len(), 1);
        // 被忽略段中的坏 cron 与坏模板不再参与校验
        assert!(validate_templates(&config).is_ok());
        assert_eq!(
            build_registry_from_config(&config)
                .unwrap()
                .webhook_route_count(),
            1
        );
    }

    #[test]
    fn invalid_webhook_section_still_fails_parsing() {
        assert!(parse("[webhook]\nenabled = \"yes\"\n").is_err());
    }

    #[test]
    fn validate_templates_ok() {
        let config = parse(sample_toml()).unwrap();
        assert!(validate_templates(&config).is_ok());
    }

    #[test]
    fn validate_templates_rejects_invalid() {
        let toml = r#"
[webhook]
enabled = true

[[webhook.routes]]
kind = "bad"
approval_channel = { frontend = "telegram", user_id = "r" }
approval_context = "x"
prompt_template = "{{body_json.a.b}}"
"#;
        let config = parse(toml).unwrap();
        assert!(validate_templates(&config).is_err());
    }

    #[test]
    fn build_registry_succeeds() {
        let config = parse(sample_toml()).unwrap();
        let registry = build_registry_from_config(&config).unwrap();
        let route = registry
            .webhook_route("github.issue_opened")
            .expect("route should exist");
        let body = serde_json::json!({"title": "bug"});
        assert_eq!(
            route.build_task_input("github.issue_opened", &body),
            "分析: bug"
        );
    }

    #[test]
    fn build_registry_duplicate_kind_uses_last_write_wins() {
        let channel = ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "r".to_string(),
            thread_id: None,
        };
        let config = TriggerConfig {
            webhook: WebhookConfig {
                enabled: true,
                listen_addr: "127.0.0.1:8080".to_string(),
                auth_token: String::new(),
                routes: vec![
                    WebhookRouteConfig {
                        kind: "dup.kind".to_string(),
                        approval_channel: channel.clone(),
                        approval_context: "ctx".to_string(),
                        prompt_template: "template A".to_string(),
                    },
                    WebhookRouteConfig {
                        kind: "dup.kind".to_string(),
                        approval_channel: channel,
                        approval_context: "ctx".to_string(),
                        prompt_template: "template B".to_string(),
                    },
                ],
            },
        };
        let registry = build_registry_from_config(&config).unwrap();
        assert_eq!(registry.webhook_route_count(), 1);
        let route = registry
            .webhook_route("dup.kind")
            .expect("duplicate kind route should exist");
        assert_eq!(route.prompt_template, "template B");
    }
}
