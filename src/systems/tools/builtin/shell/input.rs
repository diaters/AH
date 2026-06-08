use crate::domain::{SessionInputRequest, ToolAction, ToolContext, ToolError};

pub struct ShellInputTool;

impl crate::domain::BuiltinTool for ShellInputTool {
    fn name(&self) -> &str {
        "shell_input"
    }

    fn execute(
        &self,
        input: &serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolAction, ToolError> {
        let handle_id = parse_session_id(input)?;
        let text = input
            .get("input")
            .and_then(|value| value.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing 'input'".to_string()))?;

        Ok(ToolAction::InputSession(SessionInputRequest {
            handle_id,
            input: text.to_string(),
            append_newline: input
                .get("append_newline")
                .and_then(|value| value.as_bool())
                .unwrap_or(true),
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
