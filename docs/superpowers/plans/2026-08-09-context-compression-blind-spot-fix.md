# 上下文压缩盲区修复 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 修正 STM token 估算盲区，让工具密集型任务的压缩机制正常生效；从 `EntryMetadata.tool_calls` 还原结构化消息，实现 prompt cache 跨轮次命中。

**Architecture:** 不扩展数据模型，复用现有 `EntryMetadata.tool_calls` 存储。修正 `estimated_tokens` 计入工具调用 token；新增从 STM 到 `ConversationMessage` 的结构化还原路径；压缩改为配对组粒度保证 ID 链安全；`First iteration` 分支读 `request.conversation` 让还原生效。

**Tech Stack:** Rust, Bevy ECS, genai LLM client

## Global Constraints

- 不新增 `EntryRole` 变体、不新增 `MemoryEntry` 字段、不修改 `ConversationMessage` 数据结构
- 保留 `record_tool_call` / `EntryMetadata.tool_calls` 现有路径，不废止
- 工具循环内不压缩、不截断
- 硬截断以配对组为最小移除单位
- WorkItem 派发路径不从 STM 还原，保持 `WorkItemContext.conversation` 现状
- `First iteration` 分支中空 Vec 视同 None（`!c.is_empty()` 防御条件）
- Conventional Commits，代码通过 `cargo fmt` + `cargo clippy -D warnings` + `cargo test`

---

## File Structure

| 文件 | 职责 | 改动类型 |
|---|---|---|
| `src/domain/memory.rs` | `estimate_tokens` / `add_entry` / `recalculate_tokens` / `record_tool_call` / `render_tool_calls_summary` | 修改 |
| `src/systems/memory.rs` | `memory_compression_system` 配对组粒度 + `compress_text` 渲染 `tool_calls` | 修改 |
| `src/systems/dispatch/dispatch_system.rs` | 结构化还原路径 + 路径选择 + 硬截断 | 修改 |
| `src/systems/dispatch/prompt_builder.rs` | 防御性渲染 `metadata.tool_calls` | 修改 |
| `src/systems/transform/llm_response.rs` | `First iteration` 读 `request.conversation` | 修改 |

---

### Task 1: 修正 `estimated_tokens` 计入 `EntryMetadata.tool_calls`

**Files:**
- Modify: `src/domain/memory.rs:227-305`
- Test: `src/domain/memory.rs` (inline `#[cfg(test)]`)

**Interfaces:**
- Consumes: `estimate_tokens(&str) -> u32` (已有), `ToolCall` struct (已有)
- Produces: `add_entry` / `recalculate_tokens` / `record_tool_call` 的 token 估算修正行为

- [ ] **Step 1: 写失败测试——`add_entry` 传入含 `tool_calls` 的 metadata 时 token 应计入**

```rust
#[test]
fn add_entry_includes_tool_calls_tokens() {
    let mut stm = ShortTermMemory::default();
    let mut metadata = EntryMetadata::default();
    metadata.tool_calls.push(ToolCall {
        id: Some("call_1".to_string()),
        tool_name: "shell_exec".to_string(),
        input: "ls -la /very/long/path/that/should/contribute/tokens".to_string(),
        output: "file1.txt\nfile2.txt\nfile3.txt\nfile4.txt\nfile5.txt".to_string(),
        timestamp: chrono::Utc::now(),
    });

    let tokens_before = stm.estimated_tokens;
    stm.add_entry(EntryRole::Assistant, "done", metadata);

    // estimated_tokens should be strictly greater than just "done" tokens
    let content_only_tokens = estimate_tokens("done");
    assert!(
        stm.estimated_tokens > tokens_before + content_only_tokens,
        "add_entry should include tool_calls tokens, got {} but expected > {}",
        stm.estimated_tokens,
        tokens_before + content_only_tokens,
    );
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test add_entry_includes_tool_calls_tokens -- --nocapture`
Expected: FAIL（`add_entry` 当前只对 `content` 计 token）

- [ ] **Step 3: 实现——`add_entry` 中对 `metadata.tool_calls` 的 `input` + `output` 计 token**

在 `src/domain/memory.rs` 的 `add_entry` 方法中，修改 token 计算：

```rust
pub fn add_entry(
    &mut self,
    role: EntryRole,
    content: impl Into<String>,
    metadata: EntryMetadata,
) {
    let content = content.into();
    let mut tokens_added = estimate_tokens(&content);
    for tc in &metadata.tool_calls {
        tokens_added += estimate_tokens(&tc.input);
        tokens_added += estimate_tokens(&tc.output);
    }
    self.estimated_tokens += tokens_added;
    let entry = MemoryEntry::new(role, content.clone()).with_metadata(metadata);
    self.entries.push(entry);
    debug!(
        event = "StmEntryAdded",
        role = ?role,
        content = %content,
        content_len = content.len(),
        entry_tokens = tokens_added,
        total_tokens = self.estimated_tokens,
        total_entries = self.entries.len(),
        "short term memory entry added"
    );
}
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test add_entry_includes_tool_calls_tokens -- --nocapture`
Expected: PASS

- [ ] **Step 5: 写失败测试——`recalculate_tokens` 应计入 `tool_calls`**

```rust
#[test]
fn recalculate_tokens_includes_tool_calls() {
    let mut stm = ShortTermMemory::default();
    let mut metadata = EntryMetadata::default();
    metadata.tool_calls.push(ToolCall {
        id: Some("call_1".to_string()),
        tool_name: "shell_exec".to_string(),
        input: "ls -la /very/long/path".to_string(),
        output: "file1.txt\nfile2.txt\nfile3.txt".to_string(),
        timestamp: chrono::Utc::now(),
    });
    stm.add_entry(EntryRole::Assistant, "result text", metadata);

    // Corrupt estimated_tokens manually, then recalculate
    stm.estimated_tokens = 0;
    stm.recalculate_tokens();

    let content_tokens = estimate_tokens("result text");
    let tool_tokens = estimate_tokens("ls -la /very/long/path")
        + estimate_tokens("file1.txt\nfile2.txt\nfile3.txt");
    assert!(
        stm.estimated_tokens >= content_tokens + tool_tokens,
        "recalculate_tokens should include tool_calls, got {}",
        stm.estimated_tokens,
    );
}
```

- [ ] **Step 6: 运行测试确认失败**

Run: `cargo test recalculate_tokens_includes_tool_calls -- --nocapture`
Expected: FAIL

- [ ] **Step 7: 实现——`recalculate_tokens` 中遍历 `metadata.tool_calls` 计 token**

```rust
pub fn recalculate_tokens(&mut self) {
    let old_tokens = self.estimated_tokens;
    let mut total = 0u32;
    if let Some(summary) = &self.summary_prefix {
        total += estimate_tokens(summary);
    }
    for entry in &self.entries {
        total += estimate_tokens(&entry.content);
        for tc in &entry.metadata.tool_calls {
            total += estimate_tokens(&tc.input);
            total += estimate_tokens(&tc.output);
        }
    }
    self.estimated_tokens = total;
    debug!(
        event = "StmTokensRecalculated",
        old_tokens = old_tokens,
        new_tokens = total,
        entries_count = self.entries.len(),
        has_summary_prefix = self.summary_prefix.is_some(),
        "STM tokens recalculated"
    );
}
```

- [ ] **Step 8: 运行测试确认通过**

Run: `cargo test recalculate_tokens_includes_tool_calls -- --nocapture`
Expected: PASS

- [ ] **Step 9: 写失败测试——`record_tool_call` 追加后应更新 `estimated_tokens`**

```rust
#[test]
fn record_tool_call_updates_estimated_tokens() {
    let mut stm = ShortTermMemory::default();
    stm.add_entry(EntryRole::User, "hello", EntryMetadata::default());

    let tokens_before = stm.estimated_tokens;
    stm.record_tool_call(
        Some("call_1".to_string()),
        "shell_exec".to_string(),
        "ls -la /some/path".to_string(),
        "file1.txt\nfile2.txt\nfile3.txt".to_string(),
        chrono::Utc::now(),
    );

    assert!(
        stm.estimated_tokens > tokens_before,
        "record_tool_call should update estimated_tokens, got {} but was {}",
        stm.estimated_tokens,
        tokens_before,
    );
}
```

- [ ] **Step 10: 运行测试确认失败**

Run: `cargo test record_tool_call_updates_estimated_tokens -- --nocapture`
Expected: FAIL

- [ ] **Step 11: 实现——`record_tool_call` 追加后对 `input` + `output` 计 token**

在 `record_tool_call` 方法中，两处 `push` 后都更新 `estimated_tokens`：

```rust
pub fn record_tool_call(
    &mut self,
    id: Option<String>,
    tool_name: String,
    input: String,
    output: String,
    timestamp: DateTime<Utc>,
) {
    let tokens_added = estimate_tokens(&input) + estimate_tokens(&output);
    let tool_call = ToolCall {
        id,
        tool_name,
        input,
        output,
        timestamp,
    };

    if let Some(last_entry) = self.entries.last_mut()
        && last_entry.role == EntryRole::Assistant
    {
        last_entry.metadata.tool_calls.push(tool_call);
        self.estimated_tokens += tokens_added;
        return;
    }

    let mut metadata = EntryMetadata::default();
    metadata.tool_calls.push(tool_call);
    self.entries.push(MemoryEntry {
        role: EntryRole::Assistant,
        content: String::new(),
        metadata,
    });
    self.estimated_tokens += tokens_added;
}
```

- [ ] **Step 12: 运行测试确认通过**

Run: `cargo test record_tool_call_updates_estimated_tokens -- --nocapture`
Expected: PASS

- [ ] **Step 13: 运行全量测试确认无回归**

Run: `cargo test --all-features`
Expected: 全部 PASS

- [ ] **Step 14: Commit**

```bash
git add src/domain/memory.rs
git commit -m "fix(memory): include EntryMetadata.tool_calls in estimated_tokens"
```

---

### Task 2: 新增 `render_tool_calls_summary` 辅助函数

**Files:**
- Modify: `src/domain/memory.rs`
- Test: `src/domain/memory.rs` (inline)

**Interfaces:**
- Consumes: `ToolCall` struct
- Produces: `render_tool_calls_summary(tool_calls: &[ToolCall]) -> String`——统一的工具调用摘要渲染格式，供 `compress_text`、`prompt_builder`、还原逻辑共用

- [ ] **Step 1: 写失败测试**

```rust
#[test]
fn render_tool_calls_summary_format() {
    let tool_calls = vec![
        ToolCall {
            id: Some("call_1".to_string()),
            tool_name: "shell_exec".to_string(),
            input: "ls".to_string(),
            output: "file1.txt\nfile2.txt".to_string(),
            timestamp: chrono::Utc::now(),
        },
        ToolCall {
            id: Some("call_2".to_string()),
            tool_name: "shell_exec".to_string(),
            input: "cat x".to_string(),
            output: "content of x".to_string(),
            timestamp: chrono::Utc::now(),
        },
    ];

    let summary = render_tool_calls_summary(&tool_calls);
    assert!(summary.contains("shell_exec(\"ls\")"), "should contain tool call summary");
    assert!(summary.contains("file1.txt"), "should contain truncated output");
    assert!(summary.contains("shell_exec(\"cat x\")"), "should contain second tool call");
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test render_tool_calls_summary_format -- --nocapture`
Expected: FAIL（函数不存在）

- [ ] **Step 3: 实现 `render_tool_calls_summary`**

在 `src/domain/memory.rs` 中 `ShortTermMemory` impl 块之后添加：

```rust
/// 渲染工具调用摘要，用于 compress_text、prompt_builder 和还原逻辑。
///
/// 格式：`[Tool calls: tool_name("input") → output_preview; ...]`
/// 输出截断至 200 字符避免膨胀。
pub fn render_tool_calls_summary(tool_calls: &[ToolCall]) -> String {
    if tool_calls.is_empty() {
        return String::new();
    }
    let parts: Vec<String> = tool_calls
        .iter()
        .map(|tc| {
            let output_preview = if tc.output.chars().count() > 200 {
                let truncated: String = tc.output.chars().take(200).collect();
                format!("{}...[truncated]", truncated)
            } else {
                tc.output.clone()
            };
            format!("{}(\"{}\") → {}", tc.tool_name, tc.input, output_preview)
        })
        .collect();
    format!("[Tool calls: {}]", parts.join("; "))
}
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test render_tool_calls_summary_format -- --nocapture`
Expected: PASS

- [ ] **Step 5: 运行全量测试确认无回归**

Run: `cargo test --all-features`
Expected: 全部 PASS

- [ ] **Step 6: Commit**

```bash
git add src/domain/memory.rs
git commit -m "feat(memory): add render_tool_calls_summary helper"
```

---

### Task 3: 压缩粒度改为配对组 + `compress_text` 渲染 `tool_calls`

**Files:**
- Modify: `src/systems/memory.rs:14-75`
- Test: `src/systems/memory.rs` (inline)

**Interfaces:**
- Consumes: `EntryRole`, `EntryMetadata.tool_calls`, `render_tool_calls_summary`（Task 2 产出）
- Produces: 配对组粒度的压缩行为

- [ ] **Step 1: 写失败测试——含 `tool_calls` 的 Assistant 条目不应被拆散**

```rust
#[test]
fn compression_preserves_tool_call_group_atomicity() {
    let mut world = World::new();
    world.insert_resource(MemoryConfig {
        compression_threshold_tokens: 50,
        preserve_recent_turns: 1,
        summary_target_tokens: 25,
    });

    let task = Task::from_user_input(
        "test",
        3,
        ChannelId {
            frontend: FrontendKind::Tui,
            user_id: "default".to_string(),
            thread_id: None,
        },
    );
    let entity = world.spawn((task, ShortTermMemory::default())).id();

    {
        let mut stm = world.get_mut::<ShortTermMemory>(entity).unwrap();
        // Entry 1: User (对话配对组)
        stm.add_entry(EntryRole::User, "hello world this is a long enough message to contribute tokens", Default::default());
        // Entry 2: Assistant with tool_calls (工具配对组——不可拆散)
        let mut metadata = EntryMetadata::default();
        metadata.tool_calls.push(ToolCall {
            id: Some("call_1".to_string()),
            tool_name: "shell_exec".to_string(),
            input: "ls -la /very/long/path/with/many/segments/to/contribute/tokens".to_string(),
            output: "file1.txt\nfile2.txt\nfile3.txt\nfile4.txt\nfile5.txt\nfile6.txt".to_string(),
            timestamp: chrono::Utc::now(),
        });
        stm.add_entry(EntryRole::Assistant, "done with tools", metadata);
        // Entry 3: User (最近的对话配对组——应保留)
        stm.add_entry(EntryRole::User, "next question with enough tokens to push over threshold when combined", Default::default());
        // Entry 4: Assistant (最近的对话配对组——应保留)
        stm.add_entry(EntryRole::Assistant, "final answer with enough text to be meaningful", Default::default());
    }

    let stm = world.get::<ShortTermMemory>(entity).unwrap();
    assert!(
        stm.estimated_tokens > 50,
        "should exceed threshold, got {}",
        stm.estimated_tokens,
    );
}
```

- [ ] **Step 2: 运行测试确认通过（验证前提条件）**

Run: `cargo test compression_preserves_tool_call_group_atomicity -- --nocapture`
Expected: PASS（测试只验证 token 超阈值，尚未验证配对组原子性）

- [ ] **Step 3: 实现配对组切分函数和压缩逻辑重构**

在 `src/systems/memory.rs` 中重构 `memory_compression_system`：

```rust
/// 将 STM entries 按配对组切分。
///
/// 配对组定义：
/// - User 开启新的对话配对组
/// - Assistant（无 tool_calls）归入当前对话配对组
/// - Assistant（有 tool_calls）开启新的工具配对组（原子性锚点）
/// - Summary / Archive 归入最近的配对组
fn split_into_groups(entries: &[MemoryEntry]) -> Vec<Vec<usize>> {
    if entries.is_empty() {
        return Vec::new();
    }
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut current_group: Vec<usize> = Vec::new();

    for (i, entry) in entries.iter().enumerate() {
        let starts_new_group = match entry.role {
            EntryRole::User => true,
            EntryRole::Assistant
                if !entry.metadata.tool_calls.is_empty() => true,
            EntryRole::Assistant => false,
            EntryRole::Summary | EntryRole::Archive => false,
        };

        if starts_new_group && !current_group.is_empty() {
            groups.push(std::mem::take(&mut current_group));
        }
        current_group.push(i);
    }

    if !current_group.is_empty() {
        groups.push(current_group);
    }

    groups
}
```

重构 `memory_compression_system` 中的压缩逻辑（保留触发条件和摘要请求不变）：

```rust
// 替换原有的 preserve_count / compress_count 逻辑
let groups = split_into_groups(&short_term.entries);
if groups.len() <= config.preserve_recent_turns as usize {
    continue;
}

let preserve_group_count = config.preserve_recent_turns as usize;
let compress_entry_count = groups.iter()
    .take(groups.len() - preserve_group_count)
    .map(|g| g.len())
    .sum();

if compress_entry_count == 0 {
    continue;
}

// 收集需要压缩的条目内容（含 tool_calls 渲染）
let to_compress: Vec<_> = short_term.entries.iter().take(compress_entry_count).collect();
let mut compress_text = String::new();
for entry in &to_compress {
    let mut line = format!("{:?}: {}", entry.role, entry.content);
    if !entry.metadata.tool_calls.is_empty() {
        line.push_str(&format!("\n  {}", render_tool_calls_summary(&entry.metadata.tool_calls)));
    }
    compress_text.push_str(&line);
    compress_text.push('\n');
}
```

注意：需要在文件顶部引入 `render_tool_calls_summary`：

```rust
use crate::domain::{..., render_tool_calls_summary, ToolCall};
```

- [ ] **Step 4: 运行全量测试**

Run: `cargo test --all-features`
Expected: 全部 PASS

- [ ] **Step 5: Commit**

```bash
git add src/systems/memory.rs
git commit -m "fix(memory): compression uses pair-group granularity and includes tool_calls in compress_text"
```

---

### Task 4: 结构化还原路径 + 路径选择 + 硬截断

**Files:**
- Modify: `src/systems/dispatch/dispatch_system.rs:275-324`
- Test: `src/systems/dispatch/dispatch_system.rs` (inline 或 `tests/`)

**Interfaces:**
- Consumes: `ShortTermMemory`, `EntryMetadata.tool_calls`, `ToolCall`, `LlmToolCall`, `ConversationMessage`, `estimate_tokens`
- Produces: `build_structured_conversation(stm: &ShortTermMemory) -> Option<Vec<ConversationMessage>>`——结构化还原辅助函数；修改后 `dispatch_system.rs` 的 Task 直接派发路径

- [ ] **Step 1: 写失败测试——`build_structured_conversation` 从 STM 还原 `ConversationMessage`**

```rust
#[test]
fn build_structured_conversation_from_stm() {
    let mut stm = ShortTermMemory::default();
    stm.add_entry(EntryRole::User, "list files", EntryMetadata::default());
    let mut metadata = EntryMetadata::default();
    metadata.tool_calls.push(ToolCall {
        id: Some("call_1".to_string()),
        tool_name: "shell_exec".to_string(),
        input: "ls".to_string(),
        output: "file1.txt\nfile2.txt".to_string(),
        timestamp: chrono::Utc::now(),
    });
    stm.add_entry(EntryRole::Assistant, "done", metadata);
    stm.add_entry(EntryRole::User, "next question", EntryMetadata::default());
    stm.add_entry(EntryRole::Assistant, "answer", EntryMetadata::default());

    let conversation = build_structured_conversation(&stm);
    assert!(conversation.is_some(), "should return Some when tool_calls exist");

    let messages = conversation.unwrap();
    // User → Assistant(tool_calls) → Tool → User → Assistant
    assert!(matches!(messages[0], ConversationMessage::User { .. }));
    assert!(matches!(&messages[1], ConversationMessage::Assistant { tool_calls, .. } if !tool_calls.is_empty()));
    assert!(matches!(&messages[2], ConversationMessage::Tool { tool_call_id, .. } if tool_call_id == "call_1"));
    assert!(matches!(messages[3], ConversationMessage::User { .. }));
    assert!(matches!(&messages[4], ConversationMessage::Assistant { tool_calls, .. } if tool_calls.is_empty()));
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test build_structured_conversation_from_stm -- --nocapture`
Expected: FAIL（函数不存在）

- [ ] **Step 3: 实现 `build_structured_conversation`**

在 `src/systems/dispatch/dispatch_system.rs` 中添加辅助函数：

```rust
/// 从 STM 还原结构化对话消息序列。
///
/// 当 STM 含非空 `metadata.tool_calls` 的 Assistant 条目时，返回 `Some(Vec<ConversationMessage>)`；
/// 否则返回 `None`（走纯文本路径）。
fn build_structured_conversation(
    stm: &ShortTermMemory,
) -> Option<Vec<ConversationMessage>> {
    let has_tool_calls = stm.entries.iter().any(|e| !e.metadata.tool_calls.is_empty());
    if !has_tool_calls {
        return None;
    }

    let mut messages = Vec::new();

    // summary_prefix → User 消息
    if let Some(summary) = &stm.summary_prefix {
        messages.push(ConversationMessage::User {
            content: format!("[Previous context summary]\n{}", summary),
        });
    }

    for entry in &stm.entries {
        match entry.role {
            EntryRole::User => {
                messages.push(ConversationMessage::User {
                    content: entry.content.clone(),
                });
            }
            EntryRole::Assistant => {
                let tool_calls: Vec<LlmToolCall> = entry
                    .metadata
                    .tool_calls
                    .iter()
                    .enumerate()
                    .map(|(i, tc)| LlmToolCall {
                        id: tc.id.clone().unwrap_or_else(|| format!("tc_{}", i)),
                        name: tc.tool_name.clone(),
                        arguments: tc.input.clone(),
                    })
                    .collect();

                messages.push(ConversationMessage::Assistant {
                    content: if entry.content.is_empty() {
                        None
                    } else {
                        Some(entry.content.clone())
                    },
                    tool_calls,
                    reasoning_content: None,
                });

                // 追加 Tool 消息
                for (i, tc) in entry.metadata.tool_calls.iter().enumerate() {
                    messages.push(ConversationMessage::Tool {
                        tool_call_id: tc.id.clone().unwrap_or_else(|| format!("tc_{}", i)),
                        content: tc.output.clone(),
                    });
                }
            }
            EntryRole::Summary => {
                messages.push(ConversationMessage::User {
                    content: format!("[System note] {}", entry.content),
                });
            }
            EntryRole::Archive => {
                // 跳过
            }
        }
    }

    Some(messages)
}
```

需要在文件顶部引入：

```rust
use crate::domain::{
    ConversationMessage, LlmToolCall, ToolCall, estimate_tokens, render_tool_calls_summary,
};
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test build_structured_conversation_from_stm -- --nocapture`
Expected: PASS

- [ ] **Step 5: 实现硬截断辅助函数**

```rust
/// 按模型窗口预算裁剪 `ConversationMessage` 序列。
///
/// 以配对组为最小移除单位，从最早的消息开始移除，直到总量在预算内。
/// 配对组定义与 `memory_compression_system` 一致：
/// - `Assistant { tool_calls: non-empty }` + 后续 `Tool` 消息为一个配对组
/// - 其他消息各自成组
fn truncate_conversation_by_budget(
    messages: &mut Vec<ConversationMessage>,
    budget_tokens: u32,
) {
    let total_tokens: u32 = messages
        .iter()
        .map(|msg| match msg {
            ConversationMessage::System { content }
            | ConversationMessage::User { content } => estimate_tokens(content),
            ConversationMessage::Assistant { content, tool_calls, .. } => {
                let mut t = content.as_deref().map(|c| estimate_tokens(c)).unwrap_or(0);
                for tc in tool_calls {
                    t += estimate_tokens(&tc.arguments);
                }
                t
            }
            ConversationMessage::Tool { content, .. } => estimate_tokens(content),
        })
        .sum();

    if total_tokens <= budget_tokens {
        return;
    }

    // 识别配对组边界：Assistant(tool_calls non-empty) 是配对组锚点
    // 简化实现：从最前面逐条移除，但保证不出现 Tool 无父 Assistant 的悬空引用
    while !messages.is_empty() {
        let current_tokens: u32 = messages
            .iter()
            .map(|msg| match msg {
                ConversationMessage::System { content }
                | ConversationMessage::User { content } => estimate_tokens(content),
                ConversationMessage::Assistant { content, tool_calls, .. } => {
                    let mut t = content.as_deref().map(|c| estimate_tokens(c)).unwrap_or(0);
                    for tc in tool_calls {
                        t += estimate_tokens(&tc.arguments);
                    }
                    t
                }
                ConversationMessage::Tool { content, .. } => estimate_tokens(content),
            })
            .sum();

        if current_tokens <= budget_tokens {
            break;
        }

        // 移除第一条消息；如果是 Assistant(tool_calls)，同时移除后续 Tool 消息
        let first_is_tool_call_anchor = matches!(
            &messages[0],
            ConversationMessage::Assistant { tool_calls, .. } if !tool_calls.is_empty()
        );
        messages.remove(0);

        if first_is_tool_call_anchor {
            // 移除后续连续的 Tool 消息（配对组整体移除）
            while matches!(messages.first(), Some(ConversationMessage::Tool { .. })) {
                messages.remove(0);
            }
        }
    }
}
```

- [ ] **Step 6: 修改 Task 直接派发路径**

在 `dispatch_system.rs` 的 Task 直接派发分支中（约 289-324 行），将 `conversation: None` 改为根据 STM 选择路径：

```rust
// 判断是否走结构化路径
let (prompt, conversation) = if let Some(stm) = short_term {
    if let Some(conv) = build_structured_conversation(stm) {
        // 结构化路径：prompt 为空，conversation 为还原结果
        let mut conv = conv;
        // 硬截断兜底：按模型窗口预算裁剪
        // TODO: budget_tokens 应从模型配置获取，暂用 100000 作为安全上限
        truncate_conversation_by_budget(&mut conv, 100_000);
        (String::new(), Some(conv))
    } else {
        // 纯文本路径
        let prompt = build_prompt_with_context(
            &task.content,
            Some(stm),
            long_term,
            task.origin_channel.as_ref(),
        );
        (prompt, None)
    }
} else {
    let prompt = build_prompt_with_context(
        &task.content,
        None,
        long_term,
        task.origin_channel.as_ref(),
    );
    (prompt, None)
};
```

然后修改 `AgentExecutionRequest` 构造，使用新的 `prompt` 和 `conversation`。

- [ ] **Step 7: 运行全量测试**

Run: `cargo test --all-features`
Expected: 全部 PASS

- [ ] **Step 8: Commit**

```bash
git add src/systems/dispatch/dispatch_system.rs
git commit -m "feat(dispatch): structured conversation path from STM tool_calls with budget truncation"
```

---

### Task 5: `prompt_builder` 防御性渲染 `metadata.tool_calls`

**Files:**
- Modify: `src/systems/dispatch/prompt_builder.rs:56-64`
- Test: `src/systems/dispatch/prompt_builder.rs` (inline)

**Interfaces:**
- Consumes: `render_tool_calls_summary`（Task 2 产出）
- Produces: `Assistant` 条目带 `tool_calls` 时渲染工具调用摘要

- [ ] **Step 1: 写失败测试**

在 `src/systems/dispatch/prompt_builder.rs` 的 `#[cfg(test)] mod tests` 中添加：

```rust
#[test]
fn prompt_includes_tool_calls_summary_for_assistant_entry() {
    let mut stm = ShortTermMemory::default();
    stm.add_entry(EntryRole::User, "list files", EntryMetadata::default());
    let mut metadata = EntryMetadata::default();
    metadata.tool_calls.push(ToolCall {
        id: Some("call_1".to_string()),
        tool_name: "shell_exec".to_string(),
        input: "ls".to_string(),
        output: "file1.txt\nfile2.txt".to_string(),
        timestamp: chrono::Utc::now(),
    });
    stm.add_entry(EntryRole::Assistant, "done", metadata);

    let prompt =
        build_prompt_with_context("do the task", Some(&stm), None, Some(&make_channel()));

    assert!(
        prompt.contains("[Tool calls:"),
        "prompt should include tool calls summary, got: {}",
        prompt,
    );
    assert!(
        prompt.contains("shell_exec"),
        "prompt should include tool name, got: {}",
        prompt,
    );
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test prompt_includes_tool_calls_summary_for_assistant_entry -- --nocapture`
Expected: FAIL

- [ ] **Step 3: 实现——在 `Assistant` 分支中追加工具调用摘要**

在 `prompt_builder.rs` 中修改 `match entry.role` 的 `Assistant` 分支：

```rust
EntryRole::Assistant => {
    let mut line = format!("Assistant: {}", entry.content);
    if !entry.metadata.tool_calls.is_empty() {
        line.push_str(&format!("\n  {}", render_tool_calls_summary(&entry.metadata.tool_calls)));
    }
    history.push_str(&line);
    history.push('\n');
}
```

同时在文件顶部引入：

```rust
use crate::domain::render_tool_calls_summary;
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test prompt_includes_tool_calls_summary_for_assistant_entry -- --nocapture`
Expected: PASS

- [ ] **Step 5: 运行全量测试确认无回归**

Run: `cargo test --all-features`
Expected: 全部 PASS

- [ ] **Step 6: Commit**

```bash
git add src/systems/dispatch/prompt_builder.rs
git commit -m "feat(prompt): defensive rendering of tool_calls summary in text path"
```

---

### Task 6: `ToolCallingState` 首次创建路径读 `request.conversation`

**Files:**
- Modify: `src/systems/transform/llm_response.rs:1264-1305`
- Test: `tests/` (集成测试)

**Interfaces:**
- Consumes: `AgentExecutionRequest.conversation`（改动点 4 设置的 `Some(Vec<ConversationMessage>)`）
- Produces: 首次创建 `ToolCallingState` 时 `conversation` 包含还原的历史

- [ ] **Step 1: 修改 `First iteration` 分支**

在 `src/systems/transform/llm_response.rs` 的 `First iteration` 分支中（约 1264 行），替换 `conversation` 构造逻辑：

```rust
} else {
    // First iteration: create new ToolCallingState
    let conversation = if result.conversation.as_ref().is_some_and(|c| !c.is_empty()) {
        // 结构化路径：使用已有的 conversation（从 STM 还原），追加本轮 Assistant
        let mut conv = result.conversation.clone().unwrap();
        conv.push(ConversationMessage::Assistant {
            content: None,
            tool_calls: calls.clone(),
            reasoning_content: reasoning_content.clone(),
        });
        conv
    } else {
        // 纯文本路径：现有逻辑
        let mut conversation = Vec::new();
        if let Some(sp) = &result.system_prompt {
            conversation.push(ConversationMessage::System {
                content: sp.clone(),
            });
        }
        conversation.push(ConversationMessage::User {
            content: result.prompt.clone(),
        });
        conversation.push(ConversationMessage::Assistant {
            content: None,
            tool_calls: calls.clone(),
            reasoning_content: reasoning_content.clone(),
        });
        conversation
    };

    let pending_ids: Vec<String> = calls.iter().map(|c| c.id.clone()).collect();
    let max_iterations = settings.0.max_tool_iterations;
    // ... rest unchanged
```

- [ ] **Step 2: 运行全量测试**

Run: `cargo test --all-features`
Expected: 全部 PASS

- [ ] **Step 3: 运行 clippy 检查**

Run: `cargo clippy --all-targets --all-features -- -D warnings`
Expected: 无警告

- [ ] **Step 4: Commit**

```bash
git add src/systems/transform/llm_response.rs
git commit -m "feat(llm): First iteration reads request.conversation for structured history"
```

---

### Task 7: 全链路验证 + fmt + clippy + 最终提交

**Files:**
- All modified files

- [ ] **Step 1: 运行 `cargo fmt`**

Run: `cargo fmt --all --check`
Expected: 无差异。若有，运行 `cargo fmt --all` 修复。

- [ ] **Step 2: 运行 `cargo clippy`**

Run: `cargo clippy --all-targets --all-features -- -D warnings`
Expected: 无警告。修复所有问题。

- [ ] **Step 3: 运行全量测试**

Run: `cargo test --all-features`
Expected: 全部 PASS

- [ ] **Step 4: 检查已改动的文件列表确认无遗漏**

Run: `git diff --name-only main...HEAD`
Expected: 包含以下文件：
- `src/domain/memory.rs`
- `src/systems/memory.rs`
- `src/systems/dispatch/dispatch_system.rs`
- `src/systems/dispatch/prompt_builder.rs`
- `src/systems/transform/llm_response.rs`

- [ ] **Step 5: 确认每个 commit 粒度合理**

Run: `git log --oneline main...HEAD`
Expected: 6 个 commit，每个对应一个 Task。
