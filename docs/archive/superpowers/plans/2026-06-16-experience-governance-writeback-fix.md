> **状态：已归档** — 对应功能已合并到 main，归档于 2026-07-05

# 经验治理写回链路修复计划

基于设计规格 `docs/superpowers/specs/2026-06-16-experience-governance-writeback-fix-design.md`。

## 实施步骤

### Step 1: `AgentConfig`/`AgentEntry` 添加 `Serialize`

**文件**: `src/domain/mod.rs:148-161`

当前 derive 只有 `Deserialize`，P1 的 `IncubatedAgentRegistry` 重写需要 `Serialize` 才能
用 `toml::to_string` 写回 `agents.toml`。

**变更**:
- `AgentConfig` derive 添加 `Serialize`
- `AgentEntry` derive 添加 `Serialize`
- 在 `use serde::Deserialize;` 处改为 `use serde::{Deserialize, Serialize};`

### Step 2: `spawn_experience_confirmation()` 创建配对 `ToolExecutionRequestMessage`

**文件**: `src/systems/contribution.rs:742-772`

**根因**: 当前只 spawn `ToolConfirmationRequestMessage`，不创建配对的
`ToolExecutionRequestMessage`。`tool_confirmation_result_system` 通过
`pending_confirmation_id` 查找执行请求，找不到则销毁响应实体，导致
`experience_approval_result_system` 永远看不到响应。

**变更**: 在 `spawn_experience_confirmation()` 末尾追加 spawn 一个占位
`ToolExecutionRequestMessage`，设置 `pending_confirmation_id = Some(request_id)`。
占位实体的 `tool_name` 为 `"experience_governance"`，`request` 字段使用
`AgentExecutionRequest` 的最小构造（只需 `task_id` 和 `agent_id`，从
`ExperienceGovernanceRequestMessage` 获取），`tool_call_id` 为 `None`，
`pending_confirmation_options` 为 `Some(ConfirmationOption::default_options())`。

### Step 3: `tool_confirmation_result_system` 对 `experience_governance` 特判

**文件**: `src/systems/tools/confirmation.rs:49-215`

在找到匹配的 `ToolExecutionRequestMessage` 之后、正常执行逻辑之前，插入特判分支：

```rust
// experience_governance 特判：销毁执行占位实体，不执行工具，不销毁响应
if tool_request.tool_name == "experience_governance" {
    debug!(
        event = "ExperienceGovernanceConfirmationSkipped",
        request_id = %response.request_id,
        "experience_governance confirmation handled by dedicated system"
    );
    commands.entity(request_entity).despawn();
    // 不 despawn response entity，留给 experience_approval_result_system
    continue;
}
```

这一步确保：
- 占位 `ToolExecutionRequestMessage` 被清理
- `ToolConfirmationResponseMessage` 保留，供 `experience_approval_result_system` 读取
- 不触发工具执行逻辑
- 输出 `ExperienceGovernanceConfirmationSkipped` debug 日志，保持审计链路可追踪

### Step 4: 显式声明系统执行顺序

**文件**: `src/plugins/execution.rs:57`

**变更**: `experience_approval_result_system` 添加 `.after(tool_confirmation_result_system)`，
确保同一帧内 `tool_confirmation_result_system` 先处理完（特判保留响应实体），
然后 `experience_approval_result_system` 再消费该响应。

**验证**: 完成后运行 `cargo check` 确认 P0 改动编译通过。

### Step 5: 重写 `IncubatedAgentRegistry`

**文件**: `src/infrastructure/incubation/agent_registry.rs`

废弃独立 JSON 文件存储，改为向 `agents.toml` 追加 TOML 格式的 `[[agent]]` 条目。

**结构体变更**: `IncubatedAgentRegistry` 改为零字段 unit struct（`pub struct IncubatedAgentRegistry;`），
仅作为 Bevy Resource 类型标记。删除 `path: PathBuf` 字段（`config_path` 从参数传入后冗余），
删除 `new(PathBuf)` 构造器，删除 `default_path()`、`load()`、`save()` 方法。
`src/plugins/memory.rs:28` 的 `IncubatedAgentRegistry::default_path()` 初始化改为
`IncubatedAgentRegistry`。

**变更**:
- `append()` 方法签名改为 `append(&self, config_path: &str, record: &IncubatedAgentRecord)`
- 逻辑：
  1. 读取 `config_path` 指向的 `agents.toml`
  2. 用 `toml::from_str` 解析为 `AgentConfig`
  3. 按 `name` 去重：若同名 Agent 已存在则返回 `Ok(())`
  4. 构造 `AgentEntry`（从 `IncubatedAgentRecord` 映射），追加到 `AgentConfig.agent`
  5. 用 `toml::to_string` 序列化
  6. 原子写回：先写 `{config_path}.tmp`，再 `fs::rename` 覆盖
- 删除 `load()` 和 `save()` 方法（不再需要内部 JSON 存储）
- 删除 `default_path()` 方法和 `new(PathBuf)` 构造器
- `IncubatedAgentRecord` 保留但标记为内部转换结构

**TOML roundtrip 回归测试**: 添加单元测试验证 `AgentConfig`（含 `AgentToolsConfig`
的 `#[serde(flatten)]` + `HashMap`）经 `toml::to_string` → `toml::from_str` roundtrip
正确。虽然 `toml 0.8` 实测 `#[serde(flatten)]` roundtrip 正常，此测试作为回归保护，
确保未来 `AgentToolsConfig` 变更不会破坏写回。

### Step 6: `writeback_incubation_proposal` 去重与状态推进

**文件**: `src/systems/contribution.rs:704-736`

**变更**:
1. 函数签名添加 `config_path: &str` 参数（从 `HarnessSettings` 传入）
2. 写回前检查 proposal 状态：
   - 若已 `Executed`，直接返回 `Ok(())`（防止多候选重复触发）
   - 将 proposal 状态设为 `Executing`
3. 调用 `agent_registry.append(config_path, &record)` 替换旧调用
4. 写回成功后：proposal 状态设为 `Executed`，更新 `updated_at`
5. 写回失败时：proposal 状态设为 `ExecutionFailed`

**调用点适配**: `experience_writeback_system` (contribution.rs:510) 中
`IncubationProposal` 分支需要从 `HarnessSettings` 获取 `agents_config_path`
传入。传递链：
1. `experience_writeback_system` 添加 `settings: Res<HarnessSettings>` 参数
2. 在 `IncubationProposal` 分支取 `&settings.agents_config_path`
3. 将 `config_path: &str` 传入 `writeback_incubation_proposal()`

**验证**: 完成后运行 `cargo check` 确认 P1 改动编译通过。

### Step 7: 审计事件补齐

**文件**: `src/systems/contribution.rs`

在以下位置添加 `tracing` 事件：

| 位置 | 事件名 | 级别 |
|---|---|---|
| `merge_into_proposal` 成功后 | `IncubationProposalMerged` | DEBUG |
| `writeback_incubation_proposal` 开始 | `IncubationExecutionStarted` | DEBUG |
| `writeback_incubation_proposal` 成功 | `IncubationExecutionSucceeded` | DEBUG |
| `writeback_incubation_proposal` 失败 | `IncubationExecutionFailed` | WARN |

`merge_into_proposal` 在 `src/domain/contribution.rs` 的 `ExperienceStore::merge_into_proposal`
方法中，需在该方法末尾添加 debug! 日志。

### Step 8: P0 集成测试

**文件**: `tests/experience_layered_governance_flow.rs`

> Spec 原文将 P0 测试标注在 `experience_collection_workitem_flow.rs`，但 P0 测试
> 验证的是审批→写回链路，属于治理（governance）范畴，放在已有治理基础设施的
> `experience_layered_governance_flow.rs` 中更合理。已回更 spec 保持一致。

新增两个测试：

1. `experience_governance_confirmation_preserves_response_for_approval_system`:
   - 创建配对实体后，`tool_confirmation_result_system` 运行不销毁响应实体
   - 验证 `ToolConfirmationResponseMessage` 仍然存在

2. `approved_candidate_spawns_writeback_request`:
   - 模拟审批通过
   - 验证 `ExperienceWritebackRequestMessage` 被创建

### Step 9: P1 集成测试

**文件**: `tests/incubation_execution_flow.rs`

重写现有测试以适配新的 `IncubatedAgentRegistry` API：

1. `incubated_agent_appended_to_agents_toml`:
   - 创建临时 `agents.toml`，追加孵化 Agent
   - 验证文件包含新 `[[agent]]` 条目且原有条目不变

2. `duplicate_incubation_skips_if_name_exists`:
   - 同名 Agent 不重复追加

3. `proposal_status_advances_to_executed`:
   - 写回成功后 proposal 状态为 `Executed`

### Step 10: 回归验证

- 确保现有 `experience_collection_workitem_flow` 测试通过
- 确保现有 `experience_layered_governance_flow` 测试通过
- 全量 `cargo test` 通过
- `cargo clippy` 通过
- `cargo fmt` 通过

## 执行顺序与依赖

```
Step 1 (AgentConfig Serialize)
  └─> Step 5 (IncubatedAgentRegistry 重写)
       └─> Step 6 (writeback 去重与状态推进)

Step 2 (配对 ToolExecutionRequestMessage)
  └─> Step 3 (特判分支)
       └─> Step 4 (执行顺序声明)

Step 7 (审计事件) — 独立

Step 8 (P0 测试) — 依赖 Step 2-4
Step 9 (P1 测试) — 依赖 Step 1, 5, 6
Step 10 (回归) — 依赖所有步骤
```

P0（Step 2-4）和 P1（Step 1, 5-6）可并行开发，但 Step 5 依赖 Step 1。
建议按 P0 → P1 顺序实施，减少上下文切换。

## 文件变更清单

| 文件 | 变更类型 | 步骤 |
|---|---|---|
| `src/domain/mod.rs` | 修改 | Step 1 |
| `src/domain/contribution.rs` | 修改 | Step 7 |
| `src/systems/contribution.rs` | 修改 | Step 2, 6, 7 |
| `src/systems/tools/confirmation.rs` | 修改 | Step 3 |
| `src/plugins/execution.rs` | 修改 | Step 4 |
| `src/infrastructure/incubation/agent_registry.rs` | 重写 | Step 5 |
| `src/plugins/memory.rs` | 修改 | Step 5 |
| `tests/experience_layered_governance_flow.rs` | 修改 | Step 8 |
| `tests/incubation_execution_flow.rs` | 修改 | Step 9 |
