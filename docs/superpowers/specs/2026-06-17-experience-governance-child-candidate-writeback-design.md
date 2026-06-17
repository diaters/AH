> **状态：当前有效**

# 经验治理子任务候选写回修复设计

## 问题背景

运行时日志 `harness_2026-06-17_09-37-09.jsonl` 暴露：当父任务分解为子任务后，子任务产生的经验候选在审批写回阶段失败。

具体表现为：

- 4 个候选均审批通过
- 首个子任务候选写回失败：`ExperienceWritebackFailed`，`error="no Approved IncubationProposal found"`
- 父任务自身候选写回成功，孵化 Agent 已追加到 `agents.toml`
- 后续子任务候选写回再次失败，错误同上

## 根因分析

### 根因 1：子任务候选按 `producer_task_id` 找不到 proposal

- 子任务候选的 `producer_task_id` 保持为子任务 ID
- 孵化提案按父任务 ID创建（`merge_into_proposal(request.task_id, ...)`）
- `experience_approval_result_system` 使用 `candidate.producer_task_id` 查找 proposal
- 因此子任务候选审批时无法找到对应提案，也不会将其置为 `Approved`

### 根因 2：`writeback_incubation_proposal` 只扫描 `Approved` 状态的提案

```rust
store.proposals.iter().find(|(_, p)| p.status == Approved)
```

- 未绑定 candidate/task ID，使用全局扫描
- 一旦某个候选的写回将提案推进到 `Executing`/`Executed`，后续候选的写回仍查找 `Approved`，找不到则报错
- 即使扫描到 `Executed` 的提案，当前代码只在找到 `Approved` 后才做去重判断，逻辑顺序错误

## 修复目标

1. 审批和写回链路使用统一的任务级 proposal 键，不再依赖候选的 `producer_task_id`
2. `writeback_incubation_proposal` 按指定任务 ID 索引，并对 `Executing`/`Executed` 状态幂等
3. 保留候选 `producer_task_id` 的原始语义，不破坏日志追踪

## 方案选择

### 推荐方案：在 `ExperienceGovernanceDecision` 中显式携带 `source_task_id`

在治理决议中新增 `source_task_id`，审批与写回均按该 ID 定位 proposal。

- 语义清晰：决议产生于某次治理任务，自然携带该任务 ID
- 不影响 `ExperienceCandidate::producer_task_id` 的原始含义
- 改动集中，风险低

## 详细设计

### 变更 1：`ExperienceGovernanceDecision` 增加 `source_task_id`

文件：`src/domain/contribution.rs`

```rust
#[derive(Debug, Clone, Component, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExperienceGovernanceDecision {
    pub candidate_id: uuid::Uuid,
    pub destination: ExperienceWritebackDestination,
    pub confirmation_policy: ExperienceConfirmationPolicy,
    pub final_risk_level: ExperienceRiskLevel,
    pub risk_overridden: bool,
    pub decision_rationale: String,
    pub source_task_id: TaskId, // 新增
}
```

### 变更 2：构造 decision 时填入 `request.task_id`

文件：`src/systems/contribution.rs`（`experience_governance_system`）

在创建 `ExperienceGovernanceDecision` 的所有分支中，统一设置：

```rust
source_task_id: request.task_id,
```

### 变更 3：审批结果系统按 `source_task_id` 定位 proposal

文件：`src/systems/contribution.rs`（`experience_approval_result_system`）

- 将 `candidate.producer_task_id` 替换为 `decision.source_task_id`
- 设置 proposal 为 `Approved` 时使用 `source_task_id`
- 去重检查（`Approved`/`Executing`/`Executed`）同样使用 `source_task_id`

### 变更 4：`writeback_incubation_proposal` 按 task_id 索引并幂等

文件：`src/systems/contribution.rs`

修改函数签名：

```rust
fn writeback_incubation_proposal(
    task_id: TaskId,
    store: &mut ExperienceStore,
    proposal_store: &IncubationProposalStore,
    agent_registry: &IncubatedAgentRegistry,
    config_path: &str,
) -> Result<(), String>
```

行为：

| proposal 状态 | 行为 |
|--------------|------|
| `Approved` | 推进到 `Executing`，持久化 proposal，追加 Agent 到 `agents.toml`，成功后置 `Executed` |
| `Executing` | 返回 `Ok(())`，当前写回在途 |
| `Executed` | 返回 `Ok(())`，已落盘，幂等 |
| `Proposed`/`Rejected`/`ExecutionFailed` | 返回错误，明确说明状态 |

调用点传入 `decision.source_task_id`：

```rust
ExperienceWritebackDestination::IncubationProposal => {
    writeback_incubation_proposal(
        decision.source_task_id,
        &mut store,
        &proposal_store,
        &agent_registry,
        &settings.0.agents_config_path,
    )
}
```

### 变更 5：错误信息精确化

当 proposal 已是 `Executed` 时，不再报 `"no Approved IncubationProposal found"`，而是返回 `"incubation proposal already executed"`。

## 测试策略

在 `tests/experience_layered_governance_flow.rs` 中新增或用例覆盖：

- 父任务分解为 2 个子任务
- 子任务各产生一个 Knowledge 候选
- 父任务自身也产生一个 Knowledge 候选
- 顶层治理时全部路由到 `IncubationProposal`
- 用户依次批准所有候选
- 断言：proposal 最终为 `Executed`，仅追加一次 Agent 记录，所有候选最终为 `Persisted`

## 文件变更清单

| 文件 | 变更类型 | 说明 |
|---|---|---|
| `src/domain/contribution.rs` | 修改 | `ExperienceGovernanceDecision` 新增 `source_task_id` |
| `src/systems/contribution.rs` | 修改 | decision 构造、审批定位、写回函数签名与逻辑 |
| `tests/experience_layered_governance_flow.rs` | 修改/新增 | 覆盖子任务候选 + 多候选审批场景 |

## 边界声明

以下内容不纳入本次最小修复：

- 将 `experience_governance` 从 `ToolConfirmation` 通道独立
- LLM 前置评估候选是否值得孵化
- 孵化 Agent 初始资产写入
- 归档旧的 2026-06-16 设计文档（作为独立文档维护任务）
