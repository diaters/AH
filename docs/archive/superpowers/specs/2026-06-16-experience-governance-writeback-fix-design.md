> **状态：已归档** — 对应功能已合并到 main，归档于 2026-07-05

# 经验治理写回链路修复设计

## 问题背景

经验治理审批→写回链路在运行时完全失效。用户批准候选后，写回操作从未执行，
候选永远停留在 `NeedsUserApproval` 状态。

同时，孵化执行链路的多个环节存在缺陷：proposal 状态未推进、重复持久化、
孵化 Agent 未接入运行时加载。

## 修复范围

### P0 — 审批->写回链路断裂（阻塞项）

**根因**：`spawn_experience_confirmation()` 只创建 `ToolConfirmationRequestMessage`，
未创建配对的 `ToolExecutionRequestMessage`。`tool_confirmation_result_system`
（Dispatch set）找不到匹配的执行请求后，销毁了 `ToolConfirmationResponseMessage`，
导致 `experience_approval_result_system`（Maintenance set）永远看不到响应。

**方案**：恢复 `ToolExecutionRequestMessage` 配对机制，并在确认系统中对
`experience_governance` 工具做特判。

### P1 — 孵化执行链路不完整

**问题**：

- Proposal 状态停在 `Approved`，未推进到 `Executing`/`Executed`
- 多候选场景下重复触发 `writeback_incubation_proposal`，Agent 注册表重复追加
- `IncubatedAgentRegistry` 使用独立 JSON 文件，`load_agents_system` 不会读取
- 文件扩展名 `.toml` 与实际 JSON 格式不一致

**方案**：废弃独立注册表，改为向 `agents.toml` 追加 TOML 格式条目；
补齐状态推进和去重保护。

### P2 — 缺失审计事件

**问题**：`IncubationProposalMerged`、`IncubationExecutionStarted/Succeeded/Failed`
等审计事件未输出日志。

**方案**：在关键路径补齐 `tracing` 事件。

## 边界声明

以下内容不纳入本次修复，作为后续演进登记：

- LLM 前置评估：在 `submit_experience_candidate` 后立即用 LLM 评估是否值得孵化
- 可编辑确认：用户在审批时编辑 Agent 名称/描述等字段（当前首版只做二值确认）
- 初始资产写入：为新 Agent 写入初始 LongTermMemory / Skill Package
- 审批通道独立：将非工具审批从 `ToolConfirmation` 通道独立出来

## 详细设计

### P0: 审批->写回链路修复

#### 改动 1: `spawn_experience_confirmation()` 同步创建配对实体

在创建 `ToolConfirmationRequestMessage` 的同时，创建配对的
`ToolExecutionRequestMessage`（占位实体），设置 `pending_confirmation_id`。

这样 `tool_confirmation_result_system` 能找到匹配的执行请求，不会提前销毁响应。

```text
文件：src/systems/contribution.rs
函数：spawn_experience_confirmation()
变更：在 spawn ToolConfirmationRequestMessage 之后，新增 spawn ToolExecutionRequestMessage
```

#### 改动 2: `tool_confirmation_result_system` 对 experience_governance 特判

找到匹配的 `ToolExecutionRequestMessage` 后，若 `tool_name == "experience_governance"`：

- 销毁执行请求占位实体
- **不**销毁 `ToolConfirmationResponseMessage`（留给 `experience_approval_result_system`）
- **不**触发工具执行
- `continue` 跳过后续逻辑

```text
文件：src/systems/tools/confirmation.rs
函数：tool_confirmation_result_system
变更：在匹配成功后、正常执行逻辑前，插入 experience_governance 特判分支
```

#### 改动 3: 显式声明系统执行顺序

确保 `experience_approval_result_system` 在 `tool_confirmation_result_system`
之后执行，避免帧内竞争。

```text
文件：src/plugins/execution.rs
变更：experience_approval_result_system 添加 .after(tool_confirmation_result_system)
```

### P1: 孵化执行链路修复

#### 改动 4: `AgentConfig` / `AgentEntry` 添加 `Serialize`

当前只有 `Deserialize`，需要加上 `Serialize` 才能用 `toml::to_string` 写回。

```text
文件：src/domain/mod.rs
变更：AgentConfig 和 AgentEntry 的 derive 中添加 Serialize
```

#### 改动 5: 重写 `IncubatedAgentRegistry`

废弃独立 JSON 文件存储，改为向 `agents.toml` 追加 `[[agent]]` 条目：

- 读取现有 `agents.toml`，解析为 `AgentConfig`
- 按 `name` 去重：若同名 Agent 已存在则跳过
- 追加新 `AgentEntry`（name、model、tags、description、tools）
- 原子写回：先写临时文件（`.toml.tmp`），再 `rename` 覆盖原文件
- 路径使用 `HarnessSettings.agents_config_path`，与 `load_agents_system` 一致

```text
文件：src/infrastructure/incubation/agent_registry.rs
变更：重写 append() 方法，参数改为接收 config_path
```

#### 改动 6: `writeback_incubation_proposal` 去重与状态推进

在写回前检查 proposal 状态：

- 若已 `Executed`，直接跳过（防止多候选重复触发）
- 写回前将状态设为 `Executing`
- 写回成功后将状态设为 `Executed` 并更新 `updated_at`
- 写回失败时将状态设为 `ExecutionFailed`

```text
文件：src/systems/contribution.rs
函数：writeback_incubation_proposal()
变更：添加状态检查和推进逻辑
```

#### 运行时行为

孵化成功后只写入 `agents.toml`，**不在当前会话 spawn Agent 实体**。
重启后 `load_agents_system` 会自动从 `agents.toml` 加载所有 `[[agent]]` 条目。

### P2: 审计事件补齐

| 位置 | 事件名 | 级别 |
|---|---|---|
| `merge_into_proposal` 成功后 | `IncubationProposalMerged` | DEBUG |
| `writeback_incubation_proposal` 开始 | `IncubationExecutionStarted` | DEBUG |
| `writeback_incubation_proposal` 成功 | `IncubationExecutionSucceeded` | DEBUG |
| `writeback_incubation_proposal` 失败 | `IncubationExecutionFailed` | WARN |

## 测试策略

### P0 集成测试

- `experience_governance_confirmation_preserves_response_for_approval_system`：
  创建配对后 `tool_confirmation_result_system` 不销毁响应实体
- `approved_candidate_spawns_writeback_request`：
  审批通过后 `ExperienceWritebackRequestMessage` 被创建

### P1 集成测试

- `incubated_agent_appended_to_agents_toml`：
  追加后 `agents.toml` 包含新条目且原有条目不变
- `duplicate_incubation_skips_if_name_exists`：
  同名 Agent 不重复追加
- `proposal_status_advances_to_executed`：
  写回成功后 proposal 状态为 `Executed`

### 回归

- 现有 `experience_collection_workitem_flow` 测试继续通过
- 现有 `experience_layered_governance_flow` 测试继续通过
- 全量 `cargo test` 通过

## 文件变更清单

| 文件 | 变更类型 | 职责 |
|---|---|---|
| `src/domain/mod.rs` | 修改 | `AgentConfig`/`AgentEntry` 添加 `Serialize` |
| `src/systems/contribution.rs` | 修改 | 配对创建 + 去重 + 状态推进 + 审计事件 |
| `src/systems/tools/confirmation.rs` | 修改 | `experience_governance` 特判 |
| `src/infrastructure/incubation/agent_registry.rs` | 重写 | 改为向 `agents.toml` 追加 |
| `src/plugins/execution.rs` | 修改 | 显式声明系统执行顺序 |
| `tests/experience_layered_governance_flow.rs` | 修改 | P0 集成测试 + 回归适配 |
| `tests/incubation_execution_flow.rs` | 修改 | P1 集成测试 |
