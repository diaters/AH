# 子任务结果传递机制 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让依赖子任务能通过 Prompt 注入获取兄弟任务的执行结果，解决子任务间数据传递断裂的问题。

**Architecture:** 在 `brain_dispatch_system` 中为子任务注入 system_prompt（含标记对总结指令）和兄弟任务结果；在 `llm_response_system` 中从子任务输出提取 `<<<RESULT>>>` 标记对内容作为 `result_summary`。无需新增依赖或 domain 字段。

**Tech Stack:** Rust, Bevy ECS, 纯字符串匹配（不引入 regex）

---

### Task 1: 添加标记对提取函数

**Files:**
- Modify: `src/systems/transform.rs`

- [ ] **Step 1: 在 `transform.rs` 顶部或模块中添加 `extract_result_summary` 函数**

在 `llm_response_system` 函数之前添加：

```rust
/// 从子任务输出中提取 <<<RESULT>>>...<<</RESULT>>> 标记对内容。
/// 提取最后一对标记。如果未找到，返回 None。
fn extract_result_summary(text: &str) -> Option<String> {
    let end_tag = "<<</RESULT>>>";
    let start_tag = "<<<RESULT>>>";

    // 从后向前找最后一个 end_tag
    let end_pos = text.rfind(end_tag)?;
    // 在 end_tag 之前找对应的 start_tag
    let before_end = &text[..end_pos];
    let start_pos = before_end.rfind(start_tag)?;

    let content_start = start_pos + start_tag.len();
    let content = text[content_start..end_pos].trim();
    if content.is_empty() {
        None
    } else {
        Some(content.to_string())
    }
}
```

- [ ] **Step 2: 添加单元测试**

在 `transform.rs` 底部的 `#[cfg(test)]` 模块中（如果没有则创建）添加：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_result_summary_basic() {
        let text = "一些分析\n\n<<<RESULT>>>\n69.7亿只\n<<</RESULT>>>";
        assert_eq!(extract_result_summary(text), Some("69.7亿只".to_string()));
    }

    #[test]
    fn test_extract_result_summary_no_marker() {
        let text = "没有标记对的普通输出";
        assert_eq!(extract_result_summary(text), None);
    }

    #[test]
    fn test_extract_result_summary_multiple_takes_last() {
        let text = "<<<RESULT>>>\n中间结果\n<<</RESULT>>>\n继续分析\n\n<<<RESULT>>>\n最终结果\n<<</RESULT>>>";
        assert_eq!(extract_result_summary(text), Some("最终结果".to_string()));
    }

    #[test]
    fn test_extract_result_summary_empty_content() {
        let text = "<<<RESULT>>>\n<<</RESULT>>>";
        assert_eq!(extract_result_summary(text), None);
    }

    #[test]
    fn test_extract_result_summary_multiline() {
        let text = "详细计算过程...\n\n<<<RESULT>>>\n一对小猫10年内可繁衍约69.7亿只\n公式: 2×3^20\n<<</RESULT>>>";
        let result = extract_result_summary(text).unwrap();
        assert!(result.contains("69.7亿只"));
        assert!(result.contains("2×3^20"));
    }
}
```

- [ ] **Step 3: 运行测试验证通过**

Run: `cargo test test_extract_result_summary`
Expected: 5 tests passed

- [ ] **Step 4: Commit**

```bash
git add src/systems/transform.rs
git commit -m "feat: add extract_result_summary for <<<RESULT>>> marker extraction"
```

---

### Task 2: 在 llm_response_system 中提取标记对并作为 result_summary

**Files:**
- Modify: `src/systems/transform.rs:329-330`（`mark_done` 调用处）

- [ ] **Step 1: 修改 `llm_response_system` 中 `OutputContent::Text` 分支的 `mark_done` 调用**

当前代码（约第 329-330 行）：

```rust
                    } else {
                        task.mark_done(content.clone(), clock.0);
```

替换为：

```rust
                    } else {
                        // 子任务：从输出中提取 <<<RESULT>>> 标记对作为 result_summary
                        let result_summary = if task.parent_task_id.is_some() {
                            match extract_result_summary(content) {
                                Some(summary) => summary,
                                None => {
                                    warn!(
                                        event = "ResultMarkerNotFound",
                                        task_id = %task.id,
                                        "sub-task output missing <<<RESULT>>> marker, using full output as fallback"
                                    );
                                    content.clone()
                                }
                            }
                        } else {
                            content.clone()
                        };
                        task.mark_done(result_summary, clock.0);
```

注意：`mark_done` 的参数会同时赋值给 `task.result_summary` 和用于日志，所以子任务的 `result_summary` 将是精炼后的标记对内容，而 `content` 仍保留完整输出用于 `UserOutputMessage`。

- [ ] **Step 2: 确认 `warn!` 宏已导入**

在 `transform.rs` 顶部检查 `use tracing::{debug, trace};` 是否包含 `warn`，如果没有则添加：

```rust
use tracing::{debug, trace, warn};
```

- [ ] **Step 3: 运行编译验证**

Run: `cargo build`
Expected: 编译成功，无错误

- [ ] **Step 4: Commit**

```bash
git add src/systems/transform.rs
git commit -m "feat: extract <<<RESULT>>> marker in sub-task outputs for result_summary"
```

---

### Task 3: 子任务 system_prompt 注入总结指令

**Files:**
- Modify: `src/systems/dispatch.rs:221-230`（`AgentSpawnRequestMessage` 构建处）

- [ ] **Step 1: 添加子任务 system_prompt 常量**

在 `dispatch.rs` 中 `brain_dispatch_system` 函数之前添加：

```rust
const SUB_TASK_SYSTEM_PROMPT: &str = "\
你是一个专注于完成特定子任务的 AI Agent。请仔细阅读任务描述，认真完成分配给你的工作。

重要：请在回答的最后，用 <<<RESULT>>> 和 <<</RESULT>>> 标记包围你的核心结论或最终答案。
标记内的内容应当精炼、自包含，便于其他任务引用。

示例格式：
（你的详细分析和推理过程...）

<<<RESULT>>>
你的精炼结论
<<</RESULT>>>";
```

- [ ] **Step 2: 修改 `brain_dispatch_system` 中 `AgentSpawnRequestMessage` 的 `task_system_prompt` 字段**

当前代码（约第 221-230 行）：

```rust
                commands.spawn(AgentSpawnRequestMessage {
                    parent_agent_id: config.parent_agent_id,
                    task_id: child_task_id,
                    name: config.child_agent_name.clone(),
                    model: config.child_agent_model.clone(),
                    description: config.child_agent_name.clone(),
                    tools: config.allowed_tools.clone(),
                    task_prompt: task.content.clone(),
                    task_system_prompt: None,
                });
```

替换 `task_system_prompt: None` 为：

```rust
                    task_system_prompt: Some(SUB_TASK_SYSTEM_PROMPT.to_string()),
```

- [ ] **Step 3: 运行编译验证**

Run: `cargo build`
Expected: 编译成功

- [ ] **Step 4: Commit**

```bash
git add src/systems/dispatch.rs
git commit -m "feat: inject RESULT marker instructions into sub-task system_prompt"
```

---

### Task 4: 依赖子任务 Prompt 注入兄弟任务结果

**Files:**
- Modify: `src/systems/dispatch.rs:221-230`（同一处 `AgentSpawnRequestMessage`）

- [ ] **Step 1: 在 `brain_dispatch_system` 中构建兄弟任务结果字符串**

在 `brain_dispatch_system` 中，`deps_satisfied` 判断为 true 之后、`commands.spawn(AgentSpawnRequestMessage {` 之前，添加兄弟结果收集逻辑：

当前代码（约第 194-221 行）：

```rust
            if !deps_satisfied {
                trace!(
                    event = "SubTaskWaitingForDependencies",
                    task_id = %task.id,
                    child_name = %config.child_agent_name,
                    depends_on = ?config.depends_on,
                    "sub-task waiting for dependencies to complete"
                );
                continue;
            }

            // 选择匹配的 Agent（基于所需工具标签）
            ...
```

在 `deps_satisfied` 检查之后、`选择匹配的 Agent` 注释之前，插入：

```rust
            // 收集依赖的兄弟任务结果
            let sibling_results = if !config.depends_on.is_empty() {
                if let Some(batch_state) = batch_states
                    .iter()
                    .find(|bs| bs.batch_id == config.batch_id)
                {
                    let mut results = Vec::new();
                    for dep_name in &config.depends_on {
                        if let Some(status) = batch_state.tasks.get(dep_name) {
                            let result_text = match &status.result_summary {
                                Some(summary) if !summary.is_empty() => summary.clone(),
                                _ => format!("[{}: 执行失败，无结果]", dep_name),
                            };
                            results.push(format!("### {}\n{}", dep_name, result_text));
                        }
                    }
                    if results.is_empty() {
                        None
                    } else {
                        Some(results)
                    }
                } else {
                    None
                }
            } else {
                None
            };
```

- [ ] **Step 2: 构建 task_prompt（含兄弟结果注入）**

将原来的 `task_prompt: task.content.clone()` 替换为带注入的版本：

```rust
                    task_prompt: if let Some(ref results) = sibling_results {
                        format!(
                            "{}\n\n## 兄弟任务结果\n\n{}\n\n请基于以上兄弟任务的结果完成你的任务。你可以直接引用这些结果，无需重新计算或搜索。",
                            task.content,
                            results.join("\n\n")
                        )
                    } else {
                        task.content.clone()
                    },
```

- [ ] **Step 3: 添加注入日志**

在 `commands.spawn(AgentSpawnRequestMessage { ... })` 之后、`task.status = ...` 之前，添加：

```rust
                if sibling_results.is_some() {
                    debug!(
                        event = "SiblingResultsInjected",
                        task_id = %task.id,
                        child_name = %config.child_agent_name,
                        depends_on = ?config.depends_on,
                        "injected sibling task results into sub-task prompt"
                    );
                }
```

- [ ] **Step 4: 运行编译验证**

Run: `cargo build`
Expected: 编译成功

- [ ] **Step 5: Commit**

```bash
git add src/systems/dispatch.rs
git commit -m "feat: inject sibling task results into dependent sub-task prompt"
```

---

### Task 5: 集成验证

**Files:**
- 无新文件修改

- [ ] **Step 1: 运行 cargo clippy**

Run: `cargo clippy -- -D warnings`
Expected: 无 warnings

- [ ] **Step 2: 运行 cargo fmt**

Run: `cargo fmt --check`
Expected: 无格式问题。如果有，运行 `cargo fmt` 修复。

- [ ] **Step 3: 运行全部测试**

Run: `cargo test`
Expected: 所有测试通过

- [ ] **Step 4: 运行 cargo build 确认最终编译**

Run: `cargo build`
Expected: 编译成功

- [ ] **Step 5: Commit（如有 fmt 修复）**

```bash
git add -A
git commit -m "chore: apply cargo fmt and clippy fixes"
```

仅在有修复时提交。
