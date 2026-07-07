# 动态 scheduled task 审批路由与事件任务审批通道检查实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 修复 `schedule_task` 动态任务触发后因 `approval_channel` 缺失导致审批请求失败的问题，补全一次性动态任务清理日志，并在事件任务审批通道指向未注册 frontend 时明确失败。

**Architecture:** 统一在 `TaskRoutingPolicy::scheduled_task` 中将 `output_channel` 同时作为 `approval_channel`；在 `cleanup_scheduled_task_if_once` 中记录 `DynamicTaskRemoved`；在 `FrontendRegistry` 增加 `has_frontend` 并在 `frontend_output_system` 审批分支中检查 frontend 注册状态。

**Tech Stack:** Rust, Bevy ECS, tracing, cargo

## Global Constraints

- 遵循现有代码风格和项目规范（`AGENTS.md`）。
- 所有代码变更需同步更新对应单元测试。
- 不新增 `schedule_task` 工具参数。
- `has_frontend` 仅检查 frontend 类型是否已注册，不检查底层 channel 运行时可用性。
- 通过分支和 PR 合并代码，禁止直接推送到 `main`。
- 提交前完成自审，确认代码、测试、文档与规范一致。

---

## File Structure

| 文件 | 职责 |
|---|---|
| `src/domain/task.rs` | 修改 `TaskRoutingPolicy::scheduled_task`，使 `approval_channel = output_channel.clone()`；更新相关单元测试。 |
| `src/systems/transform/trigger_task.rs` | 在 `cleanup_scheduled_task_if_once` 中增加 `DynamicTaskRemoved` 日志；更新相关单元测试。 |
| `src/app/mod.rs` | 为 `FrontendRegistry` 增加 `has_frontend` 方法；添加单元测试。 |
| `src/systems/frontend_output.rs` | 在审批请求分支中增加 frontend 注册状态检查，未注册时标记任务失败并记录 `FrontendApprovalRouteInvalid`；添加单元测试。 |
| `tests/schedule_task_approval_routing.rs` | 新增集成测试：验证动态 scheduled task 触发后审批请求路由到指定 output_channel。 |
| `tests/disabled_approval_channel_fails_event_task.rs` | 新增集成测试：验证审批通道 frontend 未注册时事件任务进入 Failed。 |
| `docs/current-state.md` / `docs/TODO.md` / `docs/README.md` | 如有必要，更新动态任务审批路由相关状态描述。 |

---

### Task 1: `TaskRoutingPolicy::scheduled_task` 设置 approval_channel

**Files:**
- Modify: `src/domain/task.rs:58-65`
- Test: `src/domain/task.rs:456-467`

**Interfaces:**
- Consumes: `TaskRoutingPolicy::scheduled_task(output_channel: Option<ChannelId>, approval_context: &str)`
- Produces: 返回的 `TaskRoutingPolicy` 中 `approval_channel` 与 `output_channel` 相同。

- [ ] **Step 1: 修改构造函数**

将 `src/domain/task.rs` 中 `scheduled_task` 改为：

```rust
/// 构造 schedule_task 动态任务的路由策略：output_channel 同时作为审批通道。
pub fn scheduled_task(output_channel: Option<ChannelId>, approval_context: &str) -> Self {
    let approval_channel = output_channel.clone();
    Self {
        output_channel,
        approval_channel,
        approval_context: Some(approval_context.to_string()),
    }
}
```

- [ ] **Step 2: 更新单元测试 `scheduled_task_routing_policy_has_output_channel_no_approval`**

将测试重命名为 `scheduled_task_routing_policy_approval_equals_output`，并更新断言：

```rust
#[test]
fn scheduled_task_routing_policy_approval_equals_output() {
    let channel = ChannelId {
        frontend: crate::domain::FrontendKind::Telegram,
        user_id: "chat".to_string(),
        thread_id: None,
    };
    let policy = TaskRoutingPolicy::scheduled_task(Some(channel.clone()), "scheduled task");
    assert_eq!(policy.output_channel, Some(channel.clone()));
    assert_eq!(policy.approval_channel, Some(channel));
    assert_eq!(policy.approval_context.as_deref(), Some("scheduled task"));
}
```

- [ ] **Step 3: 运行相关单元测试**

Run: `cargo test --lib scheduled_task_routing_policy_approval_equals_output -- --nocapture`
Expected: PASS

- [ ] **Step 4: 提交**

```bash
git add src/domain/task.rs
git commit -m "$(cat <<'EOF'
fix: map scheduled_task output_channel to approval_channel

TaskRoutingPolicy::scheduled_task now clones output_channel into
approval_channel so that tool confirmation requests from dynamically
scheduled tasks can be routed to the configured IM user.
EOF
)"
```

---

### Task 2: 一次性动态任务清理日志

**Files:**
- Modify: `src/systems/transform/trigger_task.rs:128-144`
- Test: `src/systems/transform/trigger_task.rs:215-261`

**Interfaces:**
- Consumes: `cleanup_scheduled_task_if_once(kind, &mut scheduler_state, &mut scheduled_registry)`
- Produces: 一次性任务触发后记录 `DynamicTaskRemoved` 结构化日志。

- [ ] **Step 1: 引入 `info` 日志宏**

将 `src/systems/transform/trigger_task.rs` 顶部的 use 改为：

```rust
use tracing::{debug, info, warn};
```

- [ ] **Step 2: 在清理函数中记录日志**

将 `cleanup_scheduled_task_if_once` 改为：

```rust
fn cleanup_scheduled_task_if_once(
    kind: &str,
    scheduler_state: &mut ResMut<SchedulerState>,
    scheduled_registry: &mut ResMut<ScheduledTaskRegistry>,
) {
    // 先拷出 is_once 标记，避免不可变借用阻塞后续 remove
    let Some(is_once) = scheduled_registry.get(kind).map(|info| info.is_once) else {
        return;
    };
    if !is_once {
        return;
    }
    scheduled_registry.remove(kind);
    scheduler_state
        .dynamic_tasks_mut()
        .retain(|t| t.kind != kind);
    info!(
        event = "DynamicTaskRemoved",
        kind = %kind,
        reason = "once scheduled task triggered",
        "dynamic once scheduled task removed after trigger"
    );
}
```

- [ ] **Step 3: 更新单元测试断言**

在 `scheduled_task_route_creates_create_task_message` 中，增加对 `approval_channel` 的断言：

```rust
assert_eq!(
    messages[0].routing_policy.approval_channel,
    Some(channel.clone())
);
```

- [ ] **Step 4: 运行相关单元测试**

Run: `cargo test --lib trigger_task_routing -- --nocapture`
Expected: PASS

- [ ] **Step 5: 提交**

```bash
git add src/systems/transform/trigger_task.rs
git commit -m "$(cat <<'EOF'
feat: log DynamicTaskRemoved when once scheduled task triggers

Adds structured logging after cleanup_scheduled_task_if_once removes a
one-time dynamic scheduled task from the registry and scheduler state.
EOF
)"
```

---

### Task 3: `FrontendRegistry::has_frontend` 辅助方法

**Files:**
- Modify: `src/app/mod.rs:175-178`
- Test: `src/app/mod.rs:337-347`

**Interfaces:**
- Consumes: `FrontendRegistry.frontends: Vec<Box<dyn Frontend>>`
- Produces: `pub fn has_frontend(&self, kind: FrontendKind) -> bool`

- [ ] **Step 1: 实现方法**

在 `src/app/mod.rs` 的 `FrontendRegistry` 结构体定义之后添加：

```rust
impl FrontendRegistry {
    /// 检查指定类型的 frontend 是否已在注册表中。
    /// 注意：返回 true 仅表示该 frontend 类型已注册，不保证底层 channel 当前可用
    ///（channel 可用性由 ChannelManager 的运行时发送结果覆盖）。
    pub fn has_frontend(&self, kind: FrontendKind) -> bool {
        self.frontends.iter().any(|f| f.kind() == kind)
    }
}
```

- [ ] **Step 2: 添加单元测试**

在 `src/app/mod.rs` 的 `#[cfg(test)]` 模块中添加：

```rust
#[test]
fn frontend_registry_has_frontend_checks_kind() {
    use crate::domain::{EngineEvent, Frontend, FrontendKind, UserAction};

    struct DummyFrontend(FrontendKind);
    impl Frontend for DummyFrontend {
        fn kind(&self) -> FrontendKind {
            self.0.clone()
        }
        fn push_event(&self, _event: EngineEvent) {}
        fn poll_actions(&self) -> Vec<UserAction> {
            vec![]
        }
    }

    let registry = FrontendRegistry {
        frontends: vec![
            Box::new(DummyFrontend(FrontendKind::Tui)),
            Box::new(DummyFrontend(FrontendKind::QQ)),
        ],
    };
    assert!(registry.has_frontend(FrontendKind::Tui));
    assert!(registry.has_frontend(FrontendKind::QQ));
    assert!(!registry.has_frontend(FrontendKind::Telegram));
    assert!(!registry.has_frontend(FrontendKind::Feishu));
}
```

- [ ] **Step 3: 运行相关单元测试**

Run: `cargo test --lib frontend_registry_has_frontend -- --nocapture`
Expected: PASS

- [ ] **Step 4: 提交**

```bash
git add src/app/mod.rs
git commit -m "$(cat <<'EOF'
feat: add FrontendRegistry::has_frontend helper

Adds a helper to check whether a frontend kind is registered in the
registry. The check only covers registration, not runtime availability.
EOF
)"
```

---

### Task 4: `frontend_output_system` 审批通道 frontend 注册检查

**Files:**
- Modify: `src/systems/frontend_output.rs:143-173`
- Test: `src/systems/frontend_output.rs:413-461` 附近新增测试

**Interfaces:**
- Consumes: `FrontendRegistry::has_frontend`, `TaskRoutingPolicy.approval_channel`
- Produces: 未注册 frontend 时任务状态变为 `Failed(Unknown)`，日志 `FrontendApprovalRouteInvalid`。

- [ ] **Step 1: 引入 `FrontendKind` 到 use 语句**

将 `src/systems/frontend_output.rs` 顶部的 use 改为：

```rust
use crate::domain::{
    Agent, AgentStatusKind, EngineEvent, EventTarget, FailureReason, FrontendKind, MessageRole,
    SystemOutputMessage, Task, TaskStatus, TaskStatusKind, ToolConfirmationRequestMessage,
    UserOutputMessage,
};
```

- [ ] **Step 2: 修改审批请求分支**

将 `src/systems/frontend_output.rs` 中 `// 审批请求` 块替换为：

```rust
    // 审批请求
    for (entity, confirmation) in &confirmations {
        // 事件任务的审批必须走路由策略中显式配置的 approval_channel。
        // 普通聊天任务的 approval_channel 与 output_channel 相同，由
        // TaskRoutingPolicy::conversational 构造时设置。
        let Some(approval_channel) = all_tasks
            .iter()
            .find(|(_, t)| t.id == confirmation.task_id)
            .and_then(|(_, t)| t.routing_policy.approval_channel.clone())
        else {
            // 缺少审批通道时，显式标记任务失败，避免任务卡在等待态
            if let Some((task_entity, task)) =
                all_tasks.iter().find(|(_, t)| t.id == confirmation.task_id)
            {
                let mut failed_task = task.clone();
                failed_task.status = TaskStatus::Failed(FailureReason::Unknown);
                failed_task.last_error =
                    Some("missing approval channel for event task approval request".to_string());
                commands.entity(task_entity).insert(failed_task);
            }
            warn!(
                event = "FrontendApprovalRouteMissing",
                task_id = %confirmation.task_id,
                request_id = %confirmation.request_id,
                "marking task failed because approval channel is missing"
            );
            commands.entity(entity).despawn();
            continue;
        };

        if !registry.has_frontend(approval_channel.frontend.clone()) {
            if let Some((task_entity, task)) =
                all_tasks.iter().find(|(_, t)| t.id == confirmation.task_id)
            {
                let frontend_name = match approval_channel.frontend {
                    FrontendKind::Tui => "tui",
                    FrontendKind::Telegram => "telegram",
                    FrontendKind::Web => "web",
                    FrontendKind::QQ => "qq",
                    FrontendKind::Feishu => "feishu",
                };
                let mut failed_task = task.clone();
                failed_task.status = TaskStatus::Failed(FailureReason::Unknown);
                failed_task.last_error = Some(format!(
                    "approval channel frontend '{}' is not enabled",
                    frontend_name
                ));
                commands.entity(task_entity).insert(failed_task);
            }
            warn!(
                event = "FrontendApprovalRouteInvalid",
                task_id = %confirmation.task_id,
                request_id = %confirmation.request_id,
                frontend = ?approval_channel.frontend,
                "marking task failed because approval channel frontend is not enabled"
            );
            commands.entity(entity).despawn();
            continue;
        }

        let target = EventTarget::Directed(vec![approval_channel]);

        debug!(
            event = "FrontendOutputApprovalRequest",
            task_id = %confirmation.task_id,
            agent_id = %confirmation.agent_id,
            request_id = %confirmation.request_id,
            tool_name = %confirmation.tool_name,
            option_count = confirmation.options.len(),
            "pushing approval request to frontends"
        );

        let options: Vec<crate::domain::ApprovalOption> = confirmation
            .options
            .iter()
            .map(|opt| crate::domain::ApprovalOption {
                id: opt.id.clone(),
                label: opt.label.clone(),
                description: if opt.id == "deny" {
                    "拒绝".to_string()
                } else {
                    match opt.mode {
                        crate::domain::GrantMode::Once => "仅本次允许".to_string(),
                        crate::domain::GrantMode::Permanent => "永久允许此工具".to_string(),
                    }
                },
            })
            .collect();

        let event = EngineEvent::ApprovalRequest {
            target,
            request_id: confirmation.request_id,
            agent_name: String::new(),
            tool_name: confirmation.tool_name.clone(),
            tool_input: confirmation.tool_input.clone(),
            options,
            approval_context: confirmation.approval_context.clone(),
        };
        for frontend in &registry.frontends {
            frontend.push_event(event.clone());
        }

        commands.entity(entity).despawn();
    }
```

- [ ] **Step 3: 添加单元测试 `approval_request_with_disabled_frontend_marks_task_failed`**

在 `src/systems/frontend_output.rs` 的测试模块末尾添加：

```rust
    #[test]
    fn approval_request_with_disabled_frontend_marks_task_failed() {
        let mut app = App::new();
        let events = Arc::new(Mutex::new(Vec::new()));
        // 只注册 Telegram frontend，QQ 未注册
        let frontend = MockFrontend {
            kind: FrontendKind::Telegram,
            events: events.clone(),
        };
        app.insert_resource(FrontendRegistry {
            frontends: vec![Box::new(frontend)],
        });
        app.add_systems(Update, frontend_output_system);

        let approval_channel = ChannelId {
            frontend: FrontendKind::QQ,
            user_id: "reviewer".to_string(),
            thread_id: None,
        };
        let task = Task::from_trigger(
            "nightly summary".to_string(),
            3,
            TaskRoutingPolicy::event(
                Some(approval_channel),
                Some("nightly summary timer".to_string()),
            ),
        );
        let task_id = task.id;
        app.world_mut().spawn(task);
        app.world_mut().spawn(ToolConfirmationRequestMessage {
            request_id: Uuid::new_v4(),
            task_id,
            agent_id: Uuid::nil(),
            tool_name: "shell_exec".to_string(),
            tool_input: serde_json::json!({"command": "date"}),
            options: ConfirmationOption::default_options(),
            source: ConfirmationSource::User,
            parent_agent_id: None,
            approval_context: Some("nightly summary timer".to_string()),
        });

        app.update();

        let task = app
            .world_mut()
            .query::<&Task>()
            .iter(app.world())
            .find(|task| task.id == task_id)
            .expect("task should remain for failure inspection");
        assert!(matches!(
            task.status,
            crate::domain::TaskStatus::Failed(crate::domain::FailureReason::Unknown)
        ));
        assert_eq!(
            task.last_error.as_deref(),
            Some("approval channel frontend 'qq' is not enabled")
        );

        let events = events.lock().unwrap();
        assert!(
            !events.iter().any(|e| matches!(e, EngineEvent::ApprovalRequest { .. })),
            "should not emit ApprovalRequest for disabled frontend"
        );
    }
```

- [ ] **Step 4: 添加单元测试 `scheduled_task_approval_request_routes_to_output_channel`**

在 `src/systems/frontend_output.rs` 的测试模块末尾添加：

```rust
    #[test]
    fn scheduled_task_approval_request_routes_to_output_channel() {
        let mut app = App::new();
        let events = Arc::new(Mutex::new(Vec::new()));
        let frontend = MockFrontend {
            kind: FrontendKind::QQ,
            events: events.clone(),
        };
        app.insert_resource(FrontendRegistry {
            frontends: vec![Box::new(frontend)],
        });
        app.add_systems(Update, frontend_output_system);

        let output_channel = ChannelId {
            frontend: FrontendKind::QQ,
            user_id: "reviewer".to_string(),
            thread_id: None,
        };
        let task = Task::from_trigger(
            "scheduled task".to_string(),
            3,
            TaskRoutingPolicy::scheduled_task(Some(output_channel.clone()), "scheduled task"),
        );
        let task_id = task.id;
        app.world_mut().spawn(task);
        app.world_mut().spawn(ToolConfirmationRequestMessage {
            request_id: Uuid::new_v4(),
            task_id,
            agent_id: Uuid::nil(),
            tool_name: "shell_exec".to_string(),
            tool_input: serde_json::Value::Null,
            options: ConfirmationOption::default_options(),
            source: ConfirmationSource::User,
            parent_agent_id: None,
            approval_context: Some("scheduled task".to_string()),
        });

        app.update();

        let events = events.lock().unwrap();
        let approval_target = events
            .iter()
            .find_map(|event| match event {
                EngineEvent::ApprovalRequest { target, .. } => Some(target.clone()),
                _ => None,
            })
            .expect("approval request should be emitted");

        match approval_target {
            EventTarget::Directed(channels) => {
                assert_eq!(channels, vec![output_channel]);
            }
            EventTarget::Broadcast => panic!("approval should route to scheduled task output channel"),
        }
    }
```

- [ ] **Step 5: 运行相关单元测试**

Run: `cargo test --lib frontend_output -- --nocapture`
Expected: PASS

- [ ] **Step 6: 提交**

```bash
git add src/systems/frontend_output.rs
git commit -m "$(cat <<'EOF'
feat: validate approval channel frontend registration

frontend_output_system now checks whether the approval_channel's
frontend kind is registered before pushing an approval request.
If not, the task is marked Failed(Unknown) with a clear error and
FrontendApprovalRouteInvalid is logged.
EOF
)"
```

---

### Task 5: 更新 `src/triggers/scheduled_task.rs` 相关测试

**Files:**
- Modify: `src/triggers/scheduled_task.rs:319-341`

**Interfaces:**
- Consumes: `TaskRoutingPolicy::scheduled_task` 的新语义
- Produces: 测试断言反映 `approval_channel == output_channel`。

- [ ] **Step 1: 更新 `build_routing_policy_uses_scheduled_task_constructor`**

```rust
#[test]
fn build_routing_policy_uses_scheduled_task_constructor() {
    let channel = sample_channel();
    let info = ScheduledTaskInfo {
        content: "x".to_string(),
        output_channel: Some(channel.clone()),
        is_once: false,
    };
    let policy = info.build_routing_policy();
    assert_eq!(policy.output_channel, Some(channel.clone()));
    assert_eq!(policy.approval_channel, Some(channel));
    assert_eq!(policy.approval_context.as_deref(), Some("scheduled task"));
}
```

- [ ] **Step 2: 更新 `build_routing_policy_supports_no_output_channel`**

```rust
#[test]
fn build_routing_policy_supports_no_output_channel() {
    let info = ScheduledTaskInfo {
        content: "y".to_string(),
        output_channel: None,
        is_once: true,
    };
    let policy = info.build_routing_policy();
    assert!(policy.output_channel.is_none());
    assert!(policy.approval_channel.is_none());
}
```

- [ ] **Step 3: 运行相关单元测试**

Run: `cargo test --lib scheduled_task -- --nocapture`
Expected: PASS

- [ ] **Step 4: 提交**

```bash
git add src/triggers/scheduled_task.rs
git commit -m "$(cat <<'EOF'
test: update scheduled_task routing policy assertions

Aligns unit tests with the new semantics where approval_channel
equals output_channel for scheduled tasks.
EOF
)"
```

---

### Task 6: 新增集成测试

**Files:**
- Create: `tests/schedule_task_approval_routing.rs`
- Create: `tests/disabled_approval_channel_fails_event_task.rs`

**Interfaces:**
- Consumes: `schedule_task` 工具、`TaskRoutingPolicy::scheduled_task`、`frontend_output_system`
- Produces: 集成测试验证端到端行为。

- [ ] **Step 1: 创建 `tests/schedule_task_approval_routing.rs`**

参考 `tests/schedule_task_tool.rs` 和 `tests/sequential_tool_confirmation.rs` 的 fixture 风格，构造一个测试：

1. 启动 ECS app，注册 QQ MockFrontend。
2. 调用 `schedule_task` 工具创建一次性任务，`output_channel=qq`，`target=某用户ID`。
3. 通过 `ExternalInput::Timer` 触发该任务（或等待触发，测试中可用时间旅行方式）。
4. 让任务执行 Agent 调用 `shell_exec`。
5. 验证 `MockFrontend` 收到 `EngineEvent::ApprovalRequest`，目标为指定 QQ 用户。
6. 验证任务状态未进入 Failed。

由于集成测试涉及时间触发，建议直接构造 `TriggerTaskMessage` 并 spawn 到 ECS，然后驱动 `app.update()`，避免真实等待。

- [ ] **Step 2: 创建 `tests/disabled_approval_channel_fails_event_task.rs`**

1. 启动 ECS app，只注册 Telegram MockFrontend（QQ 未注册）。
2. 构造一个 `Task::from_trigger`，`routing_policy = TaskRoutingPolicy::event(Some(qq_channel), Some("test"))`。
3. spawn `ToolConfirmationRequestMessage`。
4. 驱动 `app.update()`。
5. 验证任务状态为 `Failed(Unknown)`，且 `last_error` 为 `"approval channel frontend 'qq' is not enabled"`。

- [ ] **Step 3: 运行新增集成测试**

Run: `cargo test --test schedule_task_approval_routing -- --nocapture`
Expected: PASS

Run: `cargo test --test disabled_approval_channel_fails_event_task -- --nocapture`
Expected: PASS

- [ ] **Step 4: 提交**

```bash
git add tests/schedule_task_approval_routing.rs tests/disabled_approval_channel_fails_event_task.rs
git commit -m "$(cat <<'EOF'
test: add integration tests for scheduled task approval routing

- schedule_task_approval_routing: verifies approval requests are
  routed to the scheduled task's output_channel.
- disabled_approval_channel_fails_event_task: verifies event tasks
  fail when approval_channel points to an unregistered frontend.
EOF
)"
```

---

### Task 7: 运行完整 CI 检查

**Files:**
- All modified files

- [ ] **Step 1: 格式化**

Run: `cargo fmt --all`
Expected: 无输出（成功）

- [ ] **Step 2: Clippy**

Run: `cargo clippy --all-targets --all-features -- -D warnings`
Expected: 无警告，退出码 0

- [ ] **Step 3: 单元测试与集成测试**

Run: `cargo test --all-features`
Expected: 全部 PASS

- [ ] **Step 4: 提交格式修复（如有）**

```bash
git add -A
git diff --cached --quiet || git commit -m "chore: apply cargo fmt"
```

---

### Task 8: 文档同步

**Files:**
- Modify: `docs/current-state.md`（如动态任务相关描述需要更新）
- Modify: `docs/TODO.md`（如相关待办需要勾选/移除）
- Modify: `docs/README.md`（如有索引需要更新）

- [ ] **Step 1: 检查 `docs/current-state.md`**

搜索 "schedule_task"、"scheduled task"、"审批"、"approval" 等关键词。如果当前状态描述与本次修复冲突或遗漏，更新为：

> 动态 scheduled task 触发后，其 `output_channel` 同时作为审批通道，支持执行期需要用户确认的工具路由。

- [ ] **Step 2: 检查 `docs/TODO.md`**

如果 TODO 中有与本次修复对应的条目，标记为已完成或移除。

- [ ] **Step 3: 检查 `docs/README.md` 索引**

确认 `docs/superpowers/specs/2026-07-07-dynamic-scheduled-task-approval-routing-design.md` 和本计划文件已在索引中列出。如未列出，补充链接。

- [ ] **Step 4: 提交**

```bash
git add docs/
git diff --cached --quiet || git commit -m "docs: sync current-state and index for scheduled task approval routing"
```

---

## Self-Review Checklist

- [ ] **Spec coverage**: 每个设计目标（G1/G2/G3）都有对应任务。
- [ ] **Placeholder scan**: 计划中没有 TBD/TODO/"implement later"。
- [ ] **Type consistency**: `has_frontend` 签名一致，`TaskRoutingPolicy::scheduled_task` 返回类型一致。
- [ ] **Test coverage**: 单元测试和集成测试覆盖了三个目标。
- [ ] **Documentation**: current-state/TODO/README 已检查。
