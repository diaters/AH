//! update_scheduled_task 工具：修改、暂停 / 恢复、立即运行动态定时任务。
//!
//! 与 delete 同构：worker 只校验入参并声明 `ToolEffect::UpdateSchedule`，
//! 定位与修改在 apply 时刻由 `commit_tool_effects_system` 完成。

use crate::domain::{
    BuiltinTool, OwnedToolContext, ScheduleSelector, ToolAction, ToolActionKind, ToolContext,
    ToolEffect, ToolError, ToolFuture, ToolWorkerOutput,
};
use crate::systems::tools::builtin::schedule_task::parse_schedule;

pub struct UpdateScheduledTaskTool;

impl BuiltinTool for UpdateScheduledTaskTool {
    fn name(&self) -> &str {
        "update_scheduled_task"
    }

    fn kind(&self) -> ToolActionKind {
        ToolActionKind::Async
    }

    fn execute(&self, _: &serde_json::Value, _: &ToolContext) -> Result<ToolAction, ToolError> {
        // Async 工具不会走到这里（dispatch 按 kind 分流）；快速失败防误调
        Err(ToolError::InternalState(
            "update_scheduled_task is async-only".to_string(),
        ))
    }

    fn run_async(&self, input: serde_json::Value, _ctx: OwnedToolContext) -> ToolFuture {
        Box::pin(async move {
            let selector = ScheduleSelector::from_input(&input).map_err(ToolError::InvalidInput)?;
            let content = match input.get("content") {
                None | Some(serde_json::Value::Null) => None,
                Some(v) => Some(
                    v.as_str()
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .ok_or_else(|| {
                            ToolError::InvalidInput(
                                "content must be a non-empty string".to_string(),
                            )
                        })?
                        .to_string(),
                ),
            };
            let schedule = input
                .get("schedule")
                .and_then(|v| v.as_str())
                .map(parse_schedule)
                .transpose()?;
            let enabled = input.get("enabled").and_then(|v| v.as_bool());
            let run_now = input
                .get("run_now")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if content.is_none() && schedule.is_none() && enabled.is_none() && !run_now {
                return Err(ToolError::InvalidInput(
                    "nothing to update: provide content, schedule, enabled or run_now".to_string(),
                ));
            }
            Ok(ToolWorkerOutput::Effect(ToolEffect::UpdateSchedule {
                selector,
                content,
                schedule,
                enabled,
                run_now,
            }))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(input: serde_json::Value) -> Result<ToolWorkerOutput, ToolError> {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(UpdateScheduledTaskTool.run_async(input, OwnedToolContext::empty_for_test(300)))
    }

    #[test]
    fn wraps_patch_into_effect() {
        let output = run(serde_json::json!({
            "name": "forum-watch",
            "content": "new prompt",
            "schedule": "cron:*/5 * * * *",
            "enabled": false,
            "run_now": true
        }))
        .unwrap();
        match output {
            ToolWorkerOutput::Effect(ToolEffect::UpdateSchedule {
                selector,
                content,
                schedule,
                enabled,
                run_now,
            }) => {
                assert_eq!(selector, ScheduleSelector::Name("forum-watch".into()));
                assert_eq!(content.as_deref(), Some("new prompt"));
                assert_eq!(schedule.unwrap().raw(), "cron:*/5 * * * *");
                assert_eq!(enabled, Some(false));
                assert!(run_now);
            }
            other => panic!("expected UpdateSchedule effect, got {other:?}"),
        }
    }

    #[test]
    fn rejects_empty_patch_missing_selector_and_bad_schedule() {
        assert!(matches!(
            run(serde_json::json!({"name": "x"})),
            Err(ToolError::InvalidInput(_))
        ));
        assert!(matches!(
            run(serde_json::json!({"enabled": true})),
            Err(ToolError::InvalidInput(_))
        ));
        assert!(matches!(
            run(serde_json::json!({"name": "x", "schedule": "cron:bad"})),
            Err(ToolError::InvalidInput(_))
        ));
        assert!(matches!(
            run(serde_json::json!({"name": "x", "content": "  "})),
            Err(ToolError::InvalidInput(_))
        ));
    }
}
