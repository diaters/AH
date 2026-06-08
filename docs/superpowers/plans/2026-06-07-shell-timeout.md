# shell Timeout Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 补齐 `shell_exec` / `shell_wait` 的超时语义，实现“超时即中断并返回，并尽可能携带 tail 输出”的一致性行为。

**Architecture:** `shell_wait` 的超时策略保持在 waiting system 层，超时触发 stop 并返回 `SessionHandle`；`shell_exec` 的阻塞执行在 native backend 内新增输出 reader，使超时返回也能带上已产生输出 tail。

**Tech Stack:** Rust、Bevy ECS、tracing、std::process

---

## 文件与职责

- 修改：[waiting.rs](file:///Users/diater/workspace/Harness/src/systems/tools/waiting.rs)
  - 在 `check_waiting_sessions_system` 中实现：`shell_wait` 超时时 stop/kill 会话，并返回 `status=stopped` + `timed_out=true` 的 `SessionHandle`。
- 修改：[native.rs](file:///Users/diater/workspace/Harness/src/systems/tools/backend/native.rs)
  - 在 `exec_blocking` 中实现：阻塞执行期间读取 stdout/stderr 到有界 buffer，超时 kill 后也能返回 tail 输出。
- 修改：[shell_tool_flow.rs](file:///Users/diater/workspace/Harness/tests/shell_tool_flow.rs)
  - 新增两个回归测试覆盖超时语义。

---

### Task 1: 为超时语义补齐测试

**Files:**
- Modify: [shell_tool_flow.rs](file:///Users/diater/workspace/Harness/tests/shell_tool_flow.rs)

- [ ] **Step 1: 添加 failing test：shell_exec 超时应返回 stopped + timed_out + 非空 tail**

在 `shell_tool_flow.rs` 末尾新增测试函数（放在现有测试之后即可）：

```rust
#[test]
fn shell_exec_times_out_returns_stopped_with_tail_output() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, vec![]);

    app.update();

    let agent_id = spawn_agent(app.world_mut());
    let task_entity = app
        .world_mut()
        .spawn((
            Task::from_user_input_ready("shell exec timeout", 3, default_channel()),
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
            "command": "i=0; while true; do echo tick-$i; i=$((i+1)); sleep 0.02; done",
            "timeout_secs": 1,
            "tail_lines": 20
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

    assert!(
        !results.is_empty(),
        "shell_exec should produce a ToolExecutionResultMessage"
    );
    let output_json = results[0]
        .tool_output
        .clone()
        .expect("shell_exec should succeed");

    assert_eq!(output_json["status"], "stopped");
    assert_eq!(output_json["timed_out"], true);

    let combined_tail = output_json["output"]["combined_tail"]
        .as_str()
        .unwrap_or_default();
    assert!(
        combined_tail.contains("tick-"),
        "timeout result should carry partial output tail"
    );
}
```

- [ ] **Step 2: 添加 failing test：shell_wait 超时应 kill 并返回 stopped + timed_out**

继续在 `shell_tool_flow.rs` 末尾新增测试：

```rust
#[test]
fn shell_wait_times_out_kills_session_and_returns_stopped() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, vec![]);

    app.update();

    let agent_id = spawn_agent(app.world_mut());
    let task_entity = app
        .world_mut()
        .spawn((
            Task::from_user_input_ready("shell wait timeout", 3, default_channel()),
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
            "command": "sleep 5"
        }),
        pending_confirmation_id: None,
        tool_call_id: Some("call_shell_start_for_wait_timeout".to_string()),
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
            "timeout_secs": 0
        }),
        pending_confirmation_id: None,
        tool_call_id: Some("call_shell_wait_timeout".to_string()),
        pending_confirmation_options: None,
    });

    let mut wait_result = None;
    for _ in 0..20 {
        app.update();
        std::thread::sleep(std::time::Duration::from_millis(20));
        {
            let app = app.world_mut();
            let mut result_query = app.query::<&harness::ToolExecutionResultMessage>();
            let results: Vec<_> = result_query.iter(app).cloned().collect();
            if let Some(result) = results.iter().find(|r| r.tool_name == "shell_wait") {
                wait_result = Some(result.clone());
                break;
            }
        }
    }

    let wait_result = wait_result.expect("shell_wait timeout result should be present");
    let output_json = wait_result.tool_output.clone().unwrap();
    assert_eq!(output_json["status"], "stopped");
    assert_eq!(output_json["timed_out"], true);

    let status_request = AgentExecutionRequest {
        task_id,
        agent_id,
        request_kind: AgentRequestKind::ToolExecution {
            tool_name: "shell_status".to_string(),
        },
        prompt: String::new(),
        system_prompt: None,
        tools: vec![],
        conversation: None,
        work_item_id: None,
    };

    app.world_mut().spawn(ToolExecutionRequestMessage {
        request: status_request,
        tool_name: "shell_status".to_string(),
        tool_input: serde_json::json!({
            "handle_id": handle_id
        }),
        pending_confirmation_id: None,
        tool_call_id: Some("call_shell_status_after_wait_timeout".to_string()),
        pending_confirmation_options: None,
    });
    app.update();

    let results = {
        let world = app.world_mut();
        let mut query = world.query::<&harness::ToolExecutionResultMessage>();
        query.iter(world).cloned().collect::<Vec<_>>()
    };

    let last = results.last().unwrap().tool_output.clone().unwrap();
    assert_eq!(last["handle"]["status"], "stopped");
}
```

- [ ] **Step 3: 运行测试并确认它们失败（TDD）**

运行：

```bash
cargo test -q shell_exec_times_out_returns_stopped_with_tail_output shell_wait_times_out_kills_session_and_returns_stopped
```

期望：
- `shell_exec_times_out_returns_stopped_with_tail_output` 失败：`combined_tail` 为空或不包含 `tick-`
- `shell_wait_times_out_kills_session_and_returns_stopped` 失败：`shell_wait` 返回 `running + timed_out=true` 或后续 `shell_status` 仍显示 `running`

---

### Task 2: 实现 shell_wait 超时即 stop 并返回 stopped

**Files:**
- Modify: [waiting.rs](file:///Users/diater/workspace/Harness/src/systems/tools/waiting.rs)

- [ ] **Step 1: 调整 check_waiting_sessions_system 的分支优先级**

在 `check_waiting_sessions_system` 中，将逻辑调整为：

- 若 `handle.is_some()`：按现有逻辑返回该 handle（正常退出路径）
- 否则若 `timed_out`：执行 stop 并返回 stopped + timed_out=true

目标修改片段（仅展示核心分支，保持现有消息结构不变）：

```rust
if let Some(handle) = handle {
    commands.spawn(ToolExecutionResultMessage {
        result: AgentExecutionResult { /* 保持原样 */ },
        tool_name: "shell_wait".to_string(),
        tool_output: Ok(serde_json::json!(handle)),
        tool_call_id: Some(info.tool_call_id.clone()),
        processed: false,
    });
    commands.entity(entity).remove::<WaitingForSessionInfo>();
    continue;
}

if timed_out {
    let stopped = backend.stop_session(crate::domain::SessionStopRequest {
        handle_id: info.handle_id,
        wait_for_exit: false,
        timeout_secs: 0,
        tail_lines: info.return_tail_lines,
    });

    let tool_output = match stopped {
        Ok(mut handle) => {
            handle.timed_out = true;
            let _ = backend
                .sessions
                .lock()
                .ok()
                .map(|mut sessions| sessions.insert(info.handle_id, handle.clone()));
            Ok(serde_json::json!(handle))
        }
        Err(error) => {
            tracing::warn!(
                event = "SessionWaitTimedOutStopFailed",
                handle_id = %info.handle_id,
                error = %error,
                "shell_wait timed out but stop_session failed"
            );
            Ok(serde_json::json!({
                "handle_id": info.handle_id.to_string(),
                "status": "running",
                "timed_out": true
            }))
        }
    };

    commands.spawn(ToolExecutionResultMessage {
        result: AgentExecutionResult { /* 保持原样 */ },
        tool_name: "shell_wait".to_string(),
        tool_output,
        tool_call_id: Some(info.tool_call_id.clone()),
        processed: false,
    });
    commands.entity(entity).remove::<WaitingForSessionInfo>();
}
```

- [ ] **Step 2: 运行新增测试，确认 shell_wait timeout 测试通过**

运行：

```bash
cargo test -q shell_wait_times_out_kills_session_and_returns_stopped
```

期望：PASS

---

### Task 3: 实现 shell_exec 超时返回携带 tail 输出

**Files:**
- Modify: [native.rs](file:///Users/diater/workspace/Harness/src/systems/tools/backend/native.rs)

- [ ] **Step 1: 在 exec_blocking 中引入有界输出 buffer + reader 线程**

在 `exec_blocking` 中，替换“退出时 wait_with_output 拼接 stdout/stderr”的策略，改为：

- spawn 2 个 reader 线程分别读取 stdout/stderr 的 lines，并写入共享 `SessionOutputBuffer`
- 退出或超时时，从 buffer 生成 `SessionOutputWindow` 并返回

新增 helper（放在 `append_output` / `window_from_buffer` 附近，便于复用）：

```rust
/// 为阻塞执行读取 stdout/stderr，并将内容追加到共享的输出缓冲区中。
fn spawn_blocking_output_reader(
    stream: Option<impl std::io::Read + Send + 'static>,
    buffer: Arc<Mutex<crate::domain::SessionOutputBuffer>>,
    prefix: &'static str,
    max_bytes: usize,
) -> Option<thread::JoinHandle<()>> {
    let Some(stream) = stream else {
        return None;
    };

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
```

在 `exec_blocking` 内部，spawn readers（注意：要在 `command.spawn()` 后立刻 `take()` stdout/stderr）：

```rust
let stdout = child.stdout.take();
let stderr = child.stderr.take();
let buffer = Arc::new(Mutex::new(crate::domain::SessionOutputBuffer::empty()));
let stdout_reader = spawn_blocking_output_reader(stdout, Arc::clone(&buffer), "", 1024 * 1024);
let stderr_reader =
    spawn_blocking_output_reader(stderr, Arc::clone(&buffer), "[stderr] ", 1024 * 1024);
```

当 `try_wait()` 返回 `Some(exit_status)` 时：

- join 两个 reader 线程（忽略 join error）
- 从 buffer 生成 `output_window = window_from_buffer(&buffer_snapshot, None, request.tail_lines)`
- 填充 `SessionHandle.output = output_window` 并返回

当达到 deadline 时：

- `child.kill()` + `child.wait()`（忽略错误即可）
- join readers
- 生成 output_window
- 返回 `status=Stopped` + `timed_out=true`

- [ ] **Step 2: 运行新增测试，确认 shell_exec timeout 测试通过**

运行：

```bash
cargo test -q shell_exec_times_out_returns_stopped_with_tail_output
```

期望：PASS

---

### Task 4: 全量回归与格式化

**Files:**
- Modify: [waiting.rs](file:///Users/diater/workspace/Harness/src/systems/tools/waiting.rs)
- Modify: [native.rs](file:///Users/diater/workspace/Harness/src/systems/tools/backend/native.rs)
- Modify: [shell_tool_flow.rs](file:///Users/diater/workspace/Harness/tests/shell_tool_flow.rs)

- [ ] **Step 1: cargo fmt**

```bash
cargo fmt
```

- [ ] **Step 2: cargo test**

```bash
cargo test
```

- [ ] **Step 3: 自检（人工）**

- `shell_wait(timeout_secs=0)` 返回 JSON 中 `status="stopped"` 且 `timed_out=true`
- `shell_exec(timeout_secs=1)` 返回 JSON 中 `timed_out=true` 且 `output.combined_tail` 非空

---

## Spec 覆盖自检

- `shell_wait` 超时即 stop：Task 2 覆盖（实现 + 新增测试）。
- `shell_exec` 超时携带 tail：Task 3 覆盖（实现 + 新增测试）。
- 不引入新字段/不改 schema：所有修改仅改变行为与 output 内容，不改结构。

