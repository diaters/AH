> **状态：当前有效**
> 与 `docs/logs.md` 和 `main.rs` 中的日志过滤实现一致。

# 日志等级重构设计

## 背景

当前日志级别使用存在两个核心问题：

1. **过滤机制不灵活**：`main.rs` 硬编码 `EnvFilter::new("debug")`，无法通过环境变量调整日志级别，dev/prod 共用同一过滤级别
2. **审计语义缺失**：`info!` 仅用于启动/关闭等操作事件，而 LLM 调用、审批流程、Task 状态变化等需要审计的内容全部使用 `debug!` 记录，默认 INFO 级别下无法看到

## 目标

- 审计事件对用户可见但简洁明了
- 工程细节不受影响，仍可通过环境变量获取
- dev/release 构建有合理的默认级别
- 遵循 tracing 生态标准做法

## 调整方案

### 1. 过滤机制

将硬编码的 `EnvFilter::new("debug")` 替换为从 `RUST_LOG` 环境变量读取，并设置环境感知的默认值：

```rust
let filter = tracing_subscriber::EnvFilter::builder()
    .with_env_var("RUST_LOG")
    .with_default_directive(
        if cfg!(debug_assertions) {
            "harness=debug".parse().unwrap()
        } else {
            "harness=info".parse().unwrap()
        }
    )
    .from_env_lossy();
```

| 场景 | RUST_LOG | 过滤级别 |
|------|----------|----------|
| `cargo run` (dev) | 未设置 | `harness=debug` |
| `cargo run --release` (prod) | 未设置 | `harness=info` |
| `RUST_LOG=harness::llm=debug cargo run` | 设置 | 全局 info，LLM 模块 debug |
| `RUST_LOG=warn cargo run` | 设置 | 只看 warn 以上 |

模块前缀使用 `harness`（Cargo.toml 的 lib name），避免第三方依赖日志污染。

### 2. 审计消息模式

`info!` 级别消息遵循以下模式：

```rust
info!(
    event = "EventName",       // PascalCase
    task_id = %task.id,        // 结构化字段
    from = ?old_status,        // 上下文
    to = ?new_status,
    "用户可读的摘要说明"         // human-readable 消息
);
```

关键规则：

- `info!` 只放用户可读的摘要，不放工程细节
- 工程细节保留在 `debug!` 级别
- 每个审计事件拆分为一个 `info!`（审计摘要）和一个 `debug!`（工程细节）

### 3. 各领域调整

#### 3.1 LLM 请求与响应

| 级别 | 事件 | 包含字段 |
|------|------|----------|
| `info!` | LlmRequestStarted | task_id, agent_id, model, tools_count |
| `info!` | LlmRequestCompleted | task_id, agent_id, model, duration_ms, response_len |
| `info!` | LlmToolCallsRequested | task_id, tool_names |
| `debug!` | LlmRequestDetails | prompt_len, has_system_prompt, has_conversation, has_reasoning |
| `debug!` | (保留现有) | LlmRequestStart 的工程细节 |

#### 3.2 审批流程

| 级别 | 事件 | 包含字段 |
|------|------|----------|
| `info!` | ApprovalRequestReceived | request_id, tool_name, parent_agent_id, child_agent_id |
| `info!` | ApprovalResolved | request_id, tool_name, decision, grant_mode |
| `debug!` | (保留现有) | tool_input, source_task_id, 原 Task 状态 |

#### 3.3 Task 状态变化

| 级别 | 事件 | 包含字段 |
|------|------|----------|
| `info!` | TaskCreated | task_id, content |
| `info!` | TaskTerminated | task_id, task_status, result_summary |
| `info!` | SummarizationTriggered | task_id, trigger, stm_entries |
| `debug!` | (保留现有) | retry_count, max_retries, last_error, ToolCallingState |

#### 3.4 工具调用

| 级别 | 事件 | 包含字段 |
|------|------|----------|
| `info!` | ToolExecutionStarted | tool_name, agent_name, permission, task_id |
| `info!` | ToolExecutionCompleted | tool_name, task_id, duration_ms |
| `info!` | ToolExecutionDenied | tool_name, agent_name, reason |
| `debug!` | (保留现有) | tool_input, required_tag, executor 查找过程 |

#### 3.5 Evaluation / Experience

| 级别 | 事件 | 包含字段 |
|------|------|----------|
| `info!` | EvaluationTriggered | task_id, turn_count, max_turns |
| `info!` | ExperienceWritebackCompleted | candidate_id, destination |
| `debug!` | (保留现有) | evaluator_agent_name, ExperienceCandidateStatus |

#### 3.6 通道消息

| 级别 | 事件 | 包含字段 |
|------|------|----------|
| `info!` | ChannelListenStarted (现有) | channel |
| `info!` | ChannelListenStopped (现有) | channel, reason |
| `info!` | ChannelConnected | channel |
| `info!` | ExternalInputReceived | kind, channel, content_len |
| `warn!` | (保留现有) | 断线重试、发送失败 |

#### 3.7 触发器

| 级别 | 事件 | 包含字段 |
|------|------|----------|
| `info!` | TriggersLoaded (现有) | webhook_count, timer_count |
| `info!` | ScheduledTaskTriggered | kind |
| `info!` | TriggerConfigLoaded | webhook_count, timer_count |

### 4. 文档更新

- `docs/logs.md`：更新级别说明表，新增 `info!` 审计定位说明；新增 `RUST_LOG` 使用说明和默认值规则；新增审计事件分类表
- `main.rs`：更新 `init_tracing` 注释

### 5. 不变的部分

- `trace!` / `debug!` / `warn!` / `error!` 的现有规范不变
- `event` 命名规范仍为 PascalCase
- 必需字段表不变

## 涉及文件

| 文件 | 变更内容 |
|------|----------|
| `src/main.rs` | 过滤机制从硬编码改为 `RUST_LOG` + 环境感知默认值 |
| `src/llm/genai.rs` | LlmRequestStart/Completed 新增 `info!`，保留 `debug!` |
| `src/systems/tools/approval.rs` | ApprovalRequestReceived/Resolved 新增 `info!` |
| `src/systems/tools/dispatch.rs` | ToolExecutionStarted/Completed/Denied 新增 `info!` |
| `src/systems/transform/task_lifecycle.rs` | TaskTerminated 新增 `info!`，TaskCreated 已在 task_creation.rs |
| `src/systems/transform/task_creation.rs` | TaskCreated 新增 `info!` |
| `src/systems/transform/trigger_task.rs` | ScheduledTaskTriggered 新增 `info!` |
| `src/systems/summarization.rs` | SummarizationTriggered 新增 `info!` |
| `src/systems/evaluation.rs` | EvaluationTriggered 新增 `info!` |
| `src/systems/experience/writeback.rs` | ExperienceWritebackCompleted 新增 `info!` |
| `src/systems/ingress.rs` | ExternalInputReceived 新增 `info!` |
| `docs/logs.md` | 更新级别说明、审计分类、RUST_LOG 使用说明 |

## 不做的事

- 不重构已废弃的旧 shell 工具日志（已在上一轮清理）
- 不修改 `tracing` 依赖版本
- 不引入新的结构化字段约束
- 不做全面的 225 条 `debug!` 审计（可作为后续独立任务）