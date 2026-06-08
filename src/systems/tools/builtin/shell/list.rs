use crate::domain::{ToolAction, ToolContext, ToolError};

pub struct ShellListTool;

impl crate::domain::BuiltinTool for ShellListTool {
    fn name(&self) -> &str {
        "shell_list"
    }

    fn execute(
        &self,
        _input: &serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolAction, ToolError> {
        Ok(ToolAction::ListSessions)
    }
}
