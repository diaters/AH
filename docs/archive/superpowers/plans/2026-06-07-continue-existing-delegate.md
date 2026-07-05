> **状态：已归档** — 对应功能已合并到 main，归档于 2026-07-05

# ContinueExisting Delegate Reuse + Brain Parse Hardening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 继续已有多轮任务时默认复用上次 `delegate` 执行者，避免继续路径触发 Brain；同时提升 Brain 决策 JSON 解析对 BOM/零宽字符的健壮性，避免误失败。

**Architecture:** 在 Dispatch 阶段收敛 Brain 触发条件（仅处理 `delegate == None` 的 Ready/Pending 任务）；在普通任务派发中优先复用 `task.delegate`；在 Brain 决策解析前做输入净化并补齐单测。

**Tech Stack:** Rust, Bevy ECS, tracing, serde_json, cargo test

---

## Files Overview

**Modify**
- `src/systems/dispatch/brain_dispatch.rs`：Brain 分发系统，跳过已绑定 delegate 的任务
- `src/systems/dispatch/task_dispatch.rs`：普通任务分发系统，优先复用 delegate
- `src/llm/brain_prompt.rs`：Brain 决策解析函数，增强健壮性并补测试

**Reference**
- `src/systems/transform/brain_decision.rs`：BrainDecision 结果消费与失败处理（不改，但作为回归参考）
- `src/domain/task.rs`：Task 状态字段与 `delegate` 字段定义（不改，但作为回归参考）

---

### Task 1: 为 BrainDecision 解析增加“不可见字符”兼容单测（先失败）

**Files:**
- Modify: `src/llm/brain_prompt.rs`

- [ ] **Step 1: 添加 BOM 前缀解析测试（应先失败）**

在 `src/llm/brain_prompt.rs` 的 `#[cfg(test)] mod tests` 中新增测试：

```rust
#[test]
fn parse_json_with_bom_prefix() {
    let raw = "\u{feff}{\"selected_agent_name\":\"worker\",\"delegate_prompt\":\"do it\",\"reasoning\":\"test\"}";
    let output = parse_brain_decision(raw).expect("should parse");
    assert_eq!(output.selected_agent_name, "worker");
}
```

- [ ] **Step 2: 添加零宽字符前缀解析测试（应先失败）**

```rust
#[test]
fn parse_json_with_zero_width_prefix() {
    let raw = "\u{200b}{\"selected_agent_name\":\"worker\",\"delegate_prompt\":\"do it\",\"reasoning\":\"test\"}";
    let output = parse_brain_decision(raw).expect("should parse");
    assert_eq!(output.selected_agent_name, "worker");
}
```

- [ ] **Step 3: 添加 code block + BOM 组合测试（应先失败）**

```rust
#[test]
fn parse_json_code_block_with_bom_prefix() {
    let raw = format!(
        "```json\n{}\n```",
        "\u{feff}{\"selected_agent_name\":\"worker\",\"delegate_prompt\":\"do it\",\"reasoning\":\"test\"}"
    );
    let output = parse_brain_decision(&raw).expect("should parse");
    assert_eq!(output.selected_agent_name, "worker");
}
```

- [ ] **Step 4: 运行单测，确认失败点与错误信息**

Run:

```bash
cargo test -q llm::brain_prompt::tests::parse_json_with_bom_prefix -- --nocapture
```

Expected:
- FAIL，报错类似 `ParseFailed("expected value at line 1 column 1")`

- [ ] **Step 5: Commit**

```bash
git add src/llm/brain_prompt.rs
git commit -m "test: add failing tests for brain decision parsing with invisible prefixes"
```

---

### Task 2: 实现 BrainDecision 解析输入净化（让测试通过）

**Files:**
- Modify: `src/llm/brain_prompt.rs`

- [ ] **Step 1: 增加输入净化函数**

在 `src/llm/brain_prompt.rs` 中新增函数（与现有风格一致，放在 `parse_brain_decision` 附近）：

```rust
fn sanitize_json_like_input(raw: &str) -> String {
    let mut s = raw.trim().to_string();

    if let Some(stripped) = s.strip_prefix('\u{feff}') {
        s = stripped.to_string();
    }

    loop {
        let next = s.strip_prefix('\u{200b}')
            .or_else(|| s.strip_prefix('\u{200c}'))
            .or_else(|| s.strip_prefix('\u{200d}'))
            .or_else(|| s.strip_prefix('\u{2060}'));

        if let Some(stripped) = next {
            s = stripped.to_string();
            continue;
        }

        break;
    }

    s
}
```

- [ ] **Step 2: 将 sanitize 应用到 extract_json_block 的结果上**

把 `parse_brain_decision` 改为：

```rust
pub fn parse_brain_decision(raw: &str) -> Result<BrainDecisionOutput, BrainDecisionError> {
    if raw.trim().is_empty() {
        return Err(BrainDecisionError::EmptyResponse);
    }

    let json_str = extract_json_block(raw);
    let json_str = sanitize_json_like_input(json_str);

    serde_json::from_str::<BrainDecisionOutput>(&json_str)
        .map_err(|e| BrainDecisionError::ParseFailed(e.to_string()))
}
```

- [ ] **Step 3: 运行新增单测，确认通过**

Run:

```bash
cargo test -q llm::brain_prompt::tests::parse_json_with_bom_prefix -- --nocapture
cargo test -q llm::brain_prompt::tests::parse_json_with_zero_width_prefix -- --nocapture
cargo test -q llm::brain_prompt::tests::parse_json_code_block_with_bom_prefix -- --nocapture
```

Expected:
- PASS

- [ ] **Step 4: 全量回归该模块测试**

Run:

```bash
cargo test -q llm::brain_prompt::tests -- --nocapture
```

Expected:
- PASS

- [ ] **Step 5: Commit**

```bash
git add src/llm/brain_prompt.rs
git commit -m "fix: harden brain decision parsing against invisible prefixes"
```

---

### Task 3: Brain Dispatch 收敛（仅对 delegate 为空的任务触发）

**Files:**
- Modify: `src/systems/dispatch/brain_dispatch.rs`

- [ ] **Step 1: 添加跳过条件与 trace 日志**

在 `brain_dispatch_system` 的任务循环中，在状态检查通过后增加：

```rust
if task.delegate.is_some() {
    trace!(
        event = "BrainDispatchSkipped",
        task_id = %task.id,
        has_delegate = true,
        task_status = ?task.status,
        "skip brain dispatch because task has delegate"
    );
    continue;
}
```

要求：
- 仅用 `trace!`，避免噪声
- 字段遵循现有结构化风格（必须含 `event`）

- [ ] **Step 2: 运行单测/编译检查**

Run:

```bash
cargo test -q
```

Expected:
- PASS

- [ ] **Step 3: Commit**

```bash
git add src/systems/dispatch/brain_dispatch.rs
git commit -m "fix: skip brain dispatch for tasks with delegate"
```

---

### Task 4: 普通任务分发优先复用 delegate

**Files:**
- Modify: `src/systems/dispatch/task_dispatch.rs`

- [ ] **Step 1: 在 task_dispatch_system 中添加 delegate 快路径（先写测试）**

在 `src/systems/dispatch/task_dispatch.rs` 的 `#[cfg(test)] mod tests` 增加一个最小系统测试，目标验证：
- 当 `task.delegate = Some(agent_id)` 且 `task.status = Ready` 时，system 生成的 `AgentExecutionRequestMessage.request.agent_id == agent_id`

建议测试结构（按现有工程风格使用 Bevy World/App 运行系统；若当前文件没有类似测试可参考其它系统测试写法）：

```rust
#[test]
fn dispatch_reuses_delegate_when_present() {
    use bevy::prelude::*;
    use crate::domain::{Agent, AgentCapabilities, AgentKind, AgentProfile, AgentToolPermissions, ChannelId, FrontendKind, SpaceToolRegistry, Task};
    use crate::systems::dispatch::task_dispatch_system;
    use crate::app::Clock;
    use uuid::Uuid;

    let mut app = App::new();
    app.insert_resource(Clock::default());
    app.insert_resource(SpaceToolRegistry::default());

    let agent_id = Uuid::new_v4();
    app.world_mut().spawn(Agent {
        id: agent_id,
        profile: AgentProfile { name: "worker".to_string(), model: "test".to_string() },
        capabilities: AgentCapabilities { tags: vec!["llm".to_string()], description: "test".to_string() },
        kind: AgentKind::Persistent,
        parent_id: None,
        bound_task_id: None,
        tool_permissions: AgentToolPermissions::default(),
        experience: Default::default(),
    });

    let mut task = Task::from_user_input_ready(
        "run something",
        3,
        ChannelId { frontend: FrontendKind::Tui, user_id: "default".to_string() },
    );
    task.delegate = Some(agent_id);
    app.world_mut().spawn(task);

    app.add_systems(Update, task_dispatch_system);
    app.update();

    let mut q = app.world_mut().query::<&crate::domain::AgentExecutionRequestMessage>();
    let reqs: Vec<_> = q.iter(app.world()).collect();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].request.agent_id, agent_id);
}
```

注意：
- 该测试会在实现 delegate 快路径之前失败（因为当前会重新选择 agent）

- [ ] **Step 2: 运行单测，确认失败**

Run:

```bash
cargo test -q dispatch_reuses_delegate_when_present -- --nocapture
```

Expected:
- FAIL（agent_id 不匹配或生成 0 条请求）

- [ ] **Step 3: 实现 delegate 快路径**

在 `task_dispatch_system` 中，在选择器之前加入：

1) 如果 `task.delegate.is_some()`：
   - 在 `agents` query 中查找对应 agent（仅 Persistent）
   - 若找到：
     - 用该 agent 构建 prompt/tools/request
     - 打日志 `AgentSelected` 的 `selection_reason = "reuse_delegate"`
     - `task.mark_waiting_for_agent(agent.id, clock.0);`
     - `commands.spawn(AgentExecutionRequestMessage { request });`
     - `continue;`
   - 若找不到：回退到原选择逻辑

需要构建 prompt/tools 的方式保持与现有一致：
- prompt：调用 `build_prompt_with_context(&task.content, short_term, long_term_for_that_agent)`
- tools：按 registry + permission 非 Deny 的逻辑

- [ ] **Step 4: 运行新增单测，确认通过**

Run:

```bash
cargo test -q dispatch_reuses_delegate_when_present -- --nocapture
```

Expected:
- PASS

- [ ] **Step 5: 全量回归**

Run:

```bash
cargo test -q
```

Expected:
- PASS

- [ ] **Step 6: Commit**

```bash
git add src/systems/dispatch/task_dispatch.rs
git commit -m "feat: reuse delegate in task dispatch for continued tasks"
```

---

### Task 5: 日志回归验证（复现你提供的场景）

**Files:**
- No code changes required (optional tweaks if logs show missing fields)

- [ ] **Step 1: 运行 TUI 复现最小流程**

手动流程：
1. 输入“请用Python运行一个http服务器来展示当前工作区文件夹的内容”
2. 允许必要工具
3. 让任务进入 `Waiting(User)`
4. 输入“我需要你来运行”

Expected（观察日志）：
- 出现 `RoutingDecision decision="continue_existing"`
- 出现 `TaskContinued new_status="Ready"`
- 不出现 `BrainDispatch`（因为 delegate 已存在）
- 出现 `AgentSelected selection_reason="reuse_delegate"` 且 `selected_agent_id` 为上次 delegate

- [ ] **Step 2: 若出现异常路径，补充最小修复并记录原因**

允许的小修复范围：
- delegate 未被设置/被覆盖：需要追溯 `mark_waiting_for_agent` 的调用点或 continue 逻辑是否重置 delegate
- agent 查找失败：确认 agent kind 与 id 规则

- [ ] **Step 3: Commit（若 Step 2 有改动）**

```bash
git add -A
git commit -m "fix: ensure continued tasks reuse delegate end-to-end"
```

---

## Plan Self-Review

**Spec coverage**
- continue_existing 默认复用 delegate：Task 3 + Task 4
- continue 不触发 Brain：Task 3
- Brain parse 兼容 BOM/零宽字符：Task 1 + Task 2

**Placeholder scan**
- 无 TBD/TODO
- 每步含具体代码/命令/期望结果

**Type consistency**
- 使用现有 `Task.delegate: Option<AgentId>`、`TaskStatus::{Ready, Pending}`、`AgentKind::Persistent`、`AgentExecutionRequestMessage`

---

## Execution Handoff

计划已保存到 `docs/superpowers/plans/2026-06-07-continue-existing-delegate.md`。两种执行方式：

1. **Subagent-Driven（推荐）**：我按 Task 拆分逐个派发子代理执行，逐步 review
2. **Inline Execution**：在当前会话中按计划逐步实现并在关键点停下来给你 review

你选哪一种？

