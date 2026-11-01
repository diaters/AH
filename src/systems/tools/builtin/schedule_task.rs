//! schedule_task Tool 实现
//!
//! Task 14 上桥后：`kind() == Async`，sync `execute` 仅防御性返回 `InternalState`，
//! 真正逻辑在 `run_async` 内——解析参数 → 构造 `ToolEffect::ScheduleTask` 效果，
//! 由 `commit_tool_effects_system` 经 `update_scheduler_state` 统一入口落账。

use chrono::Utc;
use uuid::Uuid;

use crate::domain::{
    BuiltinTool, ChannelId, FrontendKind, OwnedToolContext, ScheduleExpr, ScheduleSpec, ToolAction,
    ToolActionKind, ToolContext, ToolEffect, ToolError, ToolFuture, ToolWorkerOutput,
};

pub struct ScheduleTaskTool;

impl BuiltinTool for ScheduleTaskTool {
    fn name(&self) -> &str {
        "schedule_task"
    }

    fn kind(&self) -> ToolActionKind {
        ToolActionKind::Async
    }

    fn execute(&self, _: &serde_json::Value, _: &ToolContext) -> Result<ToolAction, ToolError> {
        // Async 工具不会走到这里（dispatch 按 kind 分流）；快速失败防误调
        Err(ToolError::InternalState(
            "schedule_task is async-only, must go through run_async".to_string(),
        ))
    }

    fn run_async(&self, input: serde_json::Value, ctx: OwnedToolContext) -> ToolFuture {
        Box::pin(async move {
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
            let output_channel =
                build_output_channel(output_channel_str, target, ctx.current_origin_channel)?;

            let name = input
                .get("name")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string);

            Ok(ToolWorkerOutput::Effect(ToolEffect::ScheduleTask {
                id: Uuid::new_v4(),
                name,
                content,
                schedule,
                output_channel,
                notify: parse_notify(&input)?.unwrap_or_default(),
                report_of: parse_report_of(&input)?.flatten(),
            }))
        })
    }
}

/// 缺省不修改策略；无效值必须拒绝，不能退回默认。
pub(crate) fn parse_notify(
    input: &serde_json::Value,
) -> Result<Option<crate::domain::NotifyPolicy>, ToolError> {
    input
        .get("notify")
        .map(|value| {
            serde_json::from_value(value.clone())
                .map_err(|_| ToolError::InvalidInput("notify must be reply or explicit".into()))
        })
        .transpose()
}

/// 双层 Option 区分不修改与显式清除关联。
pub(crate) fn parse_report_of(
    input: &serde_json::Value,
) -> Result<Option<Option<Uuid>>, ToolError> {
    input
        .get("report_of")
        .map(|value| {
            if value.is_null() {
                return Ok(None);
            }
            value
                .as_str()
                .and_then(|value| Uuid::parse_str(value).ok())
                .map(Some)
                .ok_or_else(|| {
                    ToolError::InvalidInput("report_of must be a schedule UUID or null".into())
                })
        })
        .transpose()
}

/// 解析调度表达式，并要求一次性触发时间在未来。
pub(crate) fn parse_schedule(s: &str) -> Result<ScheduleExpr, ToolError> {
    let expr = ScheduleExpr::parse(s).map_err(|e| ToolError::InvalidInput(e.to_string()))?;
    if let ScheduleSpec::Once(at) = expr.spec()
        && *at <= Utc::now()
    {
        return Err(ToolError::InvalidInput(
            "scheduled time is in the past".to_string(),
        ));
    }
    Ok(expr)
}

/// 构造输出通道。
///
/// - 显式指定 `output_channel` 时必须提供 `target`。
/// - 未指定时从当前任务继承 `origin_channel`（由 dispatch 注入到
///   `OwnedToolContext.current_origin_channel`）。
fn build_output_channel(
    output_channel_str: Option<&str>,
    target: Option<&str>,
    inherited: Option<ChannelId>,
) -> Result<Option<ChannelId>, ToolError> {
    if let Some(frontend_str) = output_channel_str {
        let frontend = FrontendKind::from_channel_name(frontend_str)
            .map_err(|e| ToolError::InvalidInput(format!("unknown output_channel: {e}")))?;
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
        inherited
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
    use crate::domain::BuiltinTool;
    use chrono::Local;

    fn future_local_iso() -> String {
        let future = Local::now() + chrono::Duration::days(1);
        future.format("%Y-%m-%dT%H:%M:%S").to_string()
    }

    fn future_rfc3339() -> String {
        let future = Local::now() + chrono::Duration::days(1);
        future.to_rfc3339()
    }

    fn inherited_channel() -> ChannelId {
        ChannelId {
            frontend: crate::domain::FrontendKind::Telegram,
            user_id: "inherited-tg".to_string(),
            thread_id: None,
        }
    }

    fn ctx_with_channel(channel: Option<ChannelId>) -> OwnedToolContext {
        let mut ctx = OwnedToolContext::empty_for_test(300);
        ctx.current_origin_channel = channel;
        ctx
    }

    fn run_async_blocking(
        input: serde_json::Value,
        ctx: OwnedToolContext,
    ) -> Result<ToolWorkerOutput, ToolError> {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(ScheduleTaskTool.run_async(input, ctx))
    }

    #[test]
    fn kind_is_async() {
        assert_eq!(ScheduleTaskTool.kind(), ToolActionKind::Async);
    }

    #[test]
    fn invalid_notify_or_report_reference_cannot_be_silently_ignored() {
        for extra in [
            serde_json::json!({"notify":"quiet"}),
            serde_json::json!({"report_of":"not-an-id"}),
        ] {
            let mut input = serde_json::json!({"content":"check", "schedule":"cron:*/5 * * * *"});
            input
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            assert!(matches!(
                run_async_blocking(input, ctx_with_channel(Some(inherited_channel()))),
                Err(ToolError::InvalidInput(_))
            ));
        }
    }

    #[test]
    fn execute_returns_internal_state_defense() {
        let result = ScheduleTaskTool.execute(
            &serde_json::json!({}),
            &crate::domain::ToolContext {
                default_wait_tasks_timeout_secs: 300,
                tool_background_after_secs: 0,
                shell_default_tail_lines: 50,
                shell_max_tail_lines: 500,
                shell_default_exec_timeout_secs: 60,
                shell_default_stop_timeout_secs: 5,
                tool_inflight_timeout_secs: 300,
                current_task_id: crate::domain::TaskId::new(),
                current_agent_id: crate::domain::AgentId::new(),
                current_origin_channel: None,
            },
        );
        assert!(matches!(result, Err(ToolError::InternalState(_))));
    }

    #[test]
    fn missing_content_returns_error() {
        let input = serde_json::json!({
            "schedule": format!("once:{}", future_local_iso())
        });
        let result = run_async_blocking(input, ctx_with_channel(None));
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
        let result = run_async_blocking(input, ctx_with_channel(None));
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
        let result = run_async_blocking(input, ctx_with_channel(None));
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
        let result = run_async_blocking(input, ctx_with_channel(None));
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
        let inherited = inherited_channel();
        let result = run_async_blocking(input, ctx_with_channel(Some(inherited.clone())));
        let output = result.unwrap();
        match output {
            ToolWorkerOutput::Effect(ToolEffect::ScheduleTask {
                content,
                schedule,
                output_channel,
                name,
                ..
            }) => {
                assert_eq!(content, "send report");
                assert!(schedule.is_once());
                assert_eq!(output_channel, Some(inherited));
                assert_eq!(name, None);
            }
            other => panic!("expected ScheduleTask effect, got {:?}", other),
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
        let result = run_async_blocking(input, ctx_with_channel(Some(inherited.clone())));
        let output = result.unwrap();
        match output {
            ToolWorkerOutput::Effect(ToolEffect::ScheduleTask {
                output_channel,
                schedule,
                ..
            }) => {
                assert_eq!(output_channel, Some(inherited));
                assert!(schedule.is_once());
            }
            other => panic!("expected ScheduleTask effect, got {:?}", other),
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
        let result = run_async_blocking(input, ctx_with_channel(None));
        let output = result.unwrap();
        match output {
            ToolWorkerOutput::Effect(ToolEffect::ScheduleTask {
                content,
                schedule,
                output_channel,
                ..
            }) => {
                assert_eq!(content, "daily standup");
                assert!(!schedule.is_once());
                assert_eq!(schedule.raw(), "cron:0 9 * * 1-5");
                let channel = output_channel.expect("output_channel should be set");
                assert_eq!(channel.frontend, crate::domain::FrontendKind::Tui);
                assert_eq!(channel.user_id, "user-1");
            }
            other => panic!("expected ScheduleTask effect, got {:?}", other),
        }
    }

    #[test]
    fn explicit_output_channel_requires_target() {
        let input = serde_json::json!({
            "content": "do something",
            "schedule": format!("once:{}", future_local_iso()),
            "output_channel": "tui"
        });
        let result = run_async_blocking(input, ctx_with_channel(None));
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
        let result = run_async_blocking(input, ctx_with_channel(None));
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
        let result = run_async_blocking(input, ctx_with_channel(None));
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
        let result = run_async_blocking(input, ctx_with_channel(Some(inherited_channel())));
        let output = result.unwrap();
        match output {
            ToolWorkerOutput::Effect(ToolEffect::ScheduleTask { output_channel, .. }) => {
                let channel = output_channel.expect("output_channel should be set");
                assert_eq!(channel.frontend, crate::domain::FrontendKind::QQ);
                assert_eq!(channel.user_id, "qq-user");
            }
            other => panic!("expected ScheduleTask effect, got {:?}", other),
        }
    }

    #[test]
    fn all_frontend_kinds_accepted() {
        use crate::domain::FrontendKind;
        for expected in FrontendKind::ALL {
            let name = expected.channel_name();
            let input = serde_json::json!({
                "content": "x",
                "schedule": format!("once:{}", future_local_iso()),
                "output_channel": name,
                "target": "u"
            });
            let result = run_async_blocking(input, ctx_with_channel(None));
            assert!(result.is_ok(), "frontend {} should be accepted", name);
            match result.unwrap() {
                ToolWorkerOutput::Effect(ToolEffect::ScheduleTask { output_channel, .. }) => {
                    let channel = output_channel.expect("output_channel should be set");
                    assert_eq!(channel.frontend, expected, "frontend {} mismatch", name);
                }
                other => panic!("expected ScheduleTask effect, got {:?}", other),
            }
        }
    }

    #[test]
    fn name_is_trimmed_and_passed_through() {
        let input = serde_json::json!({
            "content": "x",
            "name": "  forum-watch  ",
            "schedule": "cron:*/10 * * * *",
            "output_channel": "tui",
            "target": "u"
        });
        match run_async_blocking(input, ctx_with_channel(None)).unwrap() {
            ToolWorkerOutput::Effect(ToolEffect::ScheduleTask { name, .. }) => {
                assert_eq!(name.as_deref(), Some("forum-watch"));
            }
            other => panic!("expected ScheduleTask effect, got {:?}", other),
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
        let result = run_async_blocking(input, ctx_with_channel(None));
        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::InvalidInput(msg) => assert!(msg.contains("cron")),
            other => panic!("expected InvalidInput, got {:?}", other),
        }
    }
}
