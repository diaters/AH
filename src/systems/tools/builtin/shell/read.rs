use crate::domain::{SessionReadRequest, ToolAction, ToolContext, ToolError};

pub struct ShellReadTool;

impl crate::domain::BuiltinTool for ShellReadTool {
    fn name(&self) -> &str {
        "shell_read"
    }

    fn execute(
        &self,
        input: &serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolAction, ToolError> {
        let handle_id = parse_session_id(input)?;
        let tail_lines = input
            .get("tail_lines")
            .and_then(|value| value.as_u64())
            .map(|value| value as usize)
            .unwrap_or(ctx.shell_default_tail_lines)
            .min(ctx.shell_max_tail_lines);

        Ok(ToolAction::ReadSession(SessionReadRequest {
            handle_id,
            tail_lines,
        }))
    }
}

/// 解析简化后契约中的 `session_id` 并映射到内部 handle 标识。
fn parse_session_id(input: &serde_json::Value) -> Result<uuid::Uuid, ToolError> {
    let session_id = input
        .get("session_id")
        .and_then(|value| value.as_str())
        .ok_or_else(|| ToolError::InvalidInput("missing 'session_id'".to_string()))?;

    uuid::Uuid::parse_str(session_id)
        .map_err(|_| ToolError::InvalidInput("invalid 'session_id'".to_string()))
}
