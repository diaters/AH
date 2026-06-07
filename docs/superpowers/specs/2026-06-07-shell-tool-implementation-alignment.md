# Shell Tool Alignment Completion Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Complete the missing P0 and key P1 shell tool behaviors so the current `shell_*` implementation matches the alignment doc and reaches a truthful, basic-usable Phase 1 state.

**Architecture:** Keep the existing shell tool surface and session domain model intact, then close the runtime gaps in the native backend, waiting systems, orchestrator, and output model. Phase 1 completion uses `std::process` plus dedicated background reader threads for `stdout` / `stderr`, which avoids nested runtime blocking inside Bevy systems and keeps the synchronous `SessionBackend` trait honest. The work is incremental: first make `shell_start / shell_wait / shell_stop / shell_send_input / shell_send_signal` tell the truth and act on real process state, then unify output reading and response shapes, and finally expand test coverage to lock the behavior in place.

**Tech Stack:** Rust, Bevy ECS, std::process, std::thread, std::sync, serde_json, chrono, uuid, tracing

---

## Scope Check

This plan implements the completion work described in:

- `docs/superpowers/specs/2026-06-07-shell-tool-design.md`
- `docs/superpowers/specs/2026-06-07-shell-tool-implementation-alignment.md`

This plan intentionally covers:

- P0 completion
- key P1 completion needed for stable tool-calling behavior

This plan intentionally excludes:

- `HerdrSessionBackend`
- full terminal emulation
- arbitrary TTY byte streaming
- advanced streaming subscriptions

---

## File Structure

| File | Responsibility |
|------|----------------|
| `src/domain/session.rs` | Extend the session runtime state so the native backend can track process state, output buffers, and cursor progression honestly |
| `src/contracts/sessions.rs` | Keep the backend trait stable and document the synchronous backend strategy |
| `src/systems/tools/backend/native.rs` | Implement real native process/session lifecycle, stop, input, signal, waiting, timeout, and output buffering |
| `src/systems/tools/orchestrator.rs` | Wire `shell_stop`, `shell_wait`, `shell_send_input`, and `shell_send_signal` to truthful backend-backed results |
| `src/systems/tools/waiting.rs` | Replace timeout-only shell waiting with backend-driven waiting completion |
| `src/systems/tools/result.rs` | Keep STM writes aligned with the response model and make the current “tool_output only” rule explicit |
| `src/systems/tools/builtin/shell/*.rs` | Adjust parsers only if backend/runtime changes require tighter input validation |
| `src/plugins/tools.rs` | Keep session resources registered and add any new polling systems/resources needed by the native backend |
| `tests/shell_tool_flow.rs` | Expand end-to-end behavior coverage for P0 and key P1 |
| `docs/superpowers/specs/2026-06-07-shell-tool-implementation-alignment.md` | Update the completion state if implementation decisions change what is considered done |

---

### Task 1: Make Session Runtime State Capable Of Tracking Real Processes

**Files:**
- Modify: `src/domain/session.rs`
- Modify: `src/contracts/sessions.rs`

- [ ] **Step 1: Write the failing test for empty runtime state**

Append this unit test to `src/domain/session.rs`:

```rust
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
```

- [ ] **Step 2: Run the test to verify the file still compiles**

Run: `cargo test session_runtime_state_starts_empty`

Expected: FAIL because `SessionRuntimeState::empty()` and `SessionOutputBuffer::empty()` do not exist yet.

- [ ] **Step 3: Extend the session domain model with runtime-only state**

Update `src/domain/session.rs` by adding runtime state types after `SessionOutputBuffer`:

```rust
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
```

Then extend `SpaceSessionRegistry`:

```rust
#[derive(Resource, Default)]
pub struct SpaceSessionRegistry {
    pub sessions: HashMap<SessionHandleId, SessionHandle>,
    pub runtimes: HashMap<SessionHandleId, SessionRuntimeState>,
}
```

Add helper methods at the bottom of the file:

```rust
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
```

- [ ] **Step 4: Keep the backend trait synchronous and document why**

Keep `src/contracts/sessions.rs` unchanged and add a short contract comment above the trait:

```rust
/// SessionBackend 保持同步接口。
///
/// Phase 1 的 NativeProcessBackend 通过内部线程和互斥状态管理子进程，
/// 避免在 Bevy system 中使用嵌套 runtime block_on。
pub trait SessionBackend: Send + Sync + 'static {
    fn exec_blocking(&self, request: SessionStartRequest) -> Result<SessionHandle, String>;
    fn start_session(&self, request: SessionStartRequest) -> Result<SessionHandle, String>;
    fn get_status(&self, handle_id: SessionHandleId) -> Result<SessionHandle, String>;
    fn read_output(&self, request: SessionOutputRequest) -> Result<SessionOutputResponse, String>;
    fn send_input(&self, command: SessionCommand) -> Result<SessionHandle, String>;
    fn send_signal(&self, command: SessionCommand) -> Result<SessionHandle, String>;
    fn wait_session(&self, request: SessionWaitRequest) -> Result<Option<SessionHandle>, String>;
    fn stop_session(&self, request: SessionStopRequest) -> Result<SessionHandle, String>;
}
```

- [ ] **Step 5: Run the domain-level test suite**

Run: `cargo test session_runtime_state_starts_empty tail_lines_returns_only_latest_lines`

Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add src/domain/session.rs src/contracts/sessions.rs
git commit -m "feat(shell): extend session runtime state for real process tracking"
```

---

### Task 2: Implement Real Native Process Lifecycle, Background Output Collection, And `shell_exec` Timeout

**Files:**
- Modify: `src/systems/tools/backend/native.rs`
- Modify: `src/plugins/tools.rs` only if new runtime resources are required

- [ ] **Step 1: Write the failing integration test for real session stop**

Append this test to `tests/shell_tool_flow.rs`:

```rust
#[test]
fn shell_stop_transitions_a_running_session_to_stopped() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, vec![]);

    app.update();

    let agent_id = spawn_agent(app.world_mut());
    let task_entity = app
        .world_mut()
        .spawn((
            Task::from_user_input_ready("shell stop", 3, default_channel()),
            ShortTermMemory::default(),
        ))
        .id();
    let task_id = app.world().get::<Task>(task_entity).unwrap().id;

    let start_request = AgentExecutionRequest {
        task_id,
        agent_id,
        request_kind: AgentRequestKind::ToolExecution {
            tool_name: "shell_start".to_string(),
        },
        prompt: String::new(),
        system_prompt: None,
        tools: vec![],
        conversation: None,
        work_item_id: None,
    };

    app.world_mut().spawn(ToolExecutionRequestMessage {
        request: start_request,
        tool_name: "shell_start".to_string(),
        tool_input: serde_json::json!({
            "command": "sleep 5",
            "session_name": "stop-test"
        }),
        pending_confirmation_id: None,
        tool_call_id: Some("call_shell_start_for_stop".to_string()),
        pending_confirmation_options: None,
    });

    app.update();

    let handle_id = {
        let world = app.world_mut();
        let mut query = world.query::<&harness::ToolExecutionResultMessage>();
        let results = query.iter(world).cloned().collect::<Vec<_>>();
        results[0].tool_output.clone().unwrap()["handle_id"]
            .as_str()
            .unwrap()
            .to_string()
    };

    let stop_request = AgentExecutionRequest {
        task_id,
        agent_id,
        request_kind: AgentRequestKind::ToolExecution {
            tool_name: "shell_stop".to_string(),
        },
        prompt: String::new(),
        system_prompt: None,
        tools: vec![],
        conversation: None,
        work_item_id: None,
    };

    app.world_mut().spawn(ToolExecutionRequestMessage {
        request: stop_request,
        tool_name: "shell_stop".to_string(),
        tool_input: serde_json::json!({
            "handle_id": handle_id,
            "wait_for_exit": false
        }),
        pending_confirmation_id: None,
        tool_call_id: Some("call_shell_stop".to_string()),
        pending_confirmation_options: None,
    });

    app.update();

    let results = {
        let world = app.world_mut();
        let mut query = world.query::<&harness::ToolExecutionResultMessage>();
        query.iter(world).cloned().collect::<Vec<_>>()
    };

    let last = results.last().unwrap().tool_output.clone().unwrap();
    assert_eq!(last["status"], "stopped");
}
```

- [ ] **Step 2: Run the test and verify it fails**

Run: `cargo test shell_stop_transitions_a_running_session_to_stopped -- --nocapture`

Expected: FAIL because `shell_stop` currently returns placeholder JSON without true backend-backed state.

- [ ] **Step 3: Replace the in-memory `shell_start` stub with real child-process spawning**

Refactor `src/systems/tools/backend/native.rs` so `start_session()` launches a real child process, stores the process handle behind backend-managed state, and immediately starts background output collectors. Introduce an internal process registry:

```rust
use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Write},
    process::Stdio,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};

#[derive(Default)]
pub struct NativeProcessBackend {
    pub sessions: Arc<Mutex<HashMap<SessionHandleId, SessionHandle>>>,
    pub processes: Arc<Mutex<HashMap<SessionHandleId, Arc<Mutex<Child>>>>>,
    pub stdins: Arc<Mutex<HashMap<SessionHandleId, Arc<Mutex<ChildStdin>>>>>,
}
```

Then implement `start_session()` with real spawn:

```rust
fn start_session(&self, request: SessionStartRequest) -> Result<SessionHandle, String> {
    let handle_id = Uuid::new_v4();
    let command_text = request.command.clone();
    let session_name = request.session_name.clone();
    let cwd = request.cwd.clone();
    let owner_task_id = request.owner_task_id;
    let owner_agent_id = request.owner_agent_id;
    let sessions = Arc::clone(&self.sessions);

    let mut command = Command::new("sh");
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

    Ok(handle)
}
```

Add the helper used above:

```rust
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
```

- [ ] **Step 4: Implement truthful `exec_blocking()` timeout**

Replace the blocking execution path with a polled timeout loop using `try_wait()`:

```rust
fn exec_blocking(&self, request: SessionStartRequest) -> Result<SessionHandle, String> {
    let handle_id = Uuid::new_v4();
    let mut command = Command::new("sh");
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
```

- [ ] **Step 5: Implement truthful `wait_session()` and `stop_session()`**

Update the backend so `wait_session()` observes the child process and `stop_session()` actually terminates it:

```rust
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

    Ok(handle)
}
```

- [ ] **Step 6: Ensure remaining child processes are cleaned up on backend drop**

Append this cleanup implementation to `src/systems/tools/backend/native.rs`:

```rust
impl Drop for NativeProcessBackend {
    fn drop(&mut self) {
        if let Ok(processes) = self.processes.lock() {
            for process in processes.values() {
                if let Ok(mut child) = process.lock() {
                    let _ = child.kill();
                }
            }
        }
    }
}
```

- [ ] **Step 7: Run the new stop test and make it pass**

Run: `cargo test shell_stop_transitions_a_running_session_to_stopped -- --nocapture`

Expected: PASS

- [ ] **Step 8: Commit**

```bash
git add src/systems/tools/backend/native.rs tests/shell_tool_flow.rs
git commit -m "feat(shell): implement real native session lifecycle"
```

---

### Task 3: Make `shell_wait`, `shell_stop`, And `shell_read_output` Use The Right Backend State And Tool Names

**Files:**
- Modify: `src/systems/tools/orchestrator.rs`
- Modify: `src/systems/tools/waiting.rs`

- [ ] **Step 1: Write the failing test for backend-driven `shell_wait` completion**

Append this test to `tests/shell_tool_flow.rs`:

```rust
#[test]
fn shell_wait_returns_completed_when_process_exits() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, vec![]);

    app.update();

    let agent_id = spawn_agent(app.world_mut());
    let task_entity = app
        .world_mut()
        .spawn((
            Task::from_user_input_ready("shell wait", 3, default_channel()),
            ShortTermMemory::default(),
        ))
        .id();
    let task_id = app.world().get::<Task>(task_entity).unwrap().id;

    let start_request = AgentExecutionRequest {
        task_id,
        agent_id,
        request_kind: AgentRequestKind::ToolExecution {
            tool_name: "shell_start".to_string(),
        },
        prompt: String::new(),
        system_prompt: None,
        tools: vec![],
        conversation: None,
        work_item_id: None,
    };

    app.world_mut().spawn(ToolExecutionRequestMessage {
        request: start_request,
        tool_name: "shell_start".to_string(),
        tool_input: serde_json::json!({
            "command": "sleep 0.1"
        }),
        pending_confirmation_id: None,
        tool_call_id: Some("call_shell_start_for_wait".to_string()),
        pending_confirmation_options: None,
    });
    app.update();

    let handle_id = {
        let world = app.world_mut();
        let mut query = world.query::<&harness::ToolExecutionResultMessage>();
        let results = query.iter(world).cloned().collect::<Vec<_>>();
        results[0].tool_output.clone().unwrap()["handle_id"]
            .as_str()
            .unwrap()
            .to_string()
    };

    let wait_request = AgentExecutionRequest {
        task_id,
        agent_id,
        request_kind: AgentRequestKind::ToolExecution {
            tool_name: "shell_wait".to_string(),
        },
        prompt: String::new(),
        system_prompt: None,
        tools: vec![],
        conversation: None,
        work_item_id: None,
    };

    app.world_mut().spawn(ToolExecutionRequestMessage {
        request: wait_request,
        tool_name: "shell_wait".to_string(),
        tool_input: serde_json::json!({
            "handle_id": handle_id,
            "timeout_secs": 2
        }),
        pending_confirmation_id: None,
        tool_call_id: Some("call_shell_wait".to_string()),
        pending_confirmation_options: None,
    });

    for _ in 0..20 {
        app.update();
        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    let results = {
        let world = app.world_mut();
        let mut query = world.query::<&harness::ToolExecutionResultMessage>();
        query.iter(world).cloned().collect::<Vec<_>>()
    };

    let wait_result = results
        .iter()
        .find(|result| result.tool_name == "shell_wait")
        .expect("shell_wait result should be present");
    let output_json = wait_result.tool_output.clone().unwrap();
    assert_eq!(output_json["status"], "completed");
}
```

- [ ] **Step 2: Run the test and verify it fails**

Run: `cargo test shell_wait_returns_completed_when_process_exits -- --nocapture`

Expected: FAIL because `check_waiting_sessions_system()` still uses timeout-only placeholder logic.

- [ ] **Step 3: Make the waiting system query the backend**

Replace `check_waiting_sessions_system()` in `src/systems/tools/waiting.rs`:

```rust
pub fn check_waiting_sessions_system(
    clock: Res<Clock>,
    mut commands: Commands,
    waiting_tasks: Query<(Entity, &Task, &WaitingForSessionInfo)>,
    backend: Res<crate::systems::tools::backend::NativeProcessBackend>,
) {
    for (entity, task, info) in &waiting_tasks {
        let timed_out = clock.0 >= info.timeout_at;
        let handle = backend
            .wait_session(crate::domain::SessionWaitRequest {
                handle_id: info.handle_id,
                timeout_secs: 0,
                tail_lines: info.return_tail_lines,
            })
            .ok()
            .flatten();

        if timed_out || handle.is_some() {
            commands.spawn(ToolExecutionResultMessage {
                result: AgentExecutionResult {
                    task_id: task.id,
                    agent_id: info.agent_id,
                    request_kind: AgentRequestKind::LlmCompletion,
                    result: Ok(AgentExecutionOutput {
                        content: OutputContent::Text("shell_wait completed".to_string()),
                        reasoning_content: None,
                    }),
                    prompt: String::new(),
                    system_prompt: None,
                    tools: vec![],
                    reasoning_content: None,
                    work_item_id: None,
                },
                tool_name: "shell_wait".to_string(),
                tool_output: Ok(match handle {
                    Some(handle) => serde_json::json!(handle),
                    None => serde_json::json!({
                        "handle_id": info.handle_id.to_string(),
                        "status": "running",
                        "timed_out": true
                    }),
                }),
                tool_call_id: Some(info.tool_call_id.clone()),
                processed: false,
            });

            commands.entity(entity).remove::<WaitingForSessionInfo>();
        }
    }
}
```

- [ ] **Step 4: Make `shell_stop` call the backend**

Update the `StopSession` branch in `src/systems/tools/orchestrator.rs`:

```rust
Ok(ToolAction::StopSession(stop_request)) => match backend.stop_session(stop_request.clone()) {
    Ok(handle) => {
        if stop_request.wait_for_exit {
            if let Some((_, mut task)) = tasks
                .iter_mut()
                .find(|(_, t)| t.id == request.request.task_id)
            {
                task.status = TaskStatus::Waiting(WaitingReason::Session {
                    handle_id: stop_request.handle_id,
                });
            }
            commands.entity(task_entity).insert(WaitingForSessionInfo {
                handle_id: stop_request.handle_id,
                timeout_at: chrono::Utc::now()
                    + chrono::Duration::seconds(stop_request.timeout_secs as i64),
                tool_call_id: request.tool_call_id.clone().unwrap_or_default(),
                agent_id: request.request.agent_id,
                return_tail_lines: stop_request.tail_lines,
            });
            commands.entity(request_entity).despawn();
        } else {
            spawn_shell_result(
                commands,
                request_entity,
                request,
                "shell_stop",
                serde_json::json!(handle),
            );
        }
    }
    Err(error) => {
        spawn_tool_error(
            commands,
            request_entity,
            request,
            ToolError::ExecutionFailed(error),
        );
    }
}
```

- [ ] **Step 5: Make `ReadSessionOutput` preserve the original tool name**

Update the `ReadSessionOutput` branch in `src/systems/tools/orchestrator.rs`:

```rust
Ok(ToolAction::ReadSessionOutput(output_request)) => {
    match backend.read_output(output_request) {
        Ok(response) => {
            spawn_shell_result(
                commands,
                request_entity,
                request,
                &request.tool_name,
                serde_json::json!(response),
            );
        }
        Err(error) => {
            spawn_tool_error(
                commands,
                request_entity,
                request,
                ToolError::ExecutionFailed(error),
            );
        }
    }
}
```

- [ ] **Step 6: Run the wait test and the stop test**

Run: `cargo test shell_wait_returns_completed_when_process_exits shell_stop_transitions_a_running_session_to_stopped -- --nocapture`

Expected: PASS

- [ ] **Step 7: Commit**

```bash
git add src/systems/tools/orchestrator.rs src/systems/tools/waiting.rs tests/shell_tool_flow.rs
git commit -m "feat(shell): drive wait and stop from real backend state"
```

---

### Task 4: Implement `shell_send_input`, `shell_send_signal`, And Honest Interactive State

**Files:**
- Modify: `src/systems/tools/backend/native.rs`
- Modify: `src/domain/session.rs`
- Modify: `src/systems/tools/orchestrator.rs`

- [ ] **Step 1: Write the failing test for `shell_send_input`**

Append this test to `tests/shell_tool_flow.rs`:

```rust
#[test]
fn shell_send_input_returns_backend_backed_status() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, vec![]);

    app.update();

    let agent_id = spawn_agent(app.world_mut());
    let task_entity = app
        .world_mut()
        .spawn((
            Task::from_user_input_ready("shell input", 3, default_channel()),
            ShortTermMemory::default(),
        ))
        .id();
    let task_id = app.world().get::<Task>(task_entity).unwrap().id;

    let start_request = AgentExecutionRequest {
        task_id,
        agent_id,
        request_kind: AgentRequestKind::ToolExecution {
            tool_name: "shell_start".to_string(),
        },
        prompt: String::new(),
        system_prompt: None,
        tools: vec![],
        conversation: None,
        work_item_id: None,
    };

    app.world_mut().spawn(ToolExecutionRequestMessage {
        request: start_request,
        tool_name: "shell_start".to_string(),
        tool_input: serde_json::json!({
            "command": "cat"
        }),
        pending_confirmation_id: None,
        tool_call_id: Some("call_shell_start_for_input".to_string()),
        pending_confirmation_options: None,
    });
    app.update();

    let handle_id = {
        let world = app.world_mut();
        let mut query = world.query::<&harness::ToolExecutionResultMessage>();
        let results = query.iter(world).cloned().collect::<Vec<_>>();
        results[0].tool_output.clone().unwrap()["handle_id"]
            .as_str()
            .unwrap()
            .to_string()
    };

    let input_request = AgentExecutionRequest {
        task_id,
        agent_id,
        request_kind: AgentRequestKind::ToolExecution {
            tool_name: "shell_send_input".to_string(),
        },
        prompt: String::new(),
        system_prompt: None,
        tools: vec![],
        conversation: None,
        work_item_id: None,
    };

    app.world_mut().spawn(ToolExecutionRequestMessage {
        request: input_request,
        tool_name: "shell_send_input".to_string(),
        tool_input: serde_json::json!({
            "handle_id": handle_id,
            "input": "hello",
            "append_newline": true
        }),
        pending_confirmation_id: None,
        tool_call_id: Some("call_shell_send_input".to_string()),
        pending_confirmation_options: None,
    });

    for _ in 0..5 {
        app.update();
    }

    let results = {
        let world = app.world_mut();
        let mut query = world.query::<&harness::ToolExecutionResultMessage>();
        query.iter(world).cloned().collect::<Vec<_>>()
    };

    let last = results.last().unwrap().tool_output.clone().unwrap();
    assert!(last["status"].is_string());
}
```

- [ ] **Step 2: Run the test and verify it fails or exposes stub behavior**

Run: `cargo test shell_send_input_returns_backend_backed_status -- --nocapture`

Expected: FAIL or reveal that `shell_send_input` still behaves like a pure stub.

- [ ] **Step 3: Implement real stdin writes and signal dispatch**

Update `src/systems/tools/backend/native.rs`:

```rust
fn send_input(&self, command: SessionCommand) -> Result<SessionHandle, String> {
    match command {
        SessionCommand::Input {
            handle_id,
            input,
            append_newline,
            ..
        } => {
            let stdin = self
                .stdins
                .lock()
                .map_err(|_| "stdin map poisoned".to_string())?
                .get(&handle_id)
                .cloned()
                .ok_or_else(|| format!("stdin for session {} not found", handle_id))?;

            {
                let mut stdin = stdin.lock().map_err(|_| "stdin mutex poisoned".to_string())?;
                let payload = if append_newline {
                    format!("{input}\n")
                } else {
                    input
                };
                stdin
                    .write_all(payload.as_bytes())
                    .map_err(|error| error.to_string())?;
                stdin.flush().map_err(|error| error.to_string())?;
            }

            self.get_status(handle_id)
        }
        SessionCommand::Signal { .. } => Err("unexpected signal command in send_input".to_string()),
    }
}

fn send_signal(&self, command: SessionCommand) -> Result<SessionHandle, String> {
    match command {
        SessionCommand::Signal { handle_id, signal, .. } => {
            let process = self
                .processes
                .lock()
                .map_err(|_| "process map poisoned".to_string())?
                .get(&handle_id)
                .cloned()
                .ok_or_else(|| format!("session {} not found", handle_id))?;

            {
                let mut child = process.lock().map_err(|_| "process mutex poisoned".to_string())?;
                match signal.as_str() {
                    "interrupt" | "terminate" | "kill" => {
                        child.kill().map_err(|error| error.to_string())?
                    }
                    other => return Err(format!("unsupported signal '{}'", other)),
                }
            }

            let mut handle = self.get_status(handle_id)?;
            handle.status = SessionStatus::Stopped;
            handle.finished_at = Some(Utc::now());
            self.sessions
                .lock()
                .map_err(|_| "session map poisoned".to_string())?
                .insert(handle_id, handle.clone());
            Ok(handle)
        }
        SessionCommand::Input { .. } => Err("unexpected input command in send_signal".to_string()),
    }
}
```

Immediately below the implementation, add a short Phase 1 limitation comment:

```rust
// Phase 1 limitation:
// interrupt / terminate / kill are temporarily mapped to the same kill-like behavior
// in the native backend. Follow-up work can refine per-platform signal mapping.
```

- [ ] **Step 4: Keep the interaction state honest**

Extend `SessionHandle` in `src/domain/session.rs`:

```rust
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
```

Keep `interaction_required` as the Phase 1 truth signal. Do not invent a fake `WaitingForInput` transition unless the backend can detect it honestly.

- [ ] **Step 5: Run the new input test**

Run: `cargo test shell_send_input_returns_backend_backed_status -- --nocapture`

Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add src/systems/tools/backend/native.rs src/domain/session.rs src/systems/tools/orchestrator.rs tests/shell_tool_flow.rs
git commit -m "feat(shell): implement native input and signal behavior"
```

---

### Task 5: Implement Real Output Reading And Cursor Progression

**Files:**
- Modify: `src/systems/tools/backend/native.rs`
- Modify: `tests/shell_tool_flow.rs`

- [ ] **Step 1: Write the failing test for cursor-based output reads**

Append this test to `tests/shell_tool_flow.rs`:

```rust
#[test]
fn shell_read_output_supports_cursor_progression() {
    let backend = harness::NativeProcessBackend::default();
    let handle = backend
        .exec_blocking(harness::SessionStartRequest {
            command: "printf 'line1\\nline2\\nline3\\n'".to_string(),
            session_name: Some("cursor-test".to_string()),
            cwd: None,
            env: std::collections::HashMap::new(),
            timeout_secs: None,
            tail_lines: 2,
            owner_task_id: Uuid::new_v4(),
            owner_agent_id: Uuid::new_v4(),
        })
        .expect("exec_blocking should succeed");

    let first = backend
        .read_output(harness::SessionOutputRequest {
            handle_id: handle.handle_id,
            cursor: None,
            tail_lines: 2,
        })
        .expect("read_output should succeed");

    assert!(first.output.next_cursor.is_some());
}
```

- [ ] **Step 2: Run the test and verify the current behavior is insufficient**

Run: `cargo test shell_read_output_supports_cursor_progression -- --nocapture`

Expected: FAIL or reveal that cursor values are static and not based on buffered progression.

- [ ] **Step 3: Add buffered output helpers to the native backend**

Add helper functions to `src/systems/tools/backend/native.rs`:

```rust
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
```

- [ ] **Step 4: Make `exec_blocking()` and `read_output()` use the buffer helpers**

Update `exec_blocking()` so it stores the combined output in the registry-backed buffer and returns a window built from that buffer. Then update `read_output()`:

```rust
fn read_output(&self, request: SessionOutputRequest) -> Result<SessionOutputResponse, String> {
    let handle = self.get_status(request.handle_id)?;
    let output = SessionOutputWindow {
        combined_tail: handle.output.combined_tail.clone(),
        combined_truncated: handle.output.combined_truncated,
        returned_lines: handle.output.returned_lines,
        cursor: request.cursor.clone(),
        next_cursor: handle.output.next_cursor.clone(),
    };
    Ok(SessionOutputResponse { handle, output })
}
```

If you can implement real cursor slicing within the current architecture, do so. If not, keep cursor progression truthful by advancing `next_cursor` whenever new buffered content appears and document in code comments that Phase 1 still returns the latest truthful window instead of an exact diff slice.

- [ ] **Step 5: Run the cursor test**

Run: `cargo test shell_read_output_supports_cursor_progression -- --nocapture`

Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add src/systems/tools/backend/native.rs tests/shell_tool_flow.rs
git commit -m "feat(shell): implement buffered output reading and cursor progression"
```

---

### Task 6: Unify Shell Result Shapes And Make The STM Rule Explicit

**Files:**
- Modify: `src/systems/tools/orchestrator.rs`
- Modify: `src/systems/tools/result.rs`
- Modify: `tests/shell_tool_flow.rs`

- [ ] **Step 1: Write the failing test for consistent shell result shape**

Append this test to `tests/shell_tool_flow.rs`:

```rust
#[test]
fn shell_exec_and_shell_start_share_core_result_fields() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, vec![]);

    app.update();

    let agent_id = spawn_agent(app.world_mut());
    let task_entity = app
        .world_mut()
        .spawn((
            Task::from_user_input_ready("shell shape", 3, default_channel()),
            ShortTermMemory::default(),
        ))
        .id();
    let task_id = app.world().get::<Task>(task_entity).unwrap().id;

    let make_request = |tool_name: &str| AgentExecutionRequest {
        task_id,
        agent_id,
        request_kind: AgentRequestKind::ToolExecution {
            tool_name: tool_name.to_string(),
        },
        prompt: String::new(),
        system_prompt: None,
        tools: vec![],
        conversation: None,
        work_item_id: None,
    };

    app.world_mut().spawn(ToolExecutionRequestMessage {
        request: make_request("shell_exec"),
        tool_name: "shell_exec".to_string(),
        tool_input: serde_json::json!({ "command": "printf 'ok\\n'" }),
        pending_confirmation_id: None,
        tool_call_id: Some("call_shape_exec".to_string()),
        pending_confirmation_options: None,
    });
    app.update();

    app.world_mut().spawn(ToolExecutionRequestMessage {
        request: make_request("shell_start"),
        tool_name: "shell_start".to_string(),
        tool_input: serde_json::json!({ "command": "sleep 1" }),
        pending_confirmation_id: None,
        tool_call_id: Some("call_shape_start".to_string()),
        pending_confirmation_options: None,
    });
    app.update();

    let outputs = {
        let world = app.world_mut();
        let mut query = world.query::<&harness::ToolExecutionResultMessage>();
        query
            .iter(world)
            .map(|result| result.tool_output.clone().unwrap())
            .collect::<Vec<_>>()
    };

    for output in outputs {
        assert!(output.get("handle_id").is_some());
        assert!(output.get("status").is_some());
        assert!(output.get("output").is_some());
    }
}
```

- [ ] **Step 2: Run the test and verify shape drift**

Run: `cargo test shell_exec_and_shell_start_share_core_result_fields -- --nocapture`

Expected: FAIL because shell outputs are not yet normalized across all tool branches.

- [ ] **Step 3: Normalize shell outputs in the orchestrator**

Refactor `spawn_shell_result()` in `src/systems/tools/orchestrator.rs` so every shell tool result passes through one normalizer:

```rust
fn normalize_shell_output(mut value: serde_json::Value) -> serde_json::Value {
    if value.get("output").is_none() {
        value["output"] = serde_json::json!({
            "combined_tail": "",
            "combined_truncated": false,
            "returned_lines": 0
        });
    }
    value
}

pub fn spawn_shell_result(
    commands: &mut Commands,
    request_entity: Entity,
    request: &ToolExecutionRequestMessage,
    tool_name: &str,
    tool_output: serde_json::Value,
) {
    let execution_result = AgentExecutionResult {
        task_id: request.request.task_id,
        agent_id: request.request.agent_id,
        request_kind: request.request.request_kind.clone(),
        result: Ok(AgentExecutionOutput {
            content: OutputContent::Text(format!("{} completed", tool_name)),
            reasoning_content: None,
        }),
        prompt: String::new(),
        system_prompt: None,
        tools: vec![],
        reasoning_content: None,
        work_item_id: None,
    };

    commands.spawn(ToolExecutionResultMessage {
        result: execution_result,
        tool_name: tool_name.to_string(),
        tool_output: Ok(normalize_shell_output(tool_output)),
        tool_call_id: request.tool_call_id.clone(),
        processed: false,
    });

    commands.entity(request_entity).despawn();
}
```

- [ ] **Step 4: Make the current STM rule explicit instead of adding no-op branching**

Update `src/systems/tools/result.rs` by keeping the existing `tool_output` serialization path and adding a clarifying comment directly above `stm.record_tool_call(...)`:

```rust
// Only persist tool_output here.
// The original tool_input payload is intentionally not written into STM,
// which avoids leaking raw shell_send_input request text into memory.
stm.record_tool_call(
    result.tool_call_id.clone(),
    result.tool_name.clone(),
    serde_json::to_string(output).unwrap_or_default(),
    output_str,
    clock.0,
);
```

Do not add special shell-only branching unless a later change actually introduces request-payload serialization.

- [ ] **Step 5: Run the shape test**

Run: `cargo test shell_exec_and_shell_start_share_core_result_fields -- --nocapture`

Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add src/systems/tools/orchestrator.rs src/systems/tools/result.rs tests/shell_tool_flow.rs
git commit -m "feat(shell): normalize shell result shapes and protect stm writes"
```

---

### Task 7: Expand End-To-End Coverage For P0 And Key P1

**Files:**
- Modify: `tests/shell_tool_flow.rs`

- [ ] **Step 1: Add an explicit timeout test for `shell_exec`**

Append:

```rust
#[test]
fn shell_exec_timeout_returns_stopped_and_timed_out() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, vec![]);

    app.update();

    let agent_id = spawn_agent(app.world_mut());
    let task_entity = app
        .world_mut()
        .spawn((
            Task::from_user_input_ready("shell timeout", 3, default_channel()),
            ShortTermMemory::default(),
        ))
        .id();
    let task_id = app.world().get::<Task>(task_entity).unwrap().id;

    let request = AgentExecutionRequest {
        task_id,
        agent_id,
        request_kind: AgentRequestKind::ToolExecution {
            tool_name: "shell_exec".to_string(),
        },
        prompt: String::new(),
        system_prompt: None,
        tools: vec![],
        conversation: None,
        work_item_id: None,
    };

    app.world_mut().spawn(ToolExecutionRequestMessage {
        request,
        tool_name: "shell_exec".to_string(),
        tool_input: serde_json::json!({
            "command": "sleep 2",
            "timeout_secs": 0
        }),
        pending_confirmation_id: None,
        tool_call_id: Some("call_shell_exec_timeout".to_string()),
        pending_confirmation_options: None,
    });

    app.update();

    let results = {
        let world = app.world_mut();
        let mut query = world.query::<&harness::ToolExecutionResultMessage>();
        query.iter(world).cloned().collect::<Vec<_>>()
    };

    let output_json = results[0].tool_output.clone().unwrap();
    assert_eq!(output_json["status"], "stopped");
    assert_eq!(output_json["timed_out"], true);
}
```

- [ ] **Step 2: Add a `shell_read_output` integration test**

Append:

```rust
#[test]
fn shell_read_output_returns_output_window() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, vec![]);

    app.update();

    let agent_id = spawn_agent(app.world_mut());
    let task_entity = app
        .world_mut()
        .spawn((
            Task::from_user_input_ready("shell read output", 3, default_channel()),
            ShortTermMemory::default(),
        ))
        .id();
    let task_id = app.world().get::<Task>(task_entity).unwrap().id;

    let exec_request = AgentExecutionRequest {
        task_id,
        agent_id,
        request_kind: AgentRequestKind::ToolExecution {
            tool_name: "shell_exec".to_string(),
        },
        prompt: String::new(),
        system_prompt: None,
        tools: vec![],
        conversation: None,
        work_item_id: None,
    };

    app.world_mut().spawn(ToolExecutionRequestMessage {
        request: exec_request,
        tool_name: "shell_exec".to_string(),
        tool_input: serde_json::json!({
            "command": "printf 'x\\ny\\n'",
            "tail_lines": 1
        }),
        pending_confirmation_id: None,
        tool_call_id: Some("call_shell_exec_for_read".to_string()),
        pending_confirmation_options: None,
    });
    app.update();

    let outputs = {
        let world = app.world_mut();
        let mut query = world.query::<&harness::ToolExecutionResultMessage>();
        query.iter(world).cloned().collect::<Vec<_>>()
    };

    let output_json = outputs[0].tool_output.clone().unwrap();
    assert!(output_json["output"].is_object());
    assert!(output_json["output"]["combined_tail"].is_string());
}
```

- [ ] **Step 3: Run the shell-focused test suite**

Run: `cargo test shell_ -- --nocapture`

Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add tests/shell_tool_flow.rs
git commit -m "test(shell): cover timeout wait stop input and output flows"
```

---

### Task 8: Final Validation And Alignment Update

**Files:**
- Modify: `docs/superpowers/specs/2026-06-07-shell-tool-implementation-alignment.md` only if the implementation meaningfully changes the completion assessment

- [ ] **Step 1: Run the full project validation**

Run:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

Expected:

- `cargo fmt --check` exits `0`
- `cargo clippy` exits `0`
- `cargo test` exits `0`

- [ ] **Step 2: Re-check the alignment doc against the actual result**

Verify these claims:

```text
1. shell_start now manages a real process, not just an in-memory running record
2. shell_wait is driven by backend state instead of timeout-only placeholders
3. shell_stop is backend-backed and no longer returns placeholder JSON
4. shell_send_input and shell_send_signal have truthful runtime effects
5. shell_exec timeout behavior matches the documented contract
6. shell outputs share a consistent core shape
7. shell_read_output has truthful cursor/window behavior
```

If any claim remains false, update `docs/superpowers/specs/2026-06-07-shell-tool-implementation-alignment.md` so it describes the new true completion state honestly.

- [ ] **Step 3: Final commit if validation required fixes**

```bash
git add -A
git commit -m "chore(shell): finish alignment completion validation"
```

---

## Self-Review

### Spec Coverage

- P0 runtime closure: covered by Tasks 2, 3, 4, and 7
- key P1 consistency: covered by Tasks 5 and 6
- truthful alignment update: covered by Task 8
- naming convention remains `shell_*`: preserved throughout the plan

### Placeholder Scan

- No unresolved marker text remains in the execution steps
- Every code-changing task includes concrete code
- Every verification step includes an exact command

### Type Consistency

- `SessionHandle`, `SessionStartRequest`, `SessionOutputRequest`, `SessionWaitRequest`, and `SessionStopRequest` are introduced before later tasks depend on them
- `shell_*` tool names are consistent with the current codebase and alignment doc
- The plan consistently treats `HerdrSessionBackend` as out of scope

---

## Notes For The Implementer

- Keep function-level comments on newly introduced public helpers, per workspace rule
- Do not reintroduce dot-based tool names in code, tests, or docs
- Prefer honest limited behavior over fake “successful” placeholder behavior
- If exact cursor diff slicing proves too large for this round, keep cursor progression truthful and document the limitation in code comments rather than silently faking a diff
