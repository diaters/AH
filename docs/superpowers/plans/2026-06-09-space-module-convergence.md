# Space Module Convergence Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 将 `Space` 收敛为只保留 `SpaceKnowledge` 和 `SpaceToolRegistry` 的全局共享资源层，并把 shell session 真源统一到 `NativeProcessBackend`，同时补齐 `env` 契约。

**Architecture:** 先删除未落地的 `Space` 资源和 `SpaceSessionRegistry` 注入，再收缩 session 领域模型与 backend 责任边界，最后补齐 `shell_exec` / `shell_start` 的 `env` 解析与子进程环境注入。整个过程保持 shell 六工具对外意图不变，并通过现有集成测试验证行为稳定。

**Tech Stack:** Rust, Bevy ECS, serde/serde_json, std::process, tracing, cargo test, cargo fmt, cargo clippy

---

## Scope Check

本计划只覆盖一个子系统：`Space` 模块与 shell session 运行时的收敛。

本计划包含：

- 删除 `SpacePreferences`、`SpaceAgentRegistry`、`SpaceRuntimeContext`
- 删除 `SpaceSessionRegistry`
- 收敛 `Session` 领域模型，删除 `cursor/wait/signal` 旧残留
- 将 shell session 真源统一到 `NativeProcessBackend`
- 为 `shell_exec` / `shell_start` 增加 `env` 参数解析和进程环境注入
- 更新测试与文档

本计划不包含：

- `Space` 概念重命名
- 知识持久化或复杂检索
- herdr backend
- 新的 shell 工具
- 审批与 Agent 演化机制扩展

---

## File Structure

| File | Responsibility |
|------|----------------|
| `src/domain/space.rs` | 删除未落地 `Space` 资源，保留 `SpaceKnowledge`、`SpaceToolRegistry`、`ToolAction`、`ToolContext` |
| `src/domain/session.rs` | 收敛公开 session 领域模型，删除 `SpaceSessionRegistry` 与 `cursor/wait/signal` 旧结构 |
| `src/domain/mod.rs` | 更新 re-export，移除已删除类型 |
| `src/app/mod.rs` | 调整 app 启动注入的 `Space` 资源集合 |
| `src/plugins/tools.rs` | 删除 `SpaceSessionRegistry` 注入 |
| `src/contracts/sessions.rs` | 若仍暴露旧 session 抽象，保持与收敛后接口一致 |
| `src/systems/tools/backend/native.rs` | 成为 shell session 唯一真源，收缩内部状态结构并注入 `env` |
| `src/systems/tools/builtin/shell/exec.rs` | 解析 `env` 参数并构造 `SessionStartRequest` |
| `src/systems/tools/builtin/shell/start.rs` | 解析 `env` 参数并构造 `SessionStartRequest` |
| `src/systems/tools/orchestrator.rs` | 继续通过 backend 处理 shell 动作，移除对 `SpaceSessionRegistry` 的依赖 |
| `src/systems/transform/task_lifecycle.rs` | 直接通过 backend 清理任务所属 session |
| `tests/shell_tool_flow.rs` | 覆盖 `env` 透传、活动 session 列表、任务终态清理、跨任务访问拒绝 |
| `tests/tool_execution_flow.rs` | 如需验证 app 资源装配边界，在此补充或新增专门测试 |
| `docs/current-state.md` | 更新当前真实能力边界 |
| `docs/design/2026-05-17-tool-space-design.md` | 补充历史状态说明，指向当前设计与当前状态文档 |

---

### Task 1: 删除未落地 `Space` 资源并收缩注入边界

**Files:**
- Modify: `src/domain/space.rs`
- Modify: `src/domain/mod.rs`
- Modify: `src/app/mod.rs`
- Modify: `src/plugins/tools.rs`
- Test: `tests/tool_execution_flow.rs`

- [ ] **Step 1: 先写一个失败测试，锁定 app 只注入精简后的 `Space` 资源**

在 `tests/tool_execution_flow.rs` 追加这个测试：

```rust
#[test]
fn app_only_inserts_minimal_space_resources() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockExecutor::default());
    let (_input_tx, input_rx) = tokio::sync::mpsc::unbounded_channel();
    let app = build_harness_app(test_config(), runtime, executor, input_rx, vec![]);
    let world = app.world();

    assert!(world.contains_resource::<harness::SpaceKnowledge>());
    assert!(world.contains_resource::<harness::SpaceToolRegistry>());

    assert!(!world.contains_resource::<harness::SpacePreferences>());
    assert!(!world.contains_resource::<harness::SpaceAgentRegistry>());
    assert!(!world.contains_resource::<harness::SpaceRuntimeContext>());
    assert!(!world.contains_resource::<harness::SpaceSessionRegistry>());
}
```

- [ ] **Step 2: 运行测试，确认它先失败**

Run:

```bash
cargo test app_only_inserts_minimal_space_resources --test tool_execution_flow -v
```

Expected: FAIL，提示一个或多个已废弃 `Space` 资源仍然被插入 `World`。

- [ ] **Step 3: 删除未落地 `Space` 资源定义和导出**

修改 `src/domain/space.rs` 与 `src/domain/mod.rs`，将 `Space` 保留项收缩为：

```rust
/// Space 级别的长期知识（用户相关）
#[derive(Resource, Default)]
pub struct SpaceKnowledge {
    pub entries: Vec<MemoryEntry>,
}

/// 全局工具注册表
#[derive(Resource, Default)]
pub struct SpaceToolRegistry {
    tools: HashMap<String, ToolDefinition>,
}

impl SpaceToolRegistry {
    /// 注册新工具。
    pub fn register(&mut self, tool: ToolDefinition) {
        self.tools.insert(tool.name.clone(), tool);
    }

    /// 获取工具定义。
    pub fn get(&self, name: &str) -> Option<&ToolDefinition> {
        self.tools.get(name)
    }

    /// 遍历所有工具定义。
    pub fn iter(&self) -> impl Iterator<Item = &ToolDefinition> {
        self.tools.values()
    }
}
```

同时删除这些导出：

```rust
pub use space::{
    SpaceAgentRegistry,
    SpacePreferences,
    SpaceRuntimeContext,
    // 以及其它已删除类型
};
```

- [ ] **Step 4: 收缩 app 与 plugin 的资源注入**

修改 `src/app/mod.rs` 与 `src/plugins/tools.rs`，使注入逻辑变为：

```rust
// src/app/mod.rs
app.insert_resource(SpaceKnowledge::default());

// src/plugins/tools.rs
app.insert_resource(tool_registry);
app.insert_resource(tool_executors);
app.insert_resource(NativeProcessBackend::default());
```

并删除：

```rust
app.insert_resource(SpacePreferences::default());
app.insert_resource(SpaceAgentRegistry::default());
app.insert_resource(SpaceRuntimeContext::default());
app.insert_resource(SpaceSessionRegistry::default());
```

- [ ] **Step 5: 运行测试，确认资源边界已收缩**

Run:

```bash
cargo test app_only_inserts_minimal_space_resources --test tool_execution_flow -v
```

Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add src/domain/space.rs src/domain/mod.rs src/app/mod.rs src/plugins/tools.rs tests/tool_execution_flow.rs
git commit -m "refactor: shrink space resource surface"
```

### Task 2: 统一 session 真源并清理旧协议残留

**Files:**
- Modify: `src/domain/session.rs`
- Modify: `src/domain/mod.rs`
- Modify: `src/systems/tools/backend/native.rs`
- Modify: `src/systems/tools/orchestrator.rs`
- Modify: `src/systems/transform/task_lifecycle.rs`
- Modify: `src/contracts/sessions.rs`
- Test: `tests/shell_tool_flow.rs`

- [ ] **Step 1: 先写一个失败测试，锁定活动 session 语义与任务终态清理**

在 `tests/shell_tool_flow.rs` 追加这个测试：

```rust
#[test]
fn shell_list_only_returns_active_sessions_after_task_cleanup() {
    let mut harness = TestHarness::new();
    let task_id = harness.spawn_task("run background command");

    let session_id = harness.shell_start_for_task(task_id, "sleep 5");
    assert!(
        harness.active_session_ids(task_id).contains(&session_id),
        "started session should be active before task cleanup"
    );

    harness.finish_task(task_id);

    assert!(
        !harness.active_session_ids(task_id).contains(&session_id),
        "terminated task should not keep active sessions"
    );
}
```

- [ ] **Step 2: 运行测试，确认它先失败**

Run:

```bash
cargo test shell_list_only_returns_active_sessions_after_task_cleanup --test shell_tool_flow -v
```

Expected: FAIL，提示活动 session 列表仍依赖旧 registry 或任务终态后未正确清理。

- [ ] **Step 3: 收缩公开 session 领域模型**

修改 `src/domain/session.rs`，删除 `SpaceSessionRegistry`、`cursor/wait/signal` 残留，并保留精简后的结构：

```rust
pub struct SessionOutputSnapshot {
    pub output: String,
    pub returned_lines: usize,
    pub truncated: bool,
}

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
    pub output: SessionOutputSnapshot,
}

pub struct SessionStartRequest {
    pub command: String,
    pub session_name: Option<String>,
    pub cwd: Option<String>,
    pub env: HashMap<String, String>,
    pub timeout_secs: Option<u64>,
    pub tail_lines: usize,
    pub owner_task_id: TaskId,
    pub owner_agent_id: AgentId,
}
```

同时删除：

```rust
pub struct SpaceSessionRegistry { .. }
pub struct SessionOutputWindow { .. }
pub struct SessionOutputRequest { .. }
pub struct SessionOutputResponse { .. }
pub struct SessionWaitRequest { .. }
pub enum SessionCommand { .. }
```

- [ ] **Step 4: 将 session 运行态内聚到 backend 私有实现**

修改 `src/systems/tools/backend/native.rs`，让 backend 持有唯一真源，并将运行态缓冲定义缩到该文件内部：

```rust
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

#[derive(Resource, Default)]
pub struct NativeProcessBackend {
    pub sessions: Arc<Mutex<HashMap<SessionHandleId, SessionHandle>>>,
    pub processes: Arc<Mutex<HashMap<SessionHandleId, Arc<Mutex<Child>>>>>,
    pub stdins: Arc<Mutex<HashMap<SessionHandleId, Arc<Mutex<ChildStdin>>>>>,
    runtimes: Arc<Mutex<HashMap<SessionHandleId, SessionRuntimeState>>>,
}
```

- [ ] **Step 5: 更新 orchestrator 与任务终态清理调用链**

修改 `src/systems/tools/orchestrator.rs` 与 `src/systems/transform/task_lifecycle.rs`，让 shell 相关逻辑只依赖 backend：

```rust
let summary = backend
    .read_session(read_request)
    .map_err(|error| ExecutionError::tool_failed("shell_read", error))?;

backend
    .assert_task_owns_session(task.id, handle_id)
    .map_err(|error| ExecutionError::tool_failed("shell_access", error))?;

backend.stop_task_sessions(task_id);
```

删除任何类似下面的访问：

```rust
let registry = world.resource::<SpaceSessionRegistry>();
registry.sessions.get(&handle_id)
```

- [ ] **Step 6: 运行测试，确认 shell 会话边界保持稳定**

Run:

```bash
cargo test --test shell_tool_flow -v
```

Expected: PASS，尤其是活动 session、跨任务访问拒绝、任务终态清理相关测试通过。

- [ ] **Step 7: Commit**

```bash
git add src/domain/session.rs src/domain/mod.rs src/systems/tools/backend/native.rs src/systems/tools/orchestrator.rs src/systems/transform/task_lifecycle.rs src/contracts/sessions.rs tests/shell_tool_flow.rs
git commit -m "refactor: unify shell session source of truth"
```

### Task 3: 补齐 `shell_exec` / `shell_start` 的 `env` 契约

**Files:**
- Modify: `src/systems/tools/builtin/shell/exec.rs`
- Modify: `src/systems/tools/builtin/shell/start.rs`
- Modify: `src/systems/tools/backend/native.rs`
- Test: `tests/shell_tool_flow.rs`

- [ ] **Step 1: 先写两个失败测试，锁定阻塞与异步 shell 的 `env` 透传**

在 `tests/shell_tool_flow.rs` 追加这两个测试：

```rust
#[test]
fn shell_exec_passes_env_to_child_process() {
    let mut harness = TestHarness::new();

    let result = harness.shell_exec(
        serde_json::json!({
            "command": "printf '%s' \"$HARNESS_TEST_ENV\"",
            "env": { "HARNESS_TEST_ENV": "visible-from-exec" }
        }),
    );

    assert_eq!(result["output"], "visible-from-exec");
}

#[test]
fn shell_start_passes_env_to_child_process() {
    let mut harness = TestHarness::new();

    let session_id = harness.shell_start(
        serde_json::json!({
            "command": "printf '%s' \"$HARNESS_TEST_ENV\"",
            "env": { "HARNESS_TEST_ENV": "visible-from-start" }
        }),
    );

    let snapshot = harness.shell_read(session_id, 20);
    assert_eq!(snapshot["output"], "visible-from-start");
}
```

- [ ] **Step 2: 运行测试，确认它们先失败**

Run:

```bash
cargo test shell_exec_passes_env_to_child_process --test shell_tool_flow -v
cargo test shell_start_passes_env_to_child_process --test shell_tool_flow -v
```

Expected: FAIL，输出为空字符串或缺少 `env` 参数解析逻辑。

- [ ] **Step 3: 为 builtin parser 增加 `env` 解析**

修改 `src/systems/tools/builtin/shell/exec.rs` 与 `src/systems/tools/builtin/shell/start.rs`，加入一个共享解析函数：

```rust
/// 解析 shell 工具输入中的环境变量对象。
fn parse_env_map(input: &serde_json::Value) -> Result<HashMap<String, String>, ToolError> {
    let Some(env) = input.get("env") else {
        return Ok(HashMap::new());
    };

    let object = env
        .as_object()
        .ok_or_else(|| ToolError::InvalidInput("'env' must be an object".to_string()))?;

    object
        .iter()
        .map(|(key, value)| {
            let value = value.as_str().ok_or_else(|| {
                ToolError::InvalidInput(format!("'env.{key}' must be a string"))
            })?;
            Ok((key.clone(), value.to_string()))
        })
        .collect()
}
```

并在请求构造中使用：

```rust
env: parse_env_map(input)?,
```

- [ ] **Step 4: 在 backend 中注入子进程环境**

修改 `src/systems/tools/backend/native.rs`，在 `exec_blocking` 与 `start_session` 构造命令时补上：

```rust
let mut command = StdCommand::new("sh");
command.arg("-c").arg(&request.command);

if let Some(cwd) = request.cwd.as_ref() {
    command.current_dir(cwd);
}

if !request.env.is_empty() {
    command.envs(&request.env);
}
```

- [ ] **Step 5: 运行测试，确认 `env` 契约兑现**

Run:

```bash
cargo test shell_exec_passes_env_to_child_process --test shell_tool_flow -v
cargo test shell_start_passes_env_to_child_process --test shell_tool_flow -v
```

Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add src/systems/tools/builtin/shell/exec.rs src/systems/tools/builtin/shell/start.rs src/systems/tools/backend/native.rs tests/shell_tool_flow.rs
git commit -m "feat: support env for shell tools"
```

### Task 4: 同步文档、诊断与全量验证

**Files:**
- Modify: `docs/current-state.md`
- Modify: `docs/design/2026-05-17-tool-space-design.md`
- Modify: `docs/superpowers/specs/2026-06-09-space-module-convergence-design.md`

- [ ] **Step 1: 更新当前状态文档，写清 `Space` 的真实边界**

修改 `docs/current-state.md`，将 `Space` 相关表述更新为：

```md
#### 全局共享资源

- `SpaceKnowledge` 用于承载用户显式写入的共享知识，当前为内存态
- `SpaceToolRegistry` 用于承载全局工具定义
- shell session 真源位于 `NativeProcessBackend`，不再作为 `Space` 资源建模
```

- [ ] **Step 2: 更新历史设计文档状态说明**

修改 `docs/design/2026-05-17-tool-space-design.md` 顶部状态说明，加入类似内容：

```md
> 说明补充（2026-06-09）：
> 本文档中 `SpacePreferences`、`SpaceAgentRegistry`、`SpaceRuntimeContext`、
> `SpaceSessionRegistry` 的设计已不再代表当前实现。
> 当前实现以 `docs/current-state.md` 和
> `docs/superpowers/specs/2026-06-09-space-module-convergence-design.md` 为准。
```

- [ ] **Step 3: 运行格式化、lint、测试与诊断**

Run:

```bash
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```

Then inspect diagnostics for recently edited files and ensure no new errors remain.

- [ ] **Step 4: Commit**

```bash
git add docs/current-state.md docs/design/2026-05-17-tool-space-design.md docs/superpowers/specs/2026-06-09-space-module-convergence-design.md
git commit -m "docs: align space module documentation"
```

## Self-Review

- Spec coverage: 已覆盖 `Space` 资源收缩、session 真源统一、旧协议清理、`env` 契约补齐、测试与文档同步。
- Placeholder scan: 计划中未使用 `TODO`、`TBD` 或“之后实现”式占位表述。
- Type consistency: 计划统一使用 `SpaceKnowledge`、`SpaceToolRegistry`、`NativeProcessBackend`、`SessionStartRequest.env` 等命名，与设计文档一致。
