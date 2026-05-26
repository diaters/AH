# 删除 Echo 工具 + 修复子任务 Agent 匹配逻辑 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 删除测试用 echo 工具，修复子任务分派时 Agent 选择逻辑——当前逻辑用 allowed_tools 与 agent tags 做交集，两个命名空间完全不同导致所有 agent 评分为 0，最终随机选中 summarizer。

**Architecture:** 1) 从 register_builtin_tools 和测试中移除 echo；2) 将子任务 agent 选择从 broken 的 tools-tags 交集改为复用已有的 select_agent_with_memory（基于 task content 与 agent tags 匹配），并在所有评分为 0 时优先选择带 "default" tag 的 agent 作为 fallback。

**Tech Stack:** Rust, Bevy ECS, tracing

---

## File Structure

| File | Responsibility |
|------|---------------|
| `src/systems/tool.rs` | 移除 EchoTool struct/impl 和 register_builtin_tools 中的 echo 注册 |
| `src/systems/dispatch.rs` | 修复 brain_dispatch_system 中子任务 agent 选择逻辑 |
| `tests/tool_execution_flow.rs` | 将 echo 替换为 knowledge_search 作为测试工具 |
| `tests/llm_tool_calling_flow.rs` | 同上 |

---

### Task 1: 删除 EchoTool 实现

**Files:**
- Modify: `src/systems/tool.rs:22-36` (EchoTool struct + impl)
- Modify: `src/systems/tool.rs:122-130` (register_builtin_tools 中的 echo 注册)

- [ ] **Step 1: 删除 EchoTool struct 和 BuiltinTool impl**

删除 `src/systems/tool.rs` 第 22-36 行：

```rust
// 删除以下全部代码
struct EchoTool;

impl BuiltinTool for EchoTool {
    fn name(&self) -> &str {
        "echo"
    }

    fn execute(
        &self,
        input: &serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolAction, ToolError> {
        Ok(ToolAction::Direct(input.clone()))
    }
}
```

- [ ] **Step 2: 从 register_builtin_tools 中删除 echo 注册**

删除 `src/systems/tool.rs` 第 122-130 行：

```rust
// 删除以下全部代码
    registry.register(ToolDefinition {
        name: "echo".to_string(),
        description: "Echo back the input message".to_string(),
        parameters: ToolSchema::default(),
        default_permission: ToolPermission::Allow,
        executor: ToolExecutorKind::Builtin("echo".to_string()),
        required_tag: None,
    });
    executors.register(Box::new(EchoTool));
```

- [ ] **Step 3: 删除 echo 相关单元测试**

删除 `src/systems/tool.rs` 中的两个测试函数：
- `register_builtin_tools_adds_echo`（约第 1453-1459 行）
- `executor_echo_direct_action`（约第 1462-1475 行）

- [ ] **Step 4: 编译验证**

Run: `cargo check 2>&1 | head -30`
Expected: 编译错误仅出现在测试文件中引用 "echo" 的地方（下一步处理）

- [ ] **Step 5: Commit**

```bash
git add src/systems/tool.rs
git commit -m "refactor: remove EchoTool and its registration"
```

---

### Task 2: 更新测试文件中的 echo 引用

**Files:**
- Modify: `tests/tool_execution_flow.rs` — 将所有 `"echo"` 替换为 `"knowledge_search"`
- Modify: `tests/llm_tool_calling_flow.rs` — 同上

- [ ] **Step 1: 更新 tests/tool_execution_flow.rs**

该文件大量使用 `"echo"` 作为测试工具。需要将所有 `"echo"` 引用替换为 `"knowledge_search"`，同时：
- 将 `ToolSchema::default()` 替换为 knowledge_search 的 schema（带 `query` 参数）
- 将 `ToolExecutorKind::Builtin("echo".to_string())` 替换为 `ToolExecutorKind::Builtin("knowledge_search".to_string())`
- 在测试中构造 tool 调用时，将 echo 的空参数 `{}` 替换为 knowledge_search 的有效参数 `{"query": "test"}`

具体替换规则：
- `"echo".to_string()` → `"knowledge_search".to_string()`
- `ToolSchema::default()` (仅在 ToolDefinition 构造中) → knowledge_search 的 schema
- `tool_name: "echo".to_string()` → `tool_name: "knowledge_search".to_string()`
- `"tools": ["echo"]` → `"tools": ["knowledge_search"]`
- `tool_input` 为空对象 `{}` 的 tool 调用 → `{"query": "test"}`

- [ ] **Step 2: 更新 tests/llm_tool_calling_flow.rs**

同样的替换规则。该文件中 echo 出现在：
- LlmToolCall 构造中 `name: "echo".to_string()`
- ToolDefinition 构造中 `name: "echo".to_string()` 和 `executor: ToolExecutorKind::Builtin("echo".to_string())`

- [ ] **Step 3: 编译并运行测试**

Run: `cargo test --test tool_execution_flow --test llm_tool_calling_flow 2>&1 | tail -20`
Expected: 所有测试通过

- [ ] **Step 4: Commit**

```bash
git add tests/tool_execution_flow.rs tests/llm_tool_calling_flow.rs
git commit -m "test: replace echo with knowledge_search in test files"
```

---

### Task 3: 修复子任务 Agent 匹配逻辑

**Files:**
- Modify: `src/systems/dispatch.rs:237-247` (brain_dispatch_system 中的子任务 agent 选择)

当前逻辑（broken）：
```rust
let child_agent = agents
    .iter()
    .filter(|a| a.kind == AgentKind::Persistent)
    .filter(|a| !a.capabilities.tags.contains(&"brain".to_string()))
    .max_by_key(|a| {
        a.capabilities
            .tags
            .iter()
            .filter(|t| config.allowed_tools.contains(t))
            .count()
    });
```

问题：`config.allowed_tools` 是工具名列表（如 `["knowledge_search"]`），`a.capabilities.tags` 是 agent 标签（如 `["llm", "default"]`），两个命名空间完全不同，交集永远为 0。

- [ ] **Step 1: 编写子任务 agent 匹配的单元测试**

在 `src/systems/dispatch.rs` 的 `#[cfg(test)]` 模块中添加测试：

```rust
#[test]
fn sub_task_agent_selection_prefers_default_on_no_match() {
    // 构造两个 agent：default-llm-agent (tags: ["llm", "default", "general"])
    // 和 summarizer (tags: ["summarization", "memory"])
    // 任务内容为中文，不包含任何 tag 关键词
    // 期望选中 default-llm-agent（因为有 "default" tag 作为 fallback）
    let default_agent = Agent {
        id: Uuid::new_v4(),
        profile: AgentProfile {
            name: "default-llm-agent".to_string(),
            model: "gpt-4.1-mini".to_string(),
            description: "General purpose LLM agent".to_string(),
        },
        capabilities: AgentCapabilities {
            tags: vec!["llm".to_string(), "default".to_string(), "general".to_string()],
            description: String::new(),
        },
        kind: AgentKind::Persistent,
        parent_id: None,
        bound_task_id: None,
        tool_permissions: AgentToolPermissions::default(),
        experience: AgentExperience::default(),
    };
    let summarizer = Agent {
        id: Uuid::new_v4(),
        profile: AgentProfile {
            name: "summarizer".to_string(),
            model: "gpt-4.1-mini".to_string(),
            description: "Summarization agent".to_string(),
        },
        capabilities: AgentCapabilities {
            tags: vec!["summarization".to_string(), "memory".to_string()],
            description: String::new(),
        },
        kind: AgentKind::Persistent,
        parent_id: None,
        bound_task_id: None,
        tool_permissions: AgentToolPermissions::default(),
        experience: AgentExperience::default(),
    };

    let agents = vec![
        (&default_agent, None as Option<&LongTermMemory>),
        (&summarizer, None as Option<&LongTermMemory>),
    ];

    let task_content = "请计算兔子的繁衍数量";
    let selected = select_agent_for_sub_task(
        agents.into_iter(),
        task_content,
    );
    assert!(selected.is_some());
    let (agent, _) = selected.unwrap();
    assert_eq!(agent.profile.name, "default-llm-agent");
}

#[test]
fn sub_task_agent_selection_prefers_higher_score() {
    // 当 task content 包含 "summarization" 时，summarizer 应得更高分
    let default_agent = Agent {
        id: Uuid::new_v4(),
        profile: AgentProfile {
            name: "default-llm-agent".to_string(),
            model: "gpt-4.1-mini".to_string(),
            description: "General purpose LLM agent".to_string(),
        },
        capabilities: AgentCapabilities {
            tags: vec!["llm".to_string(), "default".to_string(), "general".to_string()],
            description: String::new(),
        },
        kind: AgentKind::Persistent,
        parent_id: None,
        bound_task_id: None,
        tool_permissions: AgentToolPermissions::default(),
        experience: AgentExperience::default(),
    };
    let summarizer = Agent {
        id: Uuid::new_v4(),
        profile: AgentProfile {
            name: "summarizer".to_string(),
            model: "gpt-4.1-mini".to_string(),
            description: "Summarization agent".to_string(),
        },
        capabilities: AgentCapabilities {
            tags: vec!["summarization".to_string(), "memory".to_string()],
            description: String::new(),
        },
        kind: AgentKind::Persistent,
        parent_id: None,
        bound_task_id: None,
        tool_permissions: AgentToolPermissions::default(),
        experience: AgentExperience::default(),
    };

    let agents = vec![
        (&default_agent, None as Option<&LongTermMemory>),
        (&summarizer, None as Option<&LongTermMemory>),
    ];

    let task_content = "Please perform summarization of the text";
    let selected = select_agent_for_sub_task(
        agents.into_iter(),
        task_content,
    );
    assert!(selected.is_some());
    let (agent, _) = selected.unwrap();
    assert_eq!(agent.profile.name, "summarizer");
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --lib dispatch 2>&1 | tail -20`
Expected: 编译失败（`select_agent_for_sub_task` 不存在）

- [ ] **Step 3: 实现 select_agent_for_sub_task 函数**

在 `src/systems/dispatch.rs` 中，在 `select_agent_with_memory` 函数之后添加：

```rust
/// 为子任务选择 Agent：基于 task content 与 agent tags 匹配评分，
/// 所有评分为 0 时优先选择带 "default" tag 的 agent 作为 fallback
fn select_agent_for_sub_task<'a>(
    agents: impl Iterator<Item = (&'a Agent, Option<&'a LongTermMemory>)>,
    task_content: &str,
) -> Option<(&'a Agent, Option<&'a LongTermMemory>)> {
    let candidates: Vec<_> = agents
        .filter(|(a, _)| a.kind == AgentKind::Persistent)
        .filter(|(a, _)| !a.capabilities.tags.contains(&"brain".to_string()))
        .collect();

    if candidates.is_empty() {
        return None;
    }

    // 计算所有候选的 match_score
    let scored: Vec<_> = candidates
        .iter()
        .map(|(a, ltm)| (*a, *ltm, match_score(a, task_content)))
        .collect();

    let max_score = scored.iter().map(|(_, _, s)| *s).max().unwrap_or(0);

    let selected = if max_score > 0 {
        // 有正向匹配：选最高分
        scored
            .into_iter()
            .filter(|(_, _, s)| *s == max_score)
            .max_by_key(|(a, _, _)| {
                // 同分时优先 "default" tag
                a.capabilities.tags.contains(&"default".to_string()) as usize
            })
    } else {
        // 全部评分为 0：fallback 到带 "default" tag 的 agent
        scored
            .into_iter()
            .filter(|(a, _, _)| a.capabilities.tags.contains(&"default".to_string()))
            .collect::<Vec<_>>()
            .into_iter()
            .max_by_key(|(a, _, _)| a.capabilities.tags.len())
    };

    if let Some((agent, ltm, score)) = selected {
        let all_scores: Vec<_> = candidates
            .iter()
            .map(|(a, _)| (a.profile.name.clone(), match_score(a, task_content)))
            .collect();
        debug!(
            event = "SubTaskAgentScoring",
            selected_agent = %agent.profile.name,
            selected_score = score,
            all_candidates_scores = ?all_scores,
            task_content_preview = %task_content.chars().take(100).collect::<String>(),
            fallback = (max_score == 0),
            "sub-task agent scoring completed"
        );
        Some((agent, ltm))
    } else {
        // 无 "default" tag 的 fallback：选第一个候选
        let (agent, ltm) = candidates.into_iter().next()?;
        debug!(
            event = "SubTaskAgentScoring",
            selected_agent = %agent.profile.name,
            selected_score = 0,
            fallback = true,
            "sub-task agent selected as last resort (no default tag found)"
        );
        Some((agent, ltm))
    }
}
```

- [ ] **Step 4: 替换 brain_dispatch_system 中的子任务 agent 选择逻辑**

将 `src/systems/dispatch.rs` 第 237-247 行替换为：

```rust
            let child_agent = select_agent_for_sub_task(
                agents.iter().map(|a| (a, None::<&LongTermMemory>)),
                &task.content,
            );
```

注意：`brain_dispatch_system` 的 `agents` Query 是 `Query<&Agent>`（无 LTM），所以需要 map 为 `(a, None)`。

- [ ] **Step 5: 运行测试确认通过**

Run: `cargo test --lib dispatch 2>&1 | tail -20`
Expected: 所有测试通过

- [ ] **Step 6: 运行全量测试**

Run: `cargo test 2>&1 | tail -30`
Expected: 所有测试通过

- [ ] **Step 7: Commit**

```bash
git add src/systems/dispatch.rs
git commit -m "fix: sub-task agent selection uses content-tag matching with default fallback"
```

---

### Task 4: 更新 tool.rs 中剩余测试的 echo 引用

**Files:**
- Modify: `src/systems/tool.rs` — 更新 `#[cfg(test)]` 模块中引用 "echo" 的测试

- [ ] **Step 1: 更新 src/systems/tool.rs 测试中的 echo 引用**

在 `src/systems/tool.rs` 的测试模块中，以下位置引用了 "echo"：
- `executor_spawn_agent` 测试中 `"tools": ["echo", "knowledge_search"]` → `"tools": ["knowledge_search"]`
- `parse_create_tasks_params` 相关测试中 `"tools": ["echo"]` → `"tools": ["knowledge_search"]`
- `AgentToolPermissions` 测试中 `.insert("echo".to_string(), ...)` → `.insert("knowledge_search".to_string(), ...)`

- [ ] **Step 2: 运行全量测试**

Run: `cargo test 2>&1 | tail -30`
Expected: 所有测试通过

- [ ] **Step 3: Commit**

```bash
git add src/systems/tool.rs
git commit -m "test: replace remaining echo references with knowledge_search"
```

---

### Task 5: cargo fmt + clippy

- [ ] **Step 1: 格式化和 lint**

Run: `cargo fmt && cargo clippy 2>&1 | tail -20`
Expected: 无警告无错误

- [ ] **Step 2: 最终全量测试**

Run: `cargo test 2>&1 | tail -30`
Expected: 所有测试通过

- [ ] **Step 3: Commit（如有格式变更）**

```bash
git add -u
git commit -m "style: apply cargo fmt and clippy fixes"
```
