use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Write},
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
        SessionBackendKind, SessionHandle, SessionHandleId, SessionInputRequest,
        SessionOutputSnapshot, SessionOutputWindow, SessionReadRequest, SessionStartRequest,
        SessionStatus, SessionSummary,
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

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let buffer = Arc::new(Mutex::new(crate::domain::SessionOutputBuffer::empty()));
        let mut stdout_reader =
            spawn_blocking_output_reader(stdout, Arc::clone(&buffer), "", 1024 * 1024);
        let mut stderr_reader =
            spawn_blocking_output_reader(stderr, Arc::clone(&buffer), "[stderr] ", 1024 * 1024);

        loop {
            if let Some(exit_status) = child.try_wait().map_err(|error| error.to_string())? {
                if let Some(reader) = stdout_reader.take() {
                    let _ = reader.join();
                }
                if let Some(reader) = stderr_reader.take() {
                    let _ = reader.join();
                }
                let buffer_snapshot = buffer
                    .lock()
                    .map_err(|_| "output buffer poisoned".to_string())?
                    .clone();

                // Generate window from buffer
                let output_window = window_from_buffer(&buffer_snapshot, None, request.tail_lines);

                let handle = SessionHandle {
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
                    output: output_window,
                };

                // Store handle in sessions map for later read_output() calls
                self.sessions
                    .lock()
                    .map_err(|_| "session map poisoned".to_string())?
                    .insert(handle_id, handle.clone());

                return Ok(handle);
            }

            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                let _ = child.kill();
                let _ = child.wait();
                if let Some(reader) = stdout_reader.take() {
                    let _ = reader.join();
                }
                if let Some(reader) = stderr_reader.take() {
                    let _ = reader.join();
                }
                let buffer_snapshot = buffer
                    .lock()
                    .map_err(|_| "output buffer poisoned".to_string())?
                    .clone();
                let output_window = window_from_buffer(&buffer_snapshot, None, request.tail_lines);
                let handle = SessionHandle {
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
                    output: output_window,
                };

                // Store handle in sessions map for later read_output() calls
                self.sessions
                    .lock()
                    .map_err(|_| "session map poisoned".to_string())?
                    .insert(handle_id, handle.clone());

                return Ok(handle);
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

    fn read_session(&self, request: SessionReadRequest) -> Result<SessionSummary, String> {
        self.refresh_session_state(request.handle_id)?;
        let mut handle = self.get_status(request.handle_id)?;
        let (combined_tail, combined_truncated, returned_lines) =
            tail_lines(&handle.output.combined_tail, request.tail_lines);
        handle.output = SessionOutputWindow {
            combined_tail,
            combined_truncated,
            returned_lines,
            cursor: None,
            next_cursor: handle.output.next_cursor.clone(),
        };
        Ok(self.session_summary(handle))
    }

    fn list_active_sessions(&self) -> Result<Vec<SessionSummary>, String> {
        let sessions = self
            .sessions
            .lock()
            .map_err(|_| "session map poisoned".to_string())?;
        Ok(sessions
            .values()
            .filter(|handle| {
                matches!(
                    handle.status,
                    SessionStatus::Starting
                        | SessionStatus::Running
                        | SessionStatus::WaitingForInput
                )
            })
            .cloned()
            .map(|handle| self.session_summary(handle))
            .collect())
    }

    fn input_session(&self, request: SessionInputRequest) -> Result<SessionHandle, String> {
        let stdin = self
            .stdins
            .lock()
            .map_err(|_| "stdin map poisoned".to_string())?
            .get(&request.handle_id)
            .cloned()
            .ok_or_else(|| format!("stdin for session {} not found", request.handle_id))?;

        {
            let mut stdin = stdin
                .lock()
                .map_err(|_| "stdin mutex poisoned".to_string())?;
            let payload = if request.append_newline {
                format!("{}\n", request.input)
            } else {
                request.input.clone()
            };
            stdin
                .write_all(payload.as_bytes())
                .map_err(|error| error.to_string())?;
            stdin.flush().map_err(|error| error.to_string())?;
        }

        debug!(
            event = "ShellInputAccepted",
            handle_id = %request.handle_id,
            input_len = request.input.len(),
            append_newline = request.append_newline,
            "shell_input wrote to stdin"
        );

        self.refresh_session_state(request.handle_id)?;
        self.get_status(request.handle_id)
    }

    fn stop_session(&self, handle_id: SessionHandleId) -> Result<SessionHandle, String> {
        let process = self
            .processes
            .lock()
            .map_err(|_| "process map poisoned".to_string())?
            .get(&handle_id)
            .cloned()
            .ok_or_else(|| format!("session {} not found", handle_id))?;

        {
            let mut child = process
                .lock()
                .map_err(|_| "process mutex poisoned".to_string())?;
            child.kill().map_err(|error| error.to_string())?;
        }

        let mut handle = self.get_status(handle_id)?;
        handle.status = SessionStatus::Stopped;
        handle.finished_at = Some(Utc::now());
        self.sessions
            .lock()
            .map_err(|_| "session map poisoned".to_string())?
            .insert(handle_id, handle.clone());

        // Clean up process and stdin resources
        self.processes
            .lock()
            .map_err(|_| "process map poisoned".to_string())?
            .remove(&handle_id);
        self.stdins
            .lock()
            .map_err(|_| "stdin map poisoned".to_string())?
            .remove(&handle_id);

        debug!(
            event = "ShellSessionStopped",
            handle_id = %handle_id,
            "shell_stop session stopped (real process killed)"
        );

        Ok(handle)
    }
}

impl NativeProcessBackend {
    /// 读取最新会话句柄，若进程已结束则先同步状态。
    fn get_status(&self, handle_id: SessionHandleId) -> Result<SessionHandle, String> {
        self.sessions
            .lock()
            .map_err(|_| "session map poisoned".to_string())?
            .get(&handle_id)
            .cloned()
            .ok_or_else(|| format!("session {} not found", handle_id))
    }

    /// 将内部输出窗口转换为对外稳定的快照 DTO。
    fn to_snapshot(&self, handle: &SessionHandle) -> SessionOutputSnapshot {
        SessionOutputSnapshot {
            output: handle.output.combined_tail.clone(),
            returned_lines: handle.output.returned_lines,
            truncated: handle.output.combined_truncated,
        }
    }

    /// 将内部句柄投影为 runtime 对外返回的摘要结构。
    fn session_summary(&self, handle: SessionHandle) -> SessionSummary {
        SessionSummary {
            handle_id: handle.handle_id,
            command: handle.command.clone(),
            cwd: handle.cwd.clone(),
            status: handle.status,
            exit_code: handle.exit_code,
            interaction_required: handle.interaction_required,
            started_at: handle.started_at,
            finished_at: handle.finished_at,
            output: self.to_snapshot(&handle),
        }
    }

    /// 同步后台进程状态到会话句柄，避免 shell_read/list 读到过期状态。
    fn refresh_session_state(&self, handle_id: SessionHandleId) -> Result<(), String> {
        let process = {
            self.processes
                .lock()
                .map_err(|_| "process map poisoned".to_string())?
                .get(&handle_id)
                .cloned()
        };

        let Some(process) = process else {
            return Ok(());
        };

        let exit_status = {
            let mut child = process
                .lock()
                .map_err(|_| "process mutex poisoned".to_string())?;
            child.try_wait().map_err(|error| error.to_string())?
        };

        if let Some(exit_status) = exit_status {
            let mut handle = self.get_status(handle_id)?;
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
                .insert(handle_id, handle);
            self.processes
                .lock()
                .map_err(|_| "process map poisoned".to_string())?
                .remove(&handle_id);
            self.stdins
                .lock()
                .map_err(|_| "stdin map poisoned".to_string())?
                .remove(&handle_id);
        }

        Ok(())
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

/// 将文本追加到输出缓冲区，并推进 cursor。
fn append_output(buffer: &mut crate::domain::SessionOutputBuffer, content: &str, max_bytes: usize) {
    if content.is_empty() {
        return;
    }

    buffer.total_bytes += content.len();
    buffer.chunks.push_back(content.to_string());
    buffer.next_cursor += 1;

    while buffer.total_bytes > max_bytes {
        if let Some(front) = buffer.chunks.pop_front() {
            buffer.total_bytes = buffer.total_bytes.saturating_sub(front.len());
        } else {
            break;
        }
    }
}

/// 为阻塞执行读取 stdout/stderr，并将内容追加到共享的输出缓冲区中。
fn spawn_blocking_output_reader(
    stream: Option<impl std::io::Read + Send + 'static>,
    buffer: Arc<Mutex<crate::domain::SessionOutputBuffer>>,
    prefix: &'static str,
    max_bytes: usize,
) -> Option<thread::JoinHandle<()>> {
    let stream = stream?;

    Some(thread::spawn(move || {
        let reader = BufReader::new(stream);
        for line in reader.lines() {
            let Ok(line) = line else {
                break;
            };

            let mut buffer = buffer.lock().expect("output buffer poisoned");
            append_output(&mut buffer, &format!("{prefix}{line}\n"), max_bytes);
        }
    }))
}

/// 根据输出缓冲区生成返回窗口。
fn window_from_buffer(
    buffer: &crate::domain::SessionOutputBuffer,
    cursor: Option<String>,
    tail_lines_limit: usize,
) -> crate::domain::SessionOutputWindow {
    let joined = buffer.chunks.iter().cloned().collect::<Vec<_>>().join("");
    let (combined_tail, combined_truncated, returned_lines) = tail_lines(&joined, tail_lines_limit);
    crate::domain::SessionOutputWindow {
        combined_tail,
        combined_truncated,
        returned_lines,
        cursor,
        next_cursor: Some(buffer.next_cursor.to_string()),
    }
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
