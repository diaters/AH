use std::{
    collections::{HashMap, VecDeque},
    io::{BufRead, BufReader, Write},
    process::{Child, ChildStdin, Command as StdCommand, Stdio},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use crate::prelude::Resource;
use chrono::Utc;
use tracing::debug;
use uuid::Uuid;

use crate::{
    contracts::SessionBackend,
    domain::{
        SessionBackendKind, SessionHandle, SessionHandleId, SessionInputRequest,
        SessionOutputSnapshot, SessionReadRequest, SessionStartRequest, SessionStatus,
        SessionSummary, TaskId,
    },
};

const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
const LIVE_SESSION_TAIL_LINES: usize = 200;

#[derive(Debug, Clone)]
struct SessionOutputBuffer {
    chunks: VecDeque<String>,
    total_bytes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionInteractionState {
    Idle,
    WaitingForInput,
    Busy,
}

#[derive(Debug, Clone)]
struct SessionRuntimeState {
    stdout: SessionOutputBuffer,
    stderr: SessionOutputBuffer,
    combined: SessionOutputBuffer,
    interaction_state: SessionInteractionState,
}

impl SessionOutputBuffer {
    /// 创建 backend 私有的空输出缓冲区。
    fn empty() -> Self {
        Self {
            chunks: VecDeque::new(),
            total_bytes: 0,
        }
    }
}

impl SessionRuntimeState {
    /// 创建 backend 私有的空运行态。
    fn empty() -> Self {
        Self {
            stdout: SessionOutputBuffer::empty(),
            stderr: SessionOutputBuffer::empty(),
            combined: SessionOutputBuffer::empty(),
            interaction_state: SessionInteractionState::Idle,
        }
    }
}

#[derive(Resource, Default, Clone)]
pub struct NativeProcessBackend {
    pub sessions: Arc<Mutex<HashMap<SessionHandleId, SessionHandle>>>,
    pub processes: Arc<Mutex<HashMap<SessionHandleId, Arc<Mutex<Child>>>>>,
    pub stdins: Arc<Mutex<HashMap<SessionHandleId, Arc<Mutex<ChildStdin>>>>>,
    runtimes: Arc<Mutex<HashMap<SessionHandleId, SessionRuntimeState>>>,
}

impl std::fmt::Debug for NativeProcessBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeProcessBackend")
            .finish_non_exhaustive()
    }
}

impl SessionBackend for NativeProcessBackend {
    fn exec_blocking(&self, request: SessionStartRequest) -> Result<SessionHandle, String> {
        let handle_id = Uuid::new_v4();
        let mut command = StdCommand::new("sh");
        command.arg("-c").arg(&request.command);
        if let Some(cwd) = request.cwd.as_ref() {
            command.current_dir(cwd);
        }
        command.envs(&request.env);
        command.stdout(Stdio::piped()).stderr(Stdio::piped());

        let mut child = command.spawn().map_err(|error| error.to_string())?;
        let started_at = Utc::now();
        let deadline = request
            .timeout_secs
            .map(|timeout_secs| Instant::now() + Duration::from_secs(timeout_secs));

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let buffer = Arc::new(Mutex::new(SessionOutputBuffer::empty()));
        let mut stdout_reader =
            spawn_blocking_output_reader(stdout, Arc::clone(&buffer), "", MAX_OUTPUT_BYTES);
        let mut stderr_reader = spawn_blocking_output_reader(
            stderr,
            Arc::clone(&buffer),
            "[stderr] ",
            MAX_OUTPUT_BYTES,
        );

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

                let output = snapshot_from_buffer(&buffer_snapshot, request.tail_lines);

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
                    output,
                };

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
                let output = snapshot_from_buffer(&buffer_snapshot, request.tail_lines);
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
                    output,
                };

                self.sessions
                    .lock()
                    .map_err(|_| "session map poisoned".to_string())?
                    .insert(handle_id, handle.clone());

                return Ok(handle);
            }

            thread::sleep(Duration::from_millis(10));
        }
    }

    /// 取消感知版本：复用 `exec_blocking` 的子进程启动 + reader 线程逻辑，
    /// 把 `try_wait + sleep(10ms)` 循环改为 `try_wait + sleep(10ms) + cancel.is_cancelled()`。
    ///
    /// cancel 触发时：kill 子进程 + wait + 返回 `Err("cancelled")`。
    /// 这是同步方法（返回 `Result`，不是 `Future`），worker 侧用 `spawn_blocking` 包裹。
    fn exec_with_cancel(
        &self,
        request: SessionStartRequest,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<SessionHandle, String> {
        let handle_id = Uuid::new_v4();
        let mut command = StdCommand::new("sh");
        command.arg("-c").arg(&request.command);
        if let Some(cwd) = request.cwd.as_ref() {
            command.current_dir(cwd);
        }
        command.envs(&request.env);
        command.stdout(Stdio::piped()).stderr(Stdio::piped());

        let mut child = command.spawn().map_err(|error| error.to_string())?;
        let started_at = Utc::now();
        let deadline = request
            .timeout_secs
            .map(|timeout_secs| Instant::now() + Duration::from_secs(timeout_secs));

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let buffer = Arc::new(Mutex::new(SessionOutputBuffer::empty()));
        let mut stdout_reader =
            spawn_blocking_output_reader(stdout, Arc::clone(&buffer), "", MAX_OUTPUT_BYTES);
        let mut stderr_reader = spawn_blocking_output_reader(
            stderr,
            Arc::clone(&buffer),
            "[stderr] ",
            MAX_OUTPUT_BYTES,
        );

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

                let output = snapshot_from_buffer(&buffer_snapshot, request.tail_lines);

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
                    output,
                };

                self.sessions
                    .lock()
                    .map_err(|_| "session map poisoned".to_string())?
                    .insert(handle_id, handle.clone());

                return Ok(handle);
            }

            // 取消信号检查：父任务终态触发 cancel_monitor → 本处 kill 子进程并返回 Err
            if cancel.is_cancelled() {
                let _ = child.kill();
                let _ = child.wait();
                if let Some(reader) = stdout_reader.take() {
                    let _ = reader.join();
                }
                if let Some(reader) = stderr_reader.take() {
                    let _ = reader.join();
                }
                return Err("cancelled".to_string());
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
                let output = snapshot_from_buffer(&buffer_snapshot, request.tail_lines);
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
                    output,
                };

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
        command.envs(&request.env);
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
            output: SessionOutputSnapshot {
                output: String::new(),
                returned_lines: 0,
                truncated: false,
            },
        };

        self.sessions
            .lock()
            .map_err(|_| "session map poisoned".to_string())?
            .insert(handle_id, handle.clone());

        self.runtimes
            .lock()
            .map_err(|_| "runtime map poisoned".to_string())?
            .insert(handle_id, SessionRuntimeState::empty());

        spawn_output_reader(
            handle_id,
            stdout,
            Arc::clone(&sessions),
            Arc::clone(&self.runtimes),
            true,
        );
        spawn_output_reader(
            handle_id,
            stderr,
            sessions,
            Arc::clone(&self.runtimes),
            false,
        );

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
        handle.output = self.output_snapshot(request.handle_id, request.tail_lines)?;
        self.sessions
            .lock()
            .map_err(|_| "session map poisoned".to_string())?
            .insert(request.handle_id, handle.clone());
        Ok(self.session_summary(handle))
    }

    fn list_active_sessions(&self) -> Result<Vec<SessionSummary>, String> {
        self.refresh_session_state_batch(self.all_session_ids()?)?;
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
        self.set_interaction_state(request.handle_id, SessionInteractionState::Busy)?;
        let stdin = self
            .stdins
            .lock()
            .map_err(|_| "stdin map poisoned".to_string())?
            .get(&request.handle_id)
            .cloned();

        let Some(stdin) = stdin else {
            self.refresh_session_state(request.handle_id)?;
            self.set_interaction_state(
                request.handle_id,
                SessionInteractionState::WaitingForInput,
            )?;
            let handle = self.get_status(request.handle_id)?;
            return Err(format!(
                "session {} is not accepting input because stdin is unavailable (status: {:?})",
                request.handle_id, handle.status
            ));
        };

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

        self.set_interaction_state(request.handle_id, SessionInteractionState::Idle)?;
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
            .cloned();

        if let Some(process) = process {
            let mut child = process
                .lock()
                .map_err(|_| "process mutex poisoned".to_string())?;
            let _ = child.kill();
            let _ = child.wait();
        }

        let mut handle = self.get_status(handle_id)?;
        handle.status = SessionStatus::Stopped;
        handle.finished_at = Some(Utc::now());
        handle.output = self.output_snapshot(handle_id, LIVE_SESSION_TAIL_LINES)?;
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

    fn list_task_sessions(&self, task_id: TaskId) -> Result<Vec<SessionSummary>, String> {
        self.refresh_session_state_batch(self.task_session_ids(task_id)?)?;
        let sessions = self
            .sessions
            .lock()
            .map_err(|_| "session map poisoned".to_string())?;
        Ok(sessions
            .values()
            .filter(|handle| {
                handle.owner_task_id == task_id
                    && matches!(
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

    fn assert_task_owns_session(
        &self,
        task_id: TaskId,
        handle_id: SessionHandleId,
    ) -> Result<(), String> {
        let handle = self.get_status(handle_id)?;
        if handle.owner_task_id != task_id {
            return Err(format!(
                "session {} does not belong to task {}",
                handle_id, task_id
            ));
        }
        Ok(())
    }

    fn stop_task_sessions(&self, task_id: TaskId) -> Result<Vec<SessionHandleId>, String> {
        let active_ids = self.active_session_ids_for_task(task_id)?;
        let mut stopped = Vec::new();
        for id in active_ids {
            match self.stop_session(id) {
                Ok(_) => {
                    stopped.push(id);
                }
                Err(e) => {
                    debug!(
                        event = "TaskSessionStopFailed",
                        handle_id = %id,
                        error = %e,
                        "failed to stop session during task cleanup, continuing"
                    );
                }
            }
        }
        Ok(stopped)
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

    /// 读取所有已知 session id，供批量状态刷新使用。
    fn all_session_ids(&self) -> Result<Vec<SessionHandleId>, String> {
        let sessions = self
            .sessions
            .lock()
            .map_err(|_| "session map poisoned".to_string())?;
        Ok(sessions.keys().copied().collect())
    }

    /// 收集指定 Task 的所有 session id，供批量状态刷新使用。
    fn task_session_ids(&self, task_id: TaskId) -> Result<Vec<SessionHandleId>, String> {
        let sessions = self
            .sessions
            .lock()
            .map_err(|_| "session map poisoned".to_string())?;
        Ok(sessions
            .values()
            .filter(|handle| handle.owner_task_id == task_id)
            .map(|handle| handle.handle_id)
            .collect())
    }

    /// 收集指定 Task 的所有活动 session id
    fn active_session_ids_for_task(&self, task_id: TaskId) -> Result<Vec<SessionHandleId>, String> {
        let sessions = self
            .sessions
            .lock()
            .map_err(|_| "session map poisoned".to_string())?;
        Ok(sessions
            .values()
            .filter(|handle| {
                handle.owner_task_id == task_id
                    && matches!(
                        handle.status,
                        SessionStatus::Starting
                            | SessionStatus::Running
                            | SessionStatus::WaitingForInput
                    )
            })
            .map(|handle| handle.handle_id)
            .collect())
    }

    /// 从 backend 私有运行态生成对外快照；若无运行态则返回已存储快照。
    fn output_snapshot(
        &self,
        handle_id: SessionHandleId,
        tail_lines_limit: usize,
    ) -> Result<SessionOutputSnapshot, String> {
        let runtime_snapshot = {
            let runtimes = self
                .runtimes
                .lock()
                .map_err(|_| "runtime map poisoned".to_string())?;
            runtimes
                .get(&handle_id)
                .map(|runtime| snapshot_from_buffer(&runtime.combined, tail_lines_limit))
        };
        if let Some(snapshot) = runtime_snapshot {
            Ok(snapshot)
        } else {
            Ok(self.get_status(handle_id)?.output)
        }
    }

    /// 将内部句柄投影为 runtime 对外返回的摘要结构。
    fn session_summary(&self, handle: SessionHandle) -> SessionSummary {
        SessionSummary::from_handle(&handle)
    }

    /// 批量刷新 session 状态，确保活动列表不会包含已经结束的进程。
    fn refresh_session_state_batch(&self, handle_ids: Vec<SessionHandleId>) -> Result<(), String> {
        for handle_id in handle_ids {
            self.refresh_session_state(handle_id)?;
        }
        Ok(())
    }

    /// 更新 backend 私有的交互状态，避免旧的公共 waiting/signal 协议泄漏到 domain。
    fn set_interaction_state(
        &self,
        handle_id: SessionHandleId,
        state: SessionInteractionState,
    ) -> Result<(), String> {
        let mut runtimes = self
            .runtimes
            .lock()
            .map_err(|_| "runtime map poisoned".to_string())?;
        if let Some(runtime) = runtimes.get_mut(&handle_id) {
            runtime.interaction_state = state;
        }
        Ok(())
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
            handle.output = self.output_snapshot(handle_id, LIVE_SESSION_TAIL_LINES)?;

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

/// 将文本追加到输出缓冲区，并维持固定大小的尾部窗口。
fn append_output(buffer: &mut SessionOutputBuffer, content: &str, max_bytes: usize) {
    if content.is_empty() {
        return;
    }

    buffer.total_bytes += content.len();
    buffer.chunks.push_back(content.to_string());

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
    buffer: Arc<Mutex<SessionOutputBuffer>>,
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

/// 根据输出缓冲区生成返回快照。
fn snapshot_from_buffer(
    buffer: &SessionOutputBuffer,
    tail_lines_limit: usize,
) -> SessionOutputSnapshot {
    let joined = buffer.chunks.iter().cloned().collect::<Vec<_>>().join("");
    let (combined_tail, combined_truncated, returned_lines) = tail_lines(&joined, tail_lines_limit);
    SessionOutputSnapshot {
        output: combined_tail,
        returned_lines,
        truncated: combined_truncated,
    }
}

/// 后台读取 stdout/stderr，并把最新快照写回 SessionHandle。
fn spawn_output_reader(
    handle_id: SessionHandleId,
    stream: Option<impl std::io::Read + Send + 'static>,
    sessions: Arc<Mutex<HashMap<SessionHandleId, SessionHandle>>>,
    runtimes: Arc<Mutex<HashMap<SessionHandleId, SessionRuntimeState>>>,
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
            let snapshot = {
                let mut runtimes = runtimes.lock().expect("runtime map poisoned");
                let Some(runtime) = runtimes.get_mut(&handle_id) else {
                    break;
                };

                let prefix = if is_stdout { "" } else { "[stderr] " };
                let chunk = format!("{prefix}{line}\n");
                let buffer = if is_stdout {
                    &mut runtime.stdout
                } else {
                    &mut runtime.stderr
                };
                append_output(buffer, &chunk, MAX_OUTPUT_BYTES);
                append_output(&mut runtime.combined, &chunk, MAX_OUTPUT_BYTES);
                runtime.interaction_state = SessionInteractionState::Idle;
                snapshot_from_buffer(&runtime.combined, LIVE_SESSION_TAIL_LINES)
            };

            let mut sessions = sessions.lock().expect("session map poisoned");
            if let Some(handle) = sessions.get_mut(&handle_id) {
                handle.output = snapshot;
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
