//! Unix 实例级进程服务。控制 RPC 与专用 lifeline 分离；仅服务进程创建实际命令。

use std::{
    collections::HashMap,
    io::{self, Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::{net::UnixStream, process::CommandExt},
    },
    path::Path,
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

use super::NativeProcessBackend;
use crate::domain::{
    SessionBackend, SessionHandleId, SessionInputRequest, SessionReadRequest, SessionStartRequest,
    TaskId,
};

pub const INTERNAL_FLAG: &str = "--internal-process-supervisor";
const RPC_FD: i32 = 198;
const LIFELINE_FD: i32 = 199;
const MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;
const MAX_REQUESTS: usize = 64;
const IO_TIMEOUT: Duration = Duration::from_secs(2);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(4);

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
    Cancel(u64),
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

struct ClientState {
    failed: AtomicBool,
    closing: AtomicBool,
    pending: Mutex<HashMap<u64, mpsc::SyncSender<Reply>>>,
}

impl ClientState {
    fn fail(&self, error: &str) {
        self.failed.store(true, Ordering::SeqCst);
        if let Ok(mut pending) = self.pending.lock() {
            for (_, sender) in pending.drain() {
                let _ = sender.send(Err(error.to_string()));
            }
        }
    }
}

pub(super) struct SupervisorClient {
    writer: Mutex<UnixStream>,
    // 无 reader / worker 线程持有此端点；主运行时可不等待 clone 释放而显式关闭。
    lifeline: Mutex<Option<UnixStream>>,
    child: Mutex<Child>,
    state: Arc<ClientState>,
    next_id: AtomicU64,
    shutdown: Mutex<Option<Result<(), String>>>,
}

impl SupervisorClient {
    pub(super) fn launch(executable: &Path) -> Result<Self, String> {
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
        // SAFETY: pre_exec 内只调用 async-signal-safe 的 dup2；源 fd 由上方 OwnedFd 保活。
        unsafe {
            command.pre_exec(move || {
                if libc::dup2(rpc_fd, RPC_FD) == -1 || libc::dup2(life_fd, LIFELINE_FD) == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command.spawn().map_err(error_text)?;
        drop((service_rpc, service_lifeline, rpc_source, life_source));
        rpc.set_read_timeout(Some(Duration::from_secs(3)))
            .map_err(error_text)?;
        rpc.set_write_timeout(Some(IO_TIMEOUT))
            .map_err(error_text)?;
        let mut reader = rpc.try_clone().map_err(error_text)?;
        let handshake: Result<Response, String> = read_frame(&mut reader);
        if !matches!(handshake, Ok(Response { id: 0, result: Ok(ref value) }) if value["protocol"] == 1)
        {
            drop(lifeline);
            let _ = child.kill();
            let _ = child.wait();
            return Err("process supervisor startup handshake failed".to_string());
        }
        rpc.set_read_timeout(None).map_err(error_text)?;
        let state = Arc::new(ClientState {
            failed: AtomicBool::new(false),
            closing: AtomicBool::new(false),
            pending: Mutex::new(HashMap::new()),
        });
        let reader_state = Arc::clone(&state);
        thread::spawn(move || {
            loop {
                let response: Response = match read_frame(&mut reader) {
                    Ok(response) => response,
                    Err(error) => {
                        reader_state.fail(&format!("process supervisor unavailable: {error}"));
                        break;
                    }
                };
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
            state,
            next_id: AtomicU64::new(1),
            shutdown: Mutex::new(None),
        })
    }

    pub(super) fn call<T: DeserializeOwned>(
        &self,
        operation: Operation,
        cancel: Option<CancellationToken>,
    ) -> Result<T, String> {
        if self.state.failed.load(Ordering::SeqCst) || self.state.closing.load(Ordering::SeqCst) {
            return Err("process supervisor is unavailable or shutting down".to_string());
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = mpsc::sync_channel(1);
        {
            let mut pending = self
                .state
                .pending
                .lock()
                .map_err(|_| "supervisor pending map poisoned")?;
            if pending.len() >= MAX_REQUESTS {
                return Err("process supervisor request capacity exceeded".to_string());
            }
            pending.insert(id, sender);
        }
        if let Err(error) = self.send(&Request { id, operation }) {
            self.state.fail(&error);
            self.close_lifeline();
            return Err(error);
        }
        let mut cancelled = false;
        loop {
            if !cancelled && cancel.as_ref().is_some_and(CancellationToken::is_cancelled) {
                if let Err(error) = self.send(&Request {
                    id: 0,
                    operation: Operation::Cancel(id),
                }) {
                    self.state.fail(&error);
                    self.close_lifeline();
                    return Err(error);
                }
                cancelled = true;
            }
            match receiver.recv_timeout(Duration::from_millis(10)) {
                Ok(reply) => return serde_json::from_value(reply?).map_err(error_text),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err("process supervisor response channel closed".to_string());
                }
            }
        }
    }

    fn send(&self, request: &Request) -> Result<(), String> {
        write_frame(
            &mut *self
                .writer
                .lock()
                .map_err(|_| "supervisor writer poisoned")?,
            request,
        )
    }

    fn close_lifeline(&self) {
        if let Ok(mut lifeline) = self.lifeline.lock() {
            lifeline.take();
        }
    }

    pub(super) fn shutdown(&self) -> Result<(), String> {
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
                Ok(Some(status)) => break Err(format!("process supervisor exited with {status}")),
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

impl Drop for SupervisorClient {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
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
    rpc.set_write_timeout(Some(IO_TIMEOUT))
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
    loop {
        if lifeline_closed(&lifeline) {
            break;
        }
        let request = match rx.recv_timeout(Duration::from_millis(10)) {
            Ok(request) => request,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        // 先检查专用存活连接；EOF 后丢弃解码器队列内尚未执行的请求。
        if lifeline_closed(&lifeline) {
            break;
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
                    // 控制循环不能排队等待工作线程持有的响应锁，否则背压会阻塞 lifeline。
                    let Ok(mut response_writer) = writer.try_lock() else {
                        break;
                    };
                    if write_frame(
                        &mut *response_writer,
                        &Response {
                            id: request.id,
                            result: Err("process supervisor worker capacity exceeded".to_string()),
                        },
                    )
                    .is_err()
                    {
                        break;
                    }
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
    }
    // 先封闭 backend 的 spawn gate，再由其批量 TERM/KILL/reap；长 exec 不占该事件循环。
    backend.shutdown()?;
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
