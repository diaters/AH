//! Shell session domain types

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

use super::{AgentId, TaskId};

pub type SessionHandleId = Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SessionBackendKind {
    Native,
    Herdr,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Starting,
    Running,
    WaitingForInput,
    Completed,
    FailedToStart,
    ExitedWithError,
    Stopped,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionOutputSnapshot {
    pub output: String,
    pub returned_lines: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionHandle {
    pub handle_id: SessionHandleId,
    pub backend: SessionBackendKind,
    pub status: SessionStatus,
    pub command: String,
    pub session_name: Option<String>,
    pub cwd: Option<String>,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub interaction_required: bool,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub owner_task_id: TaskId,
    pub owner_agent_id: AgentId,
    pub output: SessionOutputSnapshot,
}

#[derive(Debug, Clone)]
pub struct SessionStartRequest {
    pub command: String,
    pub session_name: Option<String>,
    pub cwd: Option<String>,
    pub env: HashMap<String, String>,
    pub timeout_secs: Option<u64>,
    pub tail_lines: usize,
    pub owner_task_id: TaskId,
    pub owner_agent_id: AgentId,
}

#[derive(Debug, Clone)]
pub struct SessionReadRequest {
    pub handle_id: SessionHandleId,
    pub tail_lines: usize,
}

#[derive(Debug, Clone)]
pub struct SessionInputRequest {
    pub handle_id: SessionHandleId,
    pub input: String,
    pub append_newline: bool,
}

#[derive(Debug, Clone)]
pub struct SessionSummary {
    pub handle_id: SessionHandleId,
    pub command: String,
    pub cwd: Option<String>,
    pub status: SessionStatus,
    pub exit_code: Option<i32>,
    pub interaction_required: bool,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub output: SessionOutputSnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShellExecResult {
    pub status: SessionStatus,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub interaction_required: bool,
    pub output: String,
    pub returned_lines: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShellSessionResult {
    pub session_id: String,
    pub command: Option<String>,
    pub cwd: Option<String>,
    pub status: SessionStatus,
    pub running: bool,
    pub exit_code: Option<i32>,
    pub interaction_required: bool,
    pub started_at: Option<DateTime<Utc>>,
    pub output: Option<String>,
    pub returned_lines: Option<usize>,
    pub truncated: Option<bool>,
    pub accepted: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShellInputAcceptedResult {
    pub session_id: String,
    pub status: SessionStatus,
    pub accepted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShellStopResult {
    pub session_id: String,
    pub status: SessionStatus,
}

impl SessionSummary {
    /// 从当前内部句柄投影为简化后的会话摘要。
    pub fn from_handle(handle: &SessionHandle) -> Self {
        Self {
            handle_id: handle.handle_id,
            command: handle.command.clone(),
            cwd: handle.cwd.clone(),
            status: handle.status,
            exit_code: handle.exit_code,
            interaction_required: handle.interaction_required,
            started_at: handle.started_at,
            finished_at: handle.finished_at,
            output: handle.output.clone(),
        }
    }
}

impl ShellExecResult {
    /// 从内部句柄构造阻塞执行的对外结果。
    pub fn from_handle(handle: &SessionHandle) -> Self {
        Self {
            status: handle.status,
            exit_code: handle.exit_code,
            timed_out: handle.timed_out,
            interaction_required: handle.interaction_required,
            output: handle.output.output.clone(),
            returned_lines: handle.output.returned_lines,
            truncated: handle.output.truncated,
        }
    }
}

impl ShellSessionResult {
    /// 从简化后的会话摘要构造工具返回值。
    pub fn from_summary(summary: &SessionSummary) -> Self {
        Self {
            session_id: summary.handle_id.to_string(),
            command: Some(summary.command.clone()),
            cwd: summary.cwd.clone(),
            status: summary.status,
            running: matches!(
                summary.status,
                SessionStatus::Starting | SessionStatus::Running | SessionStatus::WaitingForInput
            ),
            exit_code: summary.exit_code,
            interaction_required: summary.interaction_required,
            started_at: Some(summary.started_at),
            output: Some(summary.output.output.clone()),
            returned_lines: Some(summary.output.returned_lines),
            truncated: Some(summary.output.truncated),
            accepted: None,
        }
    }

    /// 构造输入已接受的轻量返回结构。
    pub fn accepted_input(handle: &SessionHandle) -> ShellInputAcceptedResult {
        ShellInputAcceptedResult {
            session_id: handle.handle_id.to_string(),
            status: handle.status,
            accepted: true,
        }
    }

    /// 构造停止会话后的最小返回结构。
    pub fn stopped(handle: &SessionHandle) -> ShellStopResult {
        ShellStopResult {
            session_id: handle.handle_id.to_string(),
            status: handle.status,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_exec_result_uses_snapshot_output() {
        let handle = SessionHandle {
            handle_id: Uuid::new_v4(),
            backend: SessionBackendKind::Native,
            status: SessionStatus::Completed,
            command: "printf 'ok'".to_string(),
            session_name: None,
            cwd: None,
            exit_code: Some(0),
            timed_out: false,
            interaction_required: false,
            started_at: Utc::now(),
            finished_at: Some(Utc::now()),
            owner_task_id: Uuid::new_v4(),
            owner_agent_id: Uuid::new_v4(),
            output: SessionOutputSnapshot {
                output: "ok".to_string(),
                returned_lines: 1,
                truncated: false,
            },
        };

        let result = ShellExecResult::from_handle(&handle);

        assert_eq!(result.output, "ok");
        assert_eq!(result.returned_lines, 1);
        assert!(!result.truncated);
    }
}
