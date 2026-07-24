//! schedule_task Tool 实现

use std::str::FromStr;

use chrono::{DateTime, Local, NaiveDateTime, TimeZone, Utc};
use cron::Schedule;
use uuid::Uuid;

use crate::domain::{ChannelId, FrontendKind, ToolAction, ToolContext, ToolError};
use crate::triggers::ScheduleSpec;

pub struct ScheduleTaskTool;

impl crate::domain::BuiltinTool for ScheduleTaskTool {
    fn name(&self) -> &str {
        "schedule_task"
    }

    fn execute(
        &self,
        input: &serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolAction, ToolError> {
        let content = input
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("content is required".to_string()))?
            .to_string();

        let schedule_str = input
            .get("schedule")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("schedule is required".to_string()))?;

        let output_channel_str = input.get("output_channel").and_then(|v| v.as_str());
        let target = input.get("target").and_then(|v| v.as_str());

        let schedule = parse_schedule(schedule_str)?;
        let output_channel = build_output_channel(output_channel_str, target, ctx)?;

        let id = Uuid::new_v4();
        let kind = format!("scheduled:{}", id);

        Ok(ToolAction::ScheduleTask {
            id,
            kind,
            content,
            schedule,
            output_channel,
        })
    }
}

/// 解析 `once:` / `cron:` 前缀的调度表达式。
///
/// - `once:<RFC3339 或 naive local>`：一次性触发，时间必须在未来。
/// - `cron:<5 字段>`：周期性触发，内部补齐为 7 字段（`"0 {user_cron} *"`）。
fn parse_schedule(s: &str) -> Result<ScheduleSpec, ToolError> {
    if let Some(rest) = s.strip_prefix("once:") {
        let local = parse_once_time(rest)?;
        if local <= Local::now() {
            return Err(ToolError::InvalidInput(
                "scheduled time is in the past".to_string(),
            ));
        }
        Ok(ScheduleSpec::Once(local.with_timezone(&Utc)))
    } else if let Some(rest) = s.strip_prefix("cron:") {
        let cron_expr = format!("0 {} *", rest);
        let schedule = Schedule::from_str(&cron_expr)
            .map_err(|e| ToolError::InvalidInput(format!("invalid cron: {}", e)))?;
        Ok(ScheduleSpec::Cron(Box::new(schedule)))
    } else {
        Err(ToolError::InvalidInput(
            "schedule must start with 'once:' or 'cron:'".to_string(),
        ))
    }
}

/// 解析一次性触发时间。
///
/// 先尝试带时区偏移的 RFC 3339，再回退到无偏移的本地时间。
fn parse_once_time(s: &str) -> Result<DateTime<Local>, ToolError> {
    // 先尝试带时区偏移的 RFC 3339
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Ok(dt.with_timezone(&Local));
    }
    // 再尝试无偏移的本地时间
    let naive = NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S")
        .map_err(|e| ToolError::InvalidInput(format!("invalid once time: {}", e)))?;
    Local
        .from_local_datetime(&naive)
        .single()
        .ok_or_else(|| ToolError::InvalidInput("ambiguous or invalid local time".to_string()))
}

/// 构造输出通道。
///
/// - 显式指定 `output_channel` 时必须提供 `target`。
/// - 未指定时从当前任务继承 `origin_channel`。
fn build_output_channel(
    output_channel_str: Option<&str>,
    target: Option<&str>,
    ctx: &ToolContext,
) -> Result<Option<ChannelId>, ToolError> {
    if let Some(frontend_str) = output_channel_str {
        let frontend = match frontend_str {
            "tui" => FrontendKind::Tui,
            "telegram" => FrontendKind::Telegram,
            "web" => FrontendKind::Web,
            "qq" => FrontendKind::QQ,
            "feishu" => FrontendKind::Feishu,
            _ => {
                return Err(ToolError::InvalidInput(format!(
                    "unknown output_channel: {}",
                    frontend_str
                )));
            }
        };
        let user_id = target
            .ok_or_else(|| {
                ToolError::InvalidInput(
                    "target is required when output_channel is provided".to_string(),
                )
            })?
            .to_string();
        Ok(Some(ChannelId {
            frontend,
            user_id,
            thread_id: None,
        }))
    } else {
        // 从当前任务继承 origin_channel
        ctx.current_origin_channel
            .clone()
            .ok_or_else(|| {
                ToolError::InvalidInput(
                    "no output_channel provided and current task has no origin_channel".to_string(),
                )
            })
            .map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{BuiltinTool, ExperienceStore, SharedKnowledgeBase};
    use crate::triggers::ScheduleSpec;
    use chrono::Local;
    use uuid::Uuid;

    fn tool_context_without_channel() -> ToolContext<'static> {
        let knowledge = Box::leak(Box::new(SharedKnowledgeBase::default()));
        let experience_store = Box::leak(Box::new(ExperienceStore::default()));
        ToolContext {
            knowledge,
            experience_store,
            default_wait_tasks_timeout_secs: 300,
            shell_default_tail_lines: 50,
            shell_max_tail_lines: 500,
            shell_default_exec_timeout_secs: 60,
            shell_default_stop_timeout_secs: 5,
            tool_inflight_timeout_secs: 300,
            current_task_id: Uuid::new_v4(),
            current_agent_id: Uuid::new_v4(),
            current_origin_channel: None,
        }
    }

    fn tool_context_with_channel(channel: ChannelId) -> ToolContext<'static> {
        let knowledge = Box::leak(Box::new(SharedKnowledgeBase::default()));
        let experience_store = Box::leak(Box::new(ExperienceStore::default()));
        ToolContext {
            knowledge,
            experience_store,
            default_wait_tasks_timeout_secs: 300,
            shell_default_tail_lines: 50,
            shell_max_tail_lines: 500,
            shell_default_exec_timeout_secs: 60,
            shell_default_stop_timeout_secs: 5,
            tool_inflight_timeout_secs: 300,
            current_task_id: Uuid::new_v4(),
            current_agent_id: Uuid::new_v4(),
            current_origin_channel: Some(channel),
        }
    }

    fn future_local_iso() -> String {
        let future = Local::now() + chrono::Duration::days(1);
        future.format("%Y-%m-%dT%H:%M:%S").to_string()
    }

    fn future_rfc3339() -> String {
        let future = Local::now() + chrono::Duration::days(1);
        future.to_rfc3339()
    }

    #[test]
    fn missing_content_returns_error() {
        let input = serde_json::json!({
            "schedule": format!("once:{}", future_local_iso())
        });
        let result = ScheduleTaskTool.execute(&input, &tool_context_without_channel());
        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::InvalidInput(msg) => assert!(msg.contains("content")),
            other => panic!("expected InvalidInput, got {:?}", other),
        }
    }

    #[test]
    fn missing_schedule_returns_error() {
        let input = serde_json::json!({
            "content": "do something"
        });
        let result = ScheduleTaskTool.execute(&input, &tool_context_without_channel());
        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::InvalidInput(msg) => assert!(msg.contains("schedule")),
            other => panic!("expected InvalidInput, got {:?}", other),
        }
    }

    #[test]
    fn invalid_schedule_prefix_returns_error() {
        let input = serde_json::json!({
            "content": "do something",
            "schedule": "every 5 minutes"
        });
        let result = ScheduleTaskTool.execute(&input, &tool_context_without_channel());
        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::InvalidInput(msg) => {
                assert!(msg.contains("once:") || msg.contains("cron:"))
            }
            other => panic!("expected InvalidInput, got {:?}", other),
        }
    }

    #[test]
    fn once_in_past_returns_error() {
        let past = Local::now() - chrono::Duration::days(1);
        let past_str = past.format("%Y-%m-%dT%H:%M:%S").to_string();
        let input = serde_json::json!({
            "content": "do something",
            "schedule": format!("once:{}", past_str)
        });
        let result = ScheduleTaskTool.execute(&input, &tool_context_without_channel());
        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::InvalidInput(msg) => assert!(msg.contains("past")),
            other => panic!("expected InvalidInput, got {:?}", other),
        }
    }

    #[test]
    fn once_naive_local_time_with_inherited_channel_succeeds() {
        let input = serde_json::json!({
            "content": "send report",
            "schedule": format!("once:{}", future_local_iso())
        });
        let inherited = ChannelId {
            frontend: crate::domain::FrontendKind::Tui,
            user_id: "inherited-user".to_string(),
            thread_id: None,
        };
        let result =
            ScheduleTaskTool.execute(&input, &tool_context_with_channel(inherited.clone()));
        assert!(result.is_ok());
        match result.unwrap() {
            ToolAction::ScheduleTask {
                content,
                schedule,
                output_channel,
                kind,
                ..
            } => {
                assert_eq!(content, "send report");
                assert!(matches!(schedule, ScheduleSpec::Once(_)));
                assert_eq!(output_channel, Some(inherited));
                assert!(kind.starts_with("scheduled:"));
            }
            other => panic!("expected ScheduleTask, got {:?}", other),
        }
    }

    #[test]
    fn once_rfc3339_with_inherited_channel_succeeds() {
        let input = serde_json::json!({
            "content": "send report",
            "schedule": format!("once:{}", future_rfc3339())
        });
        let inherited = ChannelId {
            frontend: crate::domain::FrontendKind::Telegram,
            user_id: "tg-123".to_string(),
            thread_id: None,
        };
        let result =
            ScheduleTaskTool.execute(&input, &tool_context_with_channel(inherited.clone()));
        assert!(result.is_ok());
        match result.unwrap() {
            ToolAction::ScheduleTask {
                output_channel,
                schedule,
                ..
            } => {
                assert_eq!(output_channel, Some(inherited));
                assert!(matches!(schedule, ScheduleSpec::Once(_)));
            }
            other => panic!("expected ScheduleTask, got {:?}", other),
        }
    }

    #[test]
    fn cron_schedule_succeeds() {
        let input = serde_json::json!({
            "content": "daily standup",
            "schedule": "cron:0 9 * * 1-5",
            "output_channel": "tui",
            "target": "user-1"
        });
        let result = ScheduleTaskTool.execute(&input, &tool_context_without_channel());
        assert!(result.is_ok());
        match result.unwrap() {
            ToolAction::ScheduleTask {
                content,
                schedule,
                output_channel,
                ..
            } => {
                assert_eq!(content, "daily standup");
                assert!(matches!(schedule, ScheduleSpec::Cron(_)));
                let channel = output_channel.expect("output_channel should be set");
                assert_eq!(channel.frontend, crate::domain::FrontendKind::Tui);
                assert_eq!(channel.user_id, "user-1");
            }
            other => panic!("expected ScheduleTask, got {:?}", other),
        }
    }

    #[test]
    fn explicit_output_channel_requires_target() {
        let input = serde_json::json!({
            "content": "do something",
            "schedule": format!("once:{}", future_local_iso()),
            "output_channel": "tui"
        });
        let result = ScheduleTaskTool.execute(&input, &tool_context_without_channel());
        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::InvalidInput(msg) => assert!(msg.contains("target")),
            other => panic!("expected InvalidInput, got {:?}", other),
        }
    }

    #[test]
    fn unknown_output_channel_returns_error() {
        let input = serde_json::json!({
            "content": "do something",
            "schedule": format!("once:{}", future_local_iso()),
            "output_channel": "irc",
            "target": "u1"
        });
        let result = ScheduleTaskTool.execute(&input, &tool_context_without_channel());
        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::InvalidInput(msg) => assert!(msg.contains("irc")),
            other => panic!("expected InvalidInput, got {:?}", other),
        }
    }

    #[test]
    fn no_output_channel_and_no_inherited_returns_error() {
        let input = serde_json::json!({
            "content": "do something",
            "schedule": format!("once:{}", future_local_iso())
        });
        let result = ScheduleTaskTool.execute(&input, &tool_context_without_channel());
        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::InvalidInput(msg) => assert!(msg.contains("output_channel")),
            other => panic!("expected InvalidInput, got {:?}", other),
        }
    }

    #[test]
    fn explicit_output_channel_overrides_inherited() {
        let input = serde_json::json!({
            "content": "do something",
            "schedule": format!("once:{}", future_local_iso()),
            "output_channel": "qq",
            "target": "qq-user"
        });
        let inherited = ChannelId {
            frontend: crate::domain::FrontendKind::Tui,
            user_id: "inherited".to_string(),
            thread_id: None,
        };
        let result = ScheduleTaskTool.execute(&input, &tool_context_with_channel(inherited));
        assert!(result.is_ok());
        match result.unwrap() {
            ToolAction::ScheduleTask { output_channel, .. } => {
                let channel = output_channel.expect("output_channel should be set");
                assert_eq!(channel.frontend, crate::domain::FrontendKind::QQ);
                assert_eq!(channel.user_id, "qq-user");
            }
            other => panic!("expected ScheduleTask, got {:?}", other),
        }
    }

    #[test]
    fn all_frontend_kinds_accepted() {
        use crate::domain::FrontendKind;
        for (name, expected) in [
            ("tui", FrontendKind::Tui),
            ("telegram", FrontendKind::Telegram),
            ("web", FrontendKind::Web),
            ("qq", FrontendKind::QQ),
            ("feishu", FrontendKind::Feishu),
        ] {
            let input = serde_json::json!({
                "content": "x",
                "schedule": format!("once:{}", future_local_iso()),
                "output_channel": name,
                "target": "u"
            });
            let result = ScheduleTaskTool.execute(&input, &tool_context_without_channel());
            assert!(result.is_ok(), "frontend {} should be accepted", name);
            match result.unwrap() {
                ToolAction::ScheduleTask { output_channel, .. } => {
                    let channel = output_channel.expect("output_channel should be set");
                    assert_eq!(channel.frontend, expected, "frontend {} mismatch", name);
                }
                other => panic!("expected ScheduleTask, got {:?}", other),
            }
        }
    }

    #[test]
    fn kind_field_starts_with_scheduled_prefix() {
        let input = serde_json::json!({
            "content": "x",
            "schedule": format!("once:{}", future_local_iso()),
            "output_channel": "tui",
            "target": "u"
        });
        let result = ScheduleTaskTool.execute(&input, &tool_context_without_channel());
        assert!(result.is_ok());
        match result.unwrap() {
            ToolAction::ScheduleTask { kind, id, .. } => {
                assert!(kind.starts_with("scheduled:"));
                let suffix = kind.strip_prefix("scheduled:").unwrap();
                let parsed = Uuid::parse_str(suffix).expect("kind suffix should be a UUID");
                assert_eq!(parsed, id);
            }
            other => panic!("expected ScheduleTask, got {:?}", other),
        }
    }

    #[test]
    fn invalid_cron_returns_error() {
        let input = serde_json::json!({
            "content": "x",
            "schedule": "cron:not-a-cron",
            "output_channel": "tui",
            "target": "u"
        });
        let result = ScheduleTaskTool.execute(&input, &tool_context_without_channel());
        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::InvalidInput(msg) => assert!(msg.contains("cron")),
            other => panic!("expected InvalidInput, got {:?}", other),
        }
    }
}
