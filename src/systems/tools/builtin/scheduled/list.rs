use tracing::warn;

use crate::domain::{
    BuiltinTool, OwnedToolContext, ToolAction, ToolActionKind, ToolContext, ToolError, ToolFuture,
    ToolWorkerOutput,
};
use crate::domain::compute_next_trigger;

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
            let registry = ctx
                .registry
                .ok_or_else(|| ToolError::InternalState("registry snapshot missing".to_string()))?;

            let mut tasks = Vec::new();
            for dt in &state.dynamic_tasks {
                let Some(info) = registry.tasks.get(&dt.kind) else {
                    // 过滤不一致项 + 必须 warn（双账本漂移的线上观测）
                    warn!(
                        event = "SchedulerLedgerInconsistency",
                        kind = %dt.kind,
                        scheduler_count = state.dynamic_tasks.len(),
                        registry_count = registry.tasks.len(),
                        "task in SchedulerState but not in Registry; filtered"
                    );
                    continue;
                };

                let mut entry = serde_json::json!({
                    "kind": dt.kind,
                    "content": info.content,
                    "output_channel": info.output_channel,
                    "is_once": info.is_once,
                    "created_at": dt.created_at.to_rfc3339(),
                });
                if let Some(next) = compute_next_trigger(&dt.schedule) {
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
