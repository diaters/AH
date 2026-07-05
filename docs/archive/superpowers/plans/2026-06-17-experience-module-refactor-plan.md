> **状态：已归档** — 对应功能已合并到 main，归档于 2026-07-05

# 经验治理模块重构实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 彻底删除旧版记忆贡献/吸收链路，将 `src/systems/contribution.rs` 拆分为 `src/systems/experience/` 下的聚焦模块，零运行时行为变化。

**Architecture:** 按经验生命周期拆分：collection（收集触发）→ governance（治理分流）→ approval（用户确认）→ writeback（统一写回）。保留现有消息类型、系统执行顺序、错误处理策略和持久化逻辑。

**Tech Stack:** Rust, Bevy ECS, genai, ratatui

---

## 文件结构

### 新建文件

- `src/systems/experience/mod.rs` — 模块导出
- `src/systems/experience/collection.rs` — 经验收集相关系统
- `src/systems/experience/governance.rs` — 顶层治理 + 确认生成辅助函数
- `src/systems/experience/approval.rs` — 审批结果处理
- `src/systems/experience/writeback.rs` — 统一写回 + 孵化执行

### 修改文件

- `src/domain/contribution.rs` — 删除旧消息类型
- `src/systems/contribution.rs` — 整体删除
- `src/systems/mod.rs` — 更新导出
- `src/lib.rs` — 删除 `extract_memory_writebacks` 公开导出
- `src/plugins/execution.rs` — 移除旧系统注册
- `src/plugins/memory.rs` — 移除旧系统注册
- `tests/multi_turn_flow.rs` — 移除旧消息类型断言
- `tests/memory_persistence_flow.rs` — 删除旧函数测试

---

## Task 1: 删除旧 domain 消息类型

**Files:**
- Modify: `src/domain/contribution.rs:1-80`

- [ ] **Step 1: 删除三个旧 struct 及其字段**

  删除以下代码：
  ```rust
  pub struct MemoryWritebackBatch { ... }
  pub struct MemoryContributionRequestMessage { ... }
  pub struct MemoryAbsorptionMessage { ... }
  ```

- [ ] **Step 2: 运行 cargo check 确认 domain 层无残留引用**

  Run: `cargo check --lib`
  Expected: 仅 `src/systems/contribution.rs` 和测试中出现未解析引用错误（将在后续任务处理）

- [ ] **Step 3: Commit**

  ```bash
  git add src/domain/contribution.rs
  git commit -m "refactor: remove obsolete memory contribution/absorption message types"
  ```

---

## Task 2: 清理旧系统与导出

**Files:**
- Modify: `src/systems/mod.rs:22` 和 `src/systems/mod.rs:51-59`
- Modify: `src/lib.rs:16`
- Modify: `src/plugins/execution.rs:11` 和 `src/plugins/execution.rs:63`
- Modify: `src/plugins/memory.rs:10` 和 `src/plugins/memory.rs:41`

- [ ] **Step 1: 更新 `src/systems/mod.rs` 导出列表**

  从 `pub use contribution::{...}` 中移除 `memory_absorption_system` 和 `memory_contribution_system`。

  删除 `extract_memory_writebacks` 包装函数：
  ```rust
  pub fn extract_memory_writebacks(...) { ... }
  ```

- [ ] **Step 2: 删除 `src/lib.rs` 公开导出**

  删除：
  ```rust
  pub use systems::extract_memory_writebacks;
  ```

- [ ] **Step 3: 更新 `src/plugins/execution.rs`**

  从 import 中移除 `memory_contribution_system`。
  从 `ExecutionPlugin::build` 的 `app.add_systems(Update, ...)` 调用链中移除：
  ```rust
  memory_contribution_system.in_set(HarnessSet::Execution),
  ```

- [ ] **Step 4: 更新 `src/plugins/memory.rs`**

  从 import 中移除 `memory_absorption_system`。
  从 `Maintenance` set 中移除：
  ```rust
  memory_absorption_system.in_set(HarnessSet::Maintenance),
  ```

- [ ] **Step 5: 运行 cargo check**

  Run: `cargo check`
  Expected: `src/systems/contribution.rs` 中出现 `extract_memory_writebacks`、`memory_contribution_system` 等未定义错误，以及导入的 `MemoryAbsorptionMessage` / `MemoryContributionRequestMessage` 未解析错误

- [ ] **Step 6: Commit**

  ```bash
  git add src/systems/mod.rs src/lib.rs src/plugins/execution.rs src/plugins/memory.rs
  git commit -m "refactor: unregister legacy memory contribution/absorption systems"
  ```

---

## Task 3: 创建 experience 模块骨架

**Files:**
- Create: `src/systems/experience/mod.rs`
- Modify: `src/systems/mod.rs`

- [ ] **Step 1: 创建 `src/systems/experience/mod.rs`**

  ```rust
  pub mod approval;
  pub mod collection;
  pub mod governance;
  pub mod writeback;

  pub use approval::experience_approval_result_system;
  pub use collection::{
      experience_collection_completion_system, experience_collection_workitem_system,
      task_terminated_experience_trigger_system,
  };
  pub use governance::experience_governance_system;
  pub use writeback::experience_writeback_system;
  ```

- [ ] **Step 2: 在 `src/systems/mod.rs` 中注册 experience 模块**

  添加：
  ```rust
  pub mod experience;
  ```

  并更新 `pub use` 为从 `experience` 模块导出：
  ```rust
  pub use experience::{
      experience_approval_result_system, experience_collection_completion_system,
      experience_collection_workitem_system, experience_governance_system,
      experience_writeback_system, task_terminated_experience_trigger_system,
  };
  ```

- [ ] **Step 3: 创建四个空子模块文件**

  ```bash
  touch src/systems/experience/collection.rs
  touch src/systems/experience/governance.rs
  touch src/systems/experience/approval.rs
  touch src/systems/experience/writeback.rs
  ```

  每个文件顶部暂时写入：
  ```rust
  use bevy::prelude::*;
  ```

- [ ] **Step 4: 运行 cargo check**

  Run: `cargo check`
  Expected: 通过编译，无新增错误（旧 `contribution.rs` 的错误仍存在）

- [ ] **Step 5: Commit**

  ```bash
  git add src/systems/experience src/systems/mod.rs
  git commit -m "chore: add experience module skeleton"
  ```

---

## Task 4: 迁移 collection 模块

**Files:**
- Create/Modify: `src/systems/experience/collection.rs`
- Modify: `src/systems/contribution.rs`

- [ ] **Step 1: 移动 `task_terminated_experience_trigger_system`**

  将 `src/systems/contribution.rs:18-59` 完整移动到 `src/systems/experience/collection.rs`。

  在 `collection.rs` 顶部添加所需导入：
  ```rust
  use bevy::prelude::*;
  use tracing::debug;

  use crate::domain::{
      ExperienceCollectionRequestMessage, Task, TaskTerminatedMessage,
  };
  ```

- [ ] **Step 2: 移动 `experience_collection_workitem_system` 与 `build_experience_collection_conversation`**

  将 `src/systems/contribution.rs:61-166`（含 `experience_collection_workitem_system` 和 `build_experience_collection_conversation`）完整移动到 `src/systems/experience/collection.rs`。

  追加导入：
  ```rust
  use crate::domain::{
      ConversationMessage, EntryRole, ShortTermMemory, SpaceToolRegistry, Task, WorkItem,
  };
  ```

- [ ] **Step 3: 移动 `experience_collection_completion_system`**

  将 `src/systems/contribution.rs:282-319` 完整移动到 `src/systems/experience/collection.rs`。

  追加导入：
  ```rust
  use crate::domain::{
      ExperienceCollectionCompletedMessage, ExperienceGovernanceRequestMessage, ExperienceStore,
  };
  ```

- [ ] **Step 4: 从 `src/systems/contribution.rs` 删除已移动代码**

  删除行号范围 `18-319` 的内容，保留剩余代码（governance、writeback、approval、旧系统）。

- [ ] **Step 5: 运行 cargo check**

  Run: `cargo check --lib`
  Expected: `collection.rs` 编译通过；`contribution.rs` 因重复定义消失、但剩余代码仍可编译

- [ ] **Step 6: Commit**

  ```bash
  git add src/systems/experience/collection.rs src/systems/contribution.rs
  git commit -m "refactor: move experience collection systems to experience/collection.rs"
  ```

---

## Task 5: 迁移 governance 模块

**Files:**
- Create/Modify: `src/systems/experience/governance.rs`
- Modify: `src/systems/contribution.rs`

- [ ] **Step 1: 移动 `experience_governance_system` 与辅助函数**

  将以下范围从 `src/systems/contribution.rs` 移动到 `src/systems/experience/governance.rs`：
  - `experience_governance_system`: `321-512`
  - `is_default_agent`: `825-827`
  - `spawn_experience_confirmation`: `829-883`
  - `spawn_incubation_confirmation`: `885-910`

  在 `governance.rs` 顶部添加导入：
  ```rust
  use bevy::prelude::*;
  use tracing::debug;

  use crate::domain::{
      Agent, AgentExecutionRequest, AgentRequestKind, ConfirmationOption, ConfirmationSource,
      ExperienceCandidate, ExperienceCandidateStatus, ExperienceConfirmationPolicy,
      ExperienceGovernanceDecision, ExperienceGovernanceRequestMessage, ExperienceKindHint,
      ExperienceRiskLevel, ExperienceStore, ExperienceWritebackDestination,
      ExperienceWritebackRequestMessage, IncubationProposalStatus, ToolConfirmationRequestMessage,
      ToolExecutionRequestMessage,
  };
  ```

- [ ] **Step 2: 从 `src/systems/contribution.rs` 删除已移动代码**

  删除 `321-512` 和 `825-910` 范围内容。

- [ ] **Step 3: 运行 cargo check**

  Run: `cargo check --lib`
  Expected: `governance.rs` 编译通过

- [ ] **Step 4: Commit**

  ```bash
  git add src/systems/experience/governance.rs src/systems/contribution.rs
  git commit -m "refactor: move experience governance systems to experience/governance.rs"
  ```

---

## Task 6: 迁移 writeback 模块

**Files:**
- Create/Modify: `src/systems/experience/writeback.rs`
- Modify: `src/systems/contribution.rs`

- [ ] **Step 1: 移动 `experience_writeback_system` 与四个写回辅助函数**

  将以下范围从 `src/systems/contribution.rs` 移动到 `src/systems/experience/writeback.rs`：
  - `experience_writeback_system`: `514-616`
  - `writeback_to_long_term_memory`: `618-652`
  - `writeback_to_skill_package`: `654-692`
  - `writeback_to_shared_knowledge_upgrade`: `694-715`
  - `writeback_incubation_proposal`: `717-823`

  在 `writeback.rs` 顶部添加导入：
  ```rust
  use bevy::prelude::*;
  use tracing::{debug, warn};

  use crate::domain::{
      ExperienceCandidate, ExperienceCandidateStatus, ExperienceGovernanceDecision,
      ExperienceStore, ExperienceWritebackDestination, ExperienceWritebackRequestMessage,
      IncubationProposalStatus, LongTermMemory, SharedKnowledgeUpgradeQueue,
  };
  use crate::infrastructure::memory::LongTermMemoryService;
  ```

- [ ] **Step 2: 从 `src/systems/contribution.rs` 删除已移动代码**

  删除 `514-823` 范围内容。

- [ ] **Step 3: 运行 cargo check**

  Run: `cargo check --lib`
  Expected: `writeback.rs` 编译通过；`contribution.rs` 仅剩 `experience_approval_result_system` 和测试

- [ ] **Step 4: Commit**

  ```bash
  git add src/systems/experience/writeback.rs src/systems/contribution.rs
  git commit -m "refactor: move experience writeback systems to experience/writeback.rs"
  ```

---

## Task 7: 迁移 approval 模块

**Files:**
- Create/Modify: `src/systems/experience/approval.rs`
- Modify: `src/systems/contribution.rs`

- [ ] **Step 1: 移动 `experience_approval_result_system`**

  将 `src/systems/contribution.rs:912-1067` 完整移动到 `src/systems/experience/approval.rs`。

  在 `approval.rs` 顶部添加导入：
  ```rust
  use bevy::prelude::*;
  use tracing::debug;

  use crate::domain::{
      ExperienceCandidateStatus, ExperienceGovernanceDecision,
      ExperienceWritebackDestination, ExperienceWritebackRequestMessage,
      ExperienceStore, IncubationProposalStatus, ToolConfirmationResponseMessage,
  };
  ```

- [ ] **Step 2: 从 `src/systems/contribution.rs` 删除已移动代码**

  删除 `912-1067` 范围内容。

- [ ] **Step 3: 运行 cargo check**

  Run: `cargo check --lib`
  Expected: `approval.rs` 编译通过

- [ ] **Step 4: Commit**

  ```bash
  git add src/systems/experience/approval.rs src/systems/contribution.rs
  git commit -m "refactor: move experience approval system to experience/approval.rs"
  ```

---

## Task 8: 迁移有效单元测试

**Files:**
- Modify: `src/systems/experience/collection.rs`
- Modify: `src/systems/experience/governance.rs`
- Modify: `src/systems/experience/approval.rs`
- Modify: `src/systems/contribution.rs`

- [ ] **Step 1: 从 `contribution.rs` 移动有效测试到对应模块**

  - `is_default_agent_detects_by_tag_not_name` → `src/systems/experience/governance.rs` 底部 `#[cfg(test)]` 模块
  - `task_scoped_agent_termination_builds_request_with_governing_agent` → `src/systems/experience/collection.rs` 底部
  - `experience_collection_completion_aggregates_child_candidates` → `src/systems/experience/collection.rs` 底部
  - `approved_executable_becomes_persisted` → `src/systems/experience/approval.rs` 底部

  删除旧测试 `memory_contribution_skips_low_value_entries_and_creates_candidates`（依赖已删除的 `extract_memory_writebacks`）。

- [ ] **Step 2: 更新每个测试模块的导入**

  每个 `#[cfg(test)] mod tests` 块使用 `use super::*;`，并补充所需类型：
  - collection 测试：`use crate::domain::{ExperienceStore, TaskId};`
  - governance 测试：`use crate::domain::Agent;`
  - approval 测试：`use crate::domain::{ExperienceCandidate, ExperienceCandidatePayload, ExperienceCandidateStatus, ExperienceKindHint};`

- [ ] **Step 3: 运行 cargo test**

  Run: `cargo test --lib`
  Expected: 上述四个迁移后的单元测试通过

- [ ] **Step 4: Commit**

  ```bash
  git add src/systems/experience/collection.rs src/systems/experience/governance.rs src/systems/experience/approval.rs src/systems/contribution.rs
  git commit -m "refactor: migrate experience unit tests to new modules"
  ```

---

## Task 9: 删除 `src/systems/contribution.rs`

**Files:**
- Delete: `src/systems/contribution.rs`

- [ ] **Step 1: 确认文件已为空或仅剩旧系统代码**

  此时 `contribution.rs` 应仅剩：
  - 顶部 import（大量已失效）
  - 旧 `memory_contribution_system`（`168-203`）
  - 旧 `extract_memory_writebacks`（`205-235`）
  - 旧 `memory_absorption_system`（`237-280`）

- [ ] **Step 2: 删除文件**

  ```bash
  git rm src/systems/contribution.rs
  ```

- [ ] **Step 3: 运行 cargo check**

  Run: `cargo check --lib`
  Expected: 通过编译，无 contribution 模块引用错误

- [ ] **Step 4: Commit**

  ```bash
  git commit -m "refactor: remove src/systems/contribution.rs"
  ```

---

## Task 10: 修复集成测试

**Files:**
- Modify: `tests/multi_turn_flow.rs:332-398`
- Modify: `tests/memory_persistence_flow.rs:9,139-172`

- [ ] **Step 1: 修改 `tests/multi_turn_flow.rs`**

  将以下断言块：
  ```rust
  // 1. MemoryContributionRequestMessage was generated, or
  // 2. MemoryAbsorptionMessage was generated (contribution processed), or
  let contribution_query = app
      .world_mut()
      .query::<&harness::MemoryContributionRequestMessage>();
  let absorption_query = app
      .world_mut()
      .query::<&harness::MemoryAbsorptionMessage>();
  ```
  替换为验证经验收集 WorkItem 已创建或候选已提交的断言。

  删除：
  ```rust
  harness::extract_memory_writebacks("child", &summary, &child_memory.entries);
  ```

  具体替换代码需根据 `multi_turn_flow.rs` 当前上下文决定，最小改动为：
  ```rust
  // 经验治理链路：验证子 Agent 终止后触发了 ExperienceCollectionRequestMessage
  let collection_requests = app
      .world_mut()
      .query::<&harness::ExperienceCollectionRequestMessage>()
      .iter(app.world())
      .collect::<Vec<_>>();
  assert!(
      !collection_requests.is_empty(),
      "child agent termination should trigger experience collection"
  );
  ```

- [ ] **Step 2: 修改 `tests/memory_persistence_flow.rs`**

  删除 import：
  ```rust
  extract_memory_writebacks,
  ```

  删除整个测试函数：
  ```rust
  #[test]
  fn extract_memory_writebacks_filters_correctly() { ... }
  ```

- [ ] **Step 3: 运行相关集成测试**

  Run:
  ```bash
  cargo test --test multi_turn_flow
  cargo test --test memory_persistence_flow
  ```
  Expected: 均通过

- [ ] **Step 4: Commit**

  ```bash
  git add tests/multi_turn_flow.rs tests/memory_persistence_flow.rs
  git commit -m "test: remove assertions for deleted memory contribution pipeline"
  ```

---

## Task 11: 最终验证与清理

**Files:**
- Modify: 任何残留的 clippy/format 问题

- [ ] **Step 1: 运行格式化检查**

  Run: `cargo fmt --all --check`
  Expected: 无格式化问题

- [ ] **Step 2: 运行 clippy**

  Run: `cargo clippy --all-targets --all-features -- -D warnings`
  Expected: 无警告

- [ ] **Step 3: 运行完整测试套件**

  Run: `cargo test --all-features`
  Expected: 全部通过

- [ ] **Step 4: 检查残留引用**

  Run:
  ```bash
  rg "MemoryContributionRequestMessage|MemoryAbsorptionMessage|extract_memory_writebacks|memory_contribution_system|memory_absorption_system" src tests
  ```
  Expected: 无任何匹配（除文档外）

- [ ] **Step 5: Commit 任何清理改动**

  ```bash
  git add -A
  git commit -m "chore: final cleanup and formatting for experience module refactor"
  ```

---

## 自检清单

| 设计文档要求 | 对应任务 |
| --- | --- |
| 删除旧消息类型 | Task 1 |
| 删除旧系统函数与导出 | Task 2 |
| 新建 `src/systems/experience/` 模块 | Task 3 |
| 拆分为 collection/governance/approval/writeback | Task 4-7 |
| 保留运行时数据流和系统顺序 | Task 4-7（严格按原代码移动） |
| 保留错误处理策略 | Task 4-7（不修改逻辑） |
| 迁移有效单元测试 | Task 8 |
| 删除 `contribution.rs` | Task 9 |
| 修复集成测试 | Task 10 |
| 通过 fmt/clippy/test | Task 11 |

## 执行交接

计划已保存到 `docs/superpowers/plans/2026-06-17-experience-module-refactor-plan.md`。

**两个执行选项：**

1. **Subagent-Driven（推荐）** — 为每个 Task 分派独立子代理，逐任务审查，适合确保大型重构不引入回归。
2. **Inline Execution** — 在本会话中使用 `executing-plans` 批量执行，适合快速推进。

请选择执行方式。
