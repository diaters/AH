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
//!
//! [timer]
//! enabled = true
//!
//! [[timer.routes]]
//! kind = "daily_summary"
//! cron = "0 9 * * 1-5"
//! approval_channel = { frontend = "telegram", user_id = "reviewer" }
//! approval_context = "daily summary"
//! prompt_template = "执行每日摘要"
//! ```

use std::path::Path;

use anyhow::{Context, Result};
use bevy_ecs::prelude::Resource;
use cron::Schedule;
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use tracing::warn;

use crate::domain::{ChannelId, EventTaskRoute, SignalTriggerRegistry};
use crate::triggers::prompt_template::validate_template;

/// triggers.toml 顶层结构
#[derive(Debug, Clone, Default, Serialize, Deserialize, Resource)]
pub struct TriggerConfig {
    #[serde(default)]
    pub webhook: WebhookConfig,
    #[serde(default)]
    pub timer: TimerConfig,
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TimerConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub routes: Vec<TimerRouteConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookRouteConfig {
    pub kind: String,
    pub approval_channel: ChannelId,
    pub approval_context: String,
    pub prompt_template: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimerRouteConfig {
    pub kind: String,
    pub cron: String,
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
    let config: TriggerConfig = toml::from_str(&text).context("parse triggers config")?;
    Ok(config)
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
    for route in &config.timer.routes {
        validate_template(&route.prompt_template).with_context(|| {
            format!(
                "invalid prompt_template for timer route kind='{}'",
                route.kind
            )
        })?;
    }
    Ok(())
}

/// 从配置构建 SignalTriggerRegistry。
///
/// `approval_channel` 必填（spec L196-204）；若为 `None` 返回 `Err`。
/// 重复 kind 采用警告 + last-write-wins 语义（spec L255-263）。
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
    for route in &config.timer.routes {
        let event_route = EventTaskRoute {
            prompt_template: route.prompt_template.clone(),
            approval_channel: Some(route.approval_channel.clone()),
            approval_context: route.approval_context.clone(),
        };
        if registry.timer_route(&route.kind).is_some() {
            warn!(
                event = "TriggerRouteKindDuplicate",
                kind = %route.kind,
                scope = "timer",
                "duplicate timer route kind, last-write-wins"
            );
        }
        registry.register_timer(route.kind.clone(), event_route);
    }
    Ok(registry)
}

/// 构建 timer 调度列表。
///
/// 用户写 5 字段标准 cron（分 时 日 月 周），加载时补齐为 `"0 {user_cron} *"`（7 字段）。
pub fn build_schedules(config: &TimerConfig) -> Result<Vec<(Schedule, String)>> {
    config
        .routes
        .iter()
        .map(|route| {
            let cron_expr = format!("0 {} *", route.cron);
            let schedule = Schedule::from_str(&cron_expr).map_err(|e| {
                anyhow::anyhow!(
                    "invalid cron '{}' for kind '{}': {}",
                    route.cron,
                    route.kind,
                    e
                )
            })?;
            Ok((schedule, route.kind.clone()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{FrontendKind, TaskTrigger};

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

[timer]
enabled = true

[[timer.routes]]
kind = "daily_summary"
cron = "0 9 * * 1-5"
approval_channel = { frontend = "telegram", user_id = "reviewer" }
approval_context = "daily summary"
prompt_template = "执行每日摘要"
"#
    }

    #[test]
    fn parse_valid_config() {
        let config: TriggerConfig = toml::from_str(sample_toml()).unwrap();
        assert!(config.webhook.enabled);
        assert_eq!(config.webhook.listen_addr, "127.0.0.1:9090");
        assert_eq!(config.webhook.routes.len(), 1);
        assert_eq!(config.webhook.routes[0].kind, "github.issue_opened");
        assert_eq!(
            config.webhook.routes[0].approval_channel.frontend,
            FrontendKind::Telegram
        );
        assert!(config.timer.enabled);
        assert_eq!(config.timer.routes[0].cron, "0 9 * * 1-5");
    }

    #[test]
    fn validate_templates_ok() {
        let config: TriggerConfig = toml::from_str(sample_toml()).unwrap();
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
        let config: TriggerConfig = toml::from_str(toml).unwrap();
        assert!(validate_templates(&config).is_err());
    }

    #[test]
    fn build_registry_succeeds() {
        let config: TriggerConfig = toml::from_str(sample_toml()).unwrap();
        let registry = build_registry_from_config(&config).unwrap();
        let trigger = TaskTrigger::Webhook {
            kind: "github.issue_opened".to_string(),
            body: serde_json::json!({"title": "bug"}),
        };
        let route = registry.route(&trigger).expect("route should exist");
        assert_eq!(route.build_task_input(&trigger).unwrap(), "分析: bug");
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
            timer: TimerConfig::default(),
        };
        let registry = build_registry_from_config(&config).unwrap();
        assert_eq!(registry.webhook_route_count(), 1);
        let route = registry
            .webhook_route("dup.kind")
            .expect("duplicate kind route should exist");
        assert_eq!(route.prompt_template, "template B");
    }

    #[test]
    fn build_schedules_parses_cron() {
        let config: TriggerConfig = toml::from_str(sample_toml()).unwrap();
        let schedules = build_schedules(&config.timer).unwrap();
        assert_eq!(schedules.len(), 1);
        assert_eq!(schedules[0].1, "daily_summary");
    }

    #[test]
    fn build_schedules_rejects_invalid_cron() {
        let toml = r#"
[timer]
enabled = true

[[timer.routes]]
kind = "bad"
cron = "not a cron"
approval_channel = { frontend = "telegram", user_id = "r" }
approval_context = "x"
prompt_template = "x"
"#;
        let config: TriggerConfig = toml::from_str(toml).unwrap();
        assert!(build_schedules(&config.timer).is_err());
    }
}
