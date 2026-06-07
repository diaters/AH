use std::{
    collections::HashMap,
    io::{BufRead, BufReader},
    process::{Child, ChildStdin, Command as StdCommand, Stdio},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
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
    pub processes: Arc<Mutex<HashMap<SessionHandleId, Arc<Mutex<Child>>>>>,
    pub stdins: Arc<Mutex<HashMap<SessionHandleId, Arc<Mutex<ChildStdin>>>>>,
}

impl SessionBackend for NativeProcessBackend {
    fn exec_blocking(&self, request: SessionStartRequest) -> Result<SessionHandle, String> {
        let handle_id = Uuid::new_v4();
        let mut command = StdCommand::new("sh");
        command.arg("-c").arg(&request.command);
        if let Some(cwd) = request.cwd.as_ref() {
            command.current_dir(cwd);
        }
        command.stdout(Stdio::piped()).stderr(Stdio::piped());

        let mut child = command.spawn().map_err(|error| error.to_string())?;
        let started_at = Utc::now();
        let deadline = request
            .timeout_secs
            .map(|timeout_secs| Instant::now() + Duration::from_secs(timeout_secs));

        loop {
            if let Some(exit_status) = child.try_wait().map_err(|error| error.to_string())? {
                let output = child.wait_with_output().map_err(|error| error.to_string())?;
                let combined = format!(
                    "{}{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr),
                );
                let (combined_tail, combined_truncated, returned_lines) =
                    tail_lines(&combined, request.tail_lines);
                return Ok(SessionHandle {
                    handle_id,
                    backend: SessionBackendKind::Native,
                    status: if exit_status.success() {
                        SessionStatus::Completed
                    } else {
                        SessionStatus::ExitedWithError
                    },
                    command: request.command,
                    session_name: request.session_name,
                    cwd: request.cwd,
                    exit_code: exit_status.code(),
                    timed_out: false,
                    interaction_required: false,
                    started_at,
                    finished_at: Some(Utc::now()),
                    owner_task_id: request.owner_task_id,
                    owner_agent_id: request.owner_agent_id,
                    output: SessionOutputWindow {
                        combined_tail,
                        combined_truncated,
                        returned_lines,
                        cursor: Some("0".to_string()),
                        next_cursor: Some("1".to_string()),
                    },
                });
            }

            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                let _ = child.kill();
                return Ok(SessionHandle {
                    handle_id,
                    backend: SessionBackendKind::Native,
                    status: SessionStatus::Stopped,
                    command: request.command,
                    session_name: request.session_name,
                    cwd: request.cwd,
                    exit_code: None,
                    timed_out: true,
                    interaction_required: false,
                    started_at,
                    finished_at: Some(Utc::now()),
                    owner_task_id: request.owner_task_id,
                    owner_agent_id: request.owner_agent_id,
                    output: SessionOutputWindow {
                        combined_tail: String::new(),
                        combined_truncated: false,
                        returned_lines: 0,
                        cursor: Some("0".to_string()),
                        next_cursor: Some("0".to_string()),
                    },
                });
            }

            thread::sleep(Duration::from_millis(10));
        }
    }

    fn start_session(&self, request: SessionStartRequest) -> Result<SessionHandle, String> {
        let handle_id = Uuid::new_v4();
        let command_text = request.command.clone();
        let session_name = request.session_name.clone();
        let cwd = request.cwd.clone();
        let owner_task_id = request.owner_task_id;
        let owner_agent_id = request.owner_agent_id;
        let sessions = Arc::clone(&self.sessions);

        let mut command = StdCommand::new("sh");
        command
            .arg("-c")
            .arg(&command_text)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(cwd) = cwd.as_ref() {
            command.current_dir(cwd);
        }
        let mut child = command.spawn().map_err(|error| error.to_string())?;
        let stdin_available = child.stdin.take().map(|s| Arc::new(Mutex::new(s)));
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let child = Arc::new(Mutex::new(child));
        self.processes
            .lock()
            .map_err(|_| "process map poisoned".to_string())?
            .insert(handle_id, child);
        if let Some(stdin) = stdin_available {
            self.stdins
                .lock()
                .map_err(|_| "stdin map poisoned".to_string())?
                .insert(handle_id, stdin);
        }

        let handle = SessionHandle {
            handle_id,
            backend: SessionBackendKind::Native,
            status: SessionStatus::Running,
            command: command_text,
            session_name,
            cwd,
            exit_code: None,
            timed_out: false,
            interaction_required: false,
            started_at: Utc::now(),
            finished_at: None,
            owner_task_id,
            owner_agent_id,
            output: SessionOutputWindow {
                combined_tail: String::new(),
                combined_truncated: false,
                returned_lines: 0,
                cursor: Some("0".to_string()),
                next_cursor: Some("0".to_string()),
            },
        };

        self.sessions
            .lock()
            .map_err(|_| "session map poisoned".to_string())?
            .insert(handle_id, handle.clone());

        spawn_output_reader(handle_id, stdout, Arc::clone(&sessions), true);
        spawn_output_reader(handle_id, stderr, sessions, false);

        debug!(
            event = "ShellSessionStarted",
            handle_id = %handle_id,
            command = %handle.command,
            "shell_start session created with real process"
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
        let process = {
            self.processes
                .lock()
                .map_err(|_| "process map poisoned".to_string())?
                .get(&request.handle_id)
                .cloned()
        };

        let Some(process) = process else {
            return self.get_status(request.handle_id).map(Some);
        };

        let status = {
            let mut child = process.lock().map_err(|_| "process mutex poisoned".to_string())?;
            child.try_wait().map_err(|error| error.to_string())?
        };

        if let Some(exit_status) = status {
            let mut handle = self.get_status(request.handle_id)?;
            handle.status = if exit_status.success() {
                SessionStatus::Completed
            } else {
                SessionStatus::ExitedWithError
            };
            handle.exit_code = exit_status.code();
            handle.finished_at = Some(Utc::now());

            self.sessions
                .lock()
                .map_err(|_| "session map poisoned".to_string())?
                .insert(request.handle_id, handle.clone());

            // Clean up process and stdin resources
            self.processes
                .lock()
                .map_err(|_| "process map poisoned".to_string())?
                .remove(&request.handle_id);
            self.stdins
                .lock()
                .map_err(|_| "stdin map poisoned".to_string())?
                .remove(&request.handle_id);

            Ok(Some(handle))
        } else {
            Ok(None)
        }
    }

    fn stop_session(&self, request: SessionStopRequest) -> Result<SessionHandle, String> {
        let process = self
            .processes
            .lock()
            .map_err(|_| "process map poisoned".to_string())?
            .get(&request.handle_id)
            .cloned()
            .ok_or_else(|| format!("session {} not found", request.handle_id))?;

        {
            let mut child = process.lock().map_err(|_| "process mutex poisoned".to_string())?;
            child.kill().map_err(|error| error.to_string())?;
        }

        let mut handle = self.get_status(request.handle_id)?;
        handle.status = SessionStatus::Stopped;
        handle.finished_at = Some(Utc::now());
        self.sessions
            .lock()
            .map_err(|_| "session map poisoned".to_string())?
            .insert(request.handle_id, handle.clone());

        // Clean up process and stdin resources
        self.processes
            .lock()
            .map_err(|_| "process map poisoned".to_string())?
            .remove(&request.handle_id);
        self.stdins
            .lock()
            .map_err(|_| "stdin map poisoned".to_string())?
            .remove(&request.handle_id);

        debug!(
            event = "ShellSessionStopped",
            handle_id = %request.handle_id,
            wait_for_exit = request.wait_for_exit,
            "shell_stop session stopped (real process killed)"
        );

        Ok(handle)
    }
}

impl Drop for NativeProcessBackend {
    fn drop(&mut self) {
        let processes: Vec<_> = self
            .processes
            .lock()
            .ok()
            .map(|map| map.values().cloned().collect())
            .unwrap_or_default();

        for process in processes {
            if let Ok(mut child) = process.lock() {
                let _ = child.kill();
            }
        }
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

/// 后台读取 stdout/stderr，并把最新窗口写回 SessionHandle。
fn spawn_output_reader(
    handle_id: SessionHandleId,
    stream: Option<impl std::io::Read + Send + 'static>,
    sessions: Arc<Mutex<HashMap<SessionHandleId, SessionHandle>>>,
    is_stdout: bool,
) {
    let Some(stream) = stream else {
        return;
    };

    thread::spawn(move || {
        let reader = BufReader::new(stream);
        for line in reader.lines() {
            let Ok(line) = line else {
                break;
            };
            let mut sessions = sessions.lock().expect("session map poisoned");
            if let Some(handle) = sessions.get_mut(&handle_id) {
                let prefix = if is_stdout { "" } else { "[stderr] " };
                let next = if handle.output.combined_tail.is_empty() {
                    format!("{prefix}{line}")
                } else {
                    format!("{}\n{prefix}{line}", handle.output.combined_tail)
                };
                let (combined_tail, combined_truncated, returned_lines) =
                    tail_lines(&next, handle.output.returned_lines.max(200));
                handle.output.combined_tail = combined_tail;
                handle.output.combined_truncated = combined_truncated;
                handle.output.returned_lines = returned_lines;
                handle.output.cursor = handle.output.next_cursor.clone();
                handle.output.next_cursor = Some(
                    handle
                        .output
                        .next_cursor
                        .as_deref()
                        .unwrap_or("0")
                        .parse::<u64>()
                        .unwrap_or(0)
                        .saturating_add(1)
                        .to_string(),
                );
            }
        }
    });
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
