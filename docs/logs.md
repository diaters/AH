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

> __注意：__ 当前日志输出到 `logs/harness_*.jsonl` 文件，`HARNESS_LOG_DIR` 可控制输出目录。

### 进程服务独立日志

生产入口另建 `HARNESS_LOG_DIR/harness_<时间>_<宿主 PID>.processes.jsonl`（目录默认 `logs`），
以同步整行 JSON 追加记录守护器启动、宿主连接关闭、进程组停止与回收。守护器直接写入该文件，
避免宿主被强杀时丢失后续回收证据。文件创建权限为 `0600`，不记录 shell 命令、环境变量或输出正文。
独立日志不受宿主 `RUST_LOG` 过滤影响；库测试默认不创建该文件。
详细协议与超时影响范围见 [实例级进程守护实现](design/2026-10-29-process-supervisor.md)。

## 审计事件分类

以下 `info!` 事件构成完整的审计轨迹：

| 领域 | 事件 | 字段 |
|------|------|------|
| LLM | LlmRequestStarted / LlmRequestCompleted | model, tools_count, duration_ms, response_len |
| LLM | LlmToolCallsRequested | tool_names |
| 审批 | ApprovalRequestReceived / ApprovalResolved | tool_name, decision, grant_mode |
| 任务 | TaskCreated / TaskTerminated | content, task_status, result_summary |
| 摘要 | SummarizationRequested / SummarizationTriggered | trigger, stm_entries, target_tokens |
| 工具 | ToolExecutionStarted / ToolExecutionDenied | tool_name, agent_name, task_id |
| 评估 | EvaluationTriggered | turn_count, max_turns |
| 输入 | ExternalInputReceived | kind, channel, content_len |

事件名遵循 PascalCase，详见「事件命名规范」。

### 异常事件（`warn!`）

以下 `warn!` 事件标识异常但可恢复的路径，不构成审计轨迹但具有运维可观测价值：

| 领域 | 事件 | 字段 | 触发条件 |
|------|------|------|----------|
| Brain | BrainDecisionParseFailed | task_id, error, raw_len, raw_preview, from/to_status | Brain LLM 返回非 JSON，无法解析 |
| Brain | BrainDecisionAgentNotFound | task_id, selected_agent, from/to_status | Brain 选择的 agent 不存在或非 Persistent |
| Brain | BrainSelectedSkillNotOwned | task_id, agent_name, skill_name | Brain 选择的 skill 不属于目标 agent |

## 日志级别使用

| 级别 | 用途 | 示例场景 |
|------|------|----------|
| `trace!` | 高频事件、周期性检查 | 心跳、tick、空轮询 |
| `debug!` | 数据流转、状态转换、决策过程 | 调度、请求构建、结果解析 |
| `info!` | __审计事件__，用户可读的摘要 | LLM 调用开始/完成、审批请求/结果、任务创建/完成、工具执行、评估触发 |
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

### 工具返回与 shell 命令结果

- `ToolExecuted.tool_returned=true` 仅表示工具正常返回结构化结果；
  `ToolExecutionFailed.tool_returned=false` 表示工具接口失败，不使用含混的 `success` 字段。
- shell 执行层独立记录 `ShellCommandCompleted` / `ShellCommandFailed`，包含
  `command_succeeded`、`exit_code`、`timed_out`、Task / Agent ID。
  非零退出、信号终止或超时记录 WARN；原始输出保留在工具结果的 DEBUG 日志中。
- `LlmRequestCompleted` 同时记录文字长度和 `tool_call_count`；文字长度为零但有工具调用
  是正常工具响应，不能据此判定模型空响应。
- QQ typing 明确返回“不支持的调用”时记录 `QqTypingUnsupported` 并在当前通道实例内缓存；
  网络失败、普通 404 和服务暂不可用不触发能力缓存。

### 审计消息要求

`info!` 级别的日志用于审计场景，除必需字段外还应包含：

- human-readable 消息正文（双引号字符串），简明描述事件
- 关键业务字段（ID、名称、状态、结果）
- __不包含__ 工程细节（prompt 原文、tool_input 参数、内部状态结构）

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

## 定时监控 P1

- `BackgroundReportFailed`：自动通知汇报失败，但原任务受管作业仍存活；包含任务和错误，
  不代表脚本停止，后续仍可查询或显式结束。
- `LlmRequestRejected`：400 / 422 参数错误的脱敏协议形状（角色、工具数量、JSON object 数、
  推理字段存在性及回合代次）；不输出此诊断对应的原始工具参数，不对相同请求自动重试。

- `ScheduleRunRecorded`：运行账本与健康提交完成，字段为 `task_id`、`schedule_id`、
  `outcome`、`notification_count`；入队成功不会触发此完成事件。
- `ScheduleAccountingFailed`：账本、健康或过期定义提交失败，附错误上下文，后续帧重试。
- `ScheduleFailureAlert`：首次达到连续失败阈值，包含 `schedule_id`、`task_id` 与 `consecutive_failures`。
- `ScheduleRecovered`：告警中的定时任务首次成功，包含 `schedule_id` 与 `task_id`。
