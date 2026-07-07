# 日志规范

本规范定义 AI Harness 项目的结构化日志标准。
项目统一使用 `tracing` 输出结构化日志。

## 默认级别

| 环境 | 默认级别 | 说明 |
|------|----------|------|
| 生产（release） | `harness=info` | 可审计事件，通过 `RUST_LOG` 调整 |
| 开发（debug） | `harness=debug` | 包含工程细节，通过 `RUST_LOG` 调整 |

## 运行时级别控制

日志级别通过 `RUST_LOG` 环境变量控制，未设置时使用环境感知的默认值：

- 开发构建（`cargo run` / `cargo build`）：`harness=debug`
- 发布构建（`cargo run --release` / `cargo build --release`）：`harness=info`

### 常用示例

```bash
# 全局只看 warn 以上
RUST_LOG=warn cargo run

# 仅将 LLM 模块切到 debug，其他保持 info
RUST_LOG=info,harness::llm=debug cargo run

# 完整格式：按模块精细化控制
RUST_LOG=harness=trace,harness::channels=info cargo run
```

模块前缀 `harness` 对应 Cargo.toml 中的 lib name，第三方依赖不会受到 harness 级别设置影响。

> **注意：** 当前日志输出到 `logs/harness_*.jsonl` 文件，`HARNESS_LOG_DIR` 可控制输出目录。

## 审计事件分类

以下 `info!` 事件构成完整的审计轨迹：

| 领域 | 事件 | 字段 |
|------|------|------|
| LLM | LlmRequestStarted / LlmRequestCompleted | model, tools_count, duration_ms, response_len |
| LLM | LlmToolCallsRequested | tool_names |
| 审批 | ApprovalRequestReceived / ApprovalResolved | tool_name, decision, grant_mode |
| 任务 | TaskCreated / TaskTerminated | content, task_status, result_summary |
| 摘要 | SummarizationRequested / SummarizationTriggered | trigger, stm_entries, target_tokens |
| 工具 | ToolExecutionStarted / ToolExecutionDenied | tool_name, agent_name, permission |
| 评估 | EvaluationTriggered | turn_count, max_turns |
| 输入 | ExternalInputReceived | kind, channel, content_len |

事件名遵循 PascalCase，详见「事件命名规范」。

## 日志级别使用

| 级别 | 用途 | 示例场景 |
|------|------|----------|
| `trace!` | 高频事件、周期性检查 | 心跳、tick、空轮询 |
| `debug!` | 数据流转、状态转换、决策过程 | 调度、请求构建、结果解析 |
| `info!` | **审计事件**，用户可读的摘要 | LLM 调用开始/完成、审批请求/结果、任务创建/完成、工具执行、评估触发 |
| `warn!` | 异常但可恢复的情况 | 降级、拒绝、非致命失败 |
| `error!` | 错误场景，必须带现场 | 执行失败、配置错误、认证失败 |

## 高频日志约束

- 每帧、每轮询、每心跳都可能触发的日志使用 `trace!`。
- 禁止把高频状态变化记录在 `debug!` 以上级别，避免日志淹没关键事件。

## 统一格式要求

所有日志都必须包含结构化字段，推荐格式如下：

```rust
debug!(
    event = "EventName",
    field1 = value1,
    field2 = value2,
    "human readable message"
);
```

## 必需字段

| 场景 | 必需字段 |
|------|----------|
| 所有日志 | `event` |
| Task 相关 | `task_id` |
| Agent 相关 | `agent_id`、`agent_name` |
| 错误 | `error`、`error_type` |

### 审计消息要求

`info!` 级别的日志用于审计场景，除必需字段外还应包含：

- human-readable 消息正文（双引号字符串），简明描述事件
- 关键业务字段（ID、名称、状态、结果）
- **不包含** 工程细节（prompt 原文、tool_input 参数、内部状态结构）

工程细节在同一事件对应的 `debug!` 中记录。

## 数据级日志要求

- 保留完整 prompt、响应内容、STM 条目等必要上下文
- 调试场景默认不做截断和脱敏
- 状态转换必须记录 `from`、`to` 与 `reason`

## 错误现场规范

错误日志应尽量覆盖以下现场信息：

- 当前任务 ID、状态、输入内容、重试次数、最近错误
- 当前 STM 状态与关键上下文
- 当前 Agent、请求类型、prompt 长度
- 错误值本身与错误类型

## 事件命名规范

- 事件名使用 PascalCase
- 动作完成用动词过去式或完成态，如 `TaskCreated`
- 状态变化用 `*Transition`，如 `TaskStatusTransition`
- 错误事件用失败语义，如 `ToolExecutionFailed`
