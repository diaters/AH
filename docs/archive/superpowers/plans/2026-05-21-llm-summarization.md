> **状态：已归档（2026-06-10）** — 本计划已执行完毕。
> 相关能力已记录在 [docs/current-state.md](../../current-state.md)。

# LLM 记忆摘要实现计划

> __For agentic workers:__ REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development
(recommended) or superpowers:executing-plans to implement this plan task-by-task.
Steps use checkbox (`- [ ]`) syntax for tracking.

__Goal:__ 实现 LLM 生成摘要替代简单拼接，支持三种触发条件：
Token 阈值、`/summarize` 指令、任务完成。

__Architecture:__ 新增独立消息类型 `SummarizationRequestMessage`
和 `SummarizationResultMessage`，通过新增 `summarization_dispatch_system`
和 `summarization_result_system` 处理摘要请求和结果。
创建专用 summarizer Agent 走现有异步执行链路。

__Tech Stack:__ Rust, Bevy ECS, async-openai, tiktoken-rs

---

## 文件结构

| 文件 | 操作 | 职责 |
|------|------|------|
| `src/domain/mod.rs` | 修改 | 新增消息类型和枚举扩展 |
| `src/llm/summarization_prompt.rs` | 新建 | 摘要 Prompt 模板 |
| `src/llm/mod.rs` | 修改 | 导出 prompt 函数 |
| `src/systems/summarization.rs` | 新建 | dispatch 和 result systems |
| `src/systems/mod.rs` | 修改 | 导出新 systems |
| `src/systems/memory.rs` | 修改 | 改为发送摘要请求 |
| `src/systems/command.rs` | 修改 | 处理 `/summarize` 指令 |
| `src/systems/transform.rs` | 修改 | 任务完成触发摘要、结果路由 |
| `src/app/mod.rs` | 修改 | 注册新 systems |
| `agents.toml` | 修改 | 添加 summarizer Agent |
| `tests/summarization_flow.rs` | 新建 | 集成测试 |

---

### Task 1: 扩展数据结构

__Files:__

- Modify: `src/domain/mod.rs`

- [ ] __Step 1: 添加 SummarizationRequestMessage__

在 `src/domain/mod.rs` 中 `ToolExecutionResultMessage` 之后添加：

```rust
/// 摘要触发来源
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SummarizationTrigger {
    /// Token 阈值触发
    TokenThreshold,
    /// 用户 /summarize 指令
    UserCommand,
    /// 任务完成
    TaskComplete,
}

/// 摘要请求消息
#[derive(Debug, Clone, Component)]
pub struct SummarizationRequestMessage {
    /// 关联的任务 ID
    pub task_id: TaskId,
    /// 待压缩的内容
    pub content_to_summarize: String,
    /// 目标 token 数
    pub target_tokens: u32,
    /// 摘要触发来源
    pub trigger: SummarizationTrigger,
}

/// 摘要结果消息
#[derive(Debug, Clone, Component)]
pub struct SummarizationResultMessage {
    /// 关联的任务 ID
    pub task_id: TaskId,
    /// 生成的摘要
    pub summary: Result<String, ExecutionError>,
}
```

- [ ] __Step 2: 扩展 WaitingReason 枚举__

在 `WaitingReason` 枚举中添加 `Summarization` 变体：

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum WaitingReason {
    Agent,
    User,
    Evaluator,
    RetryBackoff,
    Approval,
    Summarization, // 新增
}
```

- [ ] __Step 3: 扩展 AgentRequestKind 枚举__

在 `AgentRequestKind` 枚举中添加 `Summarization` 变体：

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum AgentRequestKind {
    LlmCompletion,
    BrainDecision,
    ToolExecution { tool_name: String },
    Summarization, // 新增
}
```

- [ ] __Step 4: 运行测试验证编译__

Run: `cargo test --quiet`

Expected: 所有测试通过

- [ ] __Step 5: 提交__

```bash
git add src/domain/mod.rs
git commit -m "feat: add summarization message types and enum variants"
```

---

### Task 2: 创建摘要 Prompt 模板

__Files:__

- Create: `src/llm/summarization_prompt.rs`
- Modify: `src/llm/mod.rs`

- [ ] __Step 1: 创建 summarization_prompt.rs__

```rust
/// 摘要系统 prompt
pub fn summarization_system_prompt() -> String {
    r#"你是一个记忆摘要专家。你的任务是将对话历史压缩为简洁的摘要。

要求：
1. 保留关键事实、决策、待办事项
2. 保留重要的人物、时间、地点信息
3. 去除重复和无关内容
4. 保持摘要的可读性和连贯性
5. 目标长度：不超过指定的 token 数

输出格式：直接输出摘要内容，不需要额外说明。"#.to_string()
}

/// 摘要用户 prompt
pub fn summarization_user_prompt(content: &str, target_tokens: u32) -> String {
    format!(
        "请将以下对话历史压缩为摘要，目标长度不超过 {} tokens：\n\n{}",
        target_tokens, content
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_prompt_not_empty() {
        let prompt = summarization_system_prompt();
        assert!(!prompt.is_empty());
        assert!(prompt.contains("摘要"));
    }

    #[test]
    fn user_prompt_contains_content() {
        let prompt = summarization_user_prompt("test content", 1000);
        assert!(prompt.contains("test content"));
        assert!(prompt.contains("1000"));
    }
}
```

- [ ] __Step 2: 修改 llm/mod.rs 导出__

在 `src/llm/mod.rs` 中添加：

```rust
mod summarization_prompt;

pub use summarization_prompt::{summarization_system_prompt, summarization_user_prompt};
```

- [ ] __Step 3: 运行测试__

Run: `cargo test --quiet`

Expected: 所有测试通过

- [ ] __Step 4: 提交__

```bash
git add src/llm/summarization_prompt.rs src/llm/mod.rs
git commit -m "feat: add summarization prompt templates"
```

---

### Task 3: 创建摘要处理 Systems

__Files:__

- Create: `src/systems/summarization.rs`

- [ ] __Step 1: 创建 summarization.rs 包含 dispatch 和 result systems__

(代码略，参见实际实现)

- [ ] __Step 2: 修改 systems/mod.rs 导出__

- [ ] __Step 3: 运行测试__

Run: `cargo test --quiet`

Expected: 所有测试通过

- [ ] __Step 4: 提交__

```bash
git add src/systems/summarization.rs src/systems/mod.rs
git commit -m "feat: add summarization dispatch and result systems"
```

---

### Task 4: 修改 memory_compression_system

__Files:__

- Modify: `src/systems/memory.rs`

(详细步骤略)

---

### Task 5: 修改 command_parse_system 处理 /summarize

__Files:__

- Modify: `src/systems/command.rs`

(详细步骤略)

---

### Task 6: 修改 task_termination_system 触发任务完成摘要

__Files:__

- Modify: `src/systems/transform.rs`

(详细步骤略)

---

### Task 7: 修改 ingest_execution_results_system 路由摘要结果

__Files:__

- Modify: `src/systems/transform.rs`

(详细步骤略)

---

### Task 8: 注册新 Systems 到 app/mod.rs

__Files:__

- Modify: `src/app/mod.rs`

(详细步骤略)

---

### Task 9: 添加 summarizer Agent 配置

__Files:__

- Modify: `agents.toml`

- [ ] __Step 1: 添加 summarizer agent 配置__

在 `agents.toml` 末尾添加：

```toml
[[agent]]
name = "summarizer"
model = "gpt-4.1-mini"
tags = ["summarization", "memory"]
description = "记忆摘要专家，负责压缩对话历史"

[agent.tools]
default_permission = "Deny"
```

---

### Task 10: 集成测试

__Files:__

- Create: `tests/summarization_flow.rs`

(详细步骤略)

---

### Task 11: 更新 TODO.md

__Files:__

- Modify: `docs/TODO.md`

---

## 自检清单

- [ ] 所有测试通过：`cargo test --all-features`
- [ ] Clippy 无警告：`cargo clippy --all-targets --all-features -- -D warnings`
- [ ] 格式正确：`cargo fmt --all --check`
- [ ] 文档更新：`docs/TODO.md` 已更新
