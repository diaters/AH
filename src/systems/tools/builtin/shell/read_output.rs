use crate::domain::{SessionOutputRequest, ToolAction, ToolContext, ToolError};

pub struct ShellReadOutputTool;

impl crate::domain::BuiltinTool for ShellReadOutputTool {
    fn name(&self) -> &str {
        "shell.read_output"
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

        Ok(ToolAction::ReadSessionOutput(SessionOutputRequest {
            handle_id: uuid::Uuid::parse_str(handle_id)
                .map_err(|_| ToolError::InvalidInput("invalid 'handle_id'".to_string()))?,
            cursor: input
                .get("cursor")
                .and_then(|v| v.as_str())
                .map(ToString::to_string),
            tail_lines,
        }))
    }
}
