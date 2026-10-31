#![cfg(unix)]

use harness::NativeProcessBackend;
use harness::domain::{AgentId, SessionBackend, SessionStartRequest, TaskId};
use std::{
    collections::HashMap,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

const HOST_DIR: &str = "HARNESS_PROCESS_LIFECYCLE_HOST_DIR";

fn request(command: String) -> SessionStartRequest {
    SessionStartRequest {
        command,
        session_name: None,
        cwd: None,
        env: HashMap::new(),
        timeout_secs: Some(30),
        tail_lines: 200,
        owner_task_id: TaskId::new(),
        owner_agent_id: AgentId::new(),
        notify_on_exit: false,
        notify_on_output: false,
    }
}

/// 实际宿主为独立测试进程；服务端必须使用构建出的生产 Harness 可执行文件。
#[test]
fn host_fixture() {
    let Some(directory) = std::env::var_os(HOST_DIR).map(PathBuf::from) else {
        return;
    };
    // 与生产正常入口一致，必须先加载环境，再启动独立服务。
    dotenvy::from_filename(".env.local").ok();
    let backend = if std::env::var("HARNESS_LIFECYCLE_EXEC_MODE").as_deref() == Ok("logged") {
        NativeProcessBackend::supervised_with_log(
            env!("CARGO_BIN_EXE_harness"),
            &directory.join("supervisor.jsonl"),
        )
        .unwrap()
    } else {
        NativeProcessBackend::supervised(env!("CARGO_BIN_EXE_harness")).unwrap()
    };
    if std::env::var("HARNESS_LIFECYCLE_EXEC_MODE").as_deref() == Ok("dotenv") {
        backend
            .exec_blocking(request(format!(
                "review-env-command > '{}'",
                directory.join("dotenv-result").display()
            )))
            .unwrap();
        return;
    }
    if std::env::var("HARNESS_LIFECYCLE_EXEC_MODE").as_deref() == Ok("eof") {
        let mut command = request("cat; echo done".to_string());
        command.timeout_secs = Some(1);
        let result = backend.exec_blocking(command).unwrap();
        assert!(
            !result.timed_out,
            "supervised exec read the protocol pipe instead of inherited EOF stdin"
        );
        assert_eq!(result.output.output, "done");
        return;
    }
    let mut command = request(format!(
        "echo $$ > '{}'; nohup sleep 120 >/dev/null 2>&1 & echo $! > '{}'; wait",
        directory.join("leader").display(),
        directory.join("descendant").display(),
    ));
    if std::env::var("HARNESS_LIFECYCLE_EXEC_MODE").as_deref() == Ok("logged") {
        command.command.push_str(" # do-not-log-review-secret");
        command.env.insert(
            "HARNESS_REVIEW_SECRET".to_string(),
            "do-not-log-review-secret".to_string(),
        );
    }
    if std::env::var("HARNESS_LIFECYCLE_EXEC_MODE").as_deref() == Ok("long") {
        let worker_backend = backend.clone();
        thread::spawn(move || worker_backend.exec_blocking(command));
    } else {
        backend.start_session(command).unwrap();
    }
    let mut action = String::new();
    std::io::stdin().read_to_string(&mut action).unwrap();
    match action.as_str() {
        "abort" => std::process::abort(),
        "panic" => panic!("intentional host unwind"),
        "shutdown" => backend.shutdown().unwrap(),
        _ => {}
    }
}

struct Host {
    child: Child,
    directory: tempfile::TempDir,
}

impl Host {
    fn launch() -> Self {
        Self::launch_mode("")
    }

    fn launch_mode(mode: &str) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "host_fixture", "--nocapture"])
            .env(HOST_DIR, directory.path())
            .env("HARNESS_LIFECYCLE_EXEC_MODE", mode)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        Self { child, directory }
    }

    fn processes(&self) -> Vec<Identity> {
        ["leader", "descendant"]
            .into_iter()
            .map(|name| Identity::from_file(&self.directory.path().join(name)))
            .collect()
    }

    fn end(&mut self, mode: &str) {
        if mode == "kill" {
            self.child.kill().unwrap();
        } else {
            self.child
                .stdin
                .take()
                .unwrap()
                .write_all(mode.as_bytes())
                .unwrap();
        }
        self.child.wait().unwrap();
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Identity {
    pid: i32,
    start: String,
    group: i32,
}

impl Identity {
    fn from_file(path: &Path) -> Self {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(pid) = std::fs::read_to_string(path)
                && let Ok(pid) = pid.trim().parse::<i32>()
                && let Some((start, _, group)) = process_state(pid)
            {
                return Self { pid, start, group };
            }
            assert!(
                Instant::now() < deadline,
                "host did not write a live process identity: {}",
                path.display()
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn running(&self) -> bool {
        process_state(self.pid)
            .is_some_and(|(start, state, _)| start == self.start && !state.contains('Z'))
    }

    fn cleanup(&self) {
        if self.pid != std::process::id() as i32 && self.running() {
            // SAFETY: 先比对当前启动身份；组长仍在时只清理其原有组，普通后代只清理自身。
            unsafe {
                if self.group == self.pid {
                    libc::killpg(self.pid, libc::SIGKILL);
                } else {
                    libc::kill(self.pid, libc::SIGKILL);
                }
            }
        }
    }
}

impl Drop for Identity {
    fn drop(&mut self) {
        self.cleanup();
    }
}

fn process_state(pid: i32) -> Option<(String, String, i32)> {
    let output = Command::new("ps")
        .args([
            "-p",
            &pid.to_string(),
            "-o",
            "lstart=",
            "-o",
            "stat=",
            "-o",
            "pgid=",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let (identity, group) = text.trim().rsplit_once(' ')?;
    let (start, state) = identity.trim().rsplit_once(' ')?;
    Some((
        start.trim().to_string(),
        state.trim().to_string(),
        group.trim().parse().ok()?,
    ))
}

fn assert_host_cleanup(mode: &str) {
    let mut host = Host::launch();
    let processes = host.processes();
    host.end(mode);
    let deadline = Instant::now() + Duration::from_secs(5);
    while processes.iter().any(Identity::running) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    let remaining: Vec<_> = processes
        .iter()
        .filter(|process| process.running())
        .map(|process| process.pid)
        .collect();
    for process in &processes {
        process.cleanup();
    }
    assert!(
        remaining.is_empty(),
        "host {mode} left live owned processes: {remaining:?}"
    );
}

#[test]
fn sigkill_of_host_reclaims_owned_process_groups() {
    assert_host_cleanup("kill");
}

#[test]
fn abort_of_host_reclaims_owned_process_groups() {
    assert_host_cleanup("abort");
}

#[test]
fn normal_host_drop_reclaims_owned_process_groups() {
    assert_host_cleanup("normal");
}

fn supervised() -> NativeProcessBackend {
    NativeProcessBackend::supervised(env!("CARGO_BIN_EXE_harness")).unwrap()
}

#[test]
fn supervised_stdout_events_remain_available_until_delivery_ack() {
    let backend = supervised();
    let mut command = request("printf 'first\\nsecond\\n'; sleep 30".to_string());
    command.notify_on_output = true;
    let handle = backend.start_session(command).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    let events = loop {
        let events = backend.poll_output_events().unwrap();
        if events.len() == 2 || Instant::now() >= deadline {
            break events;
        }
        thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(
        events
            .iter()
            .map(|event| event.content.as_str())
            .collect::<Vec<_>>(),
        ["first", "second"]
    );
    assert_eq!(backend.poll_output_events().unwrap(), events);
    backend.ack_output_events(handle.handle_id, 1).unwrap();
    assert_eq!(backend.poll_output_events().unwrap()[0].sequence, 2);
    backend.ack_output_events(handle.handle_id, 2).unwrap();
    assert!(backend.poll_output_events().unwrap().is_empty());
    backend.shutdown().unwrap();
}

#[test]
fn panic_unwind_of_host_reclaims_owned_process_groups() {
    assert_host_cleanup("panic");
}

#[test]
fn explicit_host_shutdown_reclaims_owned_process_groups() {
    assert_host_cleanup("shutdown");
}

#[test]
fn independent_instances_only_reclaim_their_own_processes() {
    let mut first = Host::launch();
    let mut second = Host::launch();
    let first_processes = first.processes();
    let second_processes = second.processes();
    first.end("kill");
    let deadline = Instant::now() + Duration::from_secs(5);
    while first_processes.iter().any(Identity::running) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    let first_clean = !first_processes.iter().any(Identity::running);
    let second_live = second_processes.iter().all(Identity::running);
    second.end("shutdown");
    for identity in first_processes.iter().chain(&second_processes) {
        identity.cleanup();
    }
    assert!(first_clean, "first instance leaked its children");
    assert!(
        second_live,
        "first instance cleanup killed second instance's children"
    );
}

#[test]
fn supervisor_is_actual_process_parent_and_runs_without_provider_setup() {
    let backend = supervised();
    let directory = tempfile::tempdir().unwrap();
    let handle = backend
        .start_session(request(format!(
            "echo $$ > '{}'; echo $PPID > '{}'; exec sleep 120",
            directory.path().join("leader").display(),
            directory.path().join("owner").display()
        )))
        .unwrap();
    let leader = Identity::from_file(&directory.path().join("leader"));
    let owner = Identity::from_file(&directory.path().join("owner"));
    let separate_owner = owner.pid != std::process::id() as i32;
    backend.stop_session(handle.handle_id).unwrap();
    backend.shutdown().unwrap();
    let survived = leader.running();
    leader.cleanup();
    assert!(separate_owner, "host itself spawned the command");
    assert!(!survived);
}

#[test]
fn long_exec_cancellation_does_not_block_rpc_or_eof_cleanup() {
    use tokio_util::sync::CancellationToken;
    let backend = supervised();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("exec");
    let cancel = CancellationToken::new();
    let worker_backend = backend.clone();
    let worker_cancel = cancel.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    let command = format!("echo $$ > '{}'; sleep 120", path.display());
    let worker = thread::spawn(move || {
        let _ = tx.send(worker_backend.exec_with_cancel(request(command), worker_cancel));
    });
    let identity = Identity::from_file(&path);
    assert_eq!(
        backend.list_active_sessions().unwrap().len(),
        1,
        "long exec serialized other RPC"
    );
    cancel.cancel();
    let result = rx.recv_timeout(Duration::from_secs(3));
    if result.is_err() {
        let _ = backend.shutdown();
    }
    let survived = identity.running();
    identity.cleanup();
    worker.join().unwrap();
    assert_eq!(result.unwrap().unwrap_err(), "cancelled");
    assert!(!survived, "cancel result preceded owned process cleanup");
    backend.shutdown().unwrap();
}

#[test]
fn exec_timeout_and_interactive_session_use_separate_stdin() {
    use harness::domain::{SessionInputRequest, SessionReadRequest};
    let backend = supervised();
    let mut timed_request = request("sleep 120".to_string());
    timed_request.timeout_secs = Some(1);
    assert!(backend.exec_blocking(timed_request).unwrap().timed_out);
    let session = backend
        .start_session(request(
            "read input; echo received:$input; sleep 120".to_string(),
        ))
        .unwrap();
    backend
        .input_session(SessionInputRequest {
            handle_id: session.handle_id,
            input: "hello".to_string(),
            append_newline: true,
        })
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    let output = loop {
        let output = backend
            .read_session(SessionReadRequest {
                handle_id: session.handle_id,
                tail_lines: 10,
            })
            .unwrap()
            .output
            .output;
        if output.contains("received:hello") || Instant::now() >= deadline {
            break output;
        }
        thread::sleep(Duration::from_millis(10));
    };
    backend.shutdown().unwrap();
    assert!(output.contains("received:hello"));
    assert!(backend.start_session(request("true".to_string())).is_err());
    backend.shutdown().unwrap();
}

#[test]
fn supervisor_death_restarts_managed_service_and_reports_lost_sessions() {
    use harness::domain::SessionReadRequest;
    let directory = tempfile::tempdir().unwrap();
    let log_path = directory.path().join("unexpected-exit.jsonl");
    let backend =
        NativeProcessBackend::supervised_with_log(env!("CARGO_BIN_EXE_harness"), &log_path)
            .unwrap();
    let lost = backend
        .start_session(request(format!(
            "echo $$ > '{}'; echo $PPID > '{}'; exec sleep 120",
            directory.path().join("leader").display(),
            directory.path().join("owner").display()
        )))
        .unwrap();
    let leader = Identity::from_file(&directory.path().join("leader"));
    let owner = Identity::from_file(&directory.path().join("owner"));
    owner.cleanup();
    let deadline = Instant::now() + Duration::from_secs(3);
    while owner.running() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    // 读取线程先发现断开；首个调用补做登记并重启新一代服务。
    thread::sleep(Duration::from_millis(100));
    let read = backend.read_session(SessionReadRequest {
        handle_id: lost.handle_id,
        tail_lines: 10,
    });
    let reason = backend.lost_session_reason(lost.handle_id);
    let restarted = backend.start_session(request(format!(
        "echo $PPID > '{}'; exec sleep 120",
        directory.path().join("new-owner").display()
    )));
    let new_owner = restarted
        .is_ok()
        .then(|| Identity::from_file(&directory.path().join("new-owner")));
    leader.cleanup();
    backend.shutdown().unwrap();
    let error = read.unwrap_err();
    assert!(error.contains("process service failed"), "{error}");
    assert!(
        reason.is_some(),
        "session of the failed generation was not marked lost"
    );
    let new_owner = new_owner.expect("backend did not restart the process service");
    assert_ne!(new_owner.pid, owner.pid);
    assert_ne!(
        new_owner.pid,
        std::process::id() as i32,
        "backend silently fell back to unmanaged execution"
    );
    let log = std::fs::read_to_string(log_path).unwrap();
    for event in [
        "ProcessSupervisorAbnormalExit",
        "ProcessSupervisorFailed",
        "ProcessSupervisorRestarted",
    ] {
        assert!(log.contains(event), "{event} was not independently logged");
    }
}

#[test]
fn frozen_supervisor_fails_single_call_without_stopping_managed_processes() {
    use harness::domain::SessionReadRequest;
    let backend = supervised();
    let directory = tempfile::tempdir().unwrap();
    let handle = backend
        .start_session(request(format!(
            "echo $$ > '{}'; echo $PPID > '{}'; exec sleep 120",
            directory.path().join("leader").display(),
            directory.path().join("owner").display()
        )))
        .unwrap();
    let leader = Identity::from_file(&directory.path().join("leader"));
    let owner = Identity::from_file(&directory.path().join("owner"));
    let read = || {
        backend.read_session(SessionReadRequest {
            handle_id: handle.handle_id,
            tail_lines: 10,
        })
    };
    // 模拟系统停顿：服务进程超过 RPC 期限没有调度。
    // SAFETY: owner 刚由会话写入且仍是本测试启动的服务进程。
    unsafe { libc::kill(owner.pid, libc::SIGSTOP) };
    let started = Instant::now();
    let stalled = read();
    let stalled_for = started.elapsed();
    thread::sleep(Duration::from_millis(500));
    // SAFETY: 同上，恢复刚暂停的服务进程。
    unsafe { libc::kill(owner.pid, libc::SIGCONT) };
    let survived = leader.running() && owner.running();
    let resumed = read();
    let next_start = backend.start_session(request("true".to_string()));
    backend.shutdown().unwrap();
    let error = stalled.unwrap_err();
    assert!(error.contains("timed out"), "{error}");
    assert!(
        !error.contains("all instance processes"),
        "a slow call stopped the whole instance: {error}"
    );
    assert!(stalled_for < Duration::from_secs(4));
    assert!(
        survived,
        "managed processes were stopped by a transient stall"
    );
    assert!(resumed.unwrap().status == harness::domain::SessionStatus::Running);
    assert!(next_start.is_ok());
    assert!(
        !leader.running(),
        "explicit shutdown still reclaims processes"
    );
}

#[test]
fn shutdown_and_concurrent_spawns_share_one_bounded_lifecycle() {
    use std::sync::{Arc, Barrier};
    let backend = supervised();
    let directory = tempfile::tempdir().unwrap();
    let barrier = Arc::new(Barrier::new(17));
    let mut workers = Vec::new();
    for id in 0..16 {
        let worker_backend = backend.clone();
        let worker_barrier = Arc::clone(&barrier);
        let path = directory.path().join(format!("race-{id}"));
        workers.push(thread::spawn(move || {
            worker_barrier.wait();
            worker_backend.start_session(request(format!(
                "echo $$ > '{}'; sleep 120",
                path.display()
            )))
        }));
    }
    barrier.wait();
    let started = Instant::now();
    backend.shutdown().unwrap();
    for worker in workers {
        let _ = worker.join().unwrap();
    }
    assert!(started.elapsed() < Duration::from_secs(5));
    // 每个成功启动的 shell 命令都包含独有目录；仅对仍匹配本轮命令的身份做诊断清理。
    let marker = directory.path().to_string_lossy();
    let mut leaked = Vec::new();
    for entry in std::fs::read_dir(directory.path()).unwrap() {
        let text = std::fs::read_to_string(entry.unwrap().path()).unwrap();
        if let Ok(pid) = text.trim().parse::<i32>() {
            let command = Command::new("ps")
                .args(["-p", &pid.to_string(), "-o", "command="])
                .output()
                .unwrap();
            if String::from_utf8_lossy(&command.stdout).contains(marker.as_ref()) {
                leaked.push(pid);
                // SAFETY: 当前组长命令仍匹配本测试独有目录，PID 尚未退出或复用。
                unsafe {
                    libc::killpg(pid, libc::SIGKILL);
                }
            }
        }
    }
    assert!(leaked.is_empty(), "spawn/shutdown race leaked {leaked:?}");
    assert!(backend.start_session(request("true".to_string())).is_err());
}

#[test]
fn sigkill_during_long_exec_reclaims_without_waiting_for_rpc_completion() {
    let mut host = Host::launch_mode("long");
    let identities = host.processes();
    host.end("kill");
    let deadline = Instant::now() + Duration::from_secs(5);
    while identities.iter().any(Identity::running) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    let leaked = identities.iter().any(Identity::running);
    for identity in &identities {
        identity.cleanup();
    }
    assert!(!leaked, "long RPC prevented lifeline EOF cleanup");
}

#[test]
fn supervised_exec_inherits_controlled_eof_stdin() {
    let directory = tempfile::tempdir().unwrap();
    let result = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "host_fixture", "--nocapture"])
        .env(HOST_DIR, directory.path())
        .env("HARNESS_LIFECYCLE_EXEC_MODE", "eof")
        .current_dir(directory.path())
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{} {}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        String::from_utf8_lossy(&result.stdout).contains("1 passed"),
        "host fixture was not executed"
    );
    assert!(
        !std::fs::read_dir(directory.path())
            .unwrap()
            .any(|entry| entry
                .unwrap()
                .path()
                .extension()
                .is_some_and(|ext| ext == "jsonl")),
        "default test constructor created a user log"
    );
}

#[test]
fn blocked_session_input_does_not_prevent_lifeline_shutdown() {
    use harness::domain::SessionInputRequest;
    let backend = supervised();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("blocked-input");
    let handle = backend
        .start_session(request(format!(
            "echo $$ > '{}'; exec sleep 120",
            path.display()
        )))
        .unwrap();
    let identity = Identity::from_file(&path);
    let worker_backend = backend.clone();
    let worker = thread::spawn(move || {
        worker_backend.input_session(SessionInputRequest {
            handle_id: handle.handle_id,
            input: "x".repeat(1024 * 1024),
            append_newline: false,
        })
    });
    thread::sleep(Duration::from_millis(50));
    let started = Instant::now();
    backend.shutdown().unwrap();
    let _ = worker.join().unwrap();
    let survived = identity.running();
    identity.cleanup();
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(
        !survived,
        "shutdown returned before cleaning a blocked input process"
    );
}

#[test]
fn bulk_supervised_shutdown_has_a_shared_deadline_and_rejects_new_starts() {
    use harness::domain::SessionReadRequest;
    let backend = supervised();
    let clone = backend.clone();
    let directory = tempfile::tempdir().unwrap();
    let mut identities = Vec::new();
    for id in 0..6 {
        let path = directory.path().join(format!("bulk-{id}"));
        let handle = backend
            .start_session(request(format!(
                "trap '' TERM; echo $$ > '{}'; echo ready; while :; do sleep 120; done",
                path.display()
            )))
            .unwrap();
        identities.push(Identity::from_file(&path));
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if backend
                .read_session(SessionReadRequest {
                    handle_id: handle.handle_id,
                    tail_lines: 10,
                })
                .unwrap()
                .output
                .output
                .contains("ready")
            {
                break;
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(10));
        }
    }
    let started = Instant::now();
    backend.shutdown().unwrap();
    let leaked = identities.iter().any(Identity::running);
    for identity in &identities {
        identity.cleanup();
    }
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(!leaked);
    assert!(clone.start_session(request("true".to_string())).is_err());
    clone.shutdown().unwrap();
}

#[test]
fn oversized_request_fails_closed_and_does_not_spawn() {
    let backend = supervised();
    assert!(
        backend
            .start_session(request("#".repeat(5 * 1024 * 1024)))
            .is_err()
    );
    backend.shutdown().unwrap();
}

#[test]
fn blocked_input_has_a_deadline_and_keeps_the_instance_running() {
    use harness::domain::SessionInputRequest;
    let backend = supervised();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("blocked-deadline");
    let handle = backend
        .start_session(request(format!(
            "echo $$ > '{}'; exec sleep 120",
            path.display()
        )))
        .unwrap();
    let identity = Identity::from_file(&path);
    let other_path = directory.path().join("other-owned");
    backend
        .start_session(request(format!(
            "echo $$ > '{}'; exec sleep 120",
            other_path.display()
        )))
        .unwrap();
    let other_identity = Identity::from_file(&other_path);
    let worker_backend = backend.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    let worker = thread::spawn(move || {
        let result = worker_backend.input_session(SessionInputRequest {
            handle_id: handle.handle_id,
            input: "x".repeat(1024 * 1024),
            append_newline: false,
        });
        let _ = tx.send(result);
    });
    let reply = rx.recv_timeout(Duration::from_secs(5));
    let finished_without_help = reply.is_ok();
    let survived_at_reply = identity.running() && other_identity.running();
    let still_usable = backend.start_session(request("true".to_string())).is_ok();
    // 只有超出观察期限后才从测试外部清理；这不能算作被测超时实现。
    let _ = backend.shutdown();
    identity.cleanup();
    other_identity.cleanup();
    worker.join().unwrap();
    assert!(
        finished_without_help,
        "blocked shell_input never reached a completion deadline without external shutdown"
    );
    let error = reply.unwrap().unwrap_err();
    assert!(
        error.contains("did not accept input"),
        "unexpected error: {error}"
    );
    assert!(
        survived_at_reply,
        "a blocked input stopped unrelated managed processes"
    );
    assert!(still_usable, "a blocked input disabled the process service");
}

#[test]
fn review_supervisor_logs_survive_host_sigkill_and_cover_cleanup() {
    let mut host = Host::launch_mode("logged");
    let identities = host.processes();
    host.end("kill");
    let deadline = Instant::now() + Duration::from_secs(5);
    while identities.iter().any(Identity::running) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    for identity in &identities {
        identity.cleanup();
    }
    let path = host.directory.path().join("supervisor.jsonl");
    let text = loop {
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        if text.contains("ProcessSupervisorStopped") && text.ends_with('\n')
            || Instant::now() >= deadline
        {
            break text;
        }
        thread::sleep(Duration::from_millis(10));
    };
    let entries: Vec<serde_json::Value> = text
        .lines()
        .map(|line| {
            serde_json::from_str(line).expect("supervisor log must contain complete JSON records")
        })
        .collect();
    for event in [
        "ProcessSupervisorStarted",
        "ShellSessionStarted",
        "ProcessLifelineClosed",
        "ProcessGroupKillSent",
        "ProcessReaped",
        "ShellSessionStopped",
        "ProcessSupervisorStopped",
    ] {
        assert!(
            entries
                .iter()
                .any(|entry| entry["fields"]["event"] == event),
            "missing independent cleanup event {event}: {text}"
        );
    }
    assert!(
        entries
            .iter()
            .filter(|entry| entry["fields"]["event"] == "ShellSessionStarted")
            .all(|entry| entry["fields"]["pid"].is_number()
                && entry["fields"]["task_id"].is_string()
                && entry["fields"]["agent_id"].is_string())
    );
    for entry in entries.iter().filter(|entry| {
        matches!(
            entry["fields"]["event"].as_str(),
            Some("ProcessGroupTermSent" | "ProcessGroupKillSent" | "ProcessReaped")
        )
    }) {
        assert!(
            entry["span"]["task_id"].is_string() && entry["span"]["session_id"].is_string(),
            "process cleanup event lost its task/session identity: {entry}"
        );
    }
    assert!(
        !text.contains("do-not-log-review-secret"),
        "command/environment payload leaked into lifecycle log"
    );
}

#[test]
fn review_dotenv_environment_reaches_supervised_shell() {
    let directory = tempfile::tempdir().unwrap();
    use std::os::unix::fs::PermissionsExt;
    let bin = directory.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let script = bin.join("review-env-command");
    std::fs::write(
        &script,
        "#!/bin/sh\nprintf '%s' \"$HARNESS_REVIEW_DOTENV_TOKEN\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    std::fs::write(
        directory.path().join(".env.local"),
        format!(
            "HARNESS_REVIEW_DOTENV_TOKEN=dotenv-review-value\nPATH={}\n",
            serde_json::to_string(&path).unwrap()
        ),
    )
    .unwrap();
    let result = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "host_fixture", "--nocapture"])
        .env(HOST_DIR, directory.path())
        .env("HARNESS_LIFECYCLE_EXEC_MODE", "dotenv")
        .env_remove("HARNESS_REVIEW_DOTENV_TOKEN")
        .env_remove("PATH")
        .current_dir(directory.path())
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{} {}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(directory.path().join("dotenv-result")).unwrap(),
        "dotenv-review-value",
        "guard was launched before dotenv environment was loaded"
    );
}
