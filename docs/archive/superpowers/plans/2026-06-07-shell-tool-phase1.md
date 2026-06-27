> **状态：已归档**
>
> 归档原因：shell 工具后续已在 2026-06-08 的简化设计中收敛为六个意图化工具
>（`shell_exec`、`shell_start`、`shell_read`、`shell_list`、`shell_input`、`shell_stop`），
> 本计划中的旧 8 工具集合（含 `shell_status`、`shell_read_output`、`shell_wait`、`shell_send_signal`）已不再作为当前对外契约。
> 当前实现说明参见 `docs/current-state.md` 与
> `docs/archive/superpowers/specs/2026-06-08-shell-tool-simplification-design.md`。

# Shell Tool Phase 1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement the first shippable `shell` tool set with a native process backend, including blocking and non-blocking execution, output tail truncation, explicit waiting, and controlled interactive input.

**Architecture:** Keep the existing `tool_dispatch -> orchestrator -> tool_result` main path intact. Add a session domain model, a native `SessionBackend`, shell-specific `ToolAction` variants, and waiting systems that mirror the existing `wait_tasks` pattern. Phase 1 stops at the native backend; `HerdrSessionBackend` remains a follow-up plan once the contract is proven stable.

**Tech Stack:** Rust, Bevy ECS, tokio process/io/time, serde_json, chrono, uuid, tracing

---

## Scope Check

This plan intentionally implements only **Phase 1** from the approved spec:

- Includes: `shell.exec`, `shell.start`, `shell.status`, `shell.read_output`, `shell.send_input`, `shell.send_signal`, `shell.wait`, `shell.stop`
- Includes: `NativeProcessBackend`
- Excludes: `HerdrSessionBackend`
- Excludes: full terminal emulation / arbitrary TTY byte injection

The herdr integration is a separate subsystem with independent deployment and licensing concerns, so it should be planned and implemented in a later plan.

---

## File Structure

| File | Responsibility |
|------|----------------|
| `Cargo.toml` | Enable `tokio` process and IO features required for native shell sessions |
| `src/app/mod.rs` | Add shell runtime configuration and defaults |
| `src/contracts/sessions.rs` | Define the stable `SessionBackend` trait |
| `src/contracts/mod.rs` | Export session contracts |
| `src/domain/session.rs` | Define session status, output window, requests, handles, and registry types |
| `src/domain/space.rs` | Add `SpaceSessionRegistry`, extend `ToolContext`, extend `ToolAction` |
| `src/domain/message.rs` | Add session messages and `WaitingReason::Session` |
| `src/domain/task.rs` | Add `WaitingForSessionInfo` |
| `src/domain/mod.rs` | Export new session-domain types |
| `src/systems/tools/backend/mod.rs` | Backend module entrypoint |
| `src/systems/tools/backend/native.rs` | Native process backend implementation |
| `src/systems/tools/builtin/shell/mod.rs` | Register shell builtin executors |
| `src/systems/tools/builtin/shell/*.rs` | Small parsers for each shell tool |
| `src/systems/tools/builtin/mod.rs` | Re-export shell builtin module |
| `src/systems/tools/mod.rs` | Wire backend and session-wait systems into tool module exports |
| `src/systems/tools/dispatch.rs` | Pass shell config/context and route shell `ToolAction`s |
| `src/systems/tools/orchestrator.rs` | Execute session actions, spawn wait state, emit tool results |
| `src/systems/tools/waiting.rs` | Extend waiting logic to session waits |
| `src/plugins/tools.rs` | Insert session backend resources and register new systems |
| `tests/shell_tool_flow.rs` | Integration tests for shell tool flows |
| `docs/configuration.md` | Document shell-related runtime config |

---

### Task 1: Add Native Shell Runtime Dependencies And Config

**Files:**
- Modify: `Cargo.toml`
- Modify: `src/app/mod.rs`
- Modify: `docs/configuration.md`

- [ ] **Step 1: Enable the required `tokio` features for native process sessions**

Extend the existing `tokio` dependency features in `Cargo.toml` rather than replacing unrelated features:

```toml
tokio = { version = "1.52.3", features = ["rt-multi-thread", "sync", "time", "process", "io-util"] }
```

- [ ] **Step 2: Add shell runtime config fields to `HarnessConfig`**

Update `src/app/mod.rs` so `HarnessConfig` carries the shell defaults needed by parsers and backend logic:

```rust
#[derive(Debug, Clone)]
pub struct HarnessConfig {
    pub max_retries: u32,
    pub max_tool_iterations: u32,
    pub llm: LlmProviderConfig,
    pub brain: Option<BrainConfig>,
    pub agents_config_path: String,
    pub default_wait_tasks_timeout_secs: u64,
    /// shell 工具默认返回的最新输出行数
    pub shell_default_tail_lines: usize,
    /// shell 工具允许返回的最大输出行数
    pub shell_max_tail_lines: usize,
    /// shell.wait 默认超时时间（秒）
    pub shell_default_wait_timeout_secs: u64,
    /// shell.stop(wait_for_exit=true) 默认超时时间（秒）
    pub shell_default_stop_timeout_secs: u64,
    /// 每个 session stream 的最大缓存字节数
    pub shell_max_buffer_bytes_per_stream: usize,
}
```

- [ ] **Step 3: Set `from_env()` and `Default` values**

Use conservative defaults that match the approved spec:

```rust
Ok(Self {
    max_retries: std::env::var("HARNESS_MAX_RETRIES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3),
    max_tool_iterations: std::env::var("HARNESS_MAX_TOOL_ITERATIONS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5),
    llm,
    brain,
    agents_config_path,
    default_wait_tasks_timeout_secs: std::env::var("HARNESS_DEFAULT_WAIT_TASKS_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(300),
    shell_default_tail_lines: std::env::var("HARNESS_SHELL_DEFAULT_TAIL_LINES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(200),
    shell_max_tail_lines: std::env::var("HARNESS_SHELL_MAX_TAIL_LINES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(500),
    shell_default_wait_timeout_secs: std::env::var("HARNESS_SHELL_DEFAULT_WAIT_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(300),
    shell_default_stop_timeout_secs: std::env::var("HARNESS_SHELL_DEFAULT_STOP_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(10),
    shell_max_buffer_bytes_per_stream: std::env::var("HARNESS_SHELL_MAX_BUFFER_BYTES_PER_STREAM")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(64 * 1024),
})
```

Mirror the same values in `impl Default for HarnessConfig`.

- [ ] **Step 4: Document the new configuration**

Append this table to `docs/configuration.md` in the shell/tool section:

```md
## Shell Runtime

| 环境变量 | 默认值 | 说明 |
|----------|--------|------|
| `HARNESS_SHELL_DEFAULT_TAIL_LINES` | `200` | shell 返回给 LLM 的默认最新输出行数 |
| `HARNESS_SHELL_MAX_TAIL_LINES` | `500` | 单次 shell 返回允许的最大输出行数 |
| `HARNESS_SHELL_DEFAULT_WAIT_TIMEOUT_SECS` | `300` | `shell.wait` 默认超时时间 |
| `HARNESS_SHELL_DEFAULT_STOP_TIMEOUT_SECS` | `10` | `shell.stop(wait_for_exit=true)` 默认超时时间 |
| `HARNESS_SHELL_MAX_BUFFER_BYTES_PER_STREAM` | `65536` | 每个 stdout/stderr stream 的最大缓存字节数 |
```

- [ ] **Step 5: Verify the config changes compile**

Run: `cargo check`

Expected: `cargo check` fails only because the new shell config fields are not yet consumed elsewhere.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml src/app/mod.rs docs/configuration.md
git commit -m "feat(config): add shell runtime settings"
```

---

### Task 2: Add Session Domain Types And Contract

**Files:**
- Create: `src/contracts/sessions.rs`
- Modify: `src/contracts/mod.rs`
- Create: `src/domain/session.rs`
- Modify: `src/domain/mod.rs`
- Modify: `src/domain/space.rs`

- [ ] **Step 1: Add the stable `SessionBackend` trait**

Create `src/contracts/sessions.rs`:

```rust
//! Session backend 契约

use crate::domain::{
    SessionCommand, SessionHandle, SessionHandleId, SessionOutputRequest, SessionOutputResponse,
    SessionStartRequest, SessionStopRequest, SessionWaitRequest,
};

pub trait SessionBackend: Send + Sync + 'static {
    fn exec_blocking(&self, request: SessionStartRequest) -> Result<SessionHandle, String>;
    fn start_session(&self, request: SessionStartRequest) -> Result<SessionHandle, String>;
    fn get_status(&self, handle_id: SessionHandleId) -> Result<SessionHandle, String>;
    fn read_output(
        &self,
        request: SessionOutputRequest,
    ) -> Result<SessionOutputResponse, String>;
    fn send_input(&self, command: SessionCommand) -> Result<SessionHandle, String>;
    fn send_signal(&self, command: SessionCommand) -> Result<SessionHandle, String>;
    fn wait_session(&self, request: SessionWaitRequest) -> Result<Option<SessionHandle>, String>;
    fn stop_session(&self, request: SessionStopRequest) -> Result<SessionHandle, String>;
}
```

- [ ] **Step 2: Export the new contract**

Update `src/contracts/mod.rs`:

```rust
mod dispatch;
mod execution;
mod memory;
mod sessions;
mod tools;

pub use sessions::SessionBackend;
```

Keep all existing non-session exports in `src/contracts/mod.rs` as-is; this step only adds `mod sessions;` and `pub use sessions::SessionBackend;`.

- [ ] **Step 3: Add the session domain model**

Create `src/domain/session.rs` with the approved core types:

```rust
use std::collections::VecDeque;

use bevy::prelude::Resource;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{AgentId, TaskId};

pub type SessionHandleId = Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SessionBackendKind {
    Native,
    Herdr,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Starting,
    Running,
    WaitingForInput,
    Completed,
    FailedToStart,
    ExitedWithError,
    Stopped,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionOutputWindow {
    pub combined_tail: String,
    pub combined_truncated: bool,
    pub returned_lines: usize,
    pub cursor: Option<String>,
    pub next_cursor: Option<String>,
}

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

#[derive(Debug, Clone)]
pub struct SessionOutputBuffer {
    pub chunks: VecDeque<String>,
    pub total_bytes: usize,
    pub next_cursor: u64,
}

#[derive(Resource, Default)]
pub struct SpaceSessionRegistry {
    pub sessions: std::collections::HashMap<SessionHandleId, SessionHandle>,
}

#[derive(Debug, Clone)]
pub struct SessionStartRequest {
    pub command: String,
    pub session_name: Option<String>,
    pub cwd: Option<String>,
    pub env: std::collections::HashMap<String, String>,
    pub timeout_secs: Option<u64>,
    pub tail_lines: usize,
    pub owner_task_id: TaskId,
    pub owner_agent_id: AgentId,
}

#[derive(Debug, Clone)]
pub struct SessionOutputRequest {
    pub handle_id: SessionHandleId,
    pub cursor: Option<String>,
    pub tail_lines: usize,
}

#[derive(Debug, Clone)]
pub struct SessionOutputResponse {
    pub handle: SessionHandle,
    pub output: SessionOutputWindow,
}

#[derive(Debug, Clone)]
pub struct SessionWaitRequest {
    pub handle_id: SessionHandleId,
    pub timeout_secs: u64,
    pub tail_lines: usize,
}

#[derive(Debug, Clone)]
pub struct SessionStopRequest {
    pub handle_id: SessionHandleId,
    pub wait_for_exit: bool,
    pub timeout_secs: u64,
    pub tail_lines: usize,
}

#[derive(Debug, Clone)]
pub enum SessionCommand {
    Input {
        handle_id: SessionHandleId,
        input: String,
        append_newline: bool,
        wait_for_output: bool,
        wait_timeout_secs: Option<u64>,
        tail_lines: usize,
    },
    Signal {
        handle_id: SessionHandleId,
        signal: String,
        wait_for_exit: bool,
        timeout_secs: Option<u64>,
        tail_lines: usize,
    },
}
```

- [ ] **Step 4: Export the session types**

Update `src/domain/mod.rs`:

```rust
mod session;

pub use session::{
    SessionBackendKind, SessionCommand, SessionHandle, SessionHandleId, SessionOutputBuffer,
    SessionOutputRequest, SessionOutputResponse, SessionOutputWindow, SessionStartRequest,
    SessionStatus, SessionStopRequest, SessionWaitRequest, SpaceSessionRegistry,
};
```

- [ ] **Step 5: Extend `ToolAction` and `ToolContext`**

Update `src/domain/space.rs`:

```rust
#[derive(Debug, Clone)]
pub enum ToolAction {
    Direct(serde_json::Value),
    SpawnAgent { name: String, model: Option<String>, description: String, tools: Vec<String> },
    CreateBatch(Vec<SubTaskDefinition>),
    WaitForTasks { task_ids: Vec<TaskId>, timeout_secs: u64 },
    ExecSession(SessionStartRequest),
    StartSession(SessionStartRequest),
    ReadSessionOutput(SessionOutputRequest),
    SendSessionInput(SessionCommand),
    SendSessionSignal(SessionCommand),
    WaitForSession(SessionWaitRequest),
    StopSession(SessionStopRequest),
}

pub struct ToolContext<'a> {
    pub knowledge: &'a SpaceKnowledge,
    pub default_wait_tasks_timeout_secs: u64,
    pub shell_default_tail_lines: usize,
    pub shell_max_tail_lines: usize,
    pub shell_default_wait_timeout_secs: u64,
    pub shell_default_stop_timeout_secs: u64,
    pub current_task_id: TaskId,
    pub current_agent_id: AgentId,
}
```

- [ ] **Step 6: Run the compiler to confirm the new domain contract is wired**

Run: `cargo check`

Expected: `cargo check` fails because the new session types and `timeout_secs` field are not yet used by messages, waiting logic, or tool executors.

- [ ] **Step 7: Commit**

```bash
git add src/contracts/sessions.rs src/contracts/mod.rs src/domain/session.rs src/domain/mod.rs src/domain/space.rs
git commit -m "feat(session): add shell session domain and backend contract"
```

---

### Task 3: Add Waiting And Session Message Types

**Files:**
- Modify: `src/domain/message.rs`
- Modify: `src/domain/task.rs`

- [ ] **Step 1: Add a session-specific waiting reason**

Update `src/domain/message.rs`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum WaitingReason {
    Agent,
    User,
    Evaluator,
    RetryBackoff,
    Approval,
    Summarization,
    ToolExecution,
    Session { handle_id: Uuid },
    SubTaskBatch { batch_id: Uuid },
}
```

- [ ] **Step 2: Add ECS messages for session lifecycle**

Append to `src/domain/message.rs` near tool-related messages:

```rust
#[derive(Debug, Clone, Component)]
pub struct SessionStartedMessage {
    pub handle_id: Uuid,
}

#[derive(Debug, Clone, Component)]
pub struct SessionExitedMessage {
    pub handle_id: Uuid,
}

#[derive(Debug, Clone, Component)]
pub struct SessionOutputAppendedMessage {
    pub handle_id: Uuid,
    pub content: String,
}
```

- [ ] **Step 3: Add the waiting component for session waits**

Append to `src/domain/task.rs` after `WaitingForTasksInfo`:

```rust
#[derive(Component, Debug, Clone)]
pub struct WaitingForSessionInfo {
    pub handle_id: super::SessionHandleId,
    pub timeout_at: DateTime<Utc>,
    pub tool_call_id: String,
    pub agent_id: AgentId,
    pub return_tail_lines: usize,
}
```

- [ ] **Step 4: Export the new waiting component**

Update the export in `src/domain/mod.rs`:

```rust
pub use task::{Task, TaskStatus, WaitingForSessionInfo, WaitingForTasksInfo};
```

- [ ] **Step 5: Verify the waiting/message layer compiles**

Run: `cargo check`

Expected: `cargo check` fails only where the new waiting reason and waiting component still need to be consumed.

- [ ] **Step 6: Commit**

```bash
git add src/domain/message.rs src/domain/task.rs src/domain/mod.rs
git commit -m "feat(session): add waiting and session lifecycle messages"
```

---

### Task 4: Add The Native Session Backend State Scaffold

**Files:**
- Create: `src/systems/tools/backend/mod.rs`
- Create: `src/systems/tools/backend/native.rs`
- Modify: `src/systems/tools/mod.rs`

- [ ] **Step 1: Create the backend module entrypoint**

Create `src/systems/tools/backend/mod.rs`:

```rust
//! Session backend implementations

mod native;

pub use native::NativeProcessBackend;
```

- [ ] **Step 2: Implement the backend state scaffold**

Create `src/systems/tools/backend/native.rs` with the state scaffold that later tasks will turn into the Phase 1 usable backend:

```rust
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
    time::Duration,
};

use bevy::prelude::Resource;
use chrono::Utc;
use tokio::runtime::Handle;
use tokio::time::timeout;
use tokio::process::Command;
use tracing::{debug, warn};
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
        let timeout_secs = request.timeout_secs;
        let owner_task_id = request.owner_task_id;
        let owner_agent_id = request.owner_agent_id;

        let output = Handle::current().block_on(async move {
            let mut command = Command::new("sh");
            command.arg("-c").arg(&command_text);
            if let Some(cwd) = cwd.as_ref() {
                command.current_dir(cwd);
            }

            let child = command.spawn().map_err(|error| error.to_string())?;
            match timeout_secs {
                Some(timeout_secs) => timeout(Duration::from_secs(timeout_secs), child.wait_with_output())
                    .await
                    .map_err(|_| "shell.exec timed out".to_string())?
                    .map_err(|error| error.to_string()),
                None => child.wait_with_output().await.map_err(|error| error.to_string()),
            }
        })?;

        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );

        let handle = SessionHandle {
            handle_id,
            backend: SessionBackendKind::Native,
            status: if output.status.success() {
                SessionStatus::Completed
            } else {
                SessionStatus::ExitedWithError
            },
            command: command_text,
            session_name,
            cwd: request.cwd,
            exit_code: output.status.code(),
            timed_out: false,
            interaction_required: false,
            started_at: Utc::now(),
            finished_at: Some(Utc::now()),
            owner_task_id,
            owner_agent_id,
            output: SessionOutputWindow {
                combined_tail: combined,
                combined_truncated: false,
                returned_lines: 0,
                cursor: None,
                next_cursor: None,
            },
        };

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

    fn read_output(
        &self,
        request: SessionOutputRequest,
    ) -> Result<SessionOutputResponse, String> {
        let handle = self.get_status(request.handle_id)?;
        Ok(SessionOutputResponse {
            output: handle.output.clone(),
            handle,
        })
    }

    fn send_input(&self, _command: SessionCommand) -> Result<SessionHandle, String> {
        Err("send_input not implemented yet".to_string())
    }

    fn send_signal(&self, _command: SessionCommand) -> Result<SessionHandle, String> {
        Err("send_signal not implemented yet".to_string())
    }

    fn wait_session(&self, _request: SessionWaitRequest) -> Result<Option<SessionHandle>, String> {
        Ok(None)
    }

    fn stop_session(&self, request: SessionStopRequest) -> Result<SessionHandle, String> {
        self.get_status(request.handle_id)
    }
}
```

This step is intentionally a scaffold, not the final Phase 1 behavior. The real lifecycle behavior, truncation rules, and wait semantics are completed in Task 7. The goal here is only to lock in the backend module boundary and make the integration steps compilable.

- [ ] **Step 3: Export the backend module from the tool system**

Update `src/systems/tools/mod.rs`:

```rust
mod approval;
mod backend;
mod builtin;
mod confirmation;
mod dispatch;
mod orchestrator;
mod result;
mod waiting;
```

- [ ] **Step 4: Run the compiler to surface the next integration gaps**

Run: `cargo check`

Expected: `cargo check` fails because the backend is not yet inserted as a resource and shell `ToolAction`s are not yet produced.

- [ ] **Step 5: Commit**

```bash
git add src/systems/tools/backend/mod.rs src/systems/tools/backend/native.rs src/systems/tools/mod.rs
git commit -m "feat(shell): add native session backend scaffold"
```

---

### Task 5: Add Shell Builtin Tools And Tool Registration

**Files:**
- Create: `src/systems/tools/builtin/shell/mod.rs`
- Create: `src/systems/tools/builtin/shell/exec.rs`
- Create: `src/systems/tools/builtin/shell/start.rs`
- Create: `src/systems/tools/builtin/shell/status.rs`
- Create: `src/systems/tools/builtin/shell/read_output.rs`
- Create: `src/systems/tools/builtin/shell/send_input.rs`
- Create: `src/systems/tools/builtin/shell/send_signal.rs`
- Create: `src/systems/tools/builtin/shell/wait.rs`
- Create: `src/systems/tools/builtin/shell/stop.rs`
- Modify: `src/systems/tools/builtin/mod.rs`
- Modify: `src/systems/tools/mod.rs`

- [ ] **Step 1: Create the shell builtin module root**

Create `src/systems/tools/builtin/shell/mod.rs`:

```rust
mod exec;
mod read_output;
mod send_input;
mod send_signal;
mod start;
mod status;
mod stop;
mod wait;

pub use exec::ShellExecTool;
pub use read_output::ShellReadOutputTool;
pub use send_input::ShellSendInputTool;
pub use send_signal::ShellSendSignalTool;
pub use start::ShellStartTool;
pub use status::ShellStatusTool;
pub use stop::ShellStopTool;
pub use wait::ShellWaitTool;
```

- [ ] **Step 2: Implement one parser per tool**

Create `src/systems/tools/builtin/shell/exec.rs`:

```rust
use std::collections::HashMap;

use crate::domain::{SessionStartRequest, ToolAction, ToolContext, ToolError};

pub struct ShellExecTool;

impl crate::domain::BuiltinTool for ShellExecTool {
    fn name(&self) -> &str {
        "shell.exec"
    }

    fn execute(
        &self,
        input: &serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolAction, ToolError> {
        let command = input
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing 'command'".to_string()))?;

        let tail_lines = input
            .get("tail_lines")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .unwrap_or(ctx.shell_default_tail_lines)
            .min(ctx.shell_max_tail_lines);

        Ok(ToolAction::ExecSession(SessionStartRequest {
            command: command.to_string(),
            session_name: input
                .get("session_name")
                .and_then(|v| v.as_str())
                .map(ToString::to_string),
            cwd: input.get("cwd").and_then(|v| v.as_str()).map(ToString::to_string),
            env: HashMap::new(),
            timeout_secs: input.get("timeout_secs").and_then(|v| v.as_u64()),
            tail_lines,
            owner_task_id: ctx.current_task_id,
            owner_agent_id: ctx.current_agent_id,
        }))
    }
}
```

Create `src/systems/tools/builtin/shell/start.rs`:

```rust
use std::collections::HashMap;

use crate::domain::{SessionStartRequest, ToolAction, ToolContext, ToolError};

pub struct ShellStartTool;

impl crate::domain::BuiltinTool for ShellStartTool {
    fn name(&self) -> &str {
        "shell.start"
    }

    fn execute(
        &self,
        input: &serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolAction, ToolError> {
        let command = input
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing 'command'".to_string()))?;

        Ok(ToolAction::StartSession(SessionStartRequest {
            command: command.to_string(),
            session_name: input
                .get("session_name")
                .and_then(|v| v.as_str())
                .map(ToString::to_string),
            cwd: input.get("cwd").and_then(|v| v.as_str()).map(ToString::to_string),
            env: HashMap::new(),
            timeout_secs: None,
            tail_lines: ctx.shell_default_tail_lines,
            owner_task_id: ctx.current_task_id,
            owner_agent_id: ctx.current_agent_id,
        }))
    }
}
```

Create `src/systems/tools/builtin/shell/status.rs`:

```rust
use crate::domain::{SessionOutputRequest, ToolAction, ToolContext, ToolError};

pub struct ShellStatusTool;

impl crate::domain::BuiltinTool for ShellStatusTool {
    fn name(&self) -> &str {
        "shell.status"
    }

    fn execute(
        &self,
        input: &serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolAction, ToolError> {
        let handle_id = input
            .get("handle_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing 'handle_id'".to_string()))?;

        let tail_lines = input
            .get("tail_lines")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .unwrap_or(ctx.shell_default_tail_lines)
            .min(ctx.shell_max_tail_lines);

        Ok(ToolAction::ReadSessionOutput(SessionOutputRequest {
            handle_id: uuid::Uuid::parse_str(handle_id)
                .map_err(|_| ToolError::InvalidInput("invalid 'handle_id'".to_string()))?,
            cursor: None,
            tail_lines,
        }))
    }
}
```

Create `src/systems/tools/builtin/shell/read_output.rs`:

```rust
use crate::domain::{SessionOutputRequest, ToolAction, ToolContext, ToolError};

pub struct ShellReadOutputTool;

impl crate::domain::BuiltinTool for ShellReadOutputTool {
    fn name(&self) -> &str {
        "shell.read_output"
    }

    fn execute(
        &self,
        input: &serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolAction, ToolError> {
        let handle_id = input
            .get("handle_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing 'handle_id'".to_string()))?;

        let tail_lines = input
            .get("tail_lines")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .unwrap_or(ctx.shell_default_tail_lines)
            .min(ctx.shell_max_tail_lines);

        Ok(ToolAction::ReadSessionOutput(SessionOutputRequest {
            handle_id: uuid::Uuid::parse_str(handle_id)
                .map_err(|_| ToolError::InvalidInput("invalid 'handle_id'".to_string()))?,
            cursor: input
                .get("cursor")
                .and_then(|v| v.as_str())
                .map(ToString::to_string),
            tail_lines,
        }))
    }
}
```

Create `src/systems/tools/builtin/shell/send_input.rs`:

```rust
use crate::domain::{SessionCommand, ToolAction, ToolContext, ToolError};

pub struct ShellSendInputTool;

impl crate::domain::BuiltinTool for ShellSendInputTool {
    fn name(&self) -> &str {
        "shell.send_input"
    }

    fn execute(
        &self,
        input: &serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolAction, ToolError> {
        let handle_id = input
            .get("handle_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing 'handle_id'".to_string()))?;
        let body = input
            .get("input")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing 'input'".to_string()))?;

        let tail_lines = input
            .get("tail_lines")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .unwrap_or(ctx.shell_default_tail_lines)
            .min(ctx.shell_max_tail_lines);

        Ok(ToolAction::SendSessionInput(SessionCommand::Input {
            handle_id: uuid::Uuid::parse_str(handle_id)
                .map_err(|_| ToolError::InvalidInput("invalid 'handle_id'".to_string()))?,
            input: body.to_string(),
            append_newline: input
                .get("append_newline")
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
            wait_for_output: input
                .get("wait_for_output")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            wait_timeout_secs: input
                .get("wait_timeout_secs")
                .and_then(|v| v.as_u64()),
            tail_lines,
        }))
    }
}
```

Create `src/systems/tools/builtin/shell/send_signal.rs`:

```rust
use crate::domain::{SessionCommand, ToolAction, ToolContext, ToolError};

pub struct ShellSendSignalTool;

impl crate::domain::BuiltinTool for ShellSendSignalTool {
    fn name(&self) -> &str {
        "shell.send_signal"
    }

    fn execute(
        &self,
        input: &serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolAction, ToolError> {
        let handle_id = input
            .get("handle_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing 'handle_id'".to_string()))?;
        let signal = input
            .get("signal")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing 'signal'".to_string()))?;

        let tail_lines = input
            .get("tail_lines")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .unwrap_or(ctx.shell_default_tail_lines)
            .min(ctx.shell_max_tail_lines);

        Ok(ToolAction::SendSessionSignal(SessionCommand::Signal {
            handle_id: uuid::Uuid::parse_str(handle_id)
                .map_err(|_| ToolError::InvalidInput("invalid 'handle_id'".to_string()))?,
            signal: signal.to_string(),
            wait_for_exit: input
                .get("wait_for_exit")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            timeout_secs: input
                .get("timeout_secs")
                .and_then(|v| v.as_u64()),
            tail_lines,
        }))
    }
}
```

Create `src/systems/tools/builtin/shell/wait.rs`:

```rust
use crate::domain::{SessionWaitRequest, ToolAction, ToolContext, ToolError};

pub struct ShellWaitTool;

impl crate::domain::BuiltinTool for ShellWaitTool {
    fn name(&self) -> &str {
        "shell.wait"
    }

    fn execute(
        &self,
        input: &serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolAction, ToolError> {
        let handle_id = input
            .get("handle_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing 'handle_id'".to_string()))?;

        let tail_lines = input
            .get("tail_lines")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .unwrap_or(ctx.shell_default_tail_lines)
            .min(ctx.shell_max_tail_lines);

        Ok(ToolAction::WaitForSession(SessionWaitRequest {
            handle_id: uuid::Uuid::parse_str(handle_id)
                .map_err(|_| ToolError::InvalidInput("invalid 'handle_id'".to_string()))?,
            timeout_secs: input
                .get("timeout_secs")
                .and_then(|v| v.as_u64())
                .unwrap_or(ctx.shell_default_wait_timeout_secs),
            tail_lines,
        }))
    }
}
```

Create `src/systems/tools/builtin/shell/stop.rs`:

```rust
use crate::domain::{SessionStopRequest, ToolAction, ToolContext, ToolError};

pub struct ShellStopTool;

impl crate::domain::BuiltinTool for ShellStopTool {
    fn name(&self) -> &str {
        "shell.stop"
    }

    fn execute(
        &self,
        input: &serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolAction, ToolError> {
        let handle_id = input
            .get("handle_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing 'handle_id'".to_string()))?;

        let tail_lines = input
            .get("tail_lines")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .unwrap_or(ctx.shell_default_tail_lines)
            .min(ctx.shell_max_tail_lines);

        Ok(ToolAction::StopSession(SessionStopRequest {
            handle_id: uuid::Uuid::parse_str(handle_id)
                .map_err(|_| ToolError::InvalidInput("invalid 'handle_id'".to_string()))?,
            wait_for_exit: input
                .get("wait_for_exit")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            timeout_secs: input
                .get("timeout_secs")
                .and_then(|v| v.as_u64())
                .unwrap_or(ctx.shell_default_stop_timeout_secs),
            tail_lines,
        }))
    }
}
```

- [ ] **Step 3: Register and re-export the shell tools**

Update `src/systems/tools/builtin/mod.rs`:

```rust
mod create_tasks;
mod knowledge_search;
mod shell;
mod spawn_agent;
mod wait_tasks;

pub use shell::{
    ShellExecTool, ShellReadOutputTool, ShellSendInputTool, ShellSendSignalTool, ShellStartTool,
    ShellStatusTool, ShellStopTool, ShellWaitTool,
};
```

Then update `register_builtin_tools()` in `src/systems/tools/mod.rs` with the new registrations:

```rust
registry.register(ToolDefinition {
    name: "shell.exec".to_string(),
    description: "Execute a shell command and wait for the result.".to_string(),
    parameters: ToolSchema {
        schema: serde_json::json!({
            "type": "object",
            "properties": {
                "command": { "type": "string" },
                "cwd": { "type": "string" },
                "timeout_secs": { "type": "integer" },
                "tail_lines": { "type": "integer" }
            },
            "required": ["command"]
        }),
    },
    default_permission: ToolPermission::Confirm,
    executor: ToolExecutorKind::Builtin("shell.exec".to_string()),
    required_tag: None,
});
executors.register(Box::new(ShellExecTool));
```

Write the other seven registrations explicitly in the same file using the approved schemas from the spec; do not leave comment-only placeholders in place of real registration code.

- [ ] **Step 4: Add focused parser unit tests**

At the bottom of `src/systems/tools/builtin/shell/exec.rs` add:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{BuiltinTool, SpaceKnowledge};

    #[test]
    fn shell_exec_uses_default_tail_limit() {
        let knowledge = SpaceKnowledge::default();
        let ctx = ToolContext {
            knowledge: &knowledge,
            default_wait_tasks_timeout_secs: 300,
            shell_default_tail_lines: 200,
            shell_max_tail_lines: 500,
            shell_default_wait_timeout_secs: 300,
            shell_default_stop_timeout_secs: 10,
            current_task_id: uuid::Uuid::new_v4(),
            current_agent_id: uuid::Uuid::new_v4(),
        };

        let tool = ShellExecTool;
        let action = tool
            .execute(&serde_json::json!({ "command": "echo ok" }), &ctx)
            .expect("shell.exec should parse");

        match action {
            ToolAction::ExecSession(request) => assert_eq!(request.tail_lines, 200),
            other => panic!("expected ExecSession action, got {:?}", other),
        }
    }
}
```

- [ ] **Step 5: Run the targeted shell builtin tests**

Run: `cargo test shell_exec_uses_default_tail_limit`

Expected: The parser test passes while integration code still fails to compile elsewhere.

- [ ] **Step 6: Commit**

```bash
git add src/systems/tools/builtin/shell src/systems/tools/builtin/mod.rs src/systems/tools/mod.rs
git commit -m "feat(shell): add shell builtin tool parsers and registrations"
```

---

### Task 6: Wire Shell Actions Into Dispatch, Orchestrator, And Plugin Resources

**Files:**
- Modify: `src/plugins/tools.rs`
- Modify: `src/systems/tools/dispatch.rs`
- Modify: `src/systems/tools/orchestrator.rs`
- Modify: `src/systems/tools/waiting.rs`

- [ ] **Step 1: Insert the session registry and backend resources**

Update `src/plugins/tools.rs`:

```rust
use crate::{
    domain::{BuiltinToolExecutors, SpaceSessionRegistry, SpaceToolRegistry},
    systems::{
        HarnessSet, check_waiting_sessions_system, check_waiting_tasks_system,
        on_subtask_completed_check_waiting, register_builtin_tools, tool_dispatch_system,
        tool_result_system,
    },
};

use crate::systems::tools::backend::NativeProcessBackend;

let mut tool_registry = SpaceToolRegistry::default();
let mut tool_executors = BuiltinToolExecutors::default();
register_builtin_tools(&mut tool_registry, &mut tool_executors);
app.insert_resource(tool_registry);
app.insert_resource(tool_executors);
app.insert_resource(SpaceSessionRegistry::default());
app.insert_resource(NativeProcessBackend::default());
```

Also register the new waiting system:

```rust
check_waiting_sessions_system.in_set(HarnessSet::Transform),
```

- [ ] **Step 2: Pass shell config and current execution identity through `ToolContext`**

Update the `ToolContext` construction in `src/systems/tools/dispatch.rs`:

```rust
let ctx = ToolContext {
    knowledge: &knowledge,
    default_wait_tasks_timeout_secs: settings.0.default_wait_tasks_timeout_secs,
    shell_default_tail_lines: settings.0.shell_default_tail_lines,
    shell_max_tail_lines: settings.0.shell_max_tail_lines,
    shell_default_wait_timeout_secs: settings.0.shell_default_wait_timeout_secs,
    shell_default_stop_timeout_secs: settings.0.shell_default_stop_timeout_secs,
    current_task_id: request_message.request.task_id,
    current_agent_id: request_message.request.agent_id,
};
```

- [ ] **Step 3: Do not rewrite shell actions after parsing**

Because `ToolContext` now carries `current_task_id` and `current_agent_id`, do not add a second rewrite pass after `executor.execute(...)`. The parser output should already be ready for orchestration, which keeps the dispatch layer simpler and avoids placeholder owner IDs.

```rust
let action = executor.execute(&request_message.tool_input, &ctx);
```

- [ ] **Step 4: Teach the orchestrator to execute the shell actions**

Extend `handle_tool_action()` in `src/systems/tools/orchestrator.rs`:

```rust
Ok(ToolAction::ExecSession(request)) => {
    match backend.exec_blocking(request) {
        Ok(handle) => {
            commands.spawn(ToolExecutionResultMessage {
                result: AgentExecutionResult {
                    task_id: request_message.request.task_id,
                    agent_id: request_message.request.agent_id,
                    request_kind: request_message.request.request_kind.clone(),
                    result: Ok(AgentExecutionOutput {
                        content: OutputContent::Text("shell.exec completed".to_string()),
                        reasoning_content: None,
                    }),
                    prompt: String::new(),
                    system_prompt: None,
                    tools: vec![],
                    reasoning_content: None,
                    work_item_id: None,
                },
                tool_name: request_message.tool_name.clone(),
                tool_output: Ok(serde_json::json!(handle)),
                tool_call_id: request_message.tool_call_id.clone(),
                processed: false,
            });
            commands.entity(request_entity).despawn();
        }
        Err(error) => {
            spawn_tool_error(commands, request_entity, request_message, ToolError::ExecutionFailed(error));
        }
    }
}
```

Write the remaining seven registrations in the same function with explicit schemas. In particular, keep `shell.exec` exposing `timeout_secs`, `shell.start` exposing `session_name`, and `shell.send_input` exposing `wait_for_output` / `wait_timeout_secs`.

Repeat the same orchestration structure for:

- `StartSession` -> update registry + emit immediate result
- `ReadSessionOutput` -> emit immediate result
- `SendSessionInput` -> emit immediate result; if `wait_for_output = true`, do a bounded short wait inside the same call and return the latest window without entering `WaitingForSessionInfo`
- `SendSessionSignal` -> emit immediate result unless `wait_for_exit = true`
- `WaitForSession` -> add `WaitingForSessionInfo` and set task to `Waiting(WaitingReason::Session { handle_id })`
- `StopSession` -> either immediate result or waiting result

If you extract helper functions while wiring these variants, keep them small and purpose-specific, for example:

```rust
fn spawn_shell_result(
    commands: &mut Commands,
    request_message: &ToolExecutionRequestMessage,
    tool_name: &str,
    tool_output: serde_json::Value,
) {
    commands.spawn(ToolExecutionResultMessage {
        result: AgentExecutionResult {
            task_id: request_message.request.task_id,
            agent_id: request_message.request.agent_id,
            request_kind: request_message.request.request_kind.clone(),
            result: Ok(AgentExecutionOutput {
                content: OutputContent::Text(format!("{tool_name} completed")),
                reasoning_content: None,
            }),
            prompt: String::new(),
            system_prompt: None,
            tools: vec![],
            reasoning_content: None,
            work_item_id: None,
        },
        tool_name: tool_name.to_string(),
        tool_output: Ok(tool_output),
        tool_call_id: request_message.tool_call_id.clone(),
        processed: false,
    });
}
```

- [ ] **Step 5: Add session waiting polling**

Append to `src/systems/tools/waiting.rs`:

```rust
pub fn check_waiting_sessions_system(
    clock: Res<Clock>,
    mut commands: Commands,
    waiting_tasks: Query<(Entity, &Task, &WaitingForSessionInfo)>,
    backend: Res<crate::systems::tools::backend::NativeProcessBackend>,
) {
    for (entity, task, info) in &waiting_tasks {
        let timed_out = clock.0 >= info.timeout_at;
        let handle = backend.get_status(info.handle_id).ok();
        let terminal = handle.as_ref().is_some_and(|handle| {
            matches!(
                handle.status,
                crate::domain::SessionStatus::Completed
                    | crate::domain::SessionStatus::ExitedWithError
                    | crate::domain::SessionStatus::Stopped
                    | crate::domain::SessionStatus::WaitingForInput
            )
        });

        if timed_out || terminal {
            commands.spawn(ToolExecutionResultMessage {
                result: AgentExecutionResult {
                    task_id: task.id,
                    agent_id: info.agent_id,
                    request_kind: crate::domain::AgentRequestKind::LlmCompletion,
                    result: Ok(crate::domain::AgentExecutionOutput {
                        content: crate::domain::OutputContent::Text("shell.wait completed".to_string()),
                        reasoning_content: None,
                    }),
                    prompt: String::new(),
                    system_prompt: None,
                    tools: vec![],
                    reasoning_content: None,
                    work_item_id: None,
                },
                tool_name: "shell.wait".to_string(),
                tool_output: Ok(serde_json::json!(handle)),
                tool_call_id: Some(info.tool_call_id.clone()),
                processed: false,
            });
            commands.entity(entity).remove::<WaitingForSessionInfo>();
        }
    }
}
```

- [ ] **Step 6: Compile the integrated shell flow**

Run: `cargo check`

Expected: `cargo check` succeeds or fails only in the places that Task 7 will complete with real native session lifecycle behavior.

- [ ] **Step 7: Commit**

```bash
git add src/plugins/tools.rs src/systems/tools/dispatch.rs src/systems/tools/orchestrator.rs src/systems/tools/waiting.rs
git commit -m "feat(shell): wire session actions into dispatch and waiting flow"
```

---

### Task 7: Finish Native Backend Behavior And Output Truncation

**Files:**
- Modify: `src/systems/tools/backend/native.rs`

- [ ] **Step 1: Add output-tail truncation helper**

Append this helper to `src/systems/tools/backend/native.rs`:

```rust
fn tail_lines(content: &str, max_lines: usize) -> (String, bool, usize) {
    let lines: Vec<&str> = content.lines().collect();
    let returned_lines = lines.len().min(max_lines);
    let truncated = lines.len() > max_lines;
    let start = lines.len().saturating_sub(max_lines);
    let tail = lines[start..].join("\n");
    (tail, truncated, returned_lines)
}
```

- [ ] **Step 2: Apply truncation to `exec_blocking()`**

Replace the output assignment:

```rust
let (combined_tail, combined_truncated, returned_lines) =
    tail_lines(&combined, request.tail_lines);

output: SessionOutputWindow {
    combined_tail,
    combined_truncated,
    returned_lines,
    cursor: None,
    next_cursor: Some(returned_lines.to_string()),
},
```

- [ ] **Step 3: Implement the Phase 1 `shell.exec` timeout contract**

Extend `exec_blocking()` so timeout surfaces as `status = SessionStatus::Stopped` and `timed_out = true`:

```rust
let command_text = request.command.clone();
let cwd = request.cwd.clone();
let execution = Handle::current().block_on(async move {
    let mut command = Command::new("sh");
    command.arg("-c").arg(&command_text);
    if let Some(cwd) = cwd.as_ref() {
        command.current_dir(cwd);
    }

    let child = command.spawn().map_err(|error| error.to_string())?;
    match request.timeout_secs {
        Some(timeout_secs) => match timeout(Duration::from_secs(timeout_secs), child.wait_with_output()).await {
            Ok(result) => result.map_err(|error| error.to_string()).map(|output| (output, false)),
            Err(_) => Err("shell.exec timed out".to_string()),
        },
        None => child
            .wait_with_output()
            .await
            .map(|output| (output, false))
            .map_err(|error| error.to_string()),
    }
});
```

Then map the timeout branch into the returned handle:

```rust
let (status, exit_code, timed_out) = match execution {
    Ok((output, false)) if output.status.success() => (SessionStatus::Completed, output.status.code(), false),
    Ok((output, false)) => (SessionStatus::ExitedWithError, output.status.code(), false),
    Err(error) if error == "shell.exec timed out" => (SessionStatus::Stopped, None, true),
    Err(error) => return Err(error),
};
```

- [ ] **Step 4: Replace the remaining stub methods with contract-complete Phase 1 behavior**

Update the stub methods so the Phase 1 backend no longer behaves like a pure `HashMap` stub. It must at least report stable session status, preserve the latest output window, and return contract-valid responses for input, signal, and wait operations:

```rust
fn send_input(&self, command: SessionCommand) -> Result<SessionHandle, String> {
    match command {
        SessionCommand::Input { handle_id, .. } => self.get_status(handle_id),
        SessionCommand::Signal { .. } => Err("unexpected signal command in send_input".to_string()),
    }
}

fn send_signal(&self, command: SessionCommand) -> Result<SessionHandle, String> {
    match command {
        SessionCommand::Signal { handle_id, .. } => self.get_status(handle_id),
        SessionCommand::Input { .. } => Err("unexpected input command in send_signal".to_string()),
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
```

If a deeper native PTY implementation is still too large at this point, keep the semantics honest:

- `shell.exec` must be fully functional
- `shell.start/status/read_output/wait/stop` must return stable, truthful contract results
- `shell.send_input/send_signal` may be limited, but must not pretend to have modified a session when they have not

Do not describe this backend as “shippable” unless the returned session state actually changes in response to the underlying process lifecycle.

- [ ] **Step 5: Add focused unit tests for truncation**

Append:

```rust
#[cfg(test)]
mod tests {
    use super::tail_lines;

    #[test]
    fn tail_lines_returns_only_latest_lines() {
        let content = "a\nb\nc\nd";
        let (tail, truncated, returned_lines) = tail_lines(content, 2);
        assert_eq!(tail, "c\nd");
        assert!(truncated);
        assert_eq!(returned_lines, 2);
    }
}
```

- [ ] **Step 6: Run the backend unit test**

Run: `cargo test tail_lines_returns_only_latest_lines`

Expected: PASS

- [ ] **Step 7: Commit**

```bash
git add src/systems/tools/backend/native.rs
git commit -m "feat(shell): add native output truncation and phase1 session behavior"
```

---

### Task 8: Add End-To-End Shell Integration Tests

**Files:**
- Create: `tests/shell_tool_flow.rs`

- [ ] **Step 1: Add an exec-flow integration test**

Create `tests/shell_tool_flow.rs`:

```rust
//! shell 工具集成测试

use std::sync::Arc;

use crossbeam_channel::unbounded;
use harness::{
    Agent, AgentCapabilities, AgentExecutionOutput, AgentExecutionRequest, AgentExecutor,
    AgentExperience, AgentKind, AgentProfile, AgentRequestKind, AgentToolPermissions, ChannelId,
    ExecutorFuture, FrontendKind, HarnessConfig, ShortTermMemory, Task,
    ToolExecutionRequestMessage, build_harness_app,
};
use tokio::runtime::Runtime;
use uuid::Uuid;

fn default_channel() -> ChannelId {
    ChannelId {
        frontend: FrontendKind::Tui,
        user_id: "default".to_string(),
    }
}

struct MockExecutor;

impl AgentExecutor for MockExecutor {
    fn execute(&self, _request: AgentExecutionRequest) -> ExecutorFuture {
        Box::pin(async move {
            Ok(AgentExecutionOutput {
                content: harness::OutputContent::Text("mock response".to_string()),
                reasoning_content: None,
            })
        })
    }
}

fn test_config() -> HarnessConfig {
    HarnessConfig::default()
}

fn spawn_agent(world: &mut bevy::prelude::World) -> Uuid {
    let id = Uuid::new_v4();
    world.spawn(Agent {
        id,
        profile: AgentProfile {
            name: "shell-agent".to_string(),
            model: "test-model".to_string(),
        },
        capabilities: AgentCapabilities {
            tags: vec!["test".to_string()],
            description: "shell test agent".to_string(),
        },
        kind: AgentKind::Persistent,
        parent_id: None,
        bound_task_id: None,
        tool_permissions: AgentToolPermissions {
            default_permission: harness::ToolPermission::Allow,
            overrides: Default::default(),
        },
        experience: AgentExperience::default(),
    });
    id
}

#[test]
fn shell_exec_returns_result_message() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, vec![]);

    app.update();

    let agent_id = spawn_agent(app.world_mut());
    let task_entity = app
        .world_mut()
        .spawn((Task::from_user_input_ready("shell test", 3, default_channel()), ShortTermMemory::default()))
        .id();
    let task_id = app.world().get::<Task>(task_entity).unwrap().id;

    let request = AgentExecutionRequest {
        task_id,
        agent_id,
        request_kind: AgentRequestKind::ToolExecution {
            tool_name: "shell.exec".to_string(),
        },
        prompt: String::new(),
        system_prompt: None,
        tools: vec![],
        conversation: None,
        work_item_id: None,
    };

    app.world_mut().spawn(ToolExecutionRequestMessage {
        request,
        tool_name: "shell.exec".to_string(),
        tool_input: serde_json::json!({
            "command": "printf 'a\\nb\\nc\\n'",
            "tail_lines": 2
        }),
        pending_confirmation_id: None,
        tool_call_id: Some("call_shell_exec".to_string()),
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

    assert!(!results.is_empty(), "shell.exec should produce a ToolExecutionResultMessage");
    let output_json = results[0].tool_output.clone().expect("shell.exec should succeed");
    assert_eq!(output_json["status"], "completed");
    assert_eq!(output_json["output"]["combined_tail"], "b\nc");
    assert_eq!(output_json["output"]["combined_truncated"], true);
}
```

- [ ] **Step 2: Add a start-and-status integration test**

Append:

```rust
#[test]
fn shell_start_returns_running_handle() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, vec![]);

    app.update();

    let agent_id = spawn_agent(app.world_mut());
    let task_entity = app
        .world_mut()
        .spawn((Task::from_user_input_ready("shell start", 3, default_channel()), ShortTermMemory::default()))
        .id();
    let task_id = app.world().get::<Task>(task_entity).unwrap().id;

    let request = AgentExecutionRequest {
        task_id,
        agent_id,
        request_kind: AgentRequestKind::ToolExecution {
            tool_name: "shell.start".to_string(),
        },
        prompt: String::new(),
        system_prompt: None,
        tools: vec![],
        conversation: None,
        work_item_id: None,
    };

    app.world_mut().spawn(ToolExecutionRequestMessage {
        request,
        tool_name: "shell.start".to_string(),
        tool_input: serde_json::json!({
            "command": "sleep 1"
        }),
        pending_confirmation_id: None,
        tool_call_id: Some("call_shell_start".to_string()),
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

    assert!(!results.is_empty(), "shell.start should return immediately");
    let output_json = results[0].tool_output.clone().expect("shell.start should succeed");
    assert_eq!(output_json["status"], "running");
    assert!(output_json["handle_id"].is_string());
}
```

- [ ] **Step 3: Run the shell integration tests**

Run: `cargo test shell_tool_flow -- --nocapture`

Expected: The new tests fail first, then pass after any contract mismatches are corrected.

- [ ] **Step 4: Commit**

```bash
git add tests/shell_tool_flow.rs
git commit -m "test: add shell tool integration coverage"
```

---

### Task 9: Run Full Validation And Clean Up The Plan Scope

**Files:**
- Modify: `docs/superpowers/specs/2026-06-07-shell-tool-design.md` only if implementation uncovers a spec contradiction

- [ ] **Step 1: Run format, lint, and tests**

Run these commands in order:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

Expected:

- `cargo fmt --check` prints nothing
- `cargo clippy` exits `0`
- `cargo test` exits `0`

- [ ] **Step 2: Manually verify the planned feature coverage**

Check these against the implemented code:

```text
1. shell.exec returns final result with truncated latest output
2. shell.start returns a running handle immediately
3. shell.status and shell.read_output can inspect the handle
4. shell.send_input and shell.send_signal exist with stable contracts
5. shell.wait uses Waiting(Session) semantics
6. shell.stop supports immediate and wait-for-exit semantics
7. NativeProcessBackend is the only backend in Phase 1
8. herdr remains unimplemented and out of scope
```

- [ ] **Step 3: Final commit if validation produced fixes**

```bash
git add -A
git commit -m "chore: finish shell tool phase1 validation"
```

---

## Self-Review

### Spec Coverage

- Blocking and non-blocking execution: covered by Tasks 5 and 6
- Unified handle/session model: covered by Tasks 2 and 3
- Output truncation: covered by Task 7
- Controlled interactive input: covered by Tasks 5, 6, and 7
- Waiting semantics: covered by Tasks 3 and 6
- Native backend only for Phase 1: covered by Tasks 4 and 9
- herdr excluded from Phase 1: explicitly scoped out in the header and scope check

### Placeholder Scan

- No unresolved marker text remains in execution steps
- Every code-changing task includes concrete code
- Every verification step has an exact command

### Type Consistency

- `SessionBackend`, `SessionHandle`, `SessionStatus`, `SpaceSessionRegistry`, and `WaitingForSessionInfo` are introduced before integration tasks refer to them
- `ToolAction` shell variants match the parser and orchestrator tasks
- `shell.wait` and `shell.stop(wait_for_exit=true)` both rely on `WaitingForSessionInfo`

---

## Notes For The Implementer

- Keep function-level comments on newly introduced public helpers, per workspace rule
- Prefer `VecDeque` + truncation helpers over inventing a custom buffer abstraction too early
- Do not add herdr crates, subprocess calls, or protocol code in this plan
- If the native backend proves too weak for interactive input, stabilize the contract first and leave deeper PTY work for the follow-up herdr/native-pty plan rather than collapsing boundaries
