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

#[derive(Resource, Clone)]
pub struct NativeProcessBackend {
    pub sessions: Arc<Mutex<HashMap<SessionHandleId, SessionHandle>>>,
    pub processes: Arc<Mutex<HashMap<SessionHandleId, Arc<Mutex<Child>>>>>,
    pub stdins: Arc<Mutex<HashMap<SessionHandleId, Arc<Mutex<ChildStdin>>>>>,
    runtimes: Arc<Mutex<HashMap<SessionHandleId, SessionRuntimeState>>>,
    lifecycle: Arc<BackendLifecycle>,
    #[cfg(unix)]
    supervisor: Option<Arc<super::supervisor::SupervisorClient>>,
    /// exec 与 shell_start 的输出读取线程，结束时限时读取管道中的剩余输出。
    readers: Arc<Mutex<HashMap<SessionHandleId, Vec<thread::JoinHandle<()>>>>>,
}

/// 所有 backend handle 共享唯一的清理责任；reader 线程不持有此 guard。
struct BackendLifecycle {
    /// spawn + 注册与 shutdown 串行化，防止关停快照之后又出现新进程。
    shutdown: Mutex<bool>,
    processes: Arc<Mutex<HashMap<SessionHandleId, Arc<Mutex<Child>>>>>,
    stdins: Arc<Mutex<HashMap<SessionHandleId, Arc<Mutex<ChildStdin>>>>>,
    readers: Arc<Mutex<HashMap<SessionHandleId, Vec<thread::JoinHandle<()>>>>>,
}

impl Default for NativeProcessBackend {
    fn default() -> Self {
        let processes = Arc::default();
        let stdins = Arc::default();
        let readers = Arc::default();
        Self {
            #[cfg(unix)]
            supervisor: None,
            sessions: Arc::default(),
            runtimes: Arc::default(),
            lifecycle: Arc::new(BackendLifecycle {
                shutdown: Mutex::new(false),
                processes: Arc::clone(&processes),
                stdins: Arc::clone(&stdins),
                readers: Arc::clone(&readers),
            }),
            processes,
            stdins,
            readers,
        }
    }
}

impl Drop for BackendLifecycle {
    fn drop(&mut self) {
        // 显式 shutdown 是主路径；最后一个 handle 的析构只作兜底。
        if let Ok(mut processes) = self.processes.lock() {
            for process in processes.values() {
                if let Ok(mut child) = process.lock() {
                    kill_process_tree(&mut child);
                    let _ = child.wait();
                }
            }
            processes.clear();
        }
        if let Ok(mut stdins) = self.stdins.lock() {
            stdins.clear();
        }
        if let Ok(mut readers) = self.readers.lock() {
            drain_readers(readers.drain().flat_map(|(_, readers)| readers).map(Some));
        }
    }
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

    /// 一次性 exec 与 shell_start 共用进程注册表、输出缓冲和 Task 归属。
    /// 所有退出路径都清理同组后代并回收组长；取消返回 `Err("cancelled")`。
    /// 这是同步方法（返回 `Result`，不是 `Future`），worker 侧用 `spawn_blocking` 包裹。
    fn exec_with_cancel(
        &self,
        request: SessionStartRequest,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<SessionHandle, String> {
        #[cfg(unix)]
        if let Some(supervisor) = &self.supervisor {
            return supervisor.call(super::supervisor::Operation::Exec(request), Some(cancel));
        }

        let tail_lines = request.tail_lines;
        let deadline = request
            .timeout_secs
            .map(|seconds| Instant::now() + Duration::from_secs(seconds));
        let handle = self.spawn_registered_process(request, Stdio::inherit())?;
        let id = handle.handle_id;
        // 保存自己的 child 引用；task cleanup 可以从公共注册表移除它。
        let process = self
            .processes
            .lock()
            .map_err(|_| "process map poisoned".to_string())?
            .get(&id)
            .cloned()
            .ok_or_else(|| "exec process disappeared".to_string())?;
        loop {
            if cancel.is_cancelled() {
                self.stop_session(id)?;
                return Err("cancelled".to_string());
            }
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                let mut handle = self.stop_session(id)?;
                handle.timed_out = true;
                handle.output = self.output_snapshot(id, tail_lines)?;
                return self.record_finished(handle);
            }
            let exit_status = {
                let mut child = process
                    .lock()
                    .map_err(|_| "process mutex poisoned".to_string())?;
                match poll_process_exit(&mut child) {
                    Ok(status) => status,
                    Err(error) => {
                        // 轮询错误同样负责清理，而非把子进程遗留给析构兜底。
                        kill_process_tree(&mut child);
                        let _ = child.wait();
                        drop(child);
                        self.stop_session(id)?;
                        return Err(error.to_string());
                    }
                }
            };
            if let Some(exit_status) = exit_status {
                self.drain_session_readers(id)?;
                let mut handle = self.get_status(id)?;
                // 显式 task cleanup 的 Stopped 语义不能被 worker 晚到的完成覆盖。
                if handle.status != SessionStatus::Stopped {
                    handle.status = if exit_status.success() {
                        SessionStatus::Completed
                    } else {
                        SessionStatus::ExitedWithError
                    };
                    handle.exit_code = exit_status.code();
                }
                handle.finished_at = Some(Utc::now());
                handle.output = self.output_snapshot(id, tail_lines)?;
                let handle = self.record_finished(handle)?;
                self.processes
                    .lock()
                    .map_err(|_| "process map poisoned".to_string())?
                    .remove(&id);
                self.stdins
                    .lock()
                    .map_err(|_| "stdin map poisoned".to_string())?
                    .remove(&id);
                return Ok(handle);
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn start_session(&self, request: SessionStartRequest) -> Result<SessionHandle, String> {
        #[cfg(unix)]
        if let Some(supervisor) = &self.supervisor {
            return supervisor.call(super::supervisor::Operation::Start(request), None);
        }

        self.spawn_registered_process(request, Stdio::piped())
    }

    fn read_session(&self, request: SessionReadRequest) -> Result<SessionSummary, String> {
        #[cfg(unix)]
        if let Some(supervisor) = &self.supervisor {
            return supervisor.call(super::supervisor::Operation::Read(request), None);
        }

        self.refresh_session_state(request.handle_id)?;
        let mut handle = self.get_status(request.handle_id)?;
        handle.output = self.output_snapshot(request.handle_id, request.tail_lines)?;
        // 读取快照不回写句柄，避免与 stop 并发时把旧 Running 状态写回注册表。
        Ok(self.session_summary(handle))
    }

    fn list_active_sessions(&self) -> Result<Vec<SessionSummary>, String> {
        #[cfg(unix)]
        if let Some(supervisor) = &self.supervisor {
            return supervisor.call(super::supervisor::Operation::List, None);
        }

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
        #[cfg(unix)]
        if let Some(supervisor) = &self.supervisor {
            return supervisor.call(super::supervisor::Operation::Input(request), None);
        }

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
        #[cfg(unix)]
        if let Some(supervisor) = &self.supervisor {
            return supervisor.call(super::supervisor::Operation::Stop(handle_id), None);
        }

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
            terminate_process_tree(&mut child);
            let _ = child.wait();
        }
        self.drain_session_readers(handle_id)?;

        let mut handle = self.get_status(handle_id)?;
        handle.status = SessionStatus::Stopped;
        handle.finished_at = Some(Utc::now());
        handle.output = self.output_snapshot(handle_id, LIVE_SESSION_TAIL_LINES)?;
        let handle = self.record_finished(handle)?;

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
        #[cfg(unix)]
        if let Some(supervisor) = &self.supervisor {
            return supervisor.call(super::supervisor::Operation::ListTask(task_id), None);
        }

        self.refresh_session_state_batch(self.task_session_ids(task_id)?)?;
        let active_ids = self.active_session_ids_for_task(task_id)?;
        self.live_summaries(active_ids)
    }

    fn assert_task_owns_session(
        &self,
        task_id: TaskId,
        handle_id: SessionHandleId,
    ) -> Result<(), String> {
        #[cfg(unix)]
        if let Some(supervisor) = &self.supervisor {
            return supervisor.call(
                super::supervisor::Operation::AssertOwner(task_id, handle_id),
                None,
            );
        }

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
        #[cfg(unix)]
        if let Some(supervisor) = &self.supervisor {
            return supervisor.call(super::supervisor::Operation::StopTask(task_id), None);
        }

        self.stop_sessions(self.active_session_ids_for_task(task_id)?)
    }
}

impl NativeProcessBackend {
    /// 创建本实例独占的进程服务；失败直接返回，不降级为本地启动。
    #[cfg(unix)]
    pub fn supervised(executable: impl AsRef<std::path::Path>) -> Result<Self, String> {
        let supervisor = super::supervisor::SupervisorClient::launch(executable.as_ref())?;
        Ok(Self {
            supervisor: Some(Arc::new(supervisor)),
            ..Self::default()
        })
    }

    pub fn supervised_current_exe() -> Result<Self, String> {
        #[cfg(unix)]
        {
            Self::supervised(std::env::current_exe().map_err(|error| error.to_string())?)
        }
        #[cfg(not(unix))]
        {
            Err("process supervision requires a supported Unix platform".to_string())
        }
    }

    /// 共用进程登记与输出生命周期；exec 继承 stdin，会话创建交互输入管道。
    fn spawn_registered_process(
        &self,
        request: SessionStartRequest,
        stdin: Stdio,
    ) -> Result<SessionHandle, String> {
        let shutdown = self
            .lifecycle
            .shutdown
            .lock()
            .map_err(|_| "shutdown mutex poisoned".to_string())?;
        if *shutdown {
            return Err("native process backend is shut down".to_string());
        }
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
            .stdin(stdin)
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

        let readers = [
            spawn_output_reader(handle_id, stdout, Arc::clone(&self.runtimes), ""),
            spawn_output_reader(handle_id, stderr, Arc::clone(&self.runtimes), "[stderr] "),
        ];
        self.readers
            .lock()
            .map_err(|_| "reader map poisoned".to_string())?
            .insert(handle_id, readers.into_iter().flatten().collect());

        debug!(
            event = "ShellSessionStarted",
            handle_id = %handle_id,
            command = %handle.command,
            "native shell process registered"
        );

        Ok(handle)
    }

    /// 幂等关停所有已注册进程；所有 clone 立即拒绝后续 spawn。
    /// 此接口由运行时所有者调用，最终析构仅提供兜底。
    pub fn shutdown(&self) -> Result<(), String> {
        #[cfg(unix)]
        if let Some(supervisor) = &self.supervisor {
            return supervisor.shutdown();
        }

        let mut shutdown = self
            .lifecycle
            .shutdown
            .lock()
            .map_err(|_| "shutdown mutex poisoned".to_string())?;
        *shutdown = true;
        let ids: Vec<_> = self
            .processes
            .lock()
            .map_err(|_| "process map poisoned".to_string())?
            .keys()
            .copied()
            .collect();
        self.stop_sessions(ids).map(|_| ())
    }

    /// 批量停止共用一秒 TERM 宽限与一次输出 drain 宽限，避免随会话数串行累加。
    fn stop_sessions(&self, ids: Vec<SessionHandleId>) -> Result<Vec<SessionHandleId>, String> {
        let processes: Vec<_> = {
            let processes = self
                .processes
                .lock()
                .map_err(|_| "process map poisoned".to_string())?;
            ids.iter()
                .filter_map(|id| processes.get(id).cloned())
                .collect()
        };
        for process in &processes {
            if let Ok(mut child) = process.lock() {
                send_termination_signal(&mut child);
            }
        }
        #[cfg(unix)]
        {
            let deadline = Instant::now() + Duration::from_secs(1);
            while processes.iter().any(|process| {
                process
                    .lock()
                    .is_ok_and(|child| matches!(observe_child_exit(&child), Ok(false)))
            }) && Instant::now() < deadline
            {
                thread::sleep(Duration::from_millis(10));
            }
        }
        for process in &processes {
            if let Ok(mut child) = process.lock() {
                kill_process_tree(&mut child);
                let _ = child.wait();
            }
        }
        let readers: Vec<_> = {
            let mut readers = self
                .readers
                .lock()
                .map_err(|_| "reader map poisoned".to_string())?;
            ids.iter()
                .flat_map(|id| readers.remove(id).unwrap_or_default())
                .collect()
        };
        drain_readers(readers.into_iter().map(Some));
        let mut stopped = Vec::new();
        let mut errors = Vec::new();
        for id in ids {
            match self.stop_session(id) {
                Ok(_) => stopped.push(id),
                Err(error) => {
                    debug!(event = "TaskSessionStopFailed", handle_id = %id, error = %error,
                        "failed to stop session during batch cleanup, continuing");
                    errors.push(format!("session {id}: {error}"));
                }
            }
        }
        if errors.is_empty() {
            Ok(stopped)
        } else {
            Err(errors.join("; "))
        }
    }

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

    /// 在宽限期内等待会话的输出读取线程读完剩余输出。
    fn drain_session_readers(&self, handle_id: SessionHandleId) -> Result<(), String> {
        let readers = self
            .readers
            .lock()
            .map_err(|_| "reader map poisoned".to_string())?
            .remove(&handle_id)
            .unwrap_or_default();
        drain_readers(readers.into_iter().map(Some));
        Ok(())
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
            poll_process_exit(&mut child).map_err(|error| error.to_string())?
        };

        if let Some(exit_status) = exit_status {
            // 进程已退出但管道中可能还有未读完的输出，先等读取线程收尾再生成结束快照
            self.drain_session_readers(handle_id)?;
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

impl NativeProcessBackend {
    /// 记录已结束的会话，并把已结束记录数量限制在上限内（淘汰最早结束的），
    /// 被淘汰会话的输出缓冲区一并释放。
    fn record_finished(&self, mut handle: SessionHandle) -> Result<SessionHandle, String> {
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| "session map poisoned".to_string())?;
        // 所有完成路径在同一把锁下合并终态，stop 的明确结果不能被迟到完成覆盖。
        if let Some(current) = sessions.get(&handle.handle_id)
            && current.status == SessionStatus::Stopped
            && handle.status != SessionStatus::Stopped
        {
            handle.status = current.status;
            handle.exit_code = current.exit_code;
            handle.timed_out = current.timed_out;
            handle.finished_at = current.finished_at;
        }
        sessions.insert(handle.handle_id, handle.clone());
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
        Ok(handle)
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

/// 观察组长退出但暂不回收 PID；在 killpg 之前保留 PID，阻止 PGID 被复用。
#[cfg(unix)]
fn observe_child_exit(child: &Child) -> std::io::Result<bool> {
    // SAFETY: waitid 写入完整 siginfo，P_PID 只观察本进程直接拥有的 child。
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            child.id() as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if result == -1 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(info.si_signo != 0)
}

/// 在组长被回收之前清理同组后代，再由 Child 保存真实退出码。
fn poll_process_exit(child: &mut Child) -> std::io::Result<Option<std::process::ExitStatus>> {
    #[cfg(unix)]
    match observe_child_exit(child) {
        Ok(false) => return Ok(None),
        Ok(true) => {
            kill_process_tree(child);
            return child.wait().map(Some);
        }
        // 另一路 stop/refresh 已回收时，Child::try_wait 读取其缓存，不再信号旧 PGID。
        Err(error) if error.raw_os_error() == Some(libc::ECHILD) => {}
        Err(error) => return Err(error),
    }
    child.try_wait()
}

fn send_termination_signal(child: &mut Child) {
    #[cfg(unix)]
    if observe_child_exit(child).is_ok()
        && let Ok(pid) = libc::pid_t::try_from(child.id())
    {
        // SAFETY: 未回收的组长 PID 保留，确保只向其原始进程组发信号。
        unsafe {
            libc::killpg(pid, libc::SIGTERM);
        }
    }
    #[cfg(not(unix))]
    let _ = child.kill();
}

/// 显式停止先给进程组最多一秒处理 TERM，再强杀同组残留；全程保留组长 PID。
fn terminate_process_tree(child: &mut Child) {
    send_termination_signal(child);
    #[cfg(unix)]
    {
        let deadline = Instant::now() + Duration::from_secs(1);
        while matches!(observe_child_exit(child), Ok(false)) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
    }
    kill_process_tree(child);
}

/// 只有尚未回收、仍由本进程拥有的组长才允许信号其 PGID。
fn kill_process_tree(child: &mut Child) {
    #[cfg(unix)]
    if observe_child_exit(child).is_ok()
        && let Ok(pid) = libc::pid_t::try_from(child.id())
    {
        // SAFETY: 未回收的组长 PID 仍保留，spawn 为它创建了独立进程组。
        unsafe {
            libc::killpg(pid, libc::SIGKILL);
        }
    }
    let _ = child.kill();
}

/// 在宽限期内等待输出读取线程结束；超时的线程被分离（后台进程仍持有管道时）。
fn drain_readers(readers: impl IntoIterator<Item = Option<thread::JoinHandle<()>>>) {
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
) -> Option<thread::JoinHandle<()>> {
    let stream = stream?;

    Some(thread::spawn(move || {
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
    }))
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

    #[cfg(unix)]
    fn pid_alive(pid: &str) -> bool {
        let Ok(pid) = pid.parse::<libc::pid_t>() else {
            return false;
        };
        // SAFETY: 信号 0 只检查进程是否存在。
        unsafe { libc::kill(pid, 0) == 0 }
    }

    #[test]
    fn stopped_terminal_state_survives_delayed_completion_writeback() {
        let backend = NativeProcessBackend::default();
        let running = backend
            .start_session(exec_request("sleep 30", None))
            .unwrap();
        let mut delayed_completion = running.clone();
        delayed_completion.status = SessionStatus::Completed;
        delayed_completion.finished_at = Some(Utc::now());
        delayed_completion.exit_code = Some(0);
        backend.stop_session(running.handle_id).unwrap();
        let returned = backend.record_finished(delayed_completion).unwrap();
        assert_eq!(returned.status, SessionStatus::Stopped);
        let stored = backend.get_status(running.handle_id).unwrap();
        assert_eq!(stored.status, SessionStatus::Stopped);
        assert_eq!(stored.exit_code, None);
    }

    /// 独立测试进程控制 stdin，避免并行测试修改宿主 fd0。
    #[test]
    fn exec_inherits_eof_stdin_instead_of_waiting_for_timeout() {
        const CHILD_ENV: &str = "HARNESS_TEST_EXEC_EOF_STDIN_CHILD";
        if std::env::var_os(CHILD_ENV).is_some() {
            let backend = NativeProcessBackend::default();
            let handle = backend
                .exec_blocking(exec_request("cat; echo done", Some(1)))
                .unwrap();
            assert!(
                !handle.timed_out,
                "one-shot exec blocked on an unwritten stdin pipe"
            );
            assert_eq!(handle.status, SessionStatus::Completed);
            assert_eq!(handle.output.output, "done");
            assert!(backend.stdins.lock().unwrap().is_empty());
            return;
        }
        let result = StdCommand::new(std::env::current_exe().unwrap())
            .args(["--exact", "systems::tools::backend::native::tests::exec_inherits_eof_stdin_instead_of_waiting_for_timeout", "--nocapture"])
            .env(CHILD_ENV, "1")
            .stdin(Stdio::null())
            .output().unwrap();
        assert!(
            result.status.success(),
            "EOF stdin child test failed:\n{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
    }

    #[cfg(unix)]
    #[test]
    fn stop_allows_sigterm_handler_before_forced_cleanup() {
        let backend = NativeProcessBackend::default();
        let handle = backend
            .start_session(exec_request(
                "trap 'echo terminated; exit 0' TERM; echo ready; while :; do sleep 1; done",
                None,
            ))
            .unwrap();
        wait_until(|| {
            backend
                .read_session(SessionReadRequest {
                    handle_id: handle.handle_id,
                    tail_lines: 10,
                })
                .unwrap()
                .output
                .output
                .contains("ready")
        });
        let stopped = backend.stop_session(handle.handle_id).unwrap();
        assert!(
            stopped.output.output.contains("terminated"),
            "SIGTERM handler never ran: {:?}",
            stopped.output
        );
    }

    #[cfg(unix)]
    #[test]
    fn final_shared_owner_drop_reaps_process() {
        let backend = NativeProcessBackend::default();
        let id = backend
            .start_session(exec_request("sleep 30", None))
            .unwrap()
            .handle_id;
        let child = backend.processes.lock().unwrap()[&id].clone();
        drop(backend);
        let child = child.lock().unwrap();
        assert_eq!(
            observe_child_exit(&child).unwrap_err().raw_os_error(),
            Some(libc::ECHILD)
        );
    }

    #[cfg(unix)]
    #[test]
    fn stop_forces_term_ignoring_process_within_bound() {
        let backend = NativeProcessBackend::default();
        let handle = backend
            .start_session(exec_request(
                "trap '' TERM; echo ready; while :; do sleep 1; done",
                None,
            ))
            .unwrap();
        wait_until(|| {
            backend
                .output_snapshot(handle.handle_id, 10)
                .unwrap()
                .output
                .contains("ready")
        });
        let started = Instant::now();
        backend.stop_session(handle.handle_id).unwrap();
        assert!(started.elapsed() >= Duration::from_secs(1));
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(backend.processes.lock().unwrap().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn shutdown_uses_one_grace_period_for_all_sessions() {
        let backend = NativeProcessBackend::default();
        for _ in 0..6 {
            let handle = backend
                .start_session(exec_request(
                    "trap '' TERM; echo ready; while :; do sleep 1; done",
                    None,
                ))
                .unwrap();
            wait_until(|| {
                backend
                    .output_snapshot(handle.handle_id, 10)
                    .unwrap()
                    .output
                    .contains("ready")
            });
        }
        let started = Instant::now();
        backend.shutdown().unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "shutdown serialized grace periods: {:?}",
            started.elapsed()
        );
        assert!(backend.processes.lock().unwrap().is_empty());
        assert!(backend.list_active_sessions().unwrap().is_empty());
    }

    #[test]
    fn shutdown_is_shared_idempotent_and_reaps_sessions() {
        let backend = NativeProcessBackend::default();
        let clone = backend.clone();
        backend
            .start_session(exec_request("sleep 30", None))
            .unwrap();
        clone.shutdown().unwrap();
        backend.shutdown().unwrap();
        assert!(backend.processes.lock().unwrap().is_empty());
        assert!(backend.list_active_sessions().unwrap().is_empty());
        assert!(backend.start_session(exec_request("true", None)).is_err());
        assert!(clone.exec_blocking(exec_request("true", None)).is_err());
    }

    #[test]
    fn dropping_clone_preserves_live_session() {
        let backend = NativeProcessBackend::default();
        let handle = backend
            .start_session(exec_request("sleep 30", None))
            .unwrap();
        drop(backend.clone());
        thread::sleep(Duration::from_millis(50));
        let still_running = backend
            .list_active_sessions()
            .unwrap()
            .iter()
            .any(|session| session.handle_id == handle.handle_id);
        let _ = backend.stop_session(handle.handle_id);
        assert!(
            still_running,
            "dropping an unrelated clone killed a live session"
        );
    }

    #[cfg(unix)]
    #[test]
    fn completed_exec_kills_nohup_descendants_on_success_and_error() {
        for exit_code in [0, 7] {
            let backend = NativeProcessBackend::default();
            let handle = backend
                .exec_blocking(exec_request(
                    &format!("nohup sleep 30 >/dev/null 2>&1 & echo $!; exit {exit_code}"),
                    None,
                ))
                .unwrap();
            assert_eq!(handle.exit_code, Some(exit_code));
            let pid = handle.output.output.trim();
            thread::sleep(Duration::from_millis(100));
            let survived = pid_alive(pid);
            if survived {
                let _ = StdCommand::new("kill").arg(pid).status();
            }
            assert!(!survived, "completed exec left nohup descendant {pid}");
        }
    }

    #[test]
    fn running_exec_is_registered_and_task_cleanup_stops_it() {
        let backend = NativeProcessBackend::default();
        let request = exec_request("sleep 30", None);
        let task_id = request.owner_task_id;
        let cancel = tokio_util::sync::CancellationToken::new();
        let worker_backend = backend.clone();
        let worker_cancel = cancel.clone();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let worker = thread::spawn(move || {
            let result = worker_backend.exec_with_cancel(request, worker_cancel);
            let _ = done_tx.send(());
            result
        });
        let deadline = Instant::now() + Duration::from_millis(500);
        let registered = loop {
            if !backend.list_task_sessions(task_id).unwrap().is_empty() {
                break true;
            }
            if Instant::now() >= deadline {
                break false;
            }
            thread::sleep(Duration::from_millis(5));
        };
        let stopped = backend.stop_task_sessions(task_id).unwrap();
        let finished_by_owner_cleanup = done_rx.recv_timeout(Duration::from_secs(1)).is_ok();
        cancel.cancel();
        let _ = worker.join().unwrap();
        assert!(
            finished_by_owner_cleanup,
            "owner cleanup failed to stop exec without cancellation fallback"
        );
        assert!(registered, "running exec was invisible to task cleanup");
        assert_eq!(stopped.len(), 1);
        assert!(backend.processes.lock().unwrap().is_empty());
        assert!(backend.list_task_sessions(task_id).unwrap().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn cancelled_exec_kills_descendants_and_releases_owned_process() {
        let backend = NativeProcessBackend::default();
        let path = std::env::temp_dir().join(format!("harness-exec-{}.pid", uuid::Uuid::new_v4()));
        let request = exec_request(
            &format!(
                "nohup sleep 30 >/dev/null 2>&1 & echo $! > '{}'; wait",
                path.display()
            ),
            None,
        );
        let cancel = tokio_util::sync::CancellationToken::new();
        let worker_backend = backend.clone();
        let worker_cancel = cancel.clone();
        let worker = thread::spawn(move || worker_backend.exec_with_cancel(request, worker_cancel));
        wait_until(|| {
            std::fs::read_to_string(&path).is_ok_and(|pid| pid.trim().parse::<u32>().is_ok())
        });
        let pid = std::fs::read_to_string(&path).unwrap();
        cancel.cancel();
        assert_eq!(worker.join().unwrap().unwrap_err(), "cancelled");
        wait_until(|| !pid_alive(pid.trim()));
        std::fs::remove_file(path).unwrap();
        assert!(backend.processes.lock().unwrap().is_empty());
        assert!(backend.list_active_sessions().unwrap().is_empty());
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

    /// 首次读到会话已结束时，快照已包含进程退出前写出的全部输出。
    #[test]
    fn finished_session_snapshot_includes_trailing_output() {
        for _ in 0..20 {
            let backend = NativeProcessBackend::default();
            let id = backend
                .start_session(exec_request("seq 1 20000", None))
                .unwrap()
                .handle_id;
            let mut last = None;
            wait_until(|| {
                let summary = backend
                    .read_session(SessionReadRequest {
                        handle_id: id,
                        tail_lines: 1,
                    })
                    .unwrap();
                let done = summary.finished_at.is_some();
                last = Some(summary);
                done
            });
            assert_eq!(last.unwrap().output.output, "20000");
        }
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
