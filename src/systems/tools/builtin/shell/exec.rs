use crate::domain::{SessionStartRequest, ToolAction, ToolContext, ToolError};

pub struct ShellExecTool;

impl crate::domain::BuiltinTool for ShellExecTool {
    fn name(&self) -> &str {
        "shell_exec"
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

        let tail_lines = input
            .get("tail_lines")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .unwrap_or(ctx.shell_default_tail_lines)
            .min(ctx.shell_max_tail_lines);

        Ok(ToolAction::ExecSession(SessionStartRequest {
            command: command.to_string(),
            session_name: None,
            cwd: input
                .get("cwd")
                .and_then(|v| v.as_str())
                .map(ToString::to_string),
            env: super::parse_env_map(input)?,
            timeout_secs: input
                .get("timeout_secs")
                .and_then(|v| v.as_u64())
                .or(Some(ctx.shell_default_exec_timeout_secs)),
            tail_lines,
            owner_task_id: ctx.current_task_id,
            owner_agent_id: ctx.current_agent_id,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{BuiltinTool, SpaceKnowledge};

    #[test]
    fn shell_exec_uses_default_tail_limit() {
        let knowledge = SpaceKnowledge::default();
        let ctx = ToolContext {
            knowledge: &knowledge,
            default_wait_tasks_timeout_secs: 300,
            shell_default_tail_lines: 200,
            shell_max_tail_lines: 500,
            shell_default_exec_timeout_secs: 300,
            shell_default_stop_timeout_secs: 10,
            current_task_id: uuid::Uuid::new_v4(),
            current_agent_id: uuid::Uuid::new_v4(),
        };

        let tool = ShellExecTool;
        let action = tool
            .execute(&serde_json::json!({ "command": "echo ok" }), &ctx)
            .expect("shell_exec should parse");

        match action {
            ToolAction::ExecSession(request) => assert_eq!(request.tail_lines, 200),
            other => panic!("expected ExecSession action, got {:?}", other),
        }
    }
}
