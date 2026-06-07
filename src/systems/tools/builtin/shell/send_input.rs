use crate::domain::{SessionCommand, ToolAction, ToolContext, ToolError};

pub struct ShellSendInputTool;

impl crate::domain::BuiltinTool for ShellSendInputTool {
    fn name(&self) -> &str {
        "shell.send_input"
    }

    fn execute(
        &self,
        input: &serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolAction, ToolError> {
        let handle_id = input
            .get("handle_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing 'handle_id'".to_string()))?;
        let body = input
            .get("input")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing 'input'".to_string()))?;

        let tail_lines = input
            .get("tail_lines")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .unwrap_or(ctx.shell_default_tail_lines)
            .min(ctx.shell_max_tail_lines);

        Ok(ToolAction::SendSessionInput(SessionCommand::Input {
            handle_id: uuid::Uuid::parse_str(handle_id)
                .map_err(|_| ToolError::InvalidInput("invalid 'handle_id'".to_string()))?,
            input: body.to_string(),
            append_newline: input
                .get("append_newline")
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
            wait_for_output: input
                .get("wait_for_output")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            wait_timeout_secs: input.get("wait_timeout_secs").and_then(|v| v.as_u64()),
            tail_lines,
        }))
    }
}
