//! Unix 实例级进程服务。控制 RPC 与专用 lifeline 分离；仅服务进程创建实际命令。
//!
//! 单次 RPC 变慢只让该次调用失败；只有连接损坏、服务退出或长时间无任何回应才关停该代服务，
//! 关停会回收该代全部受管进程，随后的调用按间隔重启新一代服务，旧会话记为丢失。

use std::{
    collections::{HashMap, HashSet},
    io::{self, Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::{net::UnixStream, process::CommandExt},
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::{
    NativeProcessBackend,
    audit::ProcessLog,
    native::{LostSessions, is_active},
};
use crate::domain::{
    SessionBackend, SessionHandle, SessionHandleId, SessionInputRequest, SessionReadRequest,
    SessionStartRequest, SessionSummary, TaskId,
};
use tracing::{error, info, warn};

pub const INTERNAL_FLAG: &str = "--internal-process-supervisor";
const LOG_FLAG: &str = "--process-log";
const RPC_FD: i32 = 198;
const LIFELINE_FD: i32 = 199;
const MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;
const MAX_REQUESTS: usize = 64;
const IO_TIMEOUT: Duration = Duration::from_secs(2);
const RPC_COMPLETION_TIMEOUT: Duration = Duration::from_secs(2);
/// 停止需要 TERM 宽限、强杀与输出 drain，给足完成时间，避免清理已成功却被报告为失败。
const STOP_COMPLETION_TIMEOUT: Duration = Duration::from_secs(5);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(4);
/// 有未回应请求且持续这么久收不到任何帧，才判定服务卡死；瞬时系统停顿不应杀掉全部进程。
const HUNG_AFTER: Duration = Duration::from_secs(30);
/// 服务端回复写入期限；宿主存活由 lifeline 判定，宿主短暂停顿不应让服务端断开连接。
const RESPONSE_WRITE_TIMEOUT: Duration = Duration::from_secs(30);
/// 相邻两次重启尝试的最小间隔，防止服务反复崩溃时形成重启风暴。
const RESTART_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Serialize, Deserialize)]
pub(super) enum Operation {
    Exec(SessionStartRequest),
    Start(SessionStartRequest),
    Read(SessionReadRequest),
    List,
    Input(SessionInputRequest),
    Stop(SessionHandleId),
    ListTask(TaskId),
    AssertOwner(TaskId, SessionHandleId),
    StopTask(TaskId),
    PollOutput,
    AckOutput(SessionHandleId, u64),
    Cancel(u64),
}

impl Operation {
    fn label(&self) -> &'static str {
        match self {
            Self::Exec(_) => "exec",
            Self::Start(_) => "start",
            Self::Read(_) => "read",
            Self::List => "list",
            Self::Input(_) => "input",
            Self::Stop(_) => "stop",
            Self::ListTask(_) => "list_task",
            Self::AssertOwner(..) => "assert_owner",
            Self::StopTask(_) => "stop_task",
            Self::PollOutput => "poll_output",
            Self::AckOutput(..) => "ack_output",
            Self::Cancel(_) => "cancel",
        }
    }

    /// 长 exec 的完成由业务 timeout / cancel 决定；其他 RPC 有覆盖排队、发送和接收的期限。
    fn completion_timeout(&self) -> Option<Duration> {
        match self {
            Self::Exec(_) => None,
            Self::Stop(_) | Self::StopTask(_) => Some(STOP_COMPLETION_TIMEOUT),
            _ => Some(RPC_COMPLETION_TIMEOUT),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Request {
    id: u64,
    operation: Operation,
}

#[derive(Serialize, Deserialize)]
struct Response {
    id: u64,
    result: Result<Value, String>,
}

type Reply = Result<Value, String>;

/// `Request` 只影响本次调用，服务继续可用；`Fatal` 表示该代连接已关停。
enum CallError {
    Request(String),
    Fatal(String),
}

enum SendError {
    /// 一个字节都未写出，帧流仍然完整。
    NotSent(String),
    /// 连接断开或写出半帧，帧流不可再复用。
    Broken(String),
}

struct ClientState {
    failed: AtomicBool,
    closing: AtomicBool,
    pending: Mutex<HashMap<u64, mpsc::SyncSender<Reply>>>,
    /// 首个导致失效的原因，供重启日志与丢失会话说明使用。
    failure: Mutex<Option<String>>,
    /// 自某个请求超时以来一直没有收到任何帧的起点；收到任意帧即清除。
    stalled_since: Mutex<Option<Instant>>,
}

impl ClientState {
    fn new() -> Self {
        Self {
            failed: AtomicBool::new(false),
            closing: AtomicBool::new(false),
            pending: Mutex::new(HashMap::new()),
            failure: Mutex::new(None),
            stalled_since: Mutex::new(None),
        }
    }

    fn fail(&self, error: &str) {
        if let Ok(mut failure) = self.failure.lock() {
            failure.get_or_insert_with(|| error.to_string());
        }
        self.failed.store(true, Ordering::SeqCst);
        if let Ok(mut pending) = self.pending.lock() {
            for (_, sender) in pending.drain() {
                let _ = sender.send(Err(error.to_string()));
            }
        }
    }

    fn failure(&self) -> String {
        self.failure
            .lock()
            .ok()
            .and_then(|failure| failure.clone())
            .unwrap_or_else(|| "process supervisor connection closed".to_string())
    }

    fn note_frame(&self) {
        if let Ok(mut stalled) = self.stalled_since.lock() {
            *stalled = None;
        }
    }

    /// 记录一次未获回应的请求；返回是否已持续 `HUNG_AFTER` 没有任何帧。
    fn note_timeout(&self, waiting_since: Instant, now: Instant) -> bool {
        let Ok(mut stalled) = self.stalled_since.lock() else {
            return true;
        };
        let since = *stalled.get_or_insert(waiting_since);
        now.saturating_duration_since(since) >= HUNG_AFTER
    }

    fn forget(&self, id: u64) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.remove(&id);
        }
    }
}

/// 单代服务连接：一个服务进程、一条 RPC 与一条 lifeline。
struct Connection {
    writer: Mutex<UnixStream>,
    // 无 reader / worker 线程持有此端点；主运行时可不等待 clone 释放而显式关闭。
    lifeline: Mutex<Option<UnixStream>>,
    child: Mutex<Child>,
    pid: u32,
    state: Arc<ClientState>,
    next_id: AtomicU64,
    shutdown: Mutex<Option<Result<(), String>>>,
    log: Option<ProcessLog>,
}

impl Connection {
    fn launch(
        executable: &Path,
        log_path: Option<&Path>,
        log: Option<ProcessLog>,
    ) -> Result<Self, String> {
        let (rpc, service_rpc) = UnixStream::pair().map_err(error_text)?;
        let (lifeline, service_lifeline) = UnixStream::pair().map_err(error_text)?;
        // 先复制到固定目标 fd 以上，避免 dup2 时覆盖尚未复制的来源。
        let rpc_source = duplicate_high(&service_rpc)?;
        let life_source = duplicate_high(&service_lifeline)?;
        let rpc_fd = rpc_source.as_raw_fd();
        let life_fd = life_source.as_raw_fd();
        let mut command = Command::new(executable);
        command
            .arg(INTERNAL_FLAG)
            .stdin(Stdio::inherit())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0);
        if let Some(path) = log_path {
            command.arg(LOG_FLAG).arg(path);
        }
        // SAFETY: pre_exec 内只调用 async-signal-safe 的 dup2；源 fd 由上方 OwnedFd 保活。
        unsafe {
            command.pre_exec(move || {
                if libc::dup2(rpc_fd, RPC_FD) == -1 || libc::dup2(life_fd, LIFELINE_FD) == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        rpc.set_read_timeout(Some(Duration::from_secs(3)))
            .map_err(error_text)?;
        rpc.set_write_timeout(Some(IO_TIMEOUT))
            .map_err(error_text)?;
        let mut reader = rpc.try_clone().map_err(error_text)?;
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                if let Some(log) = &log {
                    log.event("ProcessSupervisorStartupFailed", serde_json::json!({
                    "error_type": "Spawn", "error": error.to_string(), "message": "process service could not be started",
                }));
                }
                return Err(error.to_string());
            }
        };
        drop((service_rpc, service_lifeline, rpc_source, life_source));
        let handshake: Result<Response, String> = read_frame(&mut reader);
        if !matches!(handshake, Ok(Response { id: 0, result: Ok(ref value) }) if value["protocol"] == 1)
        {
            if let Some(log) = &log {
                log.event("ProcessSupervisorStartupFailed", serde_json::json!({
                "supervisor_pid": child.id(), "error_type": "Handshake", "error": "startup handshake failed",
                "message": "process service startup was not confirmed",
            }));
            }
            drop(lifeline);
            let _ = child.kill();
            let _ = child.wait();
            return Err("process supervisor startup handshake failed".to_string());
        }
        if let Err(error) = rpc.set_read_timeout(None) {
            drop(lifeline);
            let _ = child.kill();
            let _ = child.wait();
            return Err(error.to_string());
        }
        let state = Arc::new(ClientState::new());
        let reader_state = Arc::clone(&state);
        let reader_log = log.clone();
        let supervisor_pid = child.id();
        thread::spawn(move || {
            loop {
                let response: Response = match read_frame(&mut reader) {
                    Ok(response) => response,
                    Err(error) => {
                        if !reader_state.closing.load(Ordering::SeqCst) {
                            if let Some(log) = &reader_log {
                                log.event("ProcessSupervisorAbnormalExit", serde_json::json!({
                                "supervisor_pid": supervisor_pid, "error_type": "ConnectionLost",
                                "error": "RPC connection ended unexpectedly", "message": "process service stopped responding",
                            }));
                            }
                            error!(
                                event = "ProcessSupervisorAbnormalExit",
                                supervisor_pid,
                                error_type = "ConnectionLost",
                                error = "RPC connection ended unexpectedly",
                                "process service stopped responding"
                            );
                        }
                        reader_state.fail(&format!("process supervisor unavailable: {error}"));
                        break;
                    }
                };
                // 任何帧（包括调用方已放弃的迟到回复）都证明服务仍在运转。
                reader_state.note_frame();
                let sender = reader_state
                    .pending
                    .lock()
                    .ok()
                    .and_then(|mut pending| pending.remove(&response.id));
                if let Some(sender) = sender {
                    let _ = sender.send(response.result);
                }
            }
        });
        Ok(Self {
            writer: Mutex::new(rpc),
            lifeline: Mutex::new(Some(lifeline)),
            child: Mutex::new(child),
            pid: supervisor_pid,
            state,
            next_id: AtomicU64::new(1),
            shutdown: Mutex::new(None),
            log,
        })
    }

    fn usable(&self) -> bool {
        !self.state.failed.load(Ordering::SeqCst) && !self.state.closing.load(Ordering::SeqCst)
    }

    fn call(
        &self,
        operation: Operation,
        cancel: Option<CancellationToken>,
    ) -> Result<Value, CallError> {
        if !self.usable() {
            return Err(CallError::Fatal(
                "process supervisor is unavailable or shutting down".to_string(),
            ));
        }
        let label = operation.label();
        let mut waiting_since = Instant::now();
        let mut deadline = operation
            .completion_timeout()
            .map(|timeout| waiting_since + timeout);
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = mpsc::sync_channel(1);
        {
            let mut pending = self.state.pending.lock().map_err(|_| {
                CallError::Fatal(self.fail_closed("supervisor pending map poisoned"))
            })?;
            if pending.len() >= MAX_REQUESTS {
                return Err(CallError::Request(
                    "process supervisor request capacity exceeded".to_string(),
                ));
            }
            pending.insert(id, sender);
        }
        self.send(
            id,
            &Request { id, operation },
            deadline.unwrap_or_else(|| Instant::now() + IO_TIMEOUT),
        )?;
        let mut cancelled = false;
        loop {
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                // 宿主线程自身被挂起时回复可能早已到达；先取走再判定超时。
                if let Ok(reply) = receiver.try_recv() {
                    return self.reply(reply);
                }
                self.state.forget(id);
                if self.state.note_timeout(waiting_since, Instant::now()) {
                    return Err(CallError::Fatal(self.fail_closed(&format!(
                        "process supervisor sent no response for {}s",
                        HUNG_AFTER.as_secs()
                    ))));
                }
                return Err(CallError::Request(format!(
                    "process supervisor {label} RPC timed out; request abandoned with unknown outcome, service kept running"
                )));
            }
            if !cancelled && cancel.as_ref().is_some_and(CancellationToken::is_cancelled) {
                waiting_since = Instant::now();
                let cancel_deadline = waiting_since + RPC_COMPLETION_TIMEOUT;
                self.send(
                    id,
                    &Request {
                        id: 0,
                        operation: Operation::Cancel(id),
                    },
                    cancel_deadline,
                )?;
                deadline = Some(cancel_deadline);
                cancelled = true;
            }
            match receiver.recv_timeout(Duration::from_millis(10)) {
                Ok(reply) => return self.reply(reply),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(CallError::Fatal(
                        self.fail_closed("process supervisor response channel closed"),
                    ));
                }
            }
        }
    }

    fn reply(&self, reply: Reply) -> Result<Value, CallError> {
        match reply {
            Ok(value) => Ok(value),
            Err(error) if self.state.failed.load(Ordering::SeqCst) => {
                Err(CallError::Fatal(self.fail_closed(&error)))
            }
            Err(error) => Err(CallError::Request(error)),
        }
    }

    fn fail_closed(&self, error: &str) -> String {
        self.state.fail(error);
        match self.shutdown() {
            Ok(()) => format!("{error}; all instance processes stopped"),
            Err(cleanup) => format!(
                "{error}; all instance processes stopped accepting requests, cleanup not confirmed: {cleanup}"
            ),
        }
    }

    fn send(&self, id: u64, request: &Request, deadline: Instant) -> Result<(), CallError> {
        let attempt = (|| {
            let bytes = encode_frame(request).map_err(SendError::NotSent)?;
            let mut frame = Vec::with_capacity(bytes.len() + 4);
            frame.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
            frame.extend_from_slice(&bytes);
            // 不能在 writer Mutex::lock 上无限排队；绝对期限也覆盖部分写入的多次系统调用。
            let mut writer =
                writer_before_deadline(&self.writer, deadline).map_err(SendError::NotSent)?;
            write_before_deadline(&mut writer, &frame, deadline).map_err(|failure| {
                if failure.written == 0 && failure.timed_out {
                    SendError::NotSent(failure.error)
                } else {
                    SendError::Broken(failure.error)
                }
            })
        })();
        match attempt {
            Ok(()) => Ok(()),
            Err(SendError::NotSent(error)) => {
                self.state.forget(id);
                Err(CallError::Request(error))
            }
            Err(SendError::Broken(error)) => Err(CallError::Fatal(self.fail_closed(&error))),
        }
    }

    fn close_lifeline(&self) {
        if let Ok(mut lifeline) = self.lifeline.lock() {
            lifeline.take();
        }
    }

    fn shutdown(&self) -> Result<(), String> {
        let mut result = self
            .shutdown
            .lock()
            .map_err(|_| "supervisor shutdown mutex poisoned")?;
        if let Some(result) = result.as_ref() {
            return result.clone();
        }
        self.state.closing.store(true, Ordering::SeqCst);
        // EOF 是权威关闭信号；所有关闭路径复用同一批量回收，不等待 RPC 队列背压。
        self.close_lifeline();
        let deadline = Instant::now() + SHUTDOWN_TIMEOUT;
        let mut child = self
            .child
            .lock()
            .map_err(|_| "supervisor child mutex poisoned")?;
        let outcome = loop {
            match child.try_wait() {
                Ok(Some(status)) if status.success() => break Ok(()),
                Ok(Some(status)) => {
                    if let Some(log) = &self.log {
                        log.event("ProcessSupervisorAbnormalExit", serde_json::json!({
                        "supervisor_pid": child.id(), "error_type": "ExitStatus", "error": status.to_string(),
                        "message": "process service exited unsuccessfully",
                    }));
                    }
                    break Err(format!("process supervisor exited with {status}"));
                }
                Err(error) => break Err(error.to_string()),
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
                Ok(None) => {
                    break Err("process supervisor shutdown exceeded four seconds".to_string());
                }
            }
        };
        *result = Some(outcome.clone());
        outcome
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

struct Slot {
    connection: Option<Arc<Connection>>,
    generation: u64,
    /// 最近一次重启尝试；首次重启立即进行，之后按间隔限流。
    restarted_at: Option<Instant>,
    closed: bool,
    shutdown: Option<Result<(), String>>,
    /// 当前代启动且尚未观察到结束的会话；该代失效时整体转入丢失记录。
    sessions: HashSet<SessionHandleId>,
}

/// 宿主侧进程服务句柄：转发 RPC，在服务失效后按间隔重启新一代服务。
pub(super) struct SupervisorClient {
    executable: PathBuf,
    log_path: Option<PathBuf>,
    log: Option<ProcessLog>,
    slot: Mutex<Slot>,
    lost: Arc<Mutex<LostSessions>>,
}

/// 调用成功后需要同步到会话登记的信息；`Operation` 发送时被移走，故先行提取。
enum Observation {
    None,
    Started,
    Read(SessionHandleId),
    Stopped(SessionHandleId),
    StoppedTask,
}

impl SupervisorClient {
    pub(super) fn launch(
        executable: &Path,
        log_path: Option<&Path>,
        lost: Arc<Mutex<LostSessions>>,
    ) -> Result<Self, String> {
        let log = log_path
            .map(ProcessLog::open)
            .transpose()
            .map_err(error_text)?;
        let connection = Connection::launch(executable, log_path, log.clone())?;
        Ok(Self {
            executable: executable.to_path_buf(),
            log_path: log_path.map(Path::to_path_buf),
            log,
            slot: Mutex::new(Slot {
                connection: Some(Arc::new(connection)),
                generation: 1,
                restarted_at: None,
                closed: false,
                shutdown: None,
                sessions: HashSet::new(),
            }),
            lost,
        })
    }

    pub(super) fn call<T: DeserializeOwned>(
        &self,
        operation: Operation,
        cancel: Option<CancellationToken>,
    ) -> Result<T, String> {
        let label = operation.label();
        let observation = match &operation {
            Operation::Start(_) => Observation::Started,
            Operation::Read(request) => Observation::Read(request.handle_id),
            Operation::Stop(id) => Observation::Stopped(*id),
            Operation::StopTask(_) => Observation::StoppedTask,
            _ => Observation::None,
        };
        let (connection, generation) = self.connection()?;
        match connection.call(operation, cancel) {
            Ok(value) => {
                self.observe(observation, &value, generation);
                serde_json::from_value(value)
                    .map_err(|error| format!("invalid process supervisor response: {error}"))
            }
            Err(CallError::Request(error)) => Err(error),
            Err(CallError::Fatal(error)) => {
                if let Ok(mut slot) = self.slot.lock() {
                    self.retire(&mut slot, &connection, label);
                }
                Err(error)
            }
        }
    }

    /// 返回可用的当前代连接；当前代已失效时记录并按间隔重启。
    fn connection(&self) -> Result<(Arc<Connection>, u64), String> {
        let mut slot = self
            .slot
            .lock()
            .map_err(|_| "supervisor slot mutex poisoned")?;
        if slot.closed {
            return Err("process supervisor is shut down".to_string());
        }
        if let Some(connection) = slot.connection.clone() {
            if connection.usable() {
                return Ok((connection, slot.generation));
            }
            // 读取线程发现连接断开时没有调用方在场；在这里补做关停与登记。
            let _ = connection.shutdown();
            self.retire(&mut slot, &connection, "connection_lost");
        }
        if let Some(wait) = slot
            .restarted_at
            .and_then(|at| RESTART_INTERVAL.checked_sub(at.elapsed()))
        {
            return Err(format!(
                "process supervisor failed and is restarting; retry after {}ms",
                wait.as_millis()
            ));
        }
        slot.restarted_at = Some(Instant::now());
        let generation = slot.generation + 1;
        match Connection::launch(&self.executable, self.log_path.as_deref(), self.log.clone()) {
            Ok(connection) => {
                let connection = Arc::new(connection);
                warn!(
                    event = "ProcessSupervisorRestarted",
                    generation,
                    supervisor_pid = connection.pid,
                    "process service restarted after failure"
                );
                self.audit(
                    "ProcessSupervisorRestarted",
                    serde_json::json!({
                        "generation": generation, "supervisor_pid": connection.pid,
                        "message": "process service restarted after failure",
                    }),
                );
                slot.generation = generation;
                slot.connection = Some(Arc::clone(&connection));
                Ok((connection, generation))
            }
            Err(error) => {
                error!(event = "ProcessSupervisorRestartFailed", generation, error_type = "Spawn",
                    error = %error, "process service restart failed");
                self.audit(
                    "ProcessSupervisorRestartFailed",
                    serde_json::json!({
                        "generation": generation, "error_type": "Spawn", "error": error,
                        "message": "process service restart failed",
                    }),
                );
                Err(format!("process supervisor restart failed: {error}"))
            }
        }
    }

    /// 让已关停的一代退出服务：记录原因并把其会话转为丢失。对同一代幂等。
    fn retire(&self, slot: &mut Slot, connection: &Arc<Connection>, operation: &str) {
        if !slot
            .connection
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, connection))
        {
            return;
        }
        slot.connection = None;
        let reason = connection.state.failure();
        let lost_sessions = slot.sessions.len();
        error!(
            event = "ProcessSupervisorFailed",
            generation = slot.generation,
            supervisor_pid = connection.pid,
            operation,
            lost_sessions,
            error_type = "ProcessBackend",
            error = %reason,
            "process service failed; its managed processes were stopped and it will be restarted"
        );
        self.audit(
            "ProcessSupervisorFailed",
            serde_json::json!({
                "generation": slot.generation, "supervisor_pid": connection.pid,
                "operation": operation, "lost_sessions": lost_sessions,
                "error_type": "ProcessBackend", "error": reason,
                "message": "process service failed; its managed processes were stopped",
            }),
        );
        if let Ok(mut lost) = self.lost.lock() {
            for id in slot.sessions.drain() {
                lost.insert(id, reason.clone());
            }
        }
    }

    fn observe(&self, observation: Observation, value: &Value, generation: u64) {
        let Ok(mut slot) = self.slot.lock() else {
            return;
        };
        match observation {
            Observation::None => {}
            Observation::Started => {
                let Ok(handle) = serde_json::from_value::<SessionHandle>(value.clone()) else {
                    return;
                };
                if slot.generation == generation && slot.connection.is_some() {
                    slot.sessions.insert(handle.handle_id);
                } else if let Ok(mut lost) = self.lost.lock() {
                    // 启动回复到达前该代已失效：会话已随该代被回收。
                    lost.insert(
                        handle.handle_id,
                        "process supervisor failed right after start".to_string(),
                    );
                }
            }
            Observation::Read(id) => {
                if serde_json::from_value::<SessionSummary>(value.clone())
                    .is_ok_and(|summary| !is_active(summary.status))
                {
                    slot.sessions.remove(&id);
                }
            }
            Observation::Stopped(id) => {
                slot.sessions.remove(&id);
            }
            Observation::StoppedTask => {
                if let Ok(ids) = serde_json::from_value::<Vec<SessionHandleId>>(value.clone()) {
                    for id in ids {
                        slot.sessions.remove(&id);
                    }
                }
            }
        }
    }

    fn audit(&self, event: &str, fields: Value) {
        if let Some(log) = &self.log {
            log.event(event, fields);
        }
    }

    pub(super) fn shutdown(&self) -> Result<(), String> {
        let mut slot = self
            .slot
            .lock()
            .map_err(|_| "supervisor slot mutex poisoned")?;
        if let Some(result) = slot.shutdown.as_ref() {
            return result.clone();
        }
        slot.closed = true;
        let outcome = slot
            .connection
            .take()
            .map_or(Ok(()), |connection| connection.shutdown());
        slot.shutdown = Some(outcome.clone());
        outcome
    }
}

impl Drop for SupervisorClient {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

fn writer_before_deadline(
    writer: &Mutex<UnixStream>,
    deadline: Instant,
) -> Result<std::sync::MutexGuard<'_, UnixStream>, String> {
    loop {
        if Instant::now() >= deadline {
            return Err("process supervisor RPC send timed out".to_string());
        }
        match writer.try_lock() {
            Ok(writer) => return Ok(writer),
            Err(std::sync::TryLockError::WouldBlock) => thread::sleep(Duration::from_millis(5)),
            Err(std::sync::TryLockError::Poisoned(_)) => {
                return Err("supervisor writer poisoned".to_string());
            }
        }
    }
}

struct WriteFailure {
    written: usize,
    timed_out: bool,
    error: String,
}

fn write_before_deadline(
    stream: &mut UnixStream,
    bytes: &[u8],
    deadline: Instant,
) -> Result<(), WriteFailure> {
    let mut written = 0;
    let fail = |written, timed_out, error: String| WriteFailure {
        written,
        timed_out,
        error,
    };
    while written < bytes.len() {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(|| {
                fail(
                    written,
                    true,
                    "process supervisor RPC send timed out".into(),
                )
            })?;
        stream
            .set_write_timeout(Some(remaining))
            .map_err(|error| fail(written, false, error.to_string()))?;
        match stream.write(&bytes[written..]) {
            Ok(0) => {
                return Err(fail(
                    written,
                    false,
                    "process supervisor connection closed during send".into(),
                ));
            }
            Ok(count) => written += count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                ) =>
            {
                return Err(fail(
                    written,
                    true,
                    "process supervisor RPC send timed out".into(),
                ));
            }
            Err(error) => return Err(fail(written, false, error.to_string())),
        }
    }
    Ok(())
}

fn duplicate_high(stream: &UnixStream) -> Result<OwnedFd, String> {
    // SAFETY: fcntl 返回全新拥有的描述符，OwnedFd 恰好回收一次。
    let fd = unsafe { libc::fcntl(stream.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 200) };
    if fd == -1 {
        return Err(io::Error::last_os_error().to_string());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// 只供 main 在任何 dotenv / provider / 日志 / TUI 初始化之前进入。
pub fn run_internal() -> Result<(), String> {
    for fd in [RPC_FD, LIFELINE_FD] {
        // SAFETY: 入口只接受启动握手传入的私有 fd，立即标为 CLOEXEC，命令不能继承。
        if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } == -1 {
            return Err("process supervisor requires private launch descriptors".to_string());
        }
    }
    // SAFETY: 这两个 fd 由启动器独占转交，尚未被其他 Rust 对象拥有。
    let mut rpc = unsafe { UnixStream::from_raw_fd(RPC_FD) };
    let lifeline = unsafe { UnixStream::from_raw_fd(LIFELINE_FD) };
    let arguments: Vec<_> = std::env::args_os().skip(2).collect();
    match arguments.as_slice() {
        [] => {}
        [flag, path] if flag == LOG_FLAG => {
            let log = ProcessLog::open(Path::new(path)).map_err(error_text)?;
            tracing_subscriber::fmt()
                .json()
                .with_ansi(false)
                .with_max_level(tracing::Level::DEBUG)
                .with_writer(log)
                .try_init()
                .map_err(error_text)?;
        }
        _ => return Err("invalid process supervisor launch arguments".to_string()),
    }
    info!(
        event = "ProcessSupervisorStarted",
        supervisor_pid = std::process::id(),
        host_pid = unsafe { libc::getppid() },
        "process service started"
    );
    rpc.set_write_timeout(Some(RESPONSE_WRITE_TIMEOUT))
        .map_err(error_text)?;
    write_frame(
        &mut rpc,
        &Response {
            id: 0,
            result: Ok(serde_json::json!({"protocol": 1})),
        },
    )?;
    let mut reader = rpc.try_clone().map_err(error_text)?;
    let writer = Arc::new(Mutex::new(rpc));
    let (tx, rx) = mpsc::sync_channel(MAX_REQUESTS);
    thread::spawn(move || {
        while let Ok(request) = read_frame::<Request>(&mut reader) {
            if tx.send(request).is_err() {
                break;
            }
        }
    });
    let backend = NativeProcessBackend::default();
    let active = Arc::new(AtomicUsize::new(0));
    let tokens = Arc::new(Mutex::new(HashMap::<u64, CancellationToken>::new()));
    let reason = loop {
        if lifeline_closed(&lifeline) {
            break "lifeline_eof";
        }
        let request = match rx.recv_timeout(Duration::from_millis(10)) {
            Ok(request) => request,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break "rpc_disconnected",
        };
        // 先检查专用存活连接；EOF 后丢弃解码器队列内尚未执行的请求。
        if lifeline_closed(&lifeline) {
            break "lifeline_eof";
        }
        match request.operation {
            Operation::Cancel(id) => {
                if let Some(token) = tokens
                    .lock()
                    .map_err(|_| "supervisor cancel map poisoned")?
                    .get(&id)
                {
                    token.cancel();
                }
            }
            operation => {
                if active.load(Ordering::SeqCst) >= MAX_REQUESTS {
                    // 控制循环不能等待响应写入，否则背压会阻塞 lifeline；宿主最多 64 个待回应请求。
                    let rejection_writer = Arc::clone(&writer);
                    thread::spawn(move || {
                        send_response(
                            &rejection_writer,
                            request.id,
                            Err("process supervisor worker capacity exceeded".to_string()),
                        );
                    });
                    continue;
                }
                active.fetch_add(1, Ordering::SeqCst);
                let token = CancellationToken::new();
                tokens
                    .lock()
                    .map_err(|_| "supervisor cancel map poisoned")?
                    .insert(request.id, token.clone());
                let worker_backend = backend.clone();
                let worker_writer = Arc::clone(&writer);
                let worker_active = Arc::clone(&active);
                let worker_tokens = Arc::clone(&tokens);
                thread::spawn(move || {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        execute(&worker_backend, operation, token)
                    }))
                    .unwrap_or_else(|_| Err("process supervisor worker panicked".to_string()));
                    send_response(&worker_writer, request.id, result);
                    if let Ok(mut tokens) = worker_tokens.lock() {
                        tokens.remove(&request.id);
                    }
                    worker_active.fetch_sub(1, Ordering::SeqCst);
                });
            }
        }
    };
    // 宿主死亡可能先唤醒 RPC 解码器；在归类关闭原因前再次观察专用 lifeline。
    let reason = if lifeline_closed(&lifeline) {
        "lifeline_eof"
    } else {
        reason
    };
    if reason == "lifeline_eof" {
        info!(
            event = "ProcessLifelineClosed",
            supervisor_pid = std::process::id(),
            "host lifeline reached EOF"
        );
    } else {
        warn!(
            event = "ProcessSupervisorServiceFault",
            supervisor_pid = std::process::id(),
            error_type = "Protocol",
            error = reason,
            "process service is closing after a control fault"
        );
    }
    // 先封闭 backend 的 spawn gate，再由其批量 TERM/KILL/reap；长 exec 不占该事件循环。
    if let Err(error) = backend.shutdown() {
        error!(event = "ProcessSupervisorCleanupFailed", supervisor_pid = std::process::id(),
            error_type = "Cleanup", error = %error, "process service cleanup was not confirmed");
        return Err(error);
    }
    info!(
        event = "ProcessSupervisorStopped",
        supervisor_pid = std::process::id(),
        reason,
        "all owned process groups reaped; process service stopped"
    );
    for token in tokens
        .lock()
        .map_err(|_| "supervisor cancel map poisoned")?
        .values()
    {
        token.cancel();
    }
    Ok(())
}

fn lifeline_closed(stream: &UnixStream) -> bool {
    let mut byte = 0u8;
    // SAFETY: 单字节缓冲有效；PEEK 不消费连接，DONTWAIT 不阻塞 EOF 检测。
    let result = unsafe {
        libc::recv(
            stream.as_raw_fd(),
            (&mut byte as *mut u8).cast(),
            1,
            libc::MSG_PEEK | libc::MSG_DONTWAIT,
        )
    };
    result >= 0
        || !matches!(
            io::Error::last_os_error().kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
        )
}

fn execute(
    backend: &NativeProcessBackend,
    operation: Operation,
    cancel: CancellationToken,
) -> Reply {
    fn value<T: Serialize>(result: Result<T, String>) -> Reply {
        serde_json::to_value(result?).map_err(error_text)
    }
    match operation {
        Operation::Exec(request) => value(backend.exec_with_cancel(request, cancel)),
        Operation::Start(request) => value(backend.start_session(request)),
        Operation::Read(request) => value(backend.read_session(request)),
        Operation::List => value(backend.list_active_sessions()),
        Operation::Input(request) => value(backend.input_session(request)),
        Operation::Stop(id) => value(backend.stop_session(id)),
        Operation::ListTask(id) => value(backend.list_task_sessions(id)),
        Operation::AssertOwner(task, id) => value(backend.assert_task_owns_session(task, id)),
        Operation::StopTask(id) => value(backend.stop_task_sessions(id)),
        Operation::PollOutput => value(backend.poll_output_events()),
        Operation::AckOutput(id, through) => value(backend.ack_output_events(id, through)),
        Operation::Cancel(_) => Err("invalid worker control operation".to_string()),
    }
}

fn send_response(writer: &Mutex<UnixStream>, id: u64, result: Reply) {
    let encoded = encode_frame(&Response { id, result }).or_else(|_| {
        encode_frame(&Response {
            id,
            result: Err("process supervisor response exceeds protocol limit".to_string()),
        })
    });
    if let Ok(bytes) = encoded {
        let mut writer = writer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if write_encoded(&mut *writer, &bytes).is_err() {
            // 部分写入不能继续复用帧流；关闭连接让宿主明确失败，服务端进入相同 EOF 清理。
            let _ = writer.shutdown(std::net::Shutdown::Both);
        }
    }
}

fn error_text(error: impl std::fmt::Display) -> String {
    error.to_string()
}

struct BoundedBytes(Vec<u8>);
impl Write for BoundedBytes {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_FRAME_BYTES.saturating_sub(self.0.len()) {
            return Err(io::Error::other("supervisor frame exceeds limit"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn encode_frame(value: &impl Serialize) -> Result<Vec<u8>, String> {
    let mut bytes = BoundedBytes(Vec::new());
    serde_json::to_writer(&mut bytes, value).map_err(error_text)?;
    Ok(bytes.0)
}

fn write_encoded(writer: &mut impl Write, bytes: &[u8]) -> Result<(), String> {
    writer
        .write_all(&(bytes.len() as u32).to_be_bytes())
        .map_err(error_text)?;
    writer.write_all(bytes).map_err(error_text)
}

fn write_frame(writer: &mut impl Write, value: &impl Serialize) -> Result<(), String> {
    write_encoded(writer, &encode_frame(value)?)
}

fn read_frame<T: DeserializeOwned>(reader: &mut impl Read) -> Result<T, String> {
    let mut size = [0; 4];
    reader.read_exact(&mut size).map_err(error_text)?;
    let size = u32::from_be_bytes(size) as usize;
    if size == 0 || size > MAX_FRAME_BYTES {
        return Err("supervisor frame exceeds limit".to_string());
    }
    let mut bytes = vec![0; size];
    reader.read_exact(&mut bytes).map_err(error_text)?;
    serde_json::from_slice(&bytes).map_err(error_text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_timeouts_are_tolerated_until_no_frame_arrives_for_the_hung_window() {
        let state = ClientState::new();
        let start = Instant::now();
        assert!(!state.note_timeout(start, start + RPC_COMPLETION_TIMEOUT));
        // 后续超时以最早未获回应的时刻计算，持续无帧才判定卡死。
        let later = start + Duration::from_secs(20);
        assert!(!state.note_timeout(later, later + RPC_COMPLETION_TIMEOUT));
        assert!(state.note_timeout(start + Duration::from_secs(28), start + HUNG_AFTER));
    }

    #[test]
    fn any_frame_resets_the_stall_window() {
        let state = ClientState::new();
        let start = Instant::now();
        assert!(!state.note_timeout(start, start + RPC_COMPLETION_TIMEOUT));
        state.note_frame();
        let later = start + Duration::from_secs(29);
        assert!(!state.note_timeout(later, later + RPC_COMPLETION_TIMEOUT));
    }

    #[test]
    fn first_failure_reason_is_kept_for_lost_session_reports() {
        let state = ClientState::new();
        state.fail("supervisor sent no response");
        state.fail("process supervisor unavailable: eof");
        assert_eq!(state.failure(), "supervisor sent no response");
    }

    #[test]
    fn writer_queue_has_a_deadline_even_when_another_request_holds_the_lock() {
        let (socket, _peer) = UnixStream::pair().unwrap();
        let writer = Mutex::new(socket);
        thread::scope(|scope| {
            let guard = writer.lock().unwrap();
            let (tx, rx) = mpsc::channel();
            let writer = &writer;
            let worker = scope.spawn(move || {
                let result =
                    writer_before_deadline(writer, Instant::now() + Duration::from_millis(25));
                let _ = tx.send(result.is_err());
            });
            let timed_out_while_locked = rx.recv_timeout(Duration::from_millis(200));
            drop(guard);
            worker.join().unwrap();
            assert!(timed_out_while_locked.unwrap());
        });
    }

    #[test]
    fn socket_backpressure_obeys_the_same_absolute_send_deadline() {
        let (mut writer, _peer) = UnixStream::pair().unwrap();
        let started = Instant::now();
        let failure = write_before_deadline(
            &mut writer,
            &vec![0; MAX_FRAME_BYTES],
            started + Duration::from_millis(50),
        )
        .err()
        .unwrap();
        assert!(failure.timed_out && failure.error.contains("timed out"));
        // 已写出部分字节的超时属于半帧，调用方必须按连接损坏处理。
        assert!(failure.written > 0);
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn oversized_response_returns_error_instead_of_hanging_caller() {
        let (server, mut client) = UnixStream::pair().unwrap();
        client
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let writer = Mutex::new(server);
        send_response(
            &writer,
            7,
            Ok(Value::String("x".repeat(MAX_FRAME_BYTES + 1))),
        );
        let reply: Response = read_frame(&mut client)
            .expect("oversized result must produce an explicit bounded error response");
        assert_eq!(reply.id, 7);
        assert!(reply.result.is_err());
    }

    #[test]
    fn frame_decoder_rejects_oversized_body_before_reading_or_allocating_it() {
        let mut bytes = std::io::Cursor::new(((MAX_FRAME_BYTES + 1) as u32).to_be_bytes());
        assert!(
            read_frame::<Value>(&mut bytes)
                .unwrap_err()
                .contains("limit")
        );
    }
}
