# 日志等级重构 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Restructure log levels to make `info!` an audit level with user-facing summaries, keeping engineering details at `debug!`, using `RUST_LOG` env var with env-aware defaults.

**Architecture:** Single main.rs filter change + per-domain upgrades from `debug!` to `info!` in ~10 modules. Each domain follows the same pattern: add `info!` audit summary, keep existing `debug!` detail.

**Tech Stack:** Rust, tracing, tracing-subscriber EnvFilter

## Global Constraints

- Must use `tracing_subscriber::EnvFilter::builder()` API (already in dependencies)
- Module prefix for default filter: `harness` (matches Cargo.toml lib name)
- `info!` messages must be user-facing summaries, not engineering dump
- `debug!` messages must be preserved (not removed, only supplemented)
- Follow existing `event = "PascalCase"` naming convention
- All changes must compile with existing `tracing` / `tracing-subscriber` versions

---

### Task 1: Filter mechanism in main.rs

**Files:**
- Modify: `src/main.rs:16-42`

**Interfaces:**
- Consumes: (none)
- Produces: `RUST_LOG` env var driven filter with env-aware defaults (dev=debug / prod=info)

- [ ] **Step 1: Replace hardcoded EnvFilter with builder pattern**

Replace lines 21-22:
```rust
    let file_filter = tracing_subscriber::EnvFilter::new("debug");
```
With:
```rust
    let file_filter = tracing_subscriber::EnvFilter::builder()
        .with_env_var("RUST_LOG")
        .with_default_directive(
            if cfg!(debug_assertions) {
                "harness=debug".parse().unwrap()
            } else {
                "harness=info".parse().unwrap()
            },
        )
        .from_env_lossy();
```

- [ ] **Step 2: Run `cargo build` to verify compilation**

- [ ] **Step 3: Verify behavior**

Run: `RUST_LOG=harness=warn cargo run & sleep 2 && kill %1`
Expected output: only warn+ messages from harness (no init `info!` messages like "HarnessStarting")

Run: `cargo run & sleep 2 && kill %1`
Expected output: includes `info!` and above (debug mode default)

- [ ] **Step 4: Commit**

```bash
git add src/main.rs
git commit -m "feat: replace hardcoded EnvFilter with RUST_LOG env var
Use tracing-subscriber EnvFilter builder with env-aware defaults:
dev=harness=debug, release=harness=info. RUST_LOG env var overrides."
```

---

### Task 2: LLM module audit events

**Files:**
- Modify: `src/llm/genai.rs:93-103`

**Interfaces:**
- Consumes: `AgentExecutionRequest` fields, `AgentExecutionOutput` 
- Produces: `info!` audit events for LLM request start, completion, tool calls

- [ ] **Step 1: Add `info` to tracing import**

Change from:
```rust
use tracing::debug;
```
To:
```rust
use tracing::{debug, info};
```

- [ ] **Step 2: Add `info!` audit summary alongside LlmRequestStart**

After the existing `debug!` event at line 93, add:
```rust
            // info! — 审计摘要
            info!(
                event = "LlmRequestStarted",
                task_id = %request.task_id,
                agent_id = %request.agent_id,
                model = %model,
                tools_count = request.tools.len(),
                "LLM 请求开始：model={model}, tools={tools_count}"
            );
```

- [ ] **Step 3: Add `info!` audit summary alongside LlmRequestCompleted**

After the existing `debug!` event at line 118, add:
```rust
            info!(
                event = "LlmRequestCompleted",
                task_id = %request.task_id,
                agent_id = %request.agent_id,
                model = %model,
                duration_ms = duration_ms,
                response_len = response.first_text().map(|c| c.len()).unwrap_or(0),
                "LLM 调用完成：{duration_ms}ms，响应 {response_len} 字符"
            );
```

- [ ] **Step 4: Add `info!` audit summary in parse_response for tool calls**

After the existing `debug!` at line 245 in `parse_response`, add:
```rust
        info!(
            event = "LlmToolCallsRequested",
            task_id = %task_id,
            tool_names = ?parsed_calls.iter().map(|c| &c.name).collect::<Vec<_>>(),
            "LLM 请求调用工具：{tool_names:?}"
        );
```

- [ ] **Step 5: Build and test**

Run: `cargo build`
Expected: compiles cleanly

- [ ] **Step 6: Commit**

```bash
git add src/llm/genai.rs
git commit -m "feat: add info-level audit events for LLM requests and tool calls"
```

---

### Task 3: Tools & Approval audit events

**Files:**
- Modify: `src/systems/tools/dispatch.rs:55-92` (ToolNotFound, AgentNotFound, ToolTagDenied — all `warn!`, keep as-is)
- Modify: `src/systems/tools/dispatch.rs:114-123` (ToolDispatch debug → add info!)
- Modify: `src/systems/tools/dispatch.rs:276-283` (ToolRequiresUserConfirmation debug — keep as-is)
- Modify: `src/systems/tools/dispatch.rs:316-320` (ToolExecutionDenied warn — add info!)
- Modify: `src/systems/tools/approval.rs:39-48`

- [ ] **Step 1: Add `info` to dispatch.rs imports**

```rust
use tracing::{debug, info, warn};
```

- [ ] **Step 2: Add `info!` audit for ToolExecutionStarted**

After the existing `debug!` at line 114-123, add:
```rust
        info!(
            event = "ToolExecutionStarted",
            tool_name = %tool_name,
            agent_id = %agent.id,
            agent_name = %agent.profile.name,
            permission = ?permission,
            task_id = %request.request.task_id,
            "工具执行开始：Agent [{agent_name}] 调用 {tool_name}，权限 {permission:?}"
        );
```

- [ ] **Step 3: Add `info!` audit for ToolExecutionDenied**

After the existing `warn!` at line 318, add:
```rust
        info!(
            event = "ToolExecutionDenied",
            tool_name = %tool_name,
            agent_name = %agent.profile.name,
            task_id = %request.request.task_id,
            "工具调用被拒绝：Agent [{agent_name}] 无权使用 {tool_name}"
        );
```

- [ ] **Step 4: Add `info` to approval.rs imports**

```rust
use tracing::{debug, info, warn};
```

- [ ] **Step 5: Add `info!` audit for approval events**

After the existing `debug!` at line 39 in `approval_dispatch_system`, add:
```rust
        info!(
            event = "ApprovalRequestReceived",
            request_id = %request.request_id,
            tool_name = %request.tool_name,
            parent_agent_id = %request.parent_agent_id,
            child_agent_id = %request.child_agent_id,
            "审批请求：Agent [{child_agent_id}] 请求调用 {tool_name}，等待 Agent [{parent_agent_id}] 审批"
        );
```

And after the existing `debug!` at line 177-181 (AgentPermissionUpdated), add:
```rust
        info!(
            event = "ApprovalResolved",
            request_id = %request_id,
            tool_name = %tool_request.tool_name,
            decision = ?result.decision,
            grant_mode = ?result.grant_mode,
            "审批结果：{tool_name} => {decision:?}，授权模式 {grant_mode:?}"
        );
```

- [ ] **Step 6: Build and test**

Run: `cargo build`
Expected: compiles cleanly

- [ ] **Step 7: Commit**

```bash
git add src/systems/tools/dispatch.rs src/systems/tools/approval.rs
git commit -m "feat: add info-level audit events for tool dispatch and approval flow"
```

---

### Task 4: Task lifecycle audit events

**Files:**
- Modify: `src/systems/transform/task_creation.rs:68-78`
- Modify: `src/systems/transform/task_lifecycle.rs:62-80` (TaskTerminated)
- Modify: `src/systems/transform/task_lifecycle.rs:129-141` (SummarizationTriggered)
- Modify: `src/systems/summarization.rs:36-44`
- Modify: `src/systems/transform/trigger_task.rs:144-149` (already has `info!` — keep)

- [ ] **Step 1: Add `info` to task_creation.rs imports**

```rust
use tracing::{debug, info};
```

- [ ] **Step 2: Add `info!` audit for TaskCreated in task_creation.rs**

Replace the existing `debug!` at line 68 with a pair:
```rust
        info!(
            event = "TaskCreated",
            task_id = %task.id,
            content = %message.content,
            "任务创建：{content}"
        );
        debug!(
            event = "TaskCreated",
            task_id = %task.id,
            content = %message.content,
            content_len = message.content.len(),
            multi_turn = task.multi_turn,
            max_retries = task.max_retries,
            stm_initial_entries = 1,
            stm_initial_tokens = stm_tokens,
            "new task spawned from user message"
        );
```

- [ ] **Step 3: Add `info` to task_lifecycle.rs imports**

```rust
use tracing::{debug, info};
```

- [ ] **Step 4: Add `info!` audit for TaskTerminated**

After the existing `debug!` at line 62-69 in `task_termination_system`, add:
```rust
        info!(
            event = "TaskTerminated",
            task_id = %task.id,
            task_status = ?task.status,
            result_summary = %task.result_summary,
            "任务完成：状态={task_status:?}，结果摘要={result_summary}"
        );
```

- [ ] **Step 5: Add `info!` audit for SummarizationTriggered**

Replace the existing `debug!` at line 129 with an `info!`:
```rust
        info!(
            event = "SummarizationTriggered",
            task_id = %task.id,
            trigger = "TaskComplete",
            stm_entries = stm.entries.len(),
            "触发摘要：STM {stm_entries} 条目"
        );
```

- [ ] **Step 6: Add `info` to summarization.rs imports**

```rust
use tracing::{debug, info};
```

- [ ] **Step 7: Add `info!` audit for summarization work item creation**

After the existing `debug!` at line 36 in `summarization_dispatch_system`, add:
```rust
        info!(
            event = "SummarizationRequested",
            task_id = %request.task_id,
            trigger = ?request.trigger,
            target_tokens = request.target_tokens,
            "摘要请求：{trigger:?}，目标 tokens {target_tokens}"
        );
```

- [ ] **Step 8: Build and test**

Run: `cargo build`
Expected: compiles cleanly

- [ ] **Step 9: Commit**

```bash
git add src/systems/transform/task_creation.rs src/systems/transform/task_lifecycle.rs src/systems/summarization.rs
git commit -m "feat: add info-level audit events for task lifecycle and summarization"
```

---

### Task 5: Supporting systems audit events

**Files:**
- Modify: `src/systems/evaluation.rs:40-43`
- Modify: `src/systems/experience/writeback.rs:106-113` (already has `info!` from existing code, add more context)
- Modify: `src/systems/ingress.rs:34-43`

- [ ] **Step 1: Add `info` to evaluation.rs imports**

```rust
use tracing::{debug, info};
```

- [ ] **Step 2: Add `info!` audit for EvaluationTriggered**

Replace the existing `debug!` at line 40 with:
```rust
        info!(
            event = "EvaluationTriggered",
            task_id = %task.id,
            turn_count,
            max_turns,
            "评估触发：任务已达 {turn_count}/{max_turns} 轮"
        );
```

- [ ] **Step 3: Add `info` to ingress.rs imports**

```rust
use tracing::{debug, info, trace};
```

- [ ] **Step 4: Add `info!` audit for ExternalInputReceived**

After the existing `debug!` at line 34-39, add:
```rust
        info!(
            event = "ExternalInputReceived",
            kind = "TextWithChannel",
            channel = ?channel,
            content_len = content.len(),
            "收到外部输入：通道={channel:?}，内容长度 {content_len}"
        );
```

No change needed for writeback.rs — it already has `info!` at line 106 and `warn!` at line 119, which are appropriate.

- [ ] **Step 5: Build and test**

Run: `cargo build`
Expected: compiles cleanly

- [ ] **Step 6: Commit**

```bash
git add src/systems/evaluation.rs src/systems/ingress.rs
git commit -m "feat: add info-level audit events for evaluation and ingress"
```

---

### Task 6: Documentation update

**Files:**
- Modify: `docs/logs.md`

- [ ] **Step 1: Update the default level table**

Change from:
```
| 环境 | 默认级别 |
|------|----------|
| 生产 | `INFO` |
| 开发 | `DEBUG` |
```

To:
```
| 环境 | 默认级别 | 说明 |
|------|----------|------|
| 生产（release） | `harness=info` | 可审计事件，通过 `RUST_LOG` 调整 |
| 开发（debug） | `harness=debug` | 包含工程细节，通过 `RUST_LOG` 调整 |
```

- [ ] **Step 2: Update the log level usage table**

Change `info!` row from:
```
| `info!` | 重要业务事件、外部交互 | 启动、任务完成、摘要触发 |
```

To:
```
| `info!` | **审计事件**，用户可读的摘要 | LLM 调用开始/完成、审批请求/结果、任务创建/完成、工具执行、评估触发 |
```

- [ ] **Step 3: Add RUST_LOG section after the level table**

```
## 运行时级别控制

日志级别通过 `RUST_LOG` 环境变量控制，未设置时使用环境感知的默认值：

- 开发构建（`cargo run` / `cargo build`）：`harness=debug`
- 发布构建（`cargo run --release` / `cargo build --release`）：`harness=info`

### 常用示例

```bash
# 全局只看 warn 以上
RUST_LOG=warn cargo run

# 仅将 LLM 模块切到 debug，其他保持 info
RUST_LOG=info,harness::llm=debug cargo run

# 完整格式：按模块精细化控制
RUST_LOG=harness=trace,harness::channels=info cargo run
```

模块前缀 `harness` 对应 Cargo.toml 中的 lib name，第三方依赖不会受到 harness 级别设置影响。

> **注意：** 当前日志输出到 `logs/harness_*.jsonl` 文件，`HARNESS_LOG_DIR` 可控制输出目录。
```

- [ ] **Step 4: Add audit event classification table**

After the level table:

```
## 审计事件分类

以下 `info!` 事件构成完整的审计轨迹：

| 领域 | 事件 | 字段 |
|------|------|------|
| LLM | LlmRequestStarted / LlmRequestCompleted | model, tools_count, duration_ms, response_len |
| LLM | LlmToolCallsRequested | tool_names |
| 审批 | ApprovalRequestReceived / ApprovalResolved | tool_name, decision, grant_mode |
| 任务 | TaskCreated / TaskTerminated | content, task_status, result_summary |
| 摘要 | SummarizationRequested / SummarizationTriggered | trigger, stm_entries, target_tokens |
| 工具 | ToolExecutionStarted / ToolExecutionDenied | tool_name, agent_name, permission |
| 评估 | EvaluationTriggered | turn_count, max_turns |
| 输入 | ExternalInputReceived | kind, channel, content_len |

事件名遵循 PascalCase，详见「事件命名规范」。
```

- [ ] **Step 5: Update the audit requirement note in "必需字段" section**

Add a note under the required fields table:

```
### 审计消息要求

`info!` 级别的日志用于审计场景，除必需字段外还应包含：

- human-readable 消息正文（双引号字符串），简明描述事件
- 关键业务字段（ID、名称、状态、结果）
- **不包含** 工程细节（prompt 原文、tool_input 参数、内部状态结构）

工程细节在同一事件对应的 `debug!` 中记录。
```

- [ ] **Step 6: Build and verify no dead links**

Run: `cargo build`
Expected: compiles cleanly (no code changes in this task)

- [ ] **Step 7: Commit**

```bash
git add docs/logs.md
git commit -m "docs: update logs.md for RUST_LOG env var and audit event classification"
```