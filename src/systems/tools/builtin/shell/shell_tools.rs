//! 会话型 shell 工具集（start / read / list / input / stop）
//!
//! 自 5 个微文件合并（P3 拆分粒度双峰治理）；`shell_exec` 体量独立，留在 `exec.rs`。

use crate::domain::{
    SessionInputRequest, SessionReadRequest, SessionStartRequest, ToolAction, ToolContext,
    ToolError,
};

pub struct ShellStartTool;

impl crate::domain::BuiltinTool for ShellStartTool {
    fn name(&self) -> &str {
        "shell_start"
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
            session_name: None,
            cwd: input
                .get("cwd")
                .and_then(|v| v.as_str())
                .map(ToString::to_string),
            env: super::parse_env_map(input)?,
            timeout_secs: None,
            tail_lines: ctx.shell_default_tail_lines,
            owner_task_id: ctx.current_task_id,
            owner_agent_id: ctx.current_agent_id,
        }))
    }
}

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
fn parse_session_id(
    input: &serde_json::Value,
) -> Result<crate::domain::SessionHandleId, ToolError> {
    let session_id = input
        .get("session_id")
        .and_then(|value| value.as_str())
        .ok_or_else(|| ToolError::InvalidInput("missing 'session_id'".to_string()))?;

    uuid::Uuid::parse_str(session_id)
        .map(crate::domain::SessionHandleId)
        .map_err(|_| ToolError::InvalidInput("invalid 'session_id'".to_string()))
}
