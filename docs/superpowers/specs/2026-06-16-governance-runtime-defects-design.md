# 经验治理模块运行时缺陷修复设计

## 问题背景

运行时日志 (`harness_2026-06-16_22-38-05.jsonl`) 暴露了经验治理模块的 3 个缺陷。
其中缺陷 1 导致多候选场景下前 N-1 个候选的 IncubationProposal 写回失败。

## 缺陷清单

### D1 — 系统集执行顺序错误（阻塞项）

**现象**：3 个候选均审批通过，前 2 个写回失败 (`no Approved IncubationProposal found`)，
仅最后 1 个成功。

**根因**：`experience_approval_result_system` 在 `HarnessSet::Maintenance` 中，
`experience_writeback_system` 在 `HarnessSet::Execution` 中。系统集链序
`Execution → ... → Maintenance`，导致 writeback 在 approval 之前执行。
审批通过后同一帧内 writeback 看不到 `Approved` proposal。

**日志证据**：

```text
14:38:59.423 ExperienceApprovalResolved    cid=3ea353b7 dest=IncubationProposal
14:38:59.451 ExperienceWritebackStarted    cid=3ea353b7 dest=IncubationProposal
14:38:59.451 ExperienceWritebackFailed     error="no Approved IncubationProposal found"
# 同一帧内 Execution 先于 Maintenance 执行，approval 设置 Approved 的动作还未发生
```

### D2 — 多候选重复写回请求

**现象**：同一 proposal 的 3 个候选各自审批后各生成一个
`ExperienceWritebackRequestMessage`，导致 `writeback_incubation_proposal` 被调用 3 次。

**根因**：`experience_approval_result_system` 为每个审批通过的候选独立生成写回请求，
未检查同一 proposal 是否已有写回请求在途。

**影响**：冗余的 `agents.toml` 读-改-写操作（去重保护使其不会重复追加 Agent 记录），
以及多余的 proposal 持久化调用。

### D3 — Deny 选项描述错误

**现象**：TUI 推送的审批选项中 Deny 的 description 为 "仅本次允许"，与 allow_once 重复。

**根因**：`frontend_output.rs` 中选项描述按 `GrantMode` 映射，Deny 的 mode 为 `Once`，
与 allow_once 的描述相同。

## 修复设计

### D1: 调整 experience_approval_result_system 系统集归属

将 `experience_approval_result_system` 从 `HarnessSet::Maintenance` 移到
`HarnessSet::Execution`，排在 `experience_writeback_system` 之前。

这样经验模块在 Execution set 内形成完整链路：

```text
governance → approval_result → writeback
```

变更：

- `src/plugins/execution.rs`：
  - `experience_approval_result_system.in_set(HarnessSet::Maintenance)` 改为
    `.in_set(HarnessSet::Execution)`
  - 添加 `.after(experience_governance_system)`
  - 添加 `.before(experience_writeback_system)`
  - 保留 `.after(tool_confirmation_result_system)` 跨集约束

### D2: 审批源头去重

在 `experience_approval_result_system` 中，对 `IncubationProposal` 目标的候选做源头去重。
在设置 proposal 状态为 `Approved` 之前，检查 proposal 当前状态：

- 若已经是 `Approved`/`Executing`/`Executed`，说明该 proposal 已有写回请求生成，
  跳过 `commands.spawn(ExperienceWritebackRequestMessage)` 和
  `commands.entity(decision_entity).despawn()`
- 仅标记候选状态为 `WritebackPending`

变更：

- `src/systems/contribution.rs`：`experience_approval_result_system` 中，
  IncubationProposal 分支添加状态检查

### D3: 修正 Deny 选项描述

在 `frontend_output.rs` 中，对 `deny` 选项做特判。

变更：

- `src/systems/frontend_output.rs`：选项描述映射中，
  先检查 `opt.id == "deny"` 输出 "拒绝"，再走 `mode` 匹配

## 文件变更清单

| 文件 | 变更类型 | 职责 |
|---|---|---|
| `src/plugins/execution.rs` | 修改 | D1: 系统集归属调整 |
| `src/systems/contribution.rs` | 修改 | D2: 审批源头去重 |
| `src/systems/frontend_output.rs` | 修改 | D3: Deny 描述修正 |
| `tests/experience_layered_governance_flow.rs` | 修改 | D1+D2 回归验证 |

## 测试策略

- 现有 P0 测试 `experience_governance_confirmation_skips_tool_execution` 和
  `approved_candidate_spawns_writeback_request` 继续通过
- 现有 P1 测试继续通过
- 全量 `cargo test` 通过
- `cargo clippy` + `cargo fmt` 通过

## 边界声明

以下内容不纳入本次修复：

- LLM 前置评估
- 可编辑确认
- 审批通道独立
