//! delete_scheduled_task 工具——写路径首个客户。
//!
//! worker 只做一件事：把入参 `kind` 包成
//! `ToolWorkerOutput::Effect(ToolEffect::DeleteScheduledTask)`。
//! 不在 worker 里查存在性——快照可能过期，`existed` 真相在 apply 时刻
//! （由 `commit_tool_effects_system` 经 `update_scheduler_state` 双资源入口产生）。
//! 写路径只有「效果 → commit」一条路径，不存在第二条直达落账的通道。

use crate::domain::{
    BuiltinTool, OwnedToolContext, ToolAction, ToolActionKind, ToolContext, ToolEffect, ToolError,
    ToolFuture, ToolWorkerOutput,
};

pub struct DeleteScheduledTaskTool;

impl BuiltinTool for DeleteScheduledTaskTool {
    fn name(&self) -> &str {
        "delete_scheduled_task"
    }

    fn kind(&self) -> ToolActionKind {
        ToolActionKind::Async
    }

    fn execute(&self, _: &serde_json::Value, _: &ToolContext) -> Result<ToolAction, ToolError> {
        // Async 工具不会走到这里（dispatch 按 kind 分流）；快速失败防误调
        Err(ToolError::InternalState(
            "delete_scheduled_task is async-only".to_string(),
        ))
    }

    fn run_async(&self, input: serde_json::Value, _ctx: OwnedToolContext) -> ToolFuture {
        Box::pin(async move {
            let kind = input
                .get("kind")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    ToolError::InvalidInput("missing required parameter: kind".to_string())
                })?;
            Ok(ToolWorkerOutput::Effect(ToolEffect::DeleteScheduledTask {
                kind: kind.to_string(),
            }))
        })
    }
}
