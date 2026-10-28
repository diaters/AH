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

use crate::{
    domain::SessionBackend,
    domain::{
        SessionBackendKind, SessionHandle, SessionHandleId, SessionInputRequest,
        SessionOutputSnapshot, SessionReadRequest, SessionStartRequest, SessionStatus,
        SessionSummary, TaskId,
    },
};

const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
const LIVE_SESSION_TAIL_LINES: usize = 200;
/// 单次返回给 LLM 的输出快照字节上限（行数上限挡不住超长单行）。
const MAX_SNAPSHOT_BYTES: usize = 64 * 1024;
/// 保留的已结束会话记录上限，超出时淘汰最早结束的记录。
const MAX_RETAINED_FINISHED_SESSIONS: usize = 128;
/// 进程退出后等待输出读取线程收尾的宽限期；后台孙进程持有管道时不再无限等待。
const READER_DRAIN_GRACE: Duration = Duration::from_millis(500);

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
        // 永不取消的 token：阻塞执行与取消感知执行共用同一实现
        self.exec_with_cancel(request, tokio_util::sync::CancellationToken::new())
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
        let handle_id = SessionHandleId::new();
        let mut command = StdCommand::new("sh");
        command.arg("-c").arg(&request.command);
        if let Some(cwd) = request.cwd.as_ref() {
            command.current_dir(cwd);
        }
        command.envs(&request.env);
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        isolate_process_group(&mut command);

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
                drain_readers([stdout_reader.take(), stderr_reader.take()]);
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

                self.record_finished(handle.clone())?;

                return Ok(handle);
            }

            // 取消信号检查：父任务终态触发 cancel_monitor → 本处 kill 子进程并返回 Err
            if cancel.is_cancelled() {
                kill_process_tree(&mut child);
                let _ = child.wait();
                drain_readers([stdout_reader.take(), stderr_reader.take()]);
                return Err("cancelled".to_string());
            }

            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                kill_process_tree(&mut child);
                let _ = child.wait();
                drain_readers([stdout_reader.take(), stderr_reader.take()]);
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

                self.record_finished(handle.clone())?;

                return Ok(handle);
            }

            thread::sleep(Duration::from_millis(10));
        }
    }

    fn start_session(&self, request: SessionStartRequest) -> Result<SessionHandle, String> {
        let handle_id = SessionHandleId::new();
        let command_text = request.command.clone();
        let session_name = request.session_name.clone();
        let cwd = request.cwd.clone();
        let owner_task_id = request.owner_task_id;
        let owner_agent_id = request.owner_agent_id;

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
        isolate_process_group(&mut command);
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

        spawn_output_reader(handle_id, stdout, Arc::clone(&self.runtimes), "");
        spawn_output_reader(handle_id, stderr, Arc::clone(&self.runtimes), "[stderr] ");

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
        let active_ids: Vec<_> = self
            .sessions
            .lock()
            .map_err(|_| "session map poisoned".to_string())?
            .values()
            .filter(|handle| is_active(handle.status))
            .map(|handle| handle.handle_id)
            .collect();
        self.live_summaries(active_ids)
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
        self.live_handle(request.handle_id)
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
            kill_process_tree(&mut child);
            let _ = child.wait();
        }

        let mut handle = self.get_status(handle_id)?;
        handle.status = SessionStatus::Stopped;
        handle.finished_at = Some(Utc::now());
        handle.output = self.output_snapshot(handle_id, LIVE_SESSION_TAIL_LINES)?;
        self.record_finished(handle.clone())?;

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
        let active_ids = self.active_session_ids_for_task(task_id)?;
        self.live_summaries(active_ids)
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
            .filter(|handle| handle.owner_task_id == task_id && is_active(handle.status))
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

    /// 读取句柄并按当前输出缓冲区现场生成快照。
    fn live_handle(&self, handle_id: SessionHandleId) -> Result<SessionHandle, String> {
        let mut handle = self.get_status(handle_id)?;
        handle.output = self.output_snapshot(handle_id, LIVE_SESSION_TAIL_LINES)?;
        Ok(handle)
    }

    /// 为一组会话生成带现场输出快照的摘要。
    fn live_summaries(
        &self,
        handle_ids: Vec<SessionHandleId>,
    ) -> Result<Vec<SessionSummary>, String> {
        handle_ids
            .into_iter()
            .map(|id| Ok(self.session_summary(self.live_handle(id)?)))
            .collect()
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

            self.record_finished(handle)?;
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
                kill_process_tree(&mut child);
            }
        }
    }
}

impl NativeProcessBackend {
    /// 记录已结束的会话，并把已结束记录数量限制在上限内（淘汰最早结束的），
    /// 被淘汰会话的输出缓冲区一并释放。
    fn record_finished(&self, handle: SessionHandle) -> Result<(), String> {
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| "session map poisoned".to_string())?;
        sessions.insert(handle.handle_id, handle);
        let mut finished: Vec<_> = sessions
            .values()
            .filter_map(|h| h.finished_at.map(|at| (at, h.handle_id)))
            .collect();
        if finished.len() > MAX_RETAINED_FINISHED_SESSIONS {
            finished.sort();
            let excess = finished.len() - MAX_RETAINED_FINISHED_SESSIONS;
            let mut runtimes = self
                .runtimes
                .lock()
                .map_err(|_| "runtime map poisoned".to_string())?;
            for (_, id) in finished.into_iter().take(excess) {
                sessions.remove(&id);
                runtimes.remove(&id);
            }
        }
        Ok(())
    }
}

/// 会话是否仍在运行（未进入结束状态）。
fn is_active(status: SessionStatus) -> bool {
    matches!(
        status,
        SessionStatus::Starting | SessionStatus::Running | SessionStatus::WaitingForInput
    )
}

/// 让子进程成为独立进程组的组长，便于超时 / 取消时连同后台孙进程一起终止。
fn isolate_process_group(command: &mut StdCommand) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(not(unix))]
    let _ = command;
}

/// 终止子进程及其进程组；只杀 `sh` 会留下孤儿进程，并让输出管道一直不关闭。
fn kill_process_tree(child: &mut Child) {
    #[cfg(unix)]
    if let Ok(pid) = libc::pid_t::try_from(child.id()) {
        // SAFETY: killpg 只向该进程组发送信号，pid 来自本进程 spawn 的子进程（组长）
        unsafe {
            libc::killpg(pid, libc::SIGKILL);
        }
    }
    let _ = child.kill();
}

/// 在宽限期内等待输出读取线程结束；超时的线程被分离（后台进程仍持有管道时）。
fn drain_readers<const N: usize>(readers: [Option<thread::JoinHandle<()>>; N]) {
    let deadline = Instant::now() + READER_DRAIN_GRACE;
    for reader in readers.into_iter().flatten() {
        while !reader.is_finished() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        if reader.is_finished() {
            let _ = reader.join();
        }
    }
}

/// 把文本截断为不超过 `max_bytes` 的尾部（落在字符边界上）。
fn tail_bytes(content: &str, max_bytes: usize) -> (&str, bool) {
    if content.len() <= max_bytes {
        return (content, false);
    }
    let mut start = content.len() - max_bytes;
    while !content.is_char_boundary(start) {
        start += 1;
    }
    (&content[start..], true)
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
    let (combined_tail, lines_truncated, _) = tail_lines(&joined, tail_lines_limit);
    let (combined_tail, bytes_truncated) = tail_bytes(&combined_tail, MAX_SNAPSHOT_BYTES);
    SessionOutputSnapshot {
        output: combined_tail.to_string(),
        returned_lines: combined_tail.lines().count(),
        truncated: lines_truncated || bytes_truncated,
    }
}

/// 后台读取 stdout/stderr 并追加到会话输出缓冲区；快照在读取时才生成，
/// 避免输出密集的进程每行都重建一次快照。
fn spawn_output_reader(
    handle_id: SessionHandleId,
    stream: Option<impl std::io::Read + Send + 'static>,
    runtimes: Arc<Mutex<HashMap<SessionHandleId, SessionRuntimeState>>>,
    prefix: &'static str,
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
            let mut runtimes = runtimes.lock().expect("runtime map poisoned");
            let Some(runtime) = runtimes.get_mut(&handle_id) else {
                break;
            };
            append_output(
                &mut runtime.combined,
                &format!("{prefix}{line}\n"),
                MAX_OUTPUT_BYTES,
            );
            runtime.interaction_state = SessionInteractionState::Idle;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exec_request(command: &str, timeout_secs: Option<u64>) -> SessionStartRequest {
        SessionStartRequest {
            command: command.to_string(),
            session_name: None,
            cwd: None,
            env: HashMap::new(),
            timeout_secs,
            tail_lines: 200,
            owner_task_id: TaskId::new(),
            owner_agent_id: crate::domain::AgentId::new(),
            notify_on_exit: false,
        }
    }

    fn pid_alive(pid: &str) -> bool {
        StdCommand::new("kill")
            .args(["-0", pid])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// 命令留下继承 stdout 的后台进程时，sh 退出后不应等到后台进程结束。
    #[cfg(unix)]
    #[test]
    fn exec_returns_promptly_when_background_child_holds_stdout() {
        let backend = NativeProcessBackend::default();
        let started = Instant::now();
        let handle = backend
            .exec_blocking(exec_request("sleep 30 & echo $!; echo done", None))
            .unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "took {:?}",
            started.elapsed()
        );
        assert!(handle.output.output.contains("done"));
        let pid = handle
            .output
            .output
            .lines()
            .next()
            .unwrap()
            .trim()
            .to_string();
        let _ = StdCommand::new("kill").arg(&pid).status();
    }

    /// 超时时整个进程组被终止，后台孙进程不会成为孤儿。
    #[cfg(unix)]
    #[test]
    fn exec_timeout_kills_whole_process_group() {
        let backend = NativeProcessBackend::default();
        let started = Instant::now();
        let handle = backend
            .exec_blocking(exec_request("sleep 30 & echo $!; sleep 30", Some(1)))
            .unwrap();
        assert!(handle.timed_out);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "took {:?}",
            started.elapsed()
        );
        let pid = handle
            .output
            .output
            .lines()
            .next()
            .unwrap()
            .trim()
            .to_string();
        thread::sleep(Duration::from_millis(100));
        assert!(
            !pid_alive(&pid),
            "background child {pid} survived the timeout"
        );
    }

    /// 快照按字节截断尾部，避免单行超长输出整体灌入上下文。
    #[test]
    fn snapshot_caps_output_bytes() {
        let mut buffer = SessionOutputBuffer::empty();
        let long_line = "x".repeat(MAX_SNAPSHOT_BYTES * 2);
        append_output(
            &mut buffer,
            &format!("{long_line}\nend\n"),
            MAX_OUTPUT_BYTES * 4,
        );
        let snapshot = snapshot_from_buffer(&buffer, 200);
        assert!(snapshot.output.len() <= MAX_SNAPSHOT_BYTES);
        assert!(snapshot.output.ends_with("end"));
        assert!(snapshot.truncated);
    }

    /// 已结束的 exec 记录数量有上限，长时间运行不会无限增长。
    #[test]
    fn finished_exec_records_are_bounded() {
        let backend = NativeProcessBackend::default();
        for _ in 0..(MAX_RETAINED_FINISHED_SESSIONS + 10) {
            backend.exec_blocking(exec_request("true", None)).unwrap();
        }
        let retained = backend.sessions.lock().unwrap().len();
        assert!(
            retained <= MAX_RETAINED_FINISHED_SESSIONS,
            "retained {retained}"
        );
    }

    fn wait_until(mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !condition() {
            assert!(Instant::now() < deadline, "condition not met within 5s");
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// 输出读取线程只追加缓冲区，快照在读取时才生成。
    #[test]
    fn live_session_snapshot_is_built_on_read() {
        let backend = NativeProcessBackend::default();
        let request = exec_request("printf 'a\\nb\\n'; sleep 30", None);
        let task_id = request.owner_task_id;
        let handle = backend.start_session(request).unwrap();
        let id = handle.handle_id;
        wait_until(|| backend.runtimes.lock().unwrap()[&id].combined.chunks.len() == 2);

        let stored = backend.sessions.lock().unwrap()[&id].output.clone();
        assert!(stored.output.is_empty(), "reader wrote a snapshot");

        let listed = backend.list_task_sessions(task_id).unwrap();
        assert_eq!(listed[0].output.output, "a\nb");
        backend.stop_session(id).unwrap();
    }

    /// 已结束的 shell_start 会话记录及其输出缓冲区数量有上限。
    #[test]
    fn finished_shell_sessions_and_buffers_are_bounded() {
        let backend = NativeProcessBackend::default();
        for _ in 0..(MAX_RETAINED_FINISHED_SESSIONS + 10) {
            let id = backend
                .start_session(exec_request("echo hi", None))
                .unwrap()
                .handle_id;
            let mut last = None;
            wait_until(|| {
                let summary = backend
                    .read_session(SessionReadRequest {
                        handle_id: id,
                        tail_lines: 10,
                    })
                    .unwrap();
                let done = summary.finished_at.is_some();
                last = Some(summary);
                done
            });
            assert!(last.unwrap().output.output.contains("hi"));
        }
        let sessions = backend.sessions.lock().unwrap().len();
        let runtimes = backend.runtimes.lock().unwrap().len();
        assert!(
            sessions <= MAX_RETAINED_FINISHED_SESSIONS,
            "sessions {sessions}"
        );
        assert!(
            runtimes <= MAX_RETAINED_FINISHED_SESSIONS,
            "runtimes {runtimes}"
        );
    }

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
