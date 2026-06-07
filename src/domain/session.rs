//! Shell session domain types

use std::collections::{HashMap, VecDeque};

use bevy::prelude::Resource;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
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
pub struct SessionOutputWindow {
    pub combined_tail: String,
    pub combined_truncated: bool,
    pub returned_lines: usize,
    pub cursor: Option<String>,
    pub next_cursor: Option<String>,
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
    pub output: SessionOutputWindow,
}

#[derive(Debug, Clone)]
pub struct SessionOutputBuffer {
    pub chunks: VecDeque<String>,
    pub total_bytes: usize,
    pub next_cursor: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionInteractionState {
    Idle,
    WaitingForInput,
    Busy,
}

#[derive(Debug)]
pub struct SessionRuntimeState {
    pub stdout: SessionOutputBuffer,
    pub stderr: SessionOutputBuffer,
    pub combined: SessionOutputBuffer,
    pub interaction_state: SessionInteractionState,
}

#[derive(Resource, Default)]
pub struct SpaceSessionRegistry {
    pub sessions: HashMap<SessionHandleId, SessionHandle>,
    pub runtimes: HashMap<SessionHandleId, SessionRuntimeState>,
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
pub struct SessionOutputRequest {
    pub handle_id: SessionHandleId,
    pub cursor: Option<String>,
    pub tail_lines: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionOutputResponse {
    pub handle: SessionHandle,
    pub output: SessionOutputWindow,
}

#[derive(Debug, Clone)]
pub struct SessionWaitRequest {
    pub handle_id: SessionHandleId,
    pub timeout_secs: u64,
    pub tail_lines: usize,
}

#[derive(Debug, Clone)]
pub struct SessionStopRequest {
    pub handle_id: SessionHandleId,
    pub wait_for_exit: bool,
    pub timeout_secs: u64,
    pub tail_lines: usize,
}

#[derive(Debug, Clone)]
pub enum SessionCommand {
    Input {
        handle_id: SessionHandleId,
        input: String,
        append_newline: bool,
        wait_for_output: bool,
        wait_timeout_secs: Option<u64>,
        tail_lines: usize,
    },
    Signal {
        handle_id: SessionHandleId,
        signal: String,
        wait_for_exit: bool,
        timeout_secs: Option<u64>,
        tail_lines: usize,
    },
}

impl SessionOutputBuffer {
    /// 创建空输出缓冲区。
    pub fn empty() -> Self {
        Self {
            chunks: VecDeque::new(),
            total_bytes: 0,
            next_cursor: 0,
        }
    }
}

impl SessionRuntimeState {
    /// 创建空运行时状态。
    pub fn empty() -> Self {
        Self {
            stdout: SessionOutputBuffer::empty(),
            stderr: SessionOutputBuffer::empty(),
            combined: SessionOutputBuffer::empty(),
            interaction_state: SessionInteractionState::Idle,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_runtime_state_starts_empty() {
        let state = SessionRuntimeState::empty();

        assert_eq!(state.stdout.total_bytes, 0);
        assert_eq!(state.stderr.total_bytes, 0);
        assert_eq!(state.combined.total_bytes, 0);
        assert_eq!(state.stdout.next_cursor, 0);
        assert_eq!(state.interaction_state, SessionInteractionState::Idle);
    }
}
