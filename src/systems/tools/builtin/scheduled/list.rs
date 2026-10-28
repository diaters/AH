use crate::domain::compute_next_trigger;
use crate::domain::{
    BuiltinTool, OwnedToolContext, ToolAction, ToolActionKind, ToolContext, ToolError, ToolFuture,
    ToolWorkerOutput,
};

pub struct ListScheduledTasksTool;

impl BuiltinTool for ListScheduledTasksTool {
    fn name(&self) -> &str {
        "list_scheduled_tasks"
    }

    fn kind(&self) -> ToolActionKind {
        ToolActionKind::Async
    }

    fn execute(&self, _: &serde_json::Value, _: &ToolContext) -> Result<ToolAction, ToolError> {
        // Async 工具不会走到这里（dispatch 按 kind 分流）；快速失败防误调
        Err(ToolError::InternalState(
            "list_scheduled_tasks is async-only".to_string(),
        ))
    }

    fn run_async(&self, _input: serde_json::Value, ctx: OwnedToolContext) -> ToolFuture {
        Box::pin(async move {
            let state = ctx.scheduler_state.ok_or_else(|| {
                ToolError::InternalState("scheduler snapshot missing".to_string())
            })?;

            let mut tasks = Vec::new();
            for def in &state.dynamic_tasks {
                let mut entry = serde_json::json!({
                    "kind": def.kind(),
                    "name": def.name,
                    "content": def.content,
                    "schedule": def.schedule.raw(),
                    "output_channel": def.output_channel,
                    "is_once": def.schedule.is_once(),
                    "enabled": def.enabled,
                    "created_at": def.created_at.to_rfc3339(),
                    "last_fired_at": def.health.last_fired_at.map(|t| t.to_rfc3339()),
                });
                if def.enabled
                    && let Some(next) = compute_next_trigger(def.schedule.spec())
                {
                    entry["next_fire_time"] = serde_json::Value::String(next.to_rfc3339());
                }
                tasks.push(entry);
            }

            Ok(ToolWorkerOutput::Value(serde_json::json!({
                "tasks": tasks,
                "count": tasks.len(),
            })))
        })
    }
}
