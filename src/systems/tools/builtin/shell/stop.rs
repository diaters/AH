use crate::domain::{ToolAction, ToolContext, ToolError};

pub struct ShellStopTool;

impl crate::domain::BuiltinTool for ShellStopTool {
    fn name(&self) -> &str {
        "shell_stop"
    }

    fn execute(
        &self,
        input: &serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolAction, ToolError> {
        let handle_id = parse_session_id(input)?;
        Ok(ToolAction::StopSession(handle_id))
    }
}

/// 解析简化契约中的 `session_id` 并映射到内部 handle 标识。
fn parse_session_id(input: &serde_json::Value) -> Result<uuid::Uuid, ToolError> {
    let session_id = input
        .get("session_id")
        .and_then(|value| value.as_str())
        .ok_or_else(|| ToolError::InvalidInput("missing 'session_id'".to_string()))?;

    uuid::Uuid::parse_str(session_id)
        .map_err(|_| ToolError::InvalidInput("invalid 'session_id'".to_string()))
}
