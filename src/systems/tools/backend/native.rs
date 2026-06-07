use std::{
    collections::HashMap,
    process::Command as StdCommand,
    sync::{Arc, Mutex},
};

use bevy::prelude::Resource;
use chrono::Utc;
use tracing::debug;
use uuid::Uuid;

use crate::{
    contracts::SessionBackend,
    domain::{
        SessionBackendKind, SessionCommand, SessionHandle, SessionHandleId, SessionOutputRequest,
        SessionOutputResponse, SessionOutputWindow, SessionStartRequest, SessionStatus,
        SessionStopRequest, SessionWaitRequest,
    },
};

#[derive(Resource, Default)]
pub struct NativeProcessBackend {
    pub sessions: Arc<Mutex<HashMap<SessionHandleId, SessionHandle>>>,
}

impl SessionBackend for NativeProcessBackend {
    fn exec_blocking(&self, request: SessionStartRequest) -> Result<SessionHandle, String> {
        let handle_id = Uuid::new_v4();
        let command_text = request.command.clone();
        let session_name = request.session_name.clone();
        let cwd = request.cwd.clone();
        let owner_task_id = request.owner_task_id;
        let owner_agent_id = request.owner_agent_id;
        let max_tail_lines = request.tail_lines;

        // Use std::process::Command for blocking execution
        let mut command = StdCommand::new("sh");
        command.arg("-c").arg(&command_text);
        if let Some(ref cwd_path) = cwd {
            command.current_dir(cwd_path);
        }

        // Execute the command
        let execution = command.output().map_err(|e| e.to_string());

        let (status, exit_code, timed_out) = match &execution {
            Ok(output) if output.status.success() => {
                (SessionStatus::Completed, output.status.code(), false)
            }
            Ok(output) => (SessionStatus::ExitedWithError, output.status.code(), false),
            Err(_) => (SessionStatus::FailedToStart, None, false),
        };

        let combined = match &execution {
            Ok(output) => format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ),
            Err(_) => String::new(),
        };

        let (combined_tail, combined_truncated, returned_lines) =
            tail_lines(&combined, max_tail_lines);

        let handle = SessionHandle {
            handle_id,
            backend: SessionBackendKind::Native,
            status,
            command: command_text,
            session_name,
            cwd,
            exit_code,
            timed_out,
            interaction_required: false,
            started_at: Utc::now(),
            finished_at: Some(Utc::now()),
            owner_task_id,
            owner_agent_id,
            output: SessionOutputWindow {
                combined_tail,
                combined_truncated,
                returned_lines,
                cursor: None,
                next_cursor: Some(returned_lines.to_string()),
            },
        };

        debug!(
            event = "ShellExecCompleted",
            handle_id = %handle_id,
            status = ?status,
            exit_code = ?exit_code,
            timed_out = timed_out,
            returned_lines = returned_lines,
            "shell_exec completed"
        );

        Ok(handle)
    }

    fn start_session(&self, request: SessionStartRequest) -> Result<SessionHandle, String> {
        let handle_id = Uuid::new_v4();
        let handle = SessionHandle {
            handle_id,
            backend: SessionBackendKind::Native,
            status: SessionStatus::Running,
            command: request.command,
            session_name: request.session_name,
            cwd: request.cwd,
            exit_code: None,
            timed_out: false,
            interaction_required: false,
            started_at: Utc::now(),
            finished_at: None,
            owner_task_id: request.owner_task_id,
            owner_agent_id: request.owner_agent_id,
            output: SessionOutputWindow {
                combined_tail: String::new(),
                combined_truncated: false,
                returned_lines: 0,
                cursor: None,
                next_cursor: Some("0".to_string()),
            },
        };

        self.sessions
            .lock()
            .map_err(|_| "session map poisoned".to_string())?
            .insert(handle_id, handle.clone());

        debug!(
            event = "ShellSessionStarted",
            handle_id = %handle_id,
            command = %handle.command,
            "shell_start session created"
        );

        Ok(handle)
    }

    fn get_status(&self, handle_id: SessionHandleId) -> Result<SessionHandle, String> {
        self.sessions
            .lock()
            .map_err(|_| "session map poisoned".to_string())?
            .get(&handle_id)
            .cloned()
            .ok_or_else(|| format!("session {} not found", handle_id))
    }

    fn read_output(&self, request: SessionOutputRequest) -> Result<SessionOutputResponse, String> {
        let handle = self.get_status(request.handle_id)?;
        Ok(SessionOutputResponse {
            output: handle.output.clone(),
            handle,
        })
    }

    fn send_input(&self, command: SessionCommand) -> Result<SessionHandle, String> {
        match command {
            SessionCommand::Input { handle_id, .. } => {
                debug!(
                    event = "ShellSendInput",
                    handle_id = %handle_id,
                    "send_input called (stub)"
                );
                self.get_status(handle_id)
            }
            SessionCommand::Signal { .. } => {
                Err("unexpected signal command in send_input".to_string())
            }
        }
    }

    fn send_signal(&self, command: SessionCommand) -> Result<SessionHandle, String> {
        match command {
            SessionCommand::Signal {
                handle_id, signal, ..
            } => {
                debug!(
                    event = "ShellSendSignal",
                    handle_id = %handle_id,
                    signal = %signal,
                    "send_signal called (stub)"
                );
                self.get_status(handle_id)
            }
            SessionCommand::Input { .. } => {
                Err("unexpected input command in send_signal".to_string())
            }
        }
    }

    fn wait_session(&self, request: SessionWaitRequest) -> Result<Option<SessionHandle>, String> {
        let handle = self.get_status(request.handle_id)?;
        if matches!(
            handle.status,
            SessionStatus::Completed
                | SessionStatus::ExitedWithError
                | SessionStatus::Stopped
                | SessionStatus::WaitingForInput
        ) {
            Ok(Some(handle))
        } else {
            Ok(None)
        }
    }

    fn stop_session(&self, request: SessionStopRequest) -> Result<SessionHandle, String> {
        let mut handle = self.get_status(request.handle_id)?;

        // Update status to Stopped
        handle.status = SessionStatus::Stopped;
        handle.finished_at = Some(Utc::now());

        // Update the session in the registry
        self.sessions
            .lock()
            .map_err(|_| "session map poisoned".to_string())?
            .insert(request.handle_id, handle.clone());

        debug!(
            event = "ShellSessionStopped",
            handle_id = %request.handle_id,
            wait_for_exit = request.wait_for_exit,
            "shell_stop session stopped"
        );

        Ok(handle)
    }
}

/// Truncates output to the last N lines.
fn tail_lines(content: &str, max_lines: usize) -> (String, bool, usize) {
    let lines: Vec<&str> = content.lines().collect();
    let returned_lines = lines.len().min(max_lines);
    let truncated = lines.len() > max_lines;
    let start = lines.len().saturating_sub(max_lines);
    let tail = lines[start..].join("\n");
    (tail, truncated, returned_lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_lines_returns_only_latest_lines() {
        let content = "a\nb\nc\nd";
        let (tail, truncated, returned_lines) = tail_lines(content, 2);
        assert_eq!(tail, "c\nd");
        assert!(truncated);
        assert_eq!(returned_lines, 2);
    }

    #[test]
    fn tail_lines_handles_short_content() {
        let content = "a\nb";
        let (tail, truncated, returned_lines) = tail_lines(content, 5);
        assert_eq!(tail, "a\nb");
        assert!(!truncated);
        assert_eq!(returned_lines, 2);
    }
}
