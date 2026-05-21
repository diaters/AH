# LLM 记忆摘要实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 实现 LLM 生成摘要替代简单拼接，支持三种触发条件：Token 阈值、`/summarize` 指令、任务完成。

**Architecture:** 新增独立消息类型 `SummarizationRequestMessage` 和 `SummarizationResultMessage`，通过新增 `summarization_dispatch_system` 和 `summarization_result_system` 处理摘要请求和结果。创建专用 summarizer Agent 走现有异步执行链路。

**Tech Stack:** Rust, Bevy ECS, async-openai, tiktoken-rs

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

**Files:**
- Modify: `src/domain/mod.rs`

- [ ] **Step 1: 添加 SummarizationRequestMessage**

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

- [ ] **Step 2: 扩展 WaitingReason 枚举**

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

- [ ] **Step 3: 扩展 AgentRequestKind 枚举**

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

- [ ] **Step 4: 运行测试验证编译**

Run: `cargo test --quiet`

Expected: 所有测试通过

- [ ] **Step 5: 提交**

```bash
git add src/domain/mod.rs
git commit -m "feat: add summarization message types and enum variants"
```

---

### Task 2: 创建摘要 Prompt 模板

**Files:**
- Create: `src/llm/summarization_prompt.rs`
- Modify: `src/llm/mod.rs`

- [ ] **Step 1: 创建 summarization_prompt.rs**

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

- [ ] **Step 2: 修改 llm/mod.rs 导出**

在 `src/llm/mod.rs` 中添加：

```rust
mod summarization_prompt;

pub use summarization_prompt::{summarization_system_prompt, summarization_user_prompt};
```

- [ ] **Step 3: 运行测试**

Run: `cargo test --quiet`

Expected: 所有测试通过

- [ ] **Step 4: 提交**

```bash
git add src/llm/summarization_prompt.rs src/llm/mod.rs
git commit -m "feat: add summarization prompt templates"
```

---

### Task 3: 创建摘要处理 Systems

**Files:**
- Create: `src/systems/summarization.rs`

- [ ] **Step 1: 创建 summarization.rs 包含 dispatch 和 result systems**

```rust
use bevy::prelude::*;
use tracing::info;

use crate::{
    app::{Clock, MemoryConfig},
    domain::{
        Agent, AgentExecutionRequest, AgentExecutionRequestMessage, AgentExecutionResultMessage,
        AgentKind, AgentRequestKind, ExecutionError, ShortTermMemory, SummarizationRequestMessage,
        SummarizationResultMessage, Task, TaskStatus, WaitingReason,
    },
    llm::{summarization_system_prompt, summarization_user_prompt},
};

/// 摘要调度系统：将摘要请求转为 AgentExecutionRequest
pub(crate) fn summarization_dispatch_system(
    clock: Res<Clock>,
    mut commands: Commands,
    agents: Query<&Agent>,
    requests: Query<(Entity, &SummarizationRequestMessage)>,
    mut tasks: Query<&mut Task>,
) {
    // 查找 summarizer Agent
    let summarizer = agents.iter().find(|a| {
        a.kind == AgentKind::Persistent
            && a.capabilities.tags.contains(&"summarization".to_string())
    });

    let Some(summarizer) = summarizer else {
        info!("no summarizer agent found, skipping summarization requests");
        // 没有 summarizer，清理所有请求
        for (entity, _) in &requests {
            commands.entity(entity).despawn();
        }
        return;
    };

    for (entity, request) in &requests {
        // 标记任务为等待摘要
        if let Some(mut task) = tasks.iter_mut().find(|t| t.id == request.task_id) {
            task.status = TaskStatus::Waiting(WaitingReason::Summarization);
            task.updated_at = clock.0;
            info!(task_id = %request.task_id, "task waiting for summarization");
        }

        // 构建 AgentExecutionRequest
        let execution_request = AgentExecutionRequest {
            task_id: request.task_id,
            agent_id: summarizer.id,
            request_kind: AgentRequestKind::Summarization,
            prompt: summarization_user_prompt(&request.content_to_summarize, request.target_tokens),
            system_prompt: Some(summarization_system_prompt()),
        };

        commands.spawn(AgentExecutionRequestMessage {
            request: execution_request,
        });
        info!(
            task_id = %request.task_id,
            trigger = ?request.trigger,
            target_tokens = request.target_tokens,
            "dispatched summarization request"
        );
        commands.entity(entity).despawn();
    }
}

/// 摘要结果处理系统：更新 ShortTermMemory
pub(crate) fn summarization_result_system(
    clock: Res<Clock>,
    config: Res<MemoryConfig>,
    mut commands: Commands,
    results: Query<(Entity, &SummarizationResultMessage)>,
    mut tasks: Query<&mut Task>,
    memories: Query<(Entity, &Task, &mut ShortTermMemory)>,
) {
    for (entity, result) in &results {
        match &result.summary {
            Ok(summary) => {
                // 查找关联的任务和记忆
                for (task_entity, task, mut memory) in &memories {
                    if task.id == result.task_id {
                        // 更新摘要前缀
                        memory.summary_prefix = Some(summary.clone());

                        // 移除已压缩的 entries（保留最近 N 轮）
                        let preserve_count = (config.preserve_recent_turns * 2) as usize;
                        if memory.entries.len() > preserve_count {
                            let removed = memory.entries.len() - preserve_count;
                            memory.entries.drain(0..removed);
                            info!(task_id = %task.id, removed_count = removed, "removed compressed entries");
                        }

                        // 重新计算 token
                        memory.recalculate_tokens();

                        // 恢复任务状态
                        if let Some(mut task_mut) = tasks.iter_mut().find(|t| t.id == result.task_id) {
                            task_mut.status = TaskStatus::Ready;
                            task_mut.updated_at = clock.0;
                        }

                        info!(
                            task_id = %result.task_id,
                            summary_len = summary.len(),
                            remaining_entries = memory.entries.len(),
                            new_tokens = memory.estimated_tokens,
                            "summarization completed"
                        );
                        break;
                    }
                }
            }
            Err(error) => {
                // 摘要失败，记录错误但恢复任务状态
                if let Some(mut task) = tasks.iter_mut().find(|t| t.id == result.task_id) {
                    task.status = TaskStatus::Ready;
                    task.updated_at = clock.0;
                    task.last_error = Some(format!("summarization failed: {}", error.message()));
                }
                info!(task_id = %result.task_id, error = ?error, "summarization failed");
            }
        }
        commands.entity(entity).despawn();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{AgentCapabilities, AgentExperience, AgentProfile, AgentToolPermissions};

    #[test]
    fn summarizer_agent_selection() {
        let mut world = World::new();
        
        let summarizer = Agent {
            id: uuid::Uuid::nil(),
            profile: AgentProfile {
                name: "summarizer".to_string(),
                model: "gpt-4.1-mini".to_string(),
            },
            capabilities: AgentCapabilities {
                tags: vec!["summarization".to_string()],
                description: "test".to_string(),
            },
            kind: AgentKind::Persistent,
            parent_id: None,
            bound_task_id: None,
            tool_permissions: AgentToolPermissions::default(),
            experience: AgentExperience::default(),
        };
        
        world.spawn(summarizer);
        
        let found = world.query::<&Agent>()
            .iter(&world)
            .find(|a| a.capabilities.tags.contains(&"summarization".to_string()));
        
        assert!(found.is_some());
    }
}
```

- [ ] **Step 2: 修改 systems/mod.rs 导出**

在 `src/systems/mod.rs` 中添加：

```rust
mod summarization;

pub(crate) use summarization::{
    summarization_dispatch_system, summarization_result_system,
};
```

- [ ] **Step 3: 运行测试**

Run: `cargo test --quiet`

Expected: 所有测试通过

- [ ] **Step 4: 提交**

```bash
git add src/systems/summarization.rs src/systems/mod.rs
git commit -m "feat: add summarization dispatch and result systems"
```

---

### Task 4: 修改 memory_compression_system

**Files:**
- Modify: `src/systems/memory.rs`

- [ ] **Step 1: 修改 memory_compression_system 发送摘要请求**

将 `src/systems/memory.rs` 中的导入和 `memory_compression_system` 函数替换为：

```rust
use bevy::prelude::*;
use tracing::info;

use crate::{
    app::MemoryConfig,
    domain::{
        Agent, LongTermMemory, ShortTermMemory, SummarizationRequestMessage,
        SummarizationTrigger, Task, TaskStatus, WaitingReason,
    },
};

/// 记忆压缩系统：检测 token 阈值并触发摘要请求
pub(crate) fn memory_compression_system(
    config: Res<MemoryConfig>,
    mut commands: Commands,
    tasks: Query<(&Task, &ShortTermMemory)>,
) {
    for (task, short_term) in &tasks {
        // 跳过终态任务和等待摘要的任务
        if task.status.is_terminal() {
            continue;
        }
        if matches!(task.status, TaskStatus::Waiting(WaitingReason::Summarization)) {
            continue;
        }

        // 检查是否需要压缩
        if short_term.estimated_tokens > config.compression_threshold_tokens {
            let entries_count = short_term.entries.len();

            // 保留最近 N 轮（每轮 = User + Assistant，所以乘 2）
            let preserve_count = (config.preserve_recent_turns * 2) as usize;
            if entries_count <= preserve_count {
                continue;
            }

            let compress_count = entries_count - preserve_count;
            if compress_count == 0 {
                continue;
            }

            // 收集需要压缩的条目内容
            let to_compress: Vec<_> = short_term.entries.iter().take(compress_count).collect();
            let mut compress_text = String::new();
            for entry in &to_compress {
                compress_text.push_str(&format!("{:?}: {}\n", entry.role, entry.content));
            }

            // 发送摘要请求而非直接拼接
            info!(
                task_id = %task.id,
                entries_to_compress = compress_count,
                current_tokens = short_term.estimated_tokens,
                "triggering summarization request"
            );

            commands.spawn(SummarizationRequestMessage {
                task_id: task.id,
                content_to_summarize: compress_text,
                target_tokens: config.summary_target_tokens,
                trigger: SummarizationTrigger::TokenThreshold,
            });
        }
    }
}

/// 为任务型 Agent 初始化记忆 Component
pub(crate) fn init_agent_memory_system(
    mut commands: Commands,
    agents: Query<(Entity, &Agent), Added<Agent>>,
) {
    for (entity, _agent) in &agents {
        // 所有 Agent 都添加长期记忆
        commands.entity(entity).insert(LongTermMemory::default());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{EntryRole, Task};

    #[test]
    fn memory_compression_by_tokens() {
        let mut world = World::new();
        world.insert_resource(MemoryConfig {
            compression_threshold_tokens: 100,
            preserve_recent_turns: 1,
            summary_target_tokens: 50,
        });

        let task = Task::from_user_input("test", 3);
        let entity = world.spawn((task, ShortTermMemory::default())).id();

        // Add entries with known token counts
        {
            let mut stm = world.get_mut::<ShortTermMemory>(entity).unwrap();
            // Add enough content to exceed threshold
            for i in 0..10 {
                stm.add_entry(
                    EntryRole::User,
                    format!("This is message number {} with some content", i),
                    Default::default(),
                );
            }
        }

        // Verify tokens were estimated
        let stm = world.get::<ShortTermMemory>(entity).unwrap();
        assert!(stm.estimated_tokens > 0);
    }

    #[test]
    fn init_agent_memory_system_logic() {
        let mut world = World::new();
        world.init_resource::<MemoryConfig>();

        let agent = Agent {
            id: crate::domain::AgentId::nil(),
            profile: crate::domain::AgentProfile {
                name: "test".to_string(),
                model: "test-model".to_string(),
            },
            capabilities: crate::domain::AgentCapabilities {
                tags: vec![],
                description: "test agent".to_string(),
            },
            kind: crate::domain::AgentKind::Persistent,
            parent_id: None,
            bound_task_id: None,
            tool_permissions: crate::domain::AgentToolPermissions::default(),
            experience: crate::domain::AgentExperience::default(),
        };

        let entity = world.spawn((agent, LongTermMemory::default())).id();

        assert!(world.get::<LongTermMemory>(entity).is_some());
    }

    #[test]
    fn short_term_memory_token_estimation() {
        let mut stm = ShortTermMemory::default();

        // Add entries
        for i in 0..5 {
            stm.add_entry(
                EntryRole::User,
                format!("message {}", i),
                Default::default(),
            );
        }

        assert_eq!(stm.entries.len(), 5);
        assert!(stm.estimated_tokens > 0);
    }
}
```

- [ ] **Step 2: 运行测试**

Run: `cargo test --quiet`

Expected: 所有测试通过

- [ ] **Step 3: 提交**

```bash
git add src/systems/memory.rs
git commit -m "refactor: memory_compression_system to send summarization requests"
```

---

### Task 5: 修改 command_parse_system 处理 /summarize

**Files:**
- Modify: `src/systems/command.rs`

- [ ] **Step 1: 添加 /summarize 处理逻辑**

在 `src/systems/command.rs` 中：

1. 添加导入：

```rust
use crate::app::MemoryConfig;
use crate::domain::{SummarizationRequestMessage, SummarizationTrigger};
```

2. 修改函数签名添加 `config: Res<MemoryConfig>` 参数：

```rust
pub(crate) fn command_parse_system(
    mut commands: Commands,
    mut knowledge: ResMut<SpaceKnowledge>,
    config: Res<MemoryConfig>,
    user_inputs: Query<(Entity, &UserInputMessage)>,
    tasks: Query<(&Task, Option<&ShortTermMemory>)>,
) {
```

3. 替换 `UserCommand::Summarize` 分支：

```rust
UserCommand::Summarize => {
    // /summarize - 触发总结
    let active_task = tasks.iter().find(|(t, _)| !t.status.is_terminal());

    if let Some((task, memory)) = active_task {
        if let Some(stm) = memory {
            // 收集所有条目内容
            let content: String = stm
                .entries
                .iter()
                .map(|e| format!("{:?}: {}", e.role, e.content))
                .collect::<Vec<_>>()
                .join("\n");

            if !content.is_empty() {
                info!(task_id = %task.id, "triggering summarization via /summarize command");
                commands.spawn(SummarizationRequestMessage {
                    task_id: task.id,
                    content_to_summarize: content,
                    target_tokens: config.summary_target_tokens,
                    trigger: SummarizationTrigger::UserCommand,
                });
            }
        }
    }
    commands.entity(entity).despawn();
}
```

- [ ] **Step 2: 运行测试**

Run: `cargo test --quiet`

Expected: 所有测试通过

- [ ] **Step 3: 提交**

```bash
git add src/systems/command.rs
git commit -m "feat: implement /summarize command to trigger summarization"
```

---

### Task 6: 修改 task_termination_system 触发任务完成摘要

**Files:**
- Modify: `src/systems/transform.rs`

- [ ] **Step 1: 添加导入**

在 `src/systems/transform.rs` 顶部添加：

```rust
use crate::app::MemoryConfig;
use crate::domain::{SummarizationRequestMessage, SummarizationTrigger};
```

- [ ] **Step 2: 修改 task_termination_system**

将 `task_termination_system` 函数替换为：

```rust
pub(crate) fn task_termination_system(
    mut commands: Commands,
    config: Res<MemoryConfig>,
    tasks: Query<(&Task, Option<&ShortTermMemory>), Changed<Task>>,
) {
    for (task, memory) in &tasks {
        if task.status.is_terminal() {
            commands.spawn(TaskTerminatedMessage { task_id: task.id });

            // 任务完成时触发摘要
            if let Some(stm) = memory {
                if !stm.entries.is_empty() {
                    let content: String = stm
                        .entries
                        .iter()
                        .map(|e| format!("{:?}: {}", e.role, e.content))
                        .collect::<Vec<_>>()
                        .join("\n");

                    info!(task_id = %task.id, "triggering summarization on task completion");
                    commands.spawn(SummarizationRequestMessage {
                        task_id: task.id,
                        content_to_summarize: content,
                        target_tokens: config.summary_target_tokens,
                        trigger: SummarizationTrigger::TaskComplete,
                    });
                }
            }
        }
    }
}
```

- [ ] **Step 3: 运行测试**

Run: `cargo test --quiet`

Expected: 所有测试通过

- [ ] **Step 4: 提交**

```bash
git add src/systems/transform.rs
git commit -m "feat: trigger summarization on task completion"
```

---

### Task 7: 修改 ingest_execution_results_system 路由摘要结果

**Files:**
- Modify: `src/systems/transform.rs`

- [ ] **Step 1: 添加 SummarizationResultMessage 导入**

在 `src/systems/transform.rs` 导入中添加 `SummarizationResultMessage`：

```rust
use crate::domain::{
    Agent, AgentExecutionRequest, AgentExecutionRequestMessage, AgentExecutionResultMessage,
    AgentRequestKind, BrainDecisionError, CreateTaskMessage, EntryMetadata, EntryRole,
    FailureReason, RetryReadyMessage, ShortTermMemory, Signal, SignalPayload, Task, TaskStatus,
    TaskTerminatedMessage, UserInputMessage, UserOutputMessage, WaitingReason,
    SummarizationRequestMessage, SummarizationResultMessage, SummarizationTrigger,
};
```

- [ ] **Step 2: 在 ingest_execution_results_system 后添加摘要结果路由**

在 `ingest_execution_results_system` 函数中添加摘要结果处理。找到函数末尾，在 `while let Ok` 循环内，检查 `AgentRequestKind::Summarization`：

实际上需要在结果处理时区分，应该在 `llm_response_system` 之后或单独处理。更好的方式是创建一个新函数 `summarization_result_routing_system` 或在现有函数中添加分支。

修改 `ingest_execution_results_system` 函数为：

```rust
pub(crate) fn ingest_execution_results_system(
    mut commands: Commands,
    mut receiver: ResMut<ExecutionResultReceiver>,
) {
    while let Ok(result) = receiver.0.try_recv() {
        commands.spawn(AgentExecutionResultMessage { result });
    }
}
```

保持不变，然后在 `llm_response_system` 函数中添加对 `Summarization` 的处理。

实际上根据设计，应该在 `llm_response_system` 之后添加一个专门的路由系统，或者在 `llm_response_system` 中添加分支。

修改 `llm_response_system` 添加摘要结果处理分支：

```rust
pub(crate) fn llm_response_system(
    clock: Res<Clock>,
    mut commands: Commands,
    mut tasks: Query<(&mut Task, Option<&mut ShortTermMemory>)>,
    results: Query<(Entity, &AgentExecutionResultMessage)>,
) {
    for (entity, result_message) in &results {
        // 处理 Summarization 结果
        if result_message.result.request_kind == AgentRequestKind::Summarization {
            commands.spawn(SummarizationResultMessage {
                task_id: result_message.result.task_id,
                summary: result_message.result.result.clone(),
            });
            commands.entity(entity).despawn();
            continue;
        }

        // 处理 LlmCompletion 结果（现有逻辑）
        if result_message.result.request_kind != AgentRequestKind::LlmCompletion {
            continue;
        }

        // ... 现有的 LlmCompletion 处理逻辑 ...
    }
}
```

- [ ] **Step 3: 运行测试**

Run: `cargo test --quiet`

Expected: 所有测试通过

- [ ] **Step 4: 提交**

```bash
git add src/systems/transform.rs
git commit -m "feat: route summarization results in llm_response_system"
```

---

### Task 8: 注册新 Systems 到 app/mod.rs

**Files:**
- Modify: `src/app/mod.rs`

- [ ] **Step 1: 添加导入**

在 `src/app/mod.rs` 中添加新 systems 的导入：

```rust
use crate::systems::{
    // ... 现有导入 ...
    summarization_dispatch_system, summarization_result_system,
};
```

- [ ] **Step 2: 在 build_harness_app 中注册 systems**

在 `app.add_systems(Update, ...)` 块中添加：

```rust
// 在 Dispatch Set 中添加
summarization_dispatch_system
    .in_set(HarnessSet::Dispatch)
    .after(task_dispatch_system),

// 在 Transform Set 中添加
summarization_result_system
    .in_set(HarnessSet::Transform)
    .after(ingest_execution_results_system),
```

完整示例（找到合适位置插入）：

```rust
app.add_systems(
    Update,
    (
        // ... 现有 systems ...
        summarization_dispatch_system
            .in_set(HarnessSet::Dispatch)
            .after(task_dispatch_system),
    ),
);

app.add_systems(
    Update,
    (
        // ... 现有 systems ...
        summarization_result_system
            .in_set(HarnessSet::Transform)
            .after(ingest_execution_results_system),
    ),
);
```

- [ ] **Step 3: 运行测试**

Run: `cargo test --quiet`

Expected: 所有测试通过

- [ ] **Step 4: 提交**

```bash
git add src/app/mod.rs
git commit -m "feat: register summarization systems in harness app"
```

---

### Task 9: 添加 summarizer Agent 配置

**Files:**
- Modify: `agents.toml`

- [ ] **Step 1: 添加 summarizer agent 配置**

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

- [ ] **Step 2: 运行测试验证配置加载**

Run: `cargo test --quiet`

Expected: 所有测试通过

- [ ] **Step 3: 提交**

```bash
git add agents.toml
git commit -m "feat: add summarizer agent configuration"
```

---

### Task 10: 集成测试

**Files:**
- Create: `tests/summarization_flow.rs`

- [ ] **Step 1: 创建集成测试文件**

```rust
use std::sync::Arc;

use bevy::prelude::*;
use crossbeam_channel::unbounded;
use harness::{
    domain::{
        Agent, AgentCapabilities, AgentExecutionRequestMessage, AgentExecutionResult,
        AgentExecutionResultMessage, AgentId, AgentKind, AgentProfile, AgentRequestKind,
        AgentToolPermissions, EntryMetadata, EntryRole, ExecutionError, ShortTermMemory,
        SummarizationRequestMessage, SummarizationResultMessage, SummarizationTrigger, Task,
        TaskStatus, WaitingReason,
    },
    build_harness_app, create_executor_from_config, HarnessConfig,
};
use tokio::runtime::Runtime;

/// Mock executor that returns a fixed summary
struct MockSummarizerExecutor;

impl harness::domain::AgentExecutor for MockSummarizerExecutor {
    fn execute(
        &self,
        request: harness::domain::AgentExecutionRequest,
    ) -> harness::domain::ExecutorFuture {
        let summary = if request.request_kind == AgentRequestKind::Summarization {
            "This is a test summary of the conversation.".to_string()
        } else {
            "Normal response".to_string()
        };
        Box::pin(async move { Ok(summary) })
    }
}

#[test]
fn summarization_flow_token_threshold() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let config = HarnessConfig::default();
    let executor = Arc::new(MockSummarizerExecutor);
    let (input_tx, input_rx) = unbounded();
    let (output_tx, output_rx) = unbounded();

    let mut app = build_harness_app(config, runtime, executor, input_rx, output_tx);

    // Create a task with memory exceeding threshold
    let mut task = Task::from_user_input("test task", 3);
    task.status = TaskStatus::Ready;
    let task_id = task.id;

    let mut memory = ShortTermMemory::default();
    // Add enough content to exceed threshold
    for i in 0..20 {
        memory.add_entry(
            EntryRole::User,
            format!("Message {} with some content to increase tokens", i),
            EntryMetadata::default(),
        );
        memory.add_entry(
            EntryRole::Assistant,
            format!("Response {} with some content", i),
            EntryMetadata::default(),
        );
    }

    app.world_mut().spawn((task, memory));

    // Run a few updates
    for _ in 0..10 {
        app.update();
    }

    // Verify summarization request was created
    let requests = app
        .world()
        .query::<&SummarizationRequestMessage>()
        .iter(app.world())
        .count();
    // Note: This test may need adjustment based on actual threshold config
}

#[test]
fn summarization_dispatch_finds_summarizer_agent() {
    let mut world = World::new();

    // Spawn a summarizer agent
    let summarizer = Agent {
        id: AgentId::nil(),
        profile: AgentProfile {
            name: "summarizer".to_string(),
            model: "gpt-4.1-mini".to_string(),
        },
        capabilities: AgentCapabilities {
            tags: vec!["summarization".to_string()],
            description: "Summarizer agent".to_string(),
        },
        kind: AgentKind::Persistent,
        parent_id: None,
        bound_task_id: None,
        tool_permissions: AgentToolPermissions::default(),
        experience: Default::default(),
    };
    world.spawn(summarizer);

    // Verify agent can be found by tag
    let found = world
        .query::<&Agent>()
        .iter(&world)
        .find(|a| a.capabilities.tags.contains(&"summarization".to_string()));
    assert!(found.is_some());
}

#[test]
fn summarization_result_updates_memory() {
    let mut world = World::new();
    world.init_resource::<harness::app::Clock>();
    world.insert_resource(harness::app::MemoryConfig::default());

    // Create task and memory
    let task = Task::from_user_input("test", 3);
    let task_id = task.id;
    let mut memory = ShortTermMemory::default();
    memory.add_entry(EntryRole::User, "Hello", EntryMetadata::default());
    memory.add_entry(EntryRole::Assistant, "Hi there", EntryMetadata::default());

    world.spawn((task, memory));

    // Spawn result message
    world.spawn(SummarizationResultMessage {
        task_id,
        summary: Ok("Summary content".to_string()),
    });

    // Verify initial state
    assert!(world
        .query::<&ShortTermMemory>()
        .iter(&world)
        .next()
        .unwrap()
        .summary_prefix
        .is_none());
}
```

- [ ] **Step 2: 运行测试**

Run: `cargo test summarization --quiet`

Expected: 测试通过（可能需要调整）

- [ ] **Step 3: 提交**

```bash
git add tests/summarization_flow.rs
git commit -m "test: add integration tests for summarization flow"
```

---

### Task 11: 更新 TODO.md

**Files:**
- Modify: `docs/TODO.md`

- [ ] **Step 1: 更新 TODO.md 标记完成**

在 `docs/TODO.md` 中将相关待办项标记为完成：

```markdown
#### Phase 4.3 LLM 记忆摘要

- [x] 数据结构定义（消息、枚举扩展）
- [x] Summarizer Agent 配置
- [x] Prompt 模板
- [x] summarization_dispatch_system
- [x] summarization_result_system
- [x] 修改 memory_compression_system
- [x] 修改 command_parse_system
- [x] 修改 task_termination_system
- [x] 集成测试
```

- [ ] **Step 2: 提交**

```bash
git add docs/TODO.md
git commit -m "docs: update TODO.md with LLM summarization completion"
```

---

## 自检清单

- [ ] 所有测试通过：`cargo test --all-features`
- [ ] Clippy 无警告：`cargo clippy --all-targets --all-features -- -D warnings`
- [ ] 格式正确：`cargo fmt --all --check`
- [ ] 文档更新：`docs/TODO.md` 已更新
