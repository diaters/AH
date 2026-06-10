# LLM 记忆摘要设计

> **状态：已归档（2026-06-10）**
>
> 本文档描述的方案已被后续设计取代，与当前实现存在以下矛盾：
>
> - 独立的 summarization 管道已删除，`Summarization` 和 `Evaluation` 已迁移到 `WorkItem` 执行闭环
>
> 当前状态请参考 [docs/current-state.md](../../current-state.md)。
> 说明：当前摘要执行链路已收敛到 `WorkItem`，不再完全按本文原始链路实现。
> 当前优先参考：`docs/current-state.md`、
> `docs/design/2026-06-06-workitem-boundary-design.md`。
> 本文档描述记忆压缩功能，使用 LLM 生成摘要替代简单拼接。

---

## 一、设计目标

- 使用 LLM 生成高质量摘要，替代当前的简单拼接实现
- 支持三种触发条件：Token 阈值、用户指令、任务完成
- 通过独立模块实现，便于未来移除（当 LLM 支持无限上下文时）
- 创建专用 summarizer Agent，支持经验积累与演化

---

## 二、整体架构

```text
触发条件                     执行链路                      结果处理
┌─────────────┐              ┌─────────────┐              ┌─────────────┐
│ Token 阈值  │──┐           │             │              │             │
├─────────────┤  │           │ Summarizer  │              │ 更新        │
│ /summarize  │──┼──→ Message ──→ Agent    ──→ Message ──→ summary_    │
├─────────────┤  │   Request    Execution    Result       prefix       │
│ 任务完成    │──┘           │ (异步)      │              │             │
└─────────────┘              └─────────────┘              └─────────────┘
```

__新增组件__：

- `SummarizationRequestMessage` — 摘要请求消息
- `SummarizationResultMessage` — 摘要结果消息
- `WaitingReason::Summarization` — 等待摘要状态
- `summarizer` Agent（持久性）

__新增 System__：

- `summarization_trigger_system` — 检测触发条件
- `summarization_dispatch_system` — 生成摘要请求
- `summarization_result_system` — 处理摘要结果

---

## 三、数据结构

### 3.1 摘要请求消息

```rust
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
```

### 3.2 摘要结果消息

```rust
/// 摘要结果消息
#[derive(Debug, Clone, Component)]
pub struct SummarizationResultMessage {
    /// 关联的任务 ID
    pub task_id: TaskId,
    /// 生成的摘要
    pub summary: Result<String, ExecutionError>,
}
```

### 3.3 扩展 WaitingReason

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

### 3.4 扩展 AgentRequestKind

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum AgentRequestKind {
    LlmCompletion,
    BrainDecision,
    ToolExecution { tool_name: String },
    Summarization, // 新增
}
```

---

## 四、System 设计

### 4.1 summarization_trigger_system

职责：检测触发条件，生成 `SummarizationRequestMessage`

触发条件处理：

| 触发来源 | 检测位置 | 处理方式 |
|---------|---------|---------|
| Token 阈值 | `memory_compression_system` | 改为发送摘要请求 |
| `/summarize` | `command_parse_system` | 新增处理分支 |
| 任务完成 | `task_termination_system` | 终态检测时触发 |

```rust
pub(crate) fn summarization_trigger_system(
    config: Res<MemoryConfig>,
    mut commands: Commands,
    tasks: Query<(&Task, &ShortTermMemory)>,
) {
    for (task, memory) in &tasks {
        if task.status.is_terminal() {
            continue;
        }

        if memory.estimated_tokens > config.compression_threshold_tokens {
            let content = extract_content_to_compress(memory, &config);
            commands.spawn(SummarizationRequestMessage {
                task_id: task.id,
                content_to_summarize: content,
                target_tokens: config.summary_target_tokens,
                trigger: SummarizationTrigger::TokenThreshold,
            });
        }
    }
}
```

### 4.2 summarization_dispatch_system

职责：将摘要请求转为 `AgentExecutionRequest`，发给 summarizer Agent

```rust
pub(crate) fn summarization_dispatch_system(
    mut commands: Commands,
    agents: Query<&Agent>,
    requests: Query<(Entity, &SummarizationRequestMessage)>,
    mut tasks: Query<&mut Task>,
    clock: Res<Clock>,
) {
    let summarizer = agents.iter()
        .find(|a| a.capabilities.tags.contains(&"summarization".to_string()));

    let Some(summarizer) = summarizer else {
        return;
    };

    for (entity, request) in &requests {
        // 标记任务为等待摘要
        if let Some(mut task) = tasks.iter_mut().find(|t| t.id == request.task_id) {
            task.status = TaskStatus::Waiting(WaitingReason::Summarization);
            task.updated_at = clock.0;
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
        commands.entity(entity).despawn();
    }
}
```

### 4.3 summarization_result_system

职责：接收 LLM 摘要结果，更新 `ShortTermMemory.summary_prefix`

```rust
pub(crate) fn summarization_result_system(
    mut commands: Commands,
    results: Query<(Entity, &SummarizationResultMessage)>,
    mut tasks: Query<&mut Task>,
    mut memories: Query<&mut ShortTermMemory>,
    config: Res<MemoryConfig>,
    clock: Res<Clock>,
) {
    for (entity, result) in &results {
        if let Ok(summary) = &result.summary {
            // 查找关联的任务和记忆
            if let Some(task) = tasks.iter().find(|t| t.id == result.task_id) {
                if let Some(mut memory) = memories.iter_mut().find(|m| true) {
                    // 更新摘要前缀
                    memory.summary_prefix = Some(summary.clone());

                    // 移除已压缩的 entries（保留最近 N 轮）
                    let preserve_count = (config.preserve_recent_turns * 2) as usize;
                    if memory.entries.len() > preserve_count {
                        memory.entries.drain(0..memory.entries.len() - preserve_count);
                    }

                    // 重新计算 token
                    memory.recalculate_tokens();

                    // 恢复任务状态
                    if let Some(mut task) = tasks.iter_mut().find(|t| t.id == result.task_id) {
                        task.status = TaskStatus::Ready;
                        task.updated_at = clock.0;
                    }
                }
            }
        }
        commands.entity(entity).despawn();
    }
}
```

---

## 五、Summarizer Agent 配置

### 5.1 agents.toml 配置

```toml
[[agent]]
name = "summarizer"
model = "gpt-4.1-mini"
tags = ["summarization", "memory"]
description = "记忆摘要专家，负责压缩对话历史"

[agent.tools]
default_permission = "deny"
```

### 5.2 Prompt 模板

```rust
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

pub fn summarization_user_prompt(content: &str, target_tokens: u32) -> String {
    format!(
        "请将以下对话历史压缩为摘要，目标长度不超过 {} tokens：\n\n{}",
        target_tokens, content
    )
}
```

---

## 六、与现有架构的集成

### 6.1 修改 memory_compression_system

当前实现：直接拼接压缩

```rust
// Phase 4.1: 简单拼接，Phase 4.2 调用 LLM 生成摘要
let new_summary = if let Some(existing) = &short_term.summary_prefix {
    format!("{}\n\n{}", existing, compress_text)
} else {
    compress_text
};
```

修改后：生成摘要请求

```rust
// 移除直接拼接逻辑，改为发送摘要请求
commands.spawn(SummarizationRequestMessage {
    task_id: task.id,
    content_to_summarize: compress_text,
    target_tokens: config.summary_target_tokens,
    trigger: SummarizationTrigger::TokenThreshold,
});
```

### 6.2 修改 command_parse_system

新增 `/summarize` 指令处理：

```rust
UserCommand::Summarize => {
    // 查找当前活跃任务
    if let Some(active_task) = tasks.iter().find(|t| !t.status.is_terminal()) {
        commands.spawn(SummarizationRequestMessage {
            task_id: active_task.id,
            content_to_summarize: memory.entries_to_compress(),
            target_tokens: config.summary_target_tokens,
            trigger: SummarizationTrigger::UserCommand,
        });
    }
    commands.entity(entity).despawn();
}
```

### 6.3 修改 task_termination_system

任务完成时触发摘要：

```rust
if task.status.is_terminal() && !memory.entries.is_empty() {
    commands.spawn(SummarizationRequestMessage {
        task_id: task.id,
        content_to_summarize: memory.all_entries_text(),
        target_tokens: config.summary_target_tokens,
        trigger: SummarizationTrigger::TaskComplete,
    });
}
```

### 6.4 修改 agent_execution_system

增加 `AgentRequestKind::Summarization` 处理：

```rust
match request.request_kind {
    AgentRequestKind::LlmCompletion | AgentRequestKind::BrainDecision => {
        // 现有逻辑
    }
    AgentRequestKind::ToolExecution { tool_name } => {
        // 现有逻辑
    }
    AgentRequestKind::Summarization => {
        // 摘要请求，直接执行 LLM 调用
        // 结果通过 SummarizationResultMessage 返回
    }
}
```

### 6.5 修改 ingest_execution_results_system

增加摘要结果路由：

```rust
match request.request_kind {
    AgentRequestKind::Summarization => {
        commands.spawn(SummarizationResultMessage {
            task_id: result.task_id,
            summary: result.result.clone(),
        });
    }
    // ... 其他分支
}
```

---

## 七、System 编排

新增 System 加入现有 `HarnessSet`：

```rust
app.add_systems(
    Update,
    (
        // Dispatch Set
        summarization_dispatch_system
            .in_set(HarnessSet::Dispatch)
            .after(task_dispatch_system),

        // Transform Set
        summarization_result_system
            .in_set(HarnessSet::Transform)
            .after(ingest_execution_results_system),
    ),
);
```

---

## 八、实施范围

### 8.1 MVP 范围

- [x] 数据结构定义（消息、枚举扩展）
- [x] Summarizer Agent 配置
- [x] Prompt 模板
- [x] `summarization_dispatch_system`
- [x] `summarization_result_system`
- [x] 修改 `memory_compression_system`
- [x] 修改 `command_parse_system`
- [x] 修改 `task_termination_system`
- [x] 修改 `agent_execution_system`
- [x] 修改 `ingest_execution_results_system`
- [x] 集成测试

### 8.2 后续扩展

- [ ] 摘要风格配置化
- [ ] 摘要质量评估
- [ ] 摘要经验积累效果观察

---

## 九、可移除性设计

当 LLM 支持无限上下文时，移除本功能的步骤：

1. 删除数据结构：
   - `SummarizationRequestMessage`
   - `SummarizationResultMessage`
   - `SummarizationTrigger`
   - `WaitingReason::Summarization`
   - `AgentRequestKind::Summarization`

2. 删除 System：
   - `summarization_dispatch_system`
   - `summarization_result_system`

3. 删除配置：
   - `agents.toml` 中的 summarizer 配置
   - `MemoryConfig` 中的压缩相关字段

4. 恢复原有逻辑：
   - `memory_compression_system` 移除或保留空实现
   - `command_parse_system` 移除 `/summarize` 处理

核心执行链路（`AgentExecutionRequest` / `AgentExecutionResult`）不受影响。
