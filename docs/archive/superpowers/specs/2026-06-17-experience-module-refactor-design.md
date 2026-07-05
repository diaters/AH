> **状态：已归档** — 对应功能已合并到 main，归档于 2026-07-05

# 经验治理模块重构设计

> **状态：当前有效**

## 背景与目标

当前 `src/systems/contribution.rs` 同时承载了经验治理主链路、旧版记忆贡献/吸收链路以及记忆压缩辅助函数，导致：

- 文件职责混杂，新功能难以定位边界
- 旧链路 `memory_contribution_system` / `memory_absorption_system` 已无上游调用，却仍占用插件注册和类型定义
- 顶层治理、审批、写回、孵化逻辑堆叠在一个文件中，不利于后续叠加 LLM 治理、组合重写、孵化资产写入等能力

本次重构目标是：**结构优雅、机制可靠**——先把旧链路和结构债务清理干净，再基于清晰模块继续演进。

## 当前问题

1. **旧链路冗余**
   - `MemoryContributionRequestMessage`、`MemoryAbsorptionMessage`、`MemoryWritebackBatch` 已无运行时生成方
   - `memory_contribution_system` / `memory_absorption_system` 仅作为过渡态保留在 `MemoryPlugin` / `ExecutionPlugin` 中
   - `extract_memory_writebacks` 仅在旧系统和自身测试中使用

2. **单文件过载**
   - `src/systems/contribution.rs` 同时包含：
     - 经验收集触发
     - 候选汇聚
     - 顶层治理分流
     - 审批结果处理
     - 统一写回执行
     - 孵化提案执行
     - 旧记忆贡献/吸收逻辑

3. **可扩展性差**
   - 新增 LLM 治理复审、子候选组合重写、共享知识自动审核等功能时，缺乏清晰的接入点

## 方案选择

### 方案一：激进重构（推荐）

一步到位：彻底删除旧链路，同时新建 `src/systems/experience/` 模块，按生命周期拆分为多个聚焦文件。

**优点：**
- 结构最清晰，后续功能有明确归属
- 一次提交完成"拆旧建新"，无中间态

**缺点：**
- 单 PR 改动大，review 成本高
- 文件移动可能导致 git diff 识别为删除+新增

### 方案二：保守两步走

第一步在同一文件中删除旧链路；第二步稳定后再拆分文件。

**优点：**
- 风险低，每步可独立验证
- 便于 review 和回滚

**缺点：**
- 周期长，中间状态仍是大文件
- 需要两次上下文切换

### 方案三：最小化拆分

删除旧链路后，在 `src/systems/` 根目录下创建 `experience_collection.rs` 等平铺文件，不新建子目录。

**优点：**
- 目录改动小，与现有风格一致
- 改动量介于方案一和方案二之间

**缺点：**
- 文件名冗长
- 无法显式表达经验治理子系统的聚合关系
- 未来扩展时根目录拥挤

**结论：选择方案一。** 当前主链路已稳定运行，测试覆盖充分，适合一次性完成结构升级。

## 模块划分

新建 `src/systems/experience/` 目录：

```text
src/systems/experience/
├── mod.rs           # 公开导出本模块内的系统函数
├── collection.rs    # 经验收集：任务终态触发 + WorkItem 创建
├── governance.rs    # 经验治理：顶层唯一分流点
├── approval.rs      # 审批结果：用户确认后推进到写回
└── writeback.rs     # 统一写回：四种去向 + 孵化执行
```

### 各模块职责

| 模块 | 包含系统 | 职责边界 |
| --- | --- | --- |
| `collection.rs` | `task_terminated_experience_trigger_system`<br>`experience_collection_workitem_system`<br>`experience_collection_completion_system` | 只负责把任务终态转换为经验收集 WorkItem，汇总候选 |
| `governance.rs` | `experience_governance_system` | 只负责根据候选 `kind_hint`、风险等级、治理者类型决定最终去向 |
| `approval.rs` | `experience_approval_result_system` | 只负责把用户确认结果映射到候选状态，并生成写回请求 |
| `writeback.rs` | `experience_writeback_system`<br>四个 `writeback_to_*` 辅助函数 | 只负责按治理决议执行持久化，并推进候选状态 |

## 数据流

重构后运行时数据流与当前保持一致：

```text
Task 进入终态
  -> task_terminated_experience_trigger_system
     生成 ExperienceCollectionRequestMessage

ExperienceCollectionRequestMessage
  -> experience_collection_workitem_system
     生成 ExperienceCollection WorkItem

WorkItem 执行完成 + Agent 调用 submit_experience_candidate
  -> 候选写入 ExperienceInbox（子任务）或 root_candidates（顶层）

ExperienceCollectionCompletedMessage
  -> experience_collection_completion_system
     子候选标记 Aggregated，顶层候选推进 GovernancePending
     生成 ExperienceGovernanceRequestMessage

ExperienceGovernanceRequestMessage
  -> experience_governance_system
     产出 ExperienceGovernanceDecision
     无需确认 -> 直接生成 ExperienceWritebackRequestMessage
     需要确认 -> 生成 ToolConfirmationRequestMessage + 占位 ToolExecutionRequestMessage

用户确认
  -> tool_confirmation_result_system
     对 experience_governance 特判：销毁占位执行请求，保留响应实体
  -> experience_approval_result_system
     候选状态 Approved -> 生成 ExperienceWritebackRequestMessage

ExperienceWritebackRequestMessage
  -> experience_writeback_system
     按 destination 调用 LongTermMemoryService / AgentAssetService /
        SharedKnowledgeUpgradeService / IncubationProposalStore
```

## 删除清单

- `src/systems/contribution.rs` 整体删除
- domain 中删除：
  - `MemoryContributionRequestMessage`
  - `MemoryAbsorptionMessage`
  - `MemoryWritebackBatch`
- `extract_memory_writebacks` 函数及导出
- `memory_contribution_system` / `memory_absorption_system`
- `src/plugins/memory.rs` 和 `src/plugins/execution.rs` 中旧系统注册

## 辅助函数归属

| 辅助函数 | 归属模块 | 可见性 |
| --- | --- | --- |
| `is_default_agent` | `experience::governance` | `pub(crate)` |
| `spawn_experience_confirmation` | `experience::governance` | 私有 |
| `spawn_incubation_confirmation` | `experience::governance` | 私有 |
| `writeback_to_long_term_memory` | `experience::writeback` | 私有 |
| `writeback_to_skill_package` | `experience::writeback` | 私有 |
| `writeback_to_shared_knowledge_upgrade` | `experience::writeback` | 私有 |
| `writeback_incubation_proposal` | `experience::writeback` | 私有 |

## 插件注册

在 `src/plugins/execution.rs` 中，经验治理相关系统仍全部放在 `HarnessSet::Execution`，相对顺序保持不变：

```rust
task_terminated_experience_trigger_system.in_set(HarnessSet::Execution),
experience_collection_workitem_system
    .in_set(HarnessSet::Execution)
    .after(task_terminated_experience_trigger_system),

experience_collection_completion_system
    .in_set(HarnessSet::Execution)
    .after(crate::systems::llm_response_system)
    .before(experience_governance_system),

experience_governance_system
    .in_set(HarnessSet::Execution)
    .after(experience_collection_completion_system),

experience_approval_result_system
    .in_set(HarnessSet::Execution)
    .after(crate::systems::tool_confirmation_result_system)
    .after(experience_governance_system)
    .before(experience_writeback_system),

experience_writeback_system
    .in_set(HarnessSet::Execution)
    .after(experience_governance_system),
```

`src/plugins/memory.rs` 中移除 `memory_contribution_system` 和 `memory_absorption_system` 注册，保留记忆压缩、初始化、衰退治理。

## 错误处理

保持现有策略：

- 写回失败 -> 候选状态 `WritebackFailed`
- 孵化执行失败 -> proposal 状态 `ExecutionFailed`
- 保留 `warn` 级审计日志
- 不新增错误类型或重试机制

## 测试策略

### 需要修改的测试

- `tests/multi_turn_flow.rs`：移除对 `MemoryContributionRequestMessage` / `MemoryAbsorptionMessage` 的查询断言
- `tests/memory_persistence_flow.rs`：删除 `extract_memory_writebacks_filters_correctly` 测试

### 保持不变的测试

- `tests/experience_candidate_flow.rs`
- `tests/experience_collection_workitem_flow.rs`
- `tests/experience_layered_governance_flow.rs`
- `tests/incubation_execution_flow.rs`

### 验证命令

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```

## 实施步骤

单 PR 内分 5 个 commit：

1. **删除旧链路**：消息类型、系统函数、`extract_memory_writebacks`、插件注册、相关测试
2. **创建 experience 模块骨架**：新建 `mod.rs` 和四个子模块文件
3. **迁移 collection 与 governance**：移动经验收集、汇聚、治理系统
4. **迁移 approval 与 writeback**：移动审批、统一写回、孵化执行
5. **清理与验证**：删除 `contribution.rs`，运行 fmt/clippy/test

## 风险与回滚

- **git diff 可读性差**：通过分 commit 缓解
- **测试遗漏**：每 commit 后运行 `cargo test`
- **插件顺序错误**：严格保留 `.before`/`.after` 关系
- **回滚**：可直接 revert 整个 PR，功能整体回退到重构前状态

## 后续方向

重构完成后，可在清晰模块上继续叠加：

- 顶层治理引入 LLM 复审/修正 `kind_hint`
- 非顶层基于多个子候选的组合重写
- 共享知识升级候选的自动 LLM 审核
- 孵化批准后写入新 Agent 初始 LTM 和 Skill Package
