# 数据级调试日志设计

## 概述

为 AI Harness 添加全面的调试日志，目标是：
1. 从日志即可完整追踪系统运行流程
2. 错误发生时提供详细的现场信息
3. 记录完整数据流转（prompt、STM 条目等）

## 设计决策

| 决策项 | 选择 | 理由 |
|--------|------|------|
| 日志粒度 | 数据级 | 完整记录 prompt、STM 内容 |
| 敏感数据处理 | 全量记录 | 调试需要完整上下文 |
| 错误现场 | 核心字段 + STM + 请求 + Agent 匹配 | 用户明确要求全部 |

## 日志层级架构

按数据流转分层，每层记录「入 → 处理 → 出」：

```
┌─────────────────────────────────────────────────────────────────┐
│ Layer 1: Ingress（外部输入）                                      │
│   - 收到原始输入内容                                              │
├─────────────────────────────────────────────────────────────────┤
│ Layer 2: Signal→Transform（信号转换）                            │
│   - Signal payload 详情                                          │
│   - CreateTaskMessage/ContinueTaskMessage 完整内容               │
├─────────────────────────────────────────────────────────────────┤
│ Layer 3: Routing（路由决策）                                      │
│   - 所有 Waiting(User) 任务列表                                   │
│   - 路由决策理由                                                  │
├─────────────────────────────────────────────────────────────────┤
│ Layer 4: Dispatch（分发决策）                                     │
│   - 所有候选 Agent 及其评分                                       │
│   - 最终选择的 Agent 及理由                                       │
│   - 完整 prompt（含 STM 历史条目）                                │
├─────────────────────────────────────────────────────────────────┤
│ Layer 5: Execution（执行）                                        │
│   - 请求参数                                                      │
│   - 响应内容                                                      │
├─────────────────────────────────────────────────────────────────┤
│ Layer 6: Response Processing（响应处理）                          │
│   - STM 更新                                                      │
│   - Task 状态转换                                                 │
├─────────────────────────────────────────────────────────────────┤
│ Layer 7: Memory（记忆管理）                                       │
│   - 压缩触发                                                      │
│   - 条目变更                                                      │
├─────────────────────────────────────────────────────────────────┤
│ Layer 8: Maintenance（系统维护）                                  │
│   - Agent 加载/创建/销毁                                          │
├─────────────────────────────────────────────────────────────────┤
│ Layer 9: Tool（工具执行）                                         │
│   - 权限判定                                                      │
│   - 执行结果                                                      │
└─────────────────────────────────────────────────────────────────┘
```

## 日志格式规范

### 统一格式

所有日志使用结构化字段，格式：

```rust
debug!(
    event = "EventName",
    field1 = value1,
    field2 = value2,
    "human readable message"
);
```

### 必需字段

| 层级 | 必需字段 |
|------|----------|
| 所有 | `event` - 事件名称（PascalCase） |
| Task 相关 | `task_id` |
| Agent 相关 | `agent_id`, `agent_name` |
| 错误 | `error`, `error_type` |

## 各层日志详情

### Layer 1: Ingress

**文件**: `src/systems/ingress.rs`

| 事件 | 字段 |
|------|------|
| ExternalInputReceived | `kind`, `content` |
| RetryWakeupTriggered | `task_id`, `retry_count`, `next_retry_at` |
| TickClock | `new_tick` |

### Layer 2: Signal→Transform

**文件**: `src/systems/transform.rs`, `src/systems/routing.rs`, `src/systems/command.rs`

| 事件 | 字段 |
|------|------|
| SignalIngested | `signal_type`, `payload` |
| TaskCreated | `task_id`, `content`, `multi_turn`, `stm_initial_entries`, `stm_initial_tokens` |
| TaskContinued | `task_id`, `user_input`, `prev_content`, `new_content`, `stm_entries_before`, `stm_entries_after` |
| CommandParsed | `command`, `raw_input` |
| TaskFinished | `task_id`, `result` |

### Layer 3: Routing

**文件**: `src/systems/routing.rs`

| 事件 | 字段 |
|------|------|
| RoutingDecision | `waiting_tasks`, `selected_task_id`, `decision` |

### Layer 4: Dispatch

**文件**: `src/systems/dispatch.rs`

| 事件 | 字段 |
|------|------|
| AgentSelection | `task_id`, `task_content`, `candidates`, `selected_agent`, `selected_agent_id`, `selection_reason`, `stm_entries`, `stm_tokens`, `stm_recent_entries` |
| PromptBuilt | `task_id`, `agent_id`, `prompt_len`, `prompt`, `system_prompt` |
| BrainDispatch | `task_id`, `brain_agent_id`, `prompt_len` |

### Layer 5: Execution

**文件**: `src/systems/execution.rs`

| 事件 | 字段 |
|------|------|
| ExecutionSubmitted | `task_id`, `agent_id`, `request_kind`, `prompt_len` |

### Layer 6: Response

**文件**: `src/systems/transform.rs`, `src/systems/summarization.rs`

| 事件 | 字段 |
|------|------|
| LlmResponseReceived | `task_id`, `request_kind`, `success`, `response_len`, `response_content` |
| TaskStatusTransition | `task_id`, `from_status`, `to_status`, `reason`, `multi_turn` |
| SummarizationCompleted | `task_id`, `summary_len` |

### Layer 7: Memory

**文件**: `src/systems/memory.rs`, `src/domain/memory.rs`

| 事件 | 字段 |
|------|------|
| CompressionTriggered | `task_id`, `current_tokens`, `threshold`, `entries_total`, `entries_to_compress` |
| StmEntryAdded | `role`, `content`, `entry_tokens`, `total_tokens`, `total_entries` |
| LtmArchiveAdded | `total_entries` |

### Layer 8: Maintenance

**文件**: `src/systems/maintenance.rs`

| 事件 | 字段 |
|------|------|
| AgentsConfigLoaded | `config_path`, `agent_count`, `agent_names` |
| PersistentAgentSpawned | `name`, `id` |
| TaskScopedAgentSpawned | `name`, `id`, `parent_id`, `task_id` |
| AgentDespawned | `name`, `task_id` |

### Layer 9: Tool

**文件**: `src/systems/tool.rs`

| 事件 | 字段 |
|------|------|
| ToolDispatch | `tool_name`, `agent_id`, `agent_name`, `permission`, `tool_input` |
| ToolExecuted | `tool_name`, `task_id`, `success`, `output` |
| ToolConfirmationRequest | `tool_name`, `agent_name`, `request_id` |
| ToolConfirmationResult | `tool_name`, `selected_option`, `mode` |

## 错误现场规范

所有错误使用 `error!` 宏，必须包含以下字段组：

### 必需字段组

```rust
error!(
    // === 核心字段 ===
    task_id = %task.id,
    task_status = ?task.status,
    task_content = %task.content,
    retry_count = task.retry_count,
    max_retries = task.max_retries,
    last_error = ?task.last_error,

    // === STM 状态 ===
    stm_entries = stm_entries_count,
    stm_tokens = stm_tokens_count,
    stm_recent = ?recent_entries,

    // === 请求详情 ===
    agent_id = %agent_id,
    request_kind = ?request_kind,
    prompt_len = prompt_len,

    // === Agent 匹配（如适用）===
    candidates = ?candidates_info,
    selected_agent = ?selected_agent_name,

    // === 错误本身 ===
    error = %error,
    error_type = %error_type_name,

    "execution error with full context"
);
```

### 错误现场注入点

| 错误类型 | 位置 | 额外字段 |
|----------|------|----------|
| LLM 执行错误 | `llm_response_system` | `response_content` |
| Brain 决策错误 | `brain_decision_system` | `brain_response` |
| Agent 选择错误 | `task_dispatch_system` | `available_agents` |
| Tool 执行错误 | `tool_result_system` | `tool_input`, `tool_output` |
| 状态转换错误 | Task 方法 | `from_status`, `to_status` |

## 文件改动清单

| 文件 | 改动 | 新增日志点 |
|------|------|-----------|
| src/systems/ingress.rs | 新增 | 5 |
| src/systems/transform.rs | 新增/修改 | 8 |
| src/systems/routing.rs | 新增 | 3 |
| src/systems/dispatch.rs | 新增 | 8 |
| src/systems/execution.rs | 新增 | 4 |
| src/systems/command.rs | 新增 | 3 |
| src/systems/memory.rs | 新增 | 3 |
| src/systems/maintenance.rs | 新增 | 4 |
| src/systems/tool.rs | 新增 | 8 |
| src/systems/contribution.rs | 新增 | 3 |
| src/systems/summarization.rs | 新增 | 4 |
| src/systems/output.rs | 新增 | 1 |
| src/domain/memory.rs | 修改 | 2 |
| src/domain/mod.rs | 新增 | 6 |

**总计**: 约 62 个新增/修改日志点

## 实施顺序

1. **基础层**: domain/mod.rs（Task 状态转换日志）
2. **数据层**: domain/memory.rs（STM/LTM 操作日志）
3. **入口层**: ingress.rs, command.rs
4. **转换层**: transform.rs, routing.rs
5. **分发层**: dispatch.rs
6. **执行层**: execution.rs
7. **响应层**: transform.rs (response 部分), summarization.rs
8. **维护层**: maintenance.rs, memory.rs, contribution.rs
9. **工具层**: tool.rs
10. **输出层**: output.rs

## 验证标准

实施完成后，应满足：

1. **流程可追踪**: 从日志可还原完整任务生命周期
2. **数据可见**: 每个 prompt 和响应都有完整记录
3. **错误可诊断**: 错误日志包含所有必需现场信息
4. **测试通过**: 所有现有测试继续通过
