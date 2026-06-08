# Shell Tool Simplification Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 将当前偏底层原语的 `shell_*` 工具重构为面向 LLM 的六个高层工具：`shell_exec`、`shell_start`、`shell_read`、`shell_list`、`shell_input`、`shell_stop`。

**Architecture:** 先收缩公开契约与工具注册表，再让 `SessionBackend`、builtin parser、orchestrator 和 plugin 调度统一围绕六个动作工作。对外返回值不再直接暴露内部 `SessionHandle` 结构，而是改为精简响应 DTO，保证 `session_id`、最新快照和活动会话列表语义稳定。

**Tech Stack:** Rust, Bevy ECS, serde/serde_json, std::process, tracing, cargo test, cargo fmt, cargo clippy

---

## Scope Check

本计划只覆盖一个子系统：`shell tool` 精简重构。

本计划包含：

- 删除 `shell_status`、`shell_read_output`、`shell_wait`、`shell_send_signal`
- 新增 `shell_list`
- 将 `shell_send_input` 重构为 `shell_input`
- 将 `shell_read_output` 语义收敛为 `shell_read`
- 将默认超时语义从 `shell_wait` 转移到 `shell_exec`
- 清理等待态与 session waiting 调度链

本计划不包含：

- herdr backend
- 严格增量游标
- 复杂 signal 语义
- 全终端仿真

---

## File Structure

| File | Responsibility |
|------|----------------|
| `src/app/mod.rs` | 调整 shell 默认配置字段，去掉 wait 默认超时，明确 exec 默认超时 |
| `src/domain/space.rs` | 收缩 `ToolAction` 与 `ToolContext`，删除 wait/signal 相关动作与上下文字段 |
| `src/domain/session.rs` | 收缩 session 请求/响应模型，新增面向工具输出的 DTO，删除 cursor/wait/signal 参数结构 |
| `src/contracts/sessions.rs` | 将 backend trait 收敛为 `exec/start/read/list_active/input/stop` 六个高层接口 |
| `src/systems/tools/mod.rs` | 重写 tool registry，只注册六个 shell 工具并更新 schema/description |
| `src/systems/tools/builtin/shell/mod.rs` | 更新 builtin 导出，移除旧模块、导出新模块 |
| `src/systems/tools/builtin/shell/exec.rs` | 保留阻塞执行 parser，读取新的默认 exec 超时 |
| `src/systems/tools/builtin/shell/start.rs` | 保留异步启动 parser，移除无关字段 |
| `src/systems/tools/builtin/shell/read.rs` | 新增最新快照读取 parser，取代 status/read_output 双入口 |
| `src/systems/tools/builtin/shell/list.rs` | 新增活动会话列表 parser |
| `src/systems/tools/builtin/shell/input.rs` | 新增受控输入 parser，取代 send_input |
| `src/systems/tools/builtin/shell/stop.rs` | 简化停止 parser，移除 wait_for_exit 等字段 |
| `src/systems/tools/backend/native.rs` | 实现新的 backend trait，输出快照、活动会话列表、简化 stop/input 语义 |
| `src/systems/tools/orchestrator.rs` | 改为处理新的 ToolAction，并通过 DTO 返回精简结果 |
| `src/plugins/tools.rs` | 删除 `check_waiting_sessions_system` 装配 |
| `src/systems/tools/waiting.rs` | 删除 shell session waiting 分支，保留非 shell 等待逻辑或移除不再使用的部分 |
| `tests/shell_tool_flow.rs` | 更新集成测试，覆盖新的六工具语义并删除 wait/signal 相关测试 |
| `docs/superpowers/specs/2026-06-08-shell-tool-simplification-design.md` | 若实现过程中发现与 spec 不一致的命名或边界，做同步修正 |

---

### Task 1: 收缩公开契约与工具注册

**Files:**
- Modify: `src/app/mod.rs`
- Modify: `src/domain/space.rs`
- Modify: `src/domain/session.rs`
- Modify: `src/contracts/sessions.rs`
- Modify: `src/systems/tools/mod.rs`
- Modify: `src/systems/tools/builtin/shell/mod.rs`
- Modify: `src/systems/tools/builtin/shell/exec.rs`
- Modify: `src/systems/tools/builtin/shell/start.rs`
- Create: `src/systems/tools/builtin/shell/read.rs`
- Create: `src/systems/tools/builtin/shell/list.rs`
- Create: `src/systems/tools/builtin/shell/input.rs`
- Modify: `src/systems/tools/builtin/shell/stop.rs`
- Delete: `src/systems/tools/builtin/shell/status.rs`
- Delete: `src/systems/tools/builtin/shell/read_output.rs`
- Delete: `src/systems/tools/builtin/shell/send_input.rs`
- Delete: `src/systems/tools/builtin/shell/send_signal.rs`
- Delete: `src/systems/tools/builtin/shell/wait.rs`
- Test: `tests/shell_tool_flow.rs`

- [ ] **Step 1: 先写一个失败测试，锁定工具注册表已经切换到六工具集合**

在 `tests/shell_tool_flow.rs` 追加这个测试：

```rust
#[test]
fn shell_registry_only_exposes_six_simplified_tools() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockExecutor);
    let (_input_tx, input_rx) = unbounded();
    let app = build_harness_app(test_config(), runtime, executor, input_rx, vec![]);

    let registry = app.world().resource::<harness::SpaceToolRegistry>();

    for name in [
        "shell_exec",
        "shell_start",
        "shell_read",
        "shell_list",
        "shell_input",
        "shell_stop",
    ] {
        assert!(registry.get(name).is_some(), "missing {name}");
    }

    for name in [
        "shell_status",
        "shell_read_output",
        "shell_send_input",
        "shell_send_signal",
        "shell_wait",
    ] {
        assert!(registry.get(name).is_none(), "legacy tool still exposed: {name}");
    }
}
```

- [ ] **Step 2: 运行测试，确认它先失败**

Run:

```bash
cargo test shell_registry_only_exposes_six_simplified_tools --test shell_tool_flow -v
```

Expected: FAIL，提示 `shell_read`、`shell_list`、`shell_input` 尚未注册，或旧工具仍然存在。

- [ ] **Step 3: 重写 session 契约与对外 DTO**

修改 `src/domain/session.rs`，将请求/响应收缩为“高层意图 + 快照返回”，核心片段应接近下面结构：

```rust
pub type SessionHandleId = Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionOutputSnapshot {
    pub output: String,
    pub returned_lines: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone)]
pub struct SessionReadRequest {
    pub handle_id: SessionHandleId,
    pub tail_lines: usize,
}

#[derive(Debug, Clone)]
pub struct SessionInputRequest {
    pub handle_id: SessionHandleId,
    pub input: String,
    pub append_newline: bool,
}

#[derive(Debug, Clone)]
pub struct SessionSummary {
    pub handle_id: SessionHandleId,
    pub command: String,
    pub cwd: Option<String>,
    pub status: SessionStatus,
    pub exit_code: Option<i32>,
    pub interaction_required: bool,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub output: SessionOutputSnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShellExecResult {
    pub status: SessionStatus,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub interaction_required: bool,
    pub output: String,
    pub returned_lines: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShellSessionResult {
    pub session_id: String,
    pub command: Option<String>,
    pub cwd: Option<String>,
    pub status: SessionStatus,
    pub running: bool,
    pub exit_code: Option<i32>,
    pub interaction_required: bool,
    pub started_at: Option<DateTime<Utc>>,
    pub output: Option<String>,
    pub returned_lines: Option<usize>,
    pub truncated: Option<bool>,
    pub accepted: Option<bool>,
}
```

同时删除：

- `cursor`
- `next_cursor`
- `SessionWaitRequest`
- `SessionCommand::Signal`
- `wait_for_output`
- `wait_timeout_secs`
- `wait_for_exit`

- [ ] **Step 4: 收缩 `ToolAction`、`ToolContext`、`SessionBackend`**

修改 `src/domain/space.rs` 和 `src/contracts/sessions.rs`，让枚举与 trait 只保留六个 shell 高层动作：

```rust
pub enum ToolAction {
    Direct(serde_json::Value),
    SpawnAgent { .. },
    CreateBatch(Vec<SubTaskDefinition>),
    WaitForTasks { task_ids: Vec<TaskId>, timeout_secs: u64 },
    ExecSession(SessionStartRequest),
    StartSession(SessionStartRequest),
    ReadSession(SessionReadRequest),
    ListSessions,
    InputSession(SessionInputRequest),
    StopSession(SessionHandleId),
}
```

```rust
pub trait SessionBackend: Send + Sync + 'static {
    fn exec_blocking(&self, request: SessionStartRequest) -> Result<SessionHandle, String>;
    fn start_session(&self, request: SessionStartRequest) -> Result<SessionHandle, String>;
    fn read_session(&self, request: SessionReadRequest) -> Result<SessionSummary, String>;
    fn list_active_sessions(&self) -> Result<Vec<SessionSummary>, String>;
    fn input_session(&self, request: SessionInputRequest) -> Result<SessionHandle, String>;
    fn stop_session(&self, handle_id: SessionHandleId) -> Result<SessionHandle, String>;
}
```

同时把 `ToolContext` 中的 `shell_default_wait_timeout_secs` 改为 `shell_default_exec_timeout_secs`。

- [ ] **Step 5: 更新配置与 builtin parser**

按以下方向修改：

- `src/app/mod.rs`
  - `shell_default_wait_timeout_secs` 改为 `shell_default_exec_timeout_secs`
  - 环境变量改为 `HARNESS_SHELL_DEFAULT_EXEC_TIMEOUT_SECS`
- `src/systems/tools/builtin/shell/exec.rs`
  - `timeout_secs` 默认取 `ctx.shell_default_exec_timeout_secs`
- `src/systems/tools/builtin/shell/start.rs`
  - 移除 `session_name`
- 新建 `read.rs`
- 新建 `list.rs`
- 新建 `input.rs`
- `stop.rs` 只解析 `session_id`

`read.rs` 最小实现应类似：

```rust
pub struct ShellReadTool;

impl crate::domain::BuiltinTool for ShellReadTool {
    fn name(&self) -> &str { "shell_read" }

    fn execute(
        &self,
        input: &serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolAction, ToolError> {
        let session_id = input
            .get("session_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing 'session_id'".to_string()))?;

        let tail_lines = input
            .get("tail_lines")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .unwrap_or(ctx.shell_default_tail_lines)
            .min(ctx.shell_max_tail_lines);

        Ok(ToolAction::ReadSession(SessionReadRequest {
            handle_id: uuid::Uuid::parse_str(session_id)
                .map_err(|_| ToolError::InvalidInput("invalid 'session_id'".to_string()))?,
            tail_lines,
        }))
    }
}
```

`list.rs` 最小实现应类似：

```rust
pub struct ShellListTool;

impl crate::domain::BuiltinTool for ShellListTool {
    fn name(&self) -> &str { "shell_list" }

    fn execute(
        &self,
        _input: &serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolAction, ToolError> {
        Ok(ToolAction::ListSessions)
    }
}
```

- [ ] **Step 6: 重写 tool registry，只暴露六个工具**

修改 `src/systems/tools/mod.rs` 和 `src/systems/tools/builtin/shell/mod.rs`：

```rust
mod exec;
mod input;
mod list;
mod read;
mod start;
mod stop;

pub use exec::ShellExecTool;
pub use input::ShellInputTool;
pub use list::ShellListTool;
pub use read::ShellReadTool;
pub use start::ShellStartTool;
pub use stop::ShellStopTool;
```

注册表中 shell 部分只保留：

```rust
registry.register(ToolDefinition { name: "shell_exec".to_string(), .. });
registry.register(ToolDefinition { name: "shell_start".to_string(), .. });
registry.register(ToolDefinition { name: "shell_read".to_string(), .. });
registry.register(ToolDefinition { name: "shell_list".to_string(), .. });
registry.register(ToolDefinition { name: "shell_input".to_string(), .. });
registry.register(ToolDefinition { name: "shell_stop".to_string(), .. });
```

- [ ] **Step 7: 运行第一个测试，确认工具注册表已经收敛**

Run:

```bash
cargo test shell_registry_only_exposes_six_simplified_tools --test shell_tool_flow -v
```

Expected: PASS

- [ ] **Step 8: 运行格式化和编译检查，确保契约层可继续推进**

Run:

```bash
cargo fmt --all
cargo test shell_registry_only_exposes_six_simplified_tools --test shell_tool_flow -v
```

Expected: `cargo fmt` 无报错，测试继续 PASS。

- [ ] **Step 9: 提交契约与注册层变更**

Run:

```bash
git add \
  src/app/mod.rs \
  src/domain/space.rs \
  src/domain/session.rs \
  src/contracts/sessions.rs \
  src/systems/tools/mod.rs \
  src/systems/tools/builtin/shell/mod.rs \
  src/systems/tools/builtin/shell/exec.rs \
  src/systems/tools/builtin/shell/start.rs \
  src/systems/tools/builtin/shell/read.rs \
  src/systems/tools/builtin/shell/list.rs \
  src/systems/tools/builtin/shell/input.rs \
  src/systems/tools/builtin/shell/stop.rs \
  tests/shell_tool_flow.rs
git rm \
  src/systems/tools/builtin/shell/status.rs \
  src/systems/tools/builtin/shell/read_output.rs \
  src/systems/tools/builtin/shell/send_input.rs \
  src/systems/tools/builtin/shell/send_signal.rs \
  src/systems/tools/builtin/shell/wait.rs
git commit -m "refactor: simplify shell tool contracts"
```

Expected: commit 成功。

---

### Task 2: 重构 backend 与 orchestrator 为六动作主链

**Files:**
- Modify: `src/systems/tools/backend/native.rs`
- Modify: `src/systems/tools/orchestrator.rs`
- Modify: `src/plugins/tools.rs`
- Modify: `src/systems/tools/waiting.rs`
- Modify: `src/systems/tools/result.rs`
- Test: `tests/shell_tool_flow.rs`

- [ ] **Step 1: 先写失败测试，锁定 `shell_read` 和 `shell_list` 的新返回结构**

在 `tests/shell_tool_flow.rs` 追加两个测试：

```rust
#[test]
fn shell_read_returns_status_and_latest_snapshot() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, vec![]);

    app.update();
    let agent_id = spawn_agent(app.world_mut());
    let task_entity = app
        .world_mut()
        .spawn((Task::from_user_input_ready("shell read", 3, default_channel()), ShortTermMemory::default()))
        .id();
    let task_id = app.world().get::<Task>(task_entity).unwrap().id;

    app.world_mut().spawn(ToolExecutionRequestMessage {
        request: AgentExecutionRequest {
            task_id,
            agent_id,
            request_kind: AgentRequestKind::ToolExecution { tool_name: "shell_start".to_string() },
            prompt: String::new(),
            system_prompt: None,
            tools: vec![],
            conversation: None,
            work_item_id: None,
        },
        tool_name: "shell_start".to_string(),
        tool_input: serde_json::json!({ "command": "printf 'hello\\n'; sleep 1" }),
        pending_confirmation_id: None,
        tool_call_id: Some("call_start_read_case".to_string()),
        pending_confirmation_options: None,
    });
    app.update();

    let session_id = {
        let world = app.world_mut();
        let mut query = world.query::<&harness::ToolExecutionResultMessage>();
        let results = query.iter(world).cloned().collect::<Vec<_>>();
        results[0].tool_output.clone().unwrap()["session_id"]
            .as_str()
            .unwrap()
            .to_string()
    };

    app.world_mut().spawn(ToolExecutionRequestMessage {
        request: AgentExecutionRequest {
            task_id,
            agent_id,
            request_kind: AgentRequestKind::ToolExecution { tool_name: "shell_read".to_string() },
            prompt: String::new(),
            system_prompt: None,
            tools: vec![],
            conversation: None,
            work_item_id: None,
        },
        tool_name: "shell_read".to_string(),
        tool_input: serde_json::json!({ "session_id": session_id, "tail_lines": 20 }),
        pending_confirmation_id: None,
        tool_call_id: Some("call_read_case".to_string()),
        pending_confirmation_options: None,
    });

    app.update();

    let world = app.world_mut();
    let mut query = world.query::<&harness::ToolExecutionResultMessage>();
    let results = query.iter(world).cloned().collect::<Vec<_>>();
    let output = results.last().unwrap().tool_output.clone().unwrap();

    assert!(output["status"].is_string());
    assert!(output["running"].is_boolean());
    assert!(output["output"].is_string());
}
```

```rust
#[test]
fn shell_list_returns_only_active_sessions() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, vec![]);

    app.update();
    let agent_id = spawn_agent(app.world_mut());
    let task_entity = app
        .world_mut()
        .spawn((Task::from_user_input_ready("shell list", 3, default_channel()), ShortTermMemory::default()))
        .id();
    let task_id = app.world().get::<Task>(task_entity).unwrap().id;

    app.world_mut().spawn(ToolExecutionRequestMessage {
        request: AgentExecutionRequest {
            task_id,
            agent_id,
            request_kind: AgentRequestKind::ToolExecution { tool_name: "shell_start".to_string() },
            prompt: String::new(),
            system_prompt: None,
            tools: vec![],
            conversation: None,
            work_item_id: None,
        },
        tool_name: "shell_start".to_string(),
        tool_input: serde_json::json!({ "command": "sleep 1" }),
        pending_confirmation_id: None,
        tool_call_id: Some("call_start_list_case".to_string()),
        pending_confirmation_options: None,
    });
    app.update();

    app.world_mut().spawn(ToolExecutionRequestMessage {
        request: AgentExecutionRequest {
            task_id,
            agent_id,
            request_kind: AgentRequestKind::ToolExecution { tool_name: "shell_list".to_string() },
            prompt: String::new(),
            system_prompt: None,
            tools: vec![],
            conversation: None,
            work_item_id: None,
        },
        tool_name: "shell_list".to_string(),
        tool_input: serde_json::json!({}),
        pending_confirmation_id: None,
        tool_call_id: Some("call_list_case".to_string()),
        pending_confirmation_options: None,
    });

    app.update();

    let world = app.world_mut();
    let mut query = world.query::<&harness::ToolExecutionResultMessage>();
    let results = query.iter(world).cloned().collect::<Vec<_>>();
    let output = results.last().unwrap().tool_output.clone().unwrap();

    assert!(output.is_array());
    assert!(!output.as_array().unwrap().is_empty());
    assert!(output[0]["session_id"].is_string());
}
```

- [ ] **Step 2: 运行测试，确认它们先失败**

Run:

```bash
cargo test shell_read_returns_status_and_latest_snapshot --test shell_tool_flow -v
cargo test shell_list_returns_only_active_sessions --test shell_tool_flow -v
```

Expected: FAIL，提示 `shell_read` / `shell_list` 还没有真实运行时实现，或者返回结构仍是旧 `handle`/`output` 嵌套结构。

- [ ] **Step 3: 重构 native backend 到六动作接口**

修改 `src/systems/tools/backend/native.rs`，保留现有输出缓冲线程模型，但把 trait 实现收敛到：

```rust
impl SessionBackend for NativeProcessBackend {
    fn exec_blocking(&self, request: SessionStartRequest) -> Result<SessionHandle, String> { .. }

    fn start_session(&self, request: SessionStartRequest) -> Result<SessionHandle, String> { .. }

    fn read_session(&self, request: SessionReadRequest) -> Result<SessionSummary, String> {
        let handle = self.get_status(request.handle_id)?;
        Ok(self.session_summary(handle))
    }

    fn list_active_sessions(&self) -> Result<Vec<SessionSummary>, String> {
        let sessions = self.sessions.lock().map_err(|_| "session map poisoned".to_string())?;
        Ok(sessions
            .values()
            .filter(|handle| matches!(handle.status, SessionStatus::Starting | SessionStatus::Running | SessionStatus::WaitingForInput))
            .cloned()
            .map(|handle| self.session_summary(handle))
            .collect())
    }

    fn input_session(&self, request: SessionInputRequest) -> Result<SessionHandle, String> { .. }

    fn stop_session(&self, handle_id: SessionHandleId) -> Result<SessionHandle, String> { .. }
}
```

同时新增 helper：

```rust
impl NativeProcessBackend {
    fn to_snapshot(&self, handle: &SessionHandle) -> SessionOutputSnapshot {
        SessionOutputSnapshot {
            output: handle.output.combined_tail.clone(),
            returned_lines: handle.output.returned_lines,
            truncated: handle.output.combined_truncated,
        }
    }

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
}
```

不要再实现：

- `get_status`
- `read_output`
- `send_signal`
- `wait_session`

这些旧 trait 方法。

- [ ] **Step 4: 用 DTO 重写 orchestrator 的 shell 返回值**

修改 `src/systems/tools/orchestrator.rs`，将 shell 分支改为：

```rust
Ok(ToolAction::ReadSession(read_request)) => {
    match backend.read_session(read_request) {
        Ok(summary) => {
            spawn_shell_result(
                commands,
                request_entity,
                request,
                "shell_read",
                serde_json::json!(ShellSessionResult::from_summary(summary)),
            );
        }
        Err(error) => spawn_tool_error(
            commands,
            request_entity,
            request,
            ToolError::ExecutionFailed(error),
        ),
    }
}
Ok(ToolAction::ListSessions) => {
    match backend.list_active_sessions() {
        Ok(sessions) => {
            let payload = sessions
                .into_iter()
                .map(ShellSessionResult::from_summary)
                .collect::<Vec<_>>();
            spawn_shell_result(
                commands,
                request_entity,
                request,
                "shell_list",
                serde_json::json!(payload),
            );
        }
        Err(error) => spawn_tool_error(
            commands,
            request_entity,
            request,
            ToolError::ExecutionFailed(error),
        ),
    }
}
Ok(ToolAction::InputSession(input_request)) => { .. }
Ok(ToolAction::StopSession(handle_id)) => { .. }
```

并删除：

- `ReadSessionOutput`
- `SendSessionSignal`
- `WaitForSession`
- `StopSession(wait_for_exit=true)` 的 waiting 分支

- [ ] **Step 5: 删除 shell session waiting 调度**

修改 `src/plugins/tools.rs` 和 `src/systems/tools/waiting.rs`：

- `ToolRuntimePlugin` 中移除 `check_waiting_sessions_system`
- `waiting.rs` 中删除或隔离所有 `WaitingForSessionInfo` 的 shell 相关逻辑
- 若 `WaitingForSessionInfo` 仅服务 shell，可一并删除相关组件/分支

目标是让 shell 工具链只保留“同步短调用”，不再进入 `Waiting(Session)`。

- [ ] **Step 6: 跑新测试，确认 read/list 行为成立**

Run:

```bash
cargo test shell_read_returns_status_and_latest_snapshot --test shell_tool_flow -v
cargo test shell_list_returns_only_active_sessions --test shell_tool_flow -v
```

Expected: PASS

- [ ] **Step 7: 跑核心 shell 测试和静态检查**

Run:

```bash
cargo test --test shell_tool_flow -v
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
```

Expected:

- shell 集成测试通过
- fmt check 通过
- clippy 无 warning

- [ ] **Step 8: 提交运行时主链重构**

Run:

```bash
git add \
  src/systems/tools/backend/native.rs \
  src/systems/tools/orchestrator.rs \
  src/plugins/tools.rs \
  src/systems/tools/waiting.rs \
  src/systems/tools/result.rs \
  tests/shell_tool_flow.rs
git commit -m "refactor: align shell runtime with simplified tools"
```

Expected: commit 成功。

---

### Task 3: 用回归测试锁住默认超时、输入和停止语义

**Files:**
- Modify: `tests/shell_tool_flow.rs`
- Modify: `src/systems/tools/backend/native.rs`
- Modify: `src/systems/tools/builtin/shell/exec.rs`
- Modify: `src/systems/tools/builtin/shell/input.rs`
- Modify: `src/systems/tools/builtin/shell/stop.rs`
- Modify: `docs/superpowers/specs/2026-06-08-shell-tool-simplification-design.md`

- [ ] **Step 1: 写失败测试，覆盖默认 exec 超时**

在 `tests/shell_tool_flow.rs` 追加：

```rust
#[test]
fn shell_exec_uses_default_timeout_when_omitted() {
    let mut config = test_config();
    config.shell_default_exec_timeout_secs = 1;

    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(config, runtime, executor, input_rx, vec![]);

    app.update();
    let agent_id = spawn_agent(app.world_mut());
    let task_entity = app
        .world_mut()
        .spawn((Task::from_user_input_ready("shell exec timeout", 3, default_channel()), ShortTermMemory::default()))
        .id();
    let task_id = app.world().get::<Task>(task_entity).unwrap().id;

    app.world_mut().spawn(ToolExecutionRequestMessage {
        request: AgentExecutionRequest {
            task_id,
            agent_id,
            request_kind: AgentRequestKind::ToolExecution { tool_name: "shell_exec".to_string() },
            prompt: String::new(),
            system_prompt: None,
            tools: vec![],
            conversation: None,
            work_item_id: None,
        },
        tool_name: "shell_exec".to_string(),
        tool_input: serde_json::json!({ "command": "sleep 2" }),
        pending_confirmation_id: None,
        tool_call_id: Some("call_exec_timeout_default".to_string()),
        pending_confirmation_options: None,
    });

    app.update();

    let world = app.world_mut();
    let mut query = world.query::<&harness::ToolExecutionResultMessage>();
    let results = query.iter(world).cloned().collect::<Vec<_>>();
    let output = results.last().unwrap().tool_output.clone().unwrap();

    assert_eq!(output["timed_out"], true);
}
```

- [ ] **Step 2: 写失败测试，覆盖 `shell_input` 和 `shell_stop` 主路径**

追加：

```rust
#[test]
fn shell_input_and_stop_follow_simplified_contract() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, vec![]);

    app.update();
    let agent_id = spawn_agent(app.world_mut());
    let task_entity = app
        .world_mut()
        .spawn((Task::from_user_input_ready("shell input stop", 3, default_channel()), ShortTermMemory::default()))
        .id();
    let task_id = app.world().get::<Task>(task_entity).unwrap().id;

    app.world_mut().spawn(ToolExecutionRequestMessage {
        request: AgentExecutionRequest {
            task_id,
            agent_id,
            request_kind: AgentRequestKind::ToolExecution { tool_name: "shell_start".to_string() },
            prompt: String::new(),
            system_prompt: None,
            tools: vec![],
            conversation: None,
            work_item_id: None,
        },
        tool_name: "shell_start".to_string(),
        tool_input: serde_json::json!({ "command": "cat" }),
        pending_confirmation_id: None,
        tool_call_id: Some("call_start_input_stop_case".to_string()),
        pending_confirmation_options: None,
    });
    app.update();

    let session_id = {
        let world = app.world_mut();
        let mut query = world.query::<&harness::ToolExecutionResultMessage>();
        let results = query.iter(world).cloned().collect::<Vec<_>>();
        results[0].tool_output.clone().unwrap()["session_id"]
            .as_str()
            .unwrap()
            .to_string()
    };

    app.world_mut().spawn(ToolExecutionRequestMessage {
        request: AgentExecutionRequest {
            task_id,
            agent_id,
            request_kind: AgentRequestKind::ToolExecution { tool_name: "shell_input".to_string() },
            prompt: String::new(),
            system_prompt: None,
            tools: vec![],
            conversation: None,
            work_item_id: None,
        },
        tool_name: "shell_input".to_string(),
        tool_input: serde_json::json!({ "session_id": session_id, "input": "hello" }),
        pending_confirmation_id: None,
        tool_call_id: Some("call_input_case".to_string()),
        pending_confirmation_options: None,
    });
    app.update();

    let world = app.world_mut();
    let mut query = world.query::<&harness::ToolExecutionResultMessage>();
    let results = query.iter(world).cloned().collect::<Vec<_>>();
    let input_output = results.last().unwrap().tool_output.clone().unwrap();
    assert_eq!(input_output["accepted"], true);
}
```

- [ ] **Step 3: 运行测试，确认它们先失败**

Run:

```bash
cargo test shell_exec_uses_default_timeout_when_omitted --test shell_tool_flow -v
cargo test shell_input_and_stop_follow_simplified_contract --test shell_tool_flow -v
```

Expected: FAIL，原因应是默认 exec 超时还未真正生效，或 `shell_input`/`shell_stop` 仍返回旧结构。

- [ ] **Step 4: 补齐默认 exec 超时和输入/停止细节**

实现时遵循以下最小代码方向：

`src/systems/tools/builtin/shell/exec.rs`

```rust
let timeout_secs = input
    .get("timeout_secs")
    .and_then(|v| v.as_u64())
    .or(Some(ctx.shell_default_exec_timeout_secs));
```

`src/systems/tools/backend/native.rs`

```rust
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

    Ok(handle)
}
```

`src/domain/session.rs`

```rust
impl ShellSessionResult {
    pub fn accepted_input(handle: &SessionHandle) -> Self {
        Self {
            session_id: handle.handle_id.to_string(),
            command: None,
            cwd: None,
            status: handle.status,
            running: matches!(handle.status, SessionStatus::Starting | SessionStatus::Running | SessionStatus::WaitingForInput),
            exit_code: handle.exit_code,
            interaction_required: handle.interaction_required,
            started_at: None,
            output: None,
            returned_lines: None,
            truncated: None,
            accepted: Some(true),
        }
    }
}
```

- [ ] **Step 5: 跑完整 shell 回归**

Run:

```bash
cargo test --test shell_tool_flow -v
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
```

Expected: 全部 PASS。

- [ ] **Step 6: 同步文档细节，避免实现与 spec 命名漂移**

如果实现中出现以下任何偏差，更新 `docs/superpowers/specs/2026-06-08-shell-tool-simplification-design.md`：

- `session_id` / `handle_id` 命名不一致
- `shell_input` 返回字段不一致
- `shell_exec` 超时默认值来源不一致

同步后文档应保持“一处定义，多处实现一致”。

- [ ] **Step 7: 提交行为锁定与文档同步**

Run:

```bash
git add \
  tests/shell_tool_flow.rs \
  src/systems/tools/backend/native.rs \
  src/systems/tools/builtin/shell/exec.rs \
  src/systems/tools/builtin/shell/input.rs \
  src/systems/tools/builtin/shell/stop.rs \
  docs/superpowers/specs/2026-06-08-shell-tool-simplification-design.md
git commit -m "test: lock simplified shell tool behavior"
```

Expected: commit 成功。

---

## Self-Review

### Spec Coverage

- “六工具最小集合”由 Task 1 覆盖
- “最新快照而不是增量游标”由 Task 1 和 Task 2 覆盖
- “活动会话列表”由 Task 2 覆盖
- “保留输入和停止能力，删除 wait/signal/status/read_output”由 Task 1 和 Task 2 覆盖
- “阻塞执行默认超时”由 Task 3 覆盖

没有遗漏的 spec 要求。

### Placeholder Scan

已检查全文：

- 没有 `TODO` / `TBD`
- 没有“后续再补”式步骤
- 每个测试步骤都有具体代码和命令
- 每个实现步骤都有明确文件和最小代码方向

### Type Consistency

本计划统一使用：

- 对外：`session_id`
- 对内：`handle_id` / `SessionHandleId`
- 工具名：`shell_exec`、`shell_start`、`shell_read`、`shell_list`、`shell_input`、`shell_stop`

没有混用旧工具名作为目标实现名称。
