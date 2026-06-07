use std::collections::HashMap;

use crate::domain::{SessionStartRequest, ToolAction, ToolContext, ToolError};

pub struct ShellStartTool;

impl crate::domain::BuiltinTool for ShellStartTool {
    fn name(&self) -> &str {
        "shell.start"
    }

    fn execute(
        &self,
        input: &serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolAction, ToolError> {
        let command = input
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing 'command'".to_string()))?;

        Ok(ToolAction::StartSession(SessionStartRequest {
            command: command.to_string(),
            session_name: input
                .get("session_name")
                .and_then(|v| v.as_str())
                .map(ToString::to_string),
            cwd: input.get("cwd").and_then(|v| v.as_str()).map(ToString::to_string),
            env: HashMap::new(),
            timeout_secs: None,
            tail_lines: ctx.shell_default_tail_lines,
            owner_task_id: ctx.current_task_id,
            owner_agent_id: ctx.current_agent_id,
        }))
    }
}
