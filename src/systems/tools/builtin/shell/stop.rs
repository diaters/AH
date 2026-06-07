use crate::domain::{SessionStopRequest, ToolAction, ToolContext, ToolError};

pub struct ShellStopTool;

impl crate::domain::BuiltinTool for ShellStopTool {
    fn name(&self) -> &str {
        "shell_stop"
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

        let tail_lines = input
            .get("tail_lines")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .unwrap_or(ctx.shell_default_tail_lines)
            .min(ctx.shell_max_tail_lines);

        Ok(ToolAction::StopSession(SessionStopRequest {
            handle_id: uuid::Uuid::parse_str(handle_id)
                .map_err(|_| ToolError::InvalidInput("invalid 'handle_id'".to_string()))?,
            wait_for_exit: input
                .get("wait_for_exit")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            timeout_secs: input
                .get("timeout_secs")
                .and_then(|v| v.as_u64())
                .unwrap_or(ctx.shell_default_stop_timeout_secs),
            tail_lines,
        }))
    }
}
