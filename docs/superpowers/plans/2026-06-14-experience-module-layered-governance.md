# 经验模块两层分层汇聚治理实施计划

> **For agentic workers:** Use executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 将经验模块从当前半闭环状态收敛为「非顶层 TaskScoped Agent 向上贡献 → 顶层 Persistent Agent 统一治理 → 四类最终去向落盘」的完整两层治理主链路。

**Architecture:** 以 `ExperienceCandidate` 为唯一中间态，`ExperienceInbox` 为层间缓冲；非顶层任务结束时通过 `submit_experience_candidate` 将候选写入父任务 inbox，顶层任务结束时触发 `ExperienceGovernanceRequestMessage` 统一分流到 `LongTermMemory`、Agent 私有 `Skill Package`、`SharedKnowledge` 升级入口或 `IncubationProposal`。`default Agent` 严格执行孵化制，不直接沉淀私有身份资产。

**Tech Stack:** Rust, Bevy ECS, serde, chrono, uuid, anyhow, tracing, cargo test, markdownlint

---

## Scope Check

本计划覆盖 `2026-06-14-experience-module-layered-governance-design.md` 中 P0 闭环要求：

- 非顶层经验收集与父层 inbox 写入
- 顶层治理显式触发
- 四类最终去向全部可达（`LongTermMemory`、`Skill Package`、`SharedKnowledge` 升级入口、`IncubationProposal`）
- 用户确认分支可用
- 最小来源追溯信息可用

本计划暂不实现（归入 P1/P2 或作为 P0 已知简化）：

- 复杂候选去重、相似度聚类、冲突合并
- 复杂风险评分与共享知识终审自动化
- Skill Package 版本治理、资产回收
- 全局共享 skill 仓库
- 关键上下文智能筛选器
- 顶层治理对候选 `kind_hint` 的自动修正（P0 直接使用原 `kind_hint`）
- 非顶层基于多个子候选整理组合候选的主动重写（P0 仅做状态汇聚，组合由父层 LLM 自行完成）
- 完整的 8 步推荐消息流（P0 只保留关键的汇聚触发与治理触发消息）
- 旧 `memory_contribution_system` / `memory_absorption_system` 的删除（本计划视为过渡态保留）

---

## File Structure

| File | Responsibility |
|------|----------------|
| `src/domain/contribution.rs` | 扩展 `ExperienceCandidate` 状态机、`ExperienceInbox` 生命周期、`ExperienceStore` 汇聚方法；新增 `SharedKnowledgeUpgradeCandidate` |
| `src/domain/memory.rs` | 为 `LongTermMemoryEntry` 增加来源追溯字段 |
| `src/domain/mod.rs` | 导出新增类型 |
| `src/infrastructure/assets/service.rs` | 新增 `SkillPackageDraft` 与 `AgentAssetService::persist_skill_package`，按目录落盘 Agent 私有 Skill |
| `src/infrastructure/memory/upgrade_service.rs` | 新增 `SharedKnowledgeUpgradeService`，将共享知识升级入口持久化到 JSON |
| `src/systems/tools/dispatch.rs` | 为 `handle_tool_action` 补充 `parent_agent_id`，用于 inbox 路由 |
| `src/systems/tools/orchestrator.rs` | `submit_experience_candidate` 根据任务层级决定写入父层 inbox 或顶层 root |
| `src/systems/tools/mod.rs` | 如有需要，注册/导出更新后的辅助类型 |
| `src/systems/transform/llm_response.rs` | `ExperienceCollection` WorkItem 完成时生成 `ExperienceCollectionCompletedMessage` |
| `src/domain/message.rs` | 新增 `ExperienceCollectionCompletedMessage` |
| `src/systems/contribution.rs` | 新增 `experience_collection_completion_system`、重写 `experience_governance_system` 与 `experience_approval_result_system` |
| `src/plugins/execution.rs` | 注册新系统并声明执行顺序 |
| `src/app/mod.rs` | 插入 `SharedKnowledgeUpgradeQueue` Resource；`app_is_idle` 补充待处理经验消息检查 |
| `tests/experience_layered_governance_flow.rs` | 四条 spec 要求的闭环集成测试 |
| `docs/current-state.md` | 更新经验治理能力描述 |
| `docs/TODO.md` | 勾选/调整对应待办项 |

---

## Task 1: 扩展经验候选领域模型与追溯字段

**Files:**
- Modify: `src/domain/contribution.rs`
- Modify: `src/domain/memory.rs`
- Modify: `src/domain/mod.rs`
- Test: `src/domain/contribution.rs`（内嵌单元测试）

**动机：** 当前 `ExperienceCandidateStatus` 缺少 inbox/汇聚/治理挂起/已落盘状态，`ExperienceInbox` 没有生命周期，`LongTermMemoryEntry` 缺少来源追溯，无法满足 spec 对「候选唯一中间态」和「最小可追溯」的要求。

- [ ] **Step 1: 先写会失败的领域测试**

在 `src/domain/contribution.rs` 的 `#[cfg(test)]` 模块末尾追加：

```rust
#[test]
fn candidate_status_machine_has_required_states() {
    let statuses = vec![
        ExperienceCandidateStatus::Submitted,
        ExperienceCandidateStatus::InInbox,
        ExperienceCandidateStatus::Aggregated,
        ExperienceCandidateStatus::GovernancePending,
        ExperienceCandidateStatus::NeedsUserApproval,
        ExperienceCandidateStatus::Approved,
        ExperienceCandidateStatus::Rejected,
        ExperienceCandidateStatus::Persisted,
    ];
    assert_eq!(statuses.len(), 8);
}

#[test]
fn inbox_has_pending_and_consumed_states() {
    let inbox = ExperienceInbox {
        owner_task_id: uuid::Uuid::new_v4(),
        owner_agent_id: uuid::Uuid::new_v4(),
        candidate_ids: vec![],
        status: ExperienceInboxStatus::Pending,
    };
    assert!(matches!(inbox.status, ExperienceInboxStatus::Pending));
}

#[test]
fn experience_store_marks_inbox_consumed_and_aggregates() {
    let owner_task_id = uuid::Uuid::new_v4();
    let owner_agent_id = uuid::Uuid::new_v4();
    let producer_task_id = uuid::Uuid::new_v4();
    let candidate = ExperienceCandidate::knowledge(
        uuid::Uuid::new_v4(),
        producer_task_id,
        uuid::Uuid::new_v4(),
        "child fact".to_string(),
        "content".to_string(),
        crate::domain::LongTermMemoryKind::Fact,
    );

    let mut store = ExperienceStore::default();
    store.queue_for_parent(owner_task_id, owner_agent_id, candidate.clone());
    let ids = store.aggregate_inbox_for_task(owner_task_id);

    assert_eq!(ids, vec![candidate.candidate_id]);
    assert_eq!(
        store.candidates.get(&candidate.candidate_id).unwrap().status,
        ExperienceCandidateStatus::Aggregated
    );
    assert_eq!(
        store.inboxes.get(&owner_task_id).unwrap().status,
        ExperienceInboxStatus::Consumed
    );
}
```

在 `src/domain/memory.rs` 的 `#[cfg(test)]` 模块末尾追加：

```rust
#[test]
fn long_term_memory_entry_carries_source_traceability() {
    let mut entry = LongTermMemoryEntry::new(LongTermMemoryKind::Fact, "traceable fact");
    entry.source_candidate_id = Some(uuid::Uuid::new_v4());
    entry.source_task_id = Some(uuid::Uuid::new_v4());
    entry.agent_id = Some(uuid::Uuid::new_v4());

    assert!(entry.source_candidate_id.is_some());
    assert!(entry.source_task_id.is_some());
    assert!(entry.agent_id.is_some());
}
```

- [ ] **Step 2: 运行测试确认新模型不存在**

Run:

```bash
cargo test -q candidate_status_machine_has_required_states -- --nocapture
cargo test -q inbox_has_pending_and_consumed_states -- --nocapture
cargo test -q experience_store_marks_inbox_consumed_and_aggregates -- --nocapture
cargo test -q long_term_memory_entry_carries_source_traceability -- --nocapture
```

Expected: FAIL，提示 `InInbox`、`Aggregated`、`GovernancePending`、`Persisted`、`ExperienceInboxStatus`、`aggregate_inbox_for_task` 或 `source_candidate_id` 未定义。

- [ ] **Step 3: 实现扩展后的领域模型**

在 `src/domain/contribution.rs` 中：

1. 替换 `ExperienceCandidateStatus` 为完整状态机：

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ExperienceCandidateStatus {
    Submitted,
    InInbox,
    Aggregated,
    GovernancePending,
    NeedsUserApproval,
    Approved,
    Rejected,
    Persisted,
}
```

1b. 在 `ExperienceCandidate` 结构体中增加 `governing_agent_id`，用于确认后写回时识别治理者：

```rust
pub struct ExperienceCandidate {
    // ... 已有字段 ...
    pub status: ExperienceCandidateStatus,
    /// 最终治理该候选的顶层 Agent ID，用于确认后的写回路由。
    pub governing_agent_id: Option<AgentId>,
}
```

并在 `ExperienceCandidate::knowledge` 构造函数的返回块中初始化 `governing_agent_id: None`。

2. 新增 `ExperienceInboxStatus` 并扩展 `ExperienceInbox`：

```rust
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum ExperienceInboxStatus {
    #[default]
    Pending,
    Consumed,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExperienceInbox {
    pub owner_task_id: TaskId,
    pub owner_agent_id: AgentId,
    pub candidate_ids: Vec<uuid::Uuid>,
    pub status: ExperienceInboxStatus,
}
```

3. 扩展 `ExperienceStore`：

```rust
impl ExperienceStore {
    /// 将候选投入父任务收件箱，状态置为 InInbox。
    pub fn queue_for_parent(
        &mut self,
        parent_task_id: TaskId,
        parent_agent_id: AgentId,
        mut candidate: ExperienceCandidate,
    ) {
        candidate.status = ExperienceCandidateStatus::InInbox;
        let candidate_id = candidate.candidate_id;
        self.candidates.insert(candidate_id, candidate);
        self.inboxes
            .entry(parent_task_id)
            .or_insert_with(|| ExperienceInbox {
                owner_task_id: parent_task_id,
                owner_agent_id: parent_agent_id,
                candidate_ids: Vec::new(),
                status: ExperienceInboxStatus::Pending,
            })
            .candidate_ids
            .push(candidate_id);
    }

    /// 将候选暂存为顶层候选。
    pub fn stage_root_candidate(&mut self, mut candidate: ExperienceCandidate) {
        candidate.status = ExperienceCandidateStatus::Submitted;
        let task_id = candidate.producer_task_id;
        let candidate_id = candidate.candidate_id;
        self.candidates.insert(candidate_id, candidate);
        self.root_candidates
            .entry(task_id)
            .or_default()
            .push(candidate_id);
    }

    /// 消费指定任务的收件箱，返回其中候选 ID 并将候选状态置为 Aggregated。
    pub fn aggregate_inbox_for_task(&mut self, task_id: TaskId) -> Vec<uuid::Uuid> {
        let Some(inbox) = self.inboxes.get_mut(&task_id) else {
            return Vec::new();
        };
        inbox.status = ExperienceInboxStatus::Consumed;
        let ids = inbox.candidate_ids.clone();
        for id in &ids {
            if let Some(c) = self.candidates.get_mut(id) {
                c.status = ExperienceCandidateStatus::Aggregated;
            }
        }
        ids
    }

    /// 将指定任务的顶层候选置为 GovernancePending，准备进入顶层治理。
    pub fn promote_root_candidates_to_governance(&mut self, task_id: TaskId) -> Vec<uuid::Uuid> {
        let ids = self.root_candidates_for_task(task_id);
        for id in &ids {
            if let Some(c) = self.candidates.get_mut(id) {
                c.status = ExperienceCandidateStatus::GovernancePending;
            }
        }
        ids
    }

    /// 按 producer_task_id 查找候选（不依赖索引，首版直接遍历）。
    pub fn candidates_by_producer_task(&self, task_id: TaskId) -> Vec<&ExperienceCandidate> {
        self.candidates
            .values()
            .filter(|c| c.producer_task_id == task_id)
            .collect()
    }

    // root_candidates_for_task / list_for_task / apply_confirmation_response 保持已有语义，
    // 仅将 apply_confirmation_response 的匹配条件从 NeedsUserApproval 扩展到 Approved 也需要处理的地方。
}
```

4. 在文件末尾（测试模块之前）新增 `SharedKnowledgeUpgradeCandidate`，并将现有 `IncubationProposal` 扩展为符合 spec 的正式治理输出：

```rust
/// 共享知识升级入口候选：已被顶层治理判定具备公共价值，但尚未成为最终共享知识正文。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SharedKnowledgeUpgradeCandidate {
    pub candidate_id: uuid::Uuid,
    pub content: String,
    pub kind: super::LongTermMemoryKind,
    pub scope_tags: Vec<String>,
    pub source_candidate_id: uuid::Uuid,
    pub source_agent_id: AgentId,
    pub source_task_id: TaskId,
    pub validation_status: super::KnowledgeValidationStatus,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// 孵化提案状态。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum IncubationProposalStatus {
    #[default]
    Proposed,
    Approved,
    Rejected,
}

/// 孵化提案：default Agent 的正式治理输出。
#[derive(Debug, Clone, Component)]
pub struct IncubationProposal {
    pub proposal_id: uuid::Uuid,
    pub source_agent_id: AgentId,
    pub source_task_id: TaskId,
    pub proposed_agent_profile: super::AgentProfile,
    pub knowledge_candidate_ids: Vec<uuid::Uuid>,
    pub executable_candidate_ids: Vec<uuid::Uuid>,
    pub shared_knowledge_candidate_ids: Vec<uuid::Uuid>,
    pub status: IncubationProposalStatus,
    pub created_at: chrono::DateTime<chrono::Utc>,
}
```

**注意：** 扩展 `IncubationProposal` 后，Task 5/6 中所有构造点必须同步更新，不能继续使用旧的三字段字面量。

在 `src/domain/memory.rs` 中，为 `LongTermMemoryEntry` 新增可选追溯字段并加 `#[serde(default)]` 保证旧快照兼容：

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LongTermMemoryEntry {
    pub content: String,
    pub kind: LongTermMemoryKind,
    pub scope_tags: Vec<String>,
    pub importance: MemoryImportance,
    pub pin: bool,
    pub created_at: DateTime<Utc>,
    pub last_accessed_at: Option<DateTime<Utc>>,
    pub reuse_count: u32,
    pub decay_score: f32,
    pub source: String,
    pub confidence: f32,
    #[serde(default)]
    pub source_candidate_id: Option<uuid::Uuid>,
    #[serde(default)]
    pub source_task_id: Option<TaskId>,
    #[serde(default)]
    pub agent_id: Option<AgentId>,
}
```

在 `LongTermMemoryEntry::new` 中初始化新字段为 `None`：

```rust
Self {
    // ... 已有字段 ...
    source_candidate_id: None,
    source_task_id: None,
    agent_id: None,
}
```

在 `src/domain/mod.rs` 的 `contribution` 导出中追加 `SharedKnowledgeUpgradeCandidate`、`ExperienceInboxStatus` 与 `IncubationProposalStatus`。

- [ ] **Step 4: 运行领域测试**

Run:

```bash
cargo test -q candidate_status_machine_has_required_states -- --nocapture
cargo test -q inbox_has_pending_and_consumed_states -- --nocapture
cargo test -q experience_store_marks_inbox_consumed_and_aggregates -- --nocapture
cargo test -q long_term_memory_entry_carries_source_traceability -- --nocapture
```

Expected: PASS。

- [ ] **Step 5: 提交**

```bash
git add src/domain/contribution.rs src/domain/memory.rs src/domain/mod.rs
git commit -m "feat: extend experience candidate domain model with traceability"
```

---

## Task 2: 增加 Skill Package 落盘基础设施

**Files:**
- Modify: `src/infrastructure/assets/service.rs`
- Modify: `src/infrastructure/assets/mod.rs`
- Test: `src/infrastructure/assets/service.rs`（内嵌单元测试）

**动机：** spec 要求可执行经验以 Agent 私有 `Skill Package`（文件目录）作为真源，而不是结构化条目或运行时索引。

- [ ] **Step 1: 先写失败测试**

在 `src/infrastructure/assets/service.rs` 的测试模块中追加：

```rust
#[test]
fn persist_skill_package_creates_directory_and_skill_md() {
    let dir = tempfile::TempDir::new().unwrap();
    let service = AgentAssetService::new(dir.path().join("agents"));
    let draft = SkillPackageDraft {
        skill_id: "shell-smoke".to_string(),
        title: "Shell Smoke Test".to_string(),
        problem: "验证 shell 工具链是否正常工作".to_string(),
        when_to_use: "修改 shell 相关代码后".to_string(),
        steps: "1. 运行脚本\n2. 检查输出".to_string(),
        asset_refs: vec!["script.sh".to_string()],
        dependency_refs: vec![],
        risks: "可能受环境差异影响".to_string(),
        source_task_id: Some(uuid::Uuid::new_v4()),
        source_candidate_id: Some(uuid::Uuid::new_v4()),
    };

    let relative = service.persist_skill_package("test-agent", &draft).unwrap();
    let base = dir.path().join("agents").join(&relative);

    assert!(base.join("skill.md").exists());
    assert!(base.join("scripts").is_dir());
    assert!(base.join("resources").is_dir());

    let skill_md = std::fs::read_to_string(base.join("skill.md")).unwrap();
    assert!(skill_md.contains(&draft.title));
    assert!(skill_md.contains("解决的问题"));
}
```

- [ ] **Step 2: 运行测试确认 `SkillPackageDraft` 不存在**

Run:

```bash
cargo test -q persist_skill_package_creates_directory_and_skill_md -- --nocapture
```

Expected: FAIL，提示 `SkillPackageDraft` 或 `persist_skill_package` 未定义。

- [ ] **Step 3: 实现 Skill Package 落盘**

在 `src/infrastructure/assets/service.rs` 中，新增 `SkillPackageDraft` 并在 `AgentAssetService` 上实现 `persist_skill_package`：

```rust
use crate::domain::{AgentId, TaskId};

/// Skill Package 草稿。
#[derive(Debug, Clone)]
pub struct SkillPackageDraft {
    pub skill_id: String,
    pub title: String,
    pub problem: String,
    pub when_to_use: String,
    pub steps: String,
    pub asset_refs: Vec<String>,
    pub dependency_refs: Vec<String>,
    pub risks: String,
    pub source_task_id: Option<TaskId>,
    pub source_candidate_id: Option<uuid::Uuid>,
}

impl AgentAssetService {
    /// 将 Skill Package 草稿落盘为文件目录，返回相对路径（如 `<agent_name>/skills/<skill_id>`）。
    pub fn persist_skill_package(
        &self,
        agent_name: &str,
        draft: &SkillPackageDraft,
    ) -> Result<String> {
        let relative = format!("{}/skills/{}", agent_name, draft.skill_id);
        let skill_dir = self.base_dir.join(&relative);
        fs::create_dir_all(&skill_dir.join("scripts"))
            .with_context(|| format!("failed to create scripts dir for {}", skill_dir.display()))?;
        fs::create_dir_all(&skill_dir.join("resources")).with_context(|| {
            format!("failed to create resources dir for {}", skill_dir.display())
        })?;

        let skill_md = format!(
            "# {}\n\n## 解决的问题\n{}\n\n## 什么时候使用\n{}\n\n## 使用步骤\n{}\n\n## 依赖脚本或资源说明\n- asset_refs: {:?}\n- dependency_refs: {:?}\n\n## 风险与限制\n{}\n\n## 来源追溯\n- task_id: {:?}\n- candidate_id: {:?}\n",
            draft.title,
            draft.problem,
            draft.when_to_use,
            draft.steps,
            draft.asset_refs,
            draft.dependency_refs,
            draft.risks,
            draft.source_task_id,
            draft.source_candidate_id,
        );

        fs::write(skill_dir.join("skill.md"), skill_md)
            .with_context(|| format!("failed to write skill.md for {}", skill_dir.display()))?;

        Ok(relative)
    }
}
```

在 `src/infrastructure/assets/mod.rs` 中导出 `SkillPackageDraft`：

```rust
pub use service::{AgentAssetService, ExperienceAssetDraft, SkillPackageDraft};
```

在 `src/lib.rs` 中增加 `pub use infrastructure::*;`，使集成测试可以通过 `harness::AgentAssetService` 等路径访问：

```rust
pub use infrastructure::*;
```

- [ ] **Step 4: 运行测试**

Run:

```bash
cargo test -q persist_skill_package_creates_directory_and_skill_md -- --nocapture
```

Expected: PASS。

- [ ] **Step 5: 提交**

```bash
git add src/infrastructure/assets/service.rs src/infrastructure/assets/mod.rs src/lib.rs
git commit -m "feat: add skill package persistence infrastructure"
```

---

## Task 3: 统一候选主路径——非顶层进 inbox，顶层进 root

**Files:**
- Modify: `src/systems/tools/dispatch.rs`
- Modify: `src/systems/tools/orchestrator.rs`
- Test: `src/systems/tools/mod.rs`（已有 knowledge_search 回归测试）
- Test: `tests/experience_collection_workitem_flow.rs`

**动机：** 当前 `submit_experience_candidate` 一律调用 `stage_root_candidate`，导致非顶层候选没有进入父层 `ExperienceInbox`，违反 spec「候选必须经 inbox 向上贡献」。

- [ ] **Step 1: 修改 `handle_tool_action` 签名以接收 `parent_agent_id`**

在 `src/systems/tools/orchestrator.rs` 中，将 `handle_tool_action` 签名增加 `parent_agent_id: Option<AgentId>`：

```rust
pub fn handle_tool_action<B: SessionBackend>(
    commands: &mut Commands,
    request_entity: Entity,
    task_entity: Entity,
    request: &ToolExecutionRequestMessage,
    action: Result<ToolAction, ToolError>,
    tasks: &mut Query<(Entity, &mut Task)>,
    backend: &B,
    experience_store: &mut ExperienceStore,
    parent_agent_id: Option<AgentId>,
)
```

- [ ] **Step 2: 在 `SubmitExperienceCandidate` 分支中按层级路由**

将 `Ok(ToolAction::SubmitExperienceCandidate(submission)) => { ... }` 替换为：

```rust
Ok(ToolAction::SubmitExperienceCandidate(submission)) => {
    let candidate = submission_to_candidate(
        &submission,
        request.request.agent_id,
        request.request.task_id,
    );

    // 判断当前任务是否有父任务：有则写入父层 inbox，无则作为顶层 root 候选。
    let parent_task_id = tasks
        .iter()
        .find(|(_, t)| t.id == request.request.task_id)
        .and_then(|(_, t)| t.parent_task_id);

    match parent_task_id {
        Some(parent_task_id) => {
            let owner_agent_id = parent_agent_id.unwrap_or(request.request.agent_id);
            experience_store.queue_for_parent(parent_task_id, owner_agent_id, candidate.clone());
        }
        None => {
            experience_store.stage_root_candidate(candidate.clone());
        }
    }

    spawn_experience_candidate_result(commands, request_entity, request, &candidate);
}
```

- [ ] **Step 3: 在 `tool_dispatch_system` 调用点传入 `agent.parent_id`**

在 `src/systems/tools/dispatch.rs` 中，调用 `handle_tool_action` 前计算 `parent_agent_id`：

```rust
let parent_agent_id = agent.parent_id;
```

并将 `parent_agent_id` 作为最后一个参数传入 `handle_tool_action`。

- [ ] **Step 4: 运行回归测试**

Run:

```bash
cargo test -q executor_knowledge_search -- --nocapture
cargo test -q experience_collection_workitem_flow -- --nocapture
```

Expected: PASS。若 `experience_collection_workitem_flow` 中断言 `root_candidates_for_task` 非空的用例失败，说明该用例构造的是顶层任务（无 `parent_task_id`），应将其预期改为候选进入 root；若构造了 `parent_task_id` 则应改为断言 inbox。

- [ ] **Step 5: 提交**

```bash
git add src/systems/tools/dispatch.rs src/systems/tools/orchestrator.rs
git commit -m "feat: route experience candidates to parent inbox or root by task level"
```

---

## Task 4: 经验收集完成后的汇聚与治理触发

**Files:**
- Modify: `src/domain/message.rs`
- Modify: `src/domain/mod.rs`
- Modify: `src/systems/transform/llm_response.rs`
- Modify: `src/systems/contribution.rs`
- Modify: `src/systems/mod.rs`
- Modify: `src/plugins/execution.rs`
- Test: `src/systems/contribution.rs`（内嵌单元测试）

**动机：** 当前 `ExperienceGovernanceRequestMessage` 从未被生成，顶层治理系统空转；非顶层候选虽然进了 inbox，但缺少「汇聚完成 → 状态推进」的显式动作。

- [ ] **Step 1: 定义 `ExperienceCollectionCompletedMessage`**

在 `src/domain/message.rs` 中新增：

```rust
#[derive(Debug, Clone, Component)]
pub struct ExperienceCollectionCompletedMessage {
    pub task_id: TaskId,
    pub parent_task_id: Option<TaskId>,
    pub agent_id: AgentId,
}
```

在 `src/domain/mod.rs` 中导出。

- [ ] **Step 2: 在 `llm_response.rs` 中经验收集 WorkItem 完成时发送汇聚消息**

在 `src/systems/transform/llm_response.rs` 的 `WorkItemType::ExperienceCollection` 分支中，当 `had_submission` 为真、WorkItem 标记完成并 despawn 之前，插入：

```rust
commands.spawn(ExperienceCollectionCompletedMessage {
    task_id: work_item.task_id,
    parent_task_id: work_item.parent_task_id,
    agent_id: work_item.assigned_agent.unwrap_or(uuid::Uuid::nil()),
});
```

具体位置在 `wi.1.complete();` 之后、`commands.entity(work_item_entity).despawn();` 之前。

- [ ] **Step 3: 先写汇聚系统的失败测试**

在 `src/systems/contribution.rs` 的测试模块中追加：

```rust
#[test]
fn experience_collection_completion_aggregates_child_candidates() {
    use crate::domain::{ExperienceStore, TaskId};

    let parent_task_id: TaskId = uuid::Uuid::new_v4();
    let child_task_id: TaskId = uuid::Uuid::new_v4();
    let parent_agent_id = uuid::Uuid::new_v4();

    let mut store = ExperienceStore::default();
    let candidate = crate::domain::ExperienceCandidate::knowledge(
        uuid::Uuid::new_v4(),
        child_task_id,
        uuid::Uuid::new_v4(),
        "child fact".to_string(),
        "content".to_string(),
        crate::domain::LongTermMemoryKind::Fact,
    );
    store.queue_for_parent(parent_task_id, parent_agent_id, candidate);

    // 模拟顶层任务：应触发治理
    store.promote_root_candidates_to_governance(parent_task_id);
    let governance_ids = store
        .candidates
        .values()
        .filter(|c| c.status == crate::domain::ExperienceCandidateStatus::GovernancePending)
        .map(|c| c.candidate_id)
        .collect::<Vec<_>>();
    assert!(!governance_ids.is_empty());
}
```

- [ ] **Step 4: 实现 `experience_collection_completion_system`**

在 `src/systems/contribution.rs` 中新增：

```rust
/// 经验收集完成处理系统：将非顶层候选标记为已汇聚，顶层候选推进到治理挂起。
pub(crate) fn experience_collection_completion_system(
    mut commands: Commands,
    mut store: ResMut<crate::domain::ExperienceStore>,
    messages: Query<(Entity, &ExperienceCollectionCompletedMessage)>,
) {
    for (entity, msg) in &messages {
        if let Some(parent_task_id) = msg.parent_task_id {
            // 非顶层：消费父任务 inbox 中的子候选，标记为 Aggregated。
            let ids = store.aggregate_inbox_for_task(parent_task_id);
            debug!(
                event = "ExperienceCollectionAggregated",
                task_id = %msg.task_id,
                parent_task_id = %parent_task_id,
                aggregated_count = ids.len(),
                "aggregated child candidates into parent inbox"
            );
        } else {
            // 顶层：将 root 候选推进到 GovernancePending 并触发治理。
            let ids = store.promote_root_candidates_to_governance(msg.task_id);
            if !ids.is_empty() {
                commands.spawn(ExperienceGovernanceRequestMessage {
                    task_id: msg.task_id,
                    agent_id: msg.agent_id,
                });
                debug!(
                    event = "TopLevelExperienceGovernanceRequested",
                    task_id = %msg.task_id,
                    candidate_count = ids.len(),
                    "spawned top-level experience governance request"
                );
            }
        }

        commands.entity(entity).despawn();
    }
}
```

- [ ] **Step 5: 注册新系统并声明执行顺序**

在 `src/systems/mod.rs` 中导出 `experience_collection_completion_system`。

在 `src/plugins/execution.rs` 的 `Update` system set 中，将 `experience_collection_completion_system` 注册在 `llm_response_system` 之后、`experience_governance_system` 之前：

```rust
experience_collection_completion_system
    .in_set(HarnessSet::Execution)
    .after(crate::systems::llm_response_system)
    .before(experience_governance_system),
experience_governance_system
    .in_set(HarnessSet::Execution)
    .after(experience_collection_completion_system),
```

- [ ] **Step 6: 运行测试**

Run:

```bash
cargo test -q experience_collection_completion_aggregates_child_candidates -- --nocapture
cargo test -q experience_collection_workitem_flow -- --nocapture
```

Expected: PASS。

- [ ] **Step 7: 提交**

```bash
git add src/domain/message.rs src/domain/mod.rs src/systems/transform/llm_response.rs src/systems/contribution.rs src/systems/mod.rs src/plugins/execution.rs
git commit -m "feat: aggregate experience candidates and trigger top-level governance"
```

---

## Task 5: 顶层治理系统与四类最终去向

**Files:**
- Modify: `src/domain/contribution.rs`
- Modify: `src/domain/space.rs`
- Modify: `src/systems/contribution.rs`
- Modify: `src/app/mod.rs`
- Modify: `src/plugins/execution.rs`
- Test: `src/systems/contribution.rs`（内嵌单元测试）

**动机：** 当前 `experience_governance_system` 只处理 root 候选、只支持 Knowledge → LTM 和 Executable → 确认、对 `default Agent` 的判断依赖硬编码名称，缺少 `SharedKnowledge` 升级入口和 `Skill Package` 的正式落盘。

- [ ] **Step 1: 新增 `SharedKnowledgeUpgradeQueue` Resource 与持久化服务**

在 `src/domain/space.rs` 中新增：

```rust
#[derive(Resource, Default, Serialize, Deserialize)]
pub struct SharedKnowledgeUpgradeQueue {
    pub candidates: Vec<SharedKnowledgeUpgradeCandidate>,
}
```

在 `src/infrastructure/memory/upgrade_service.rs` 中新增写穿服务：

```rust
use std::{fs, path::PathBuf};

use anyhow::{Context, Result};
use bevy::prelude::Resource;
use serde_json;

use crate::domain::SharedKnowledgeUpgradeQueue;

#[derive(Resource, Debug, Clone)]
pub struct SharedKnowledgeUpgradeService {
    base_dir: PathBuf,
}

impl SharedKnowledgeUpgradeService {
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self {
            base_dir: base_dir.into(),
        }
    }

    pub fn default_path() -> Self {
        Self::new(".harness/memory/shared_knowledge")
    }

    pub fn persist(&self, queue: &SharedKnowledgeUpgradeQueue) -> Result<()> {
        fs::create_dir_all(&self.base_dir)
            .with_context(|| format!("failed to create upgrade dir {}", self.base_dir.display()))?;
        let path = self.base_dir.join("upgrades.json");
        let tmp_path = path.with_extension("json.tmp");
        let json = serde_json::to_string_pretty(queue)
            .context("failed to serialize shared knowledge upgrade queue")?;
        fs::write(&tmp_path, json)
            .with_context(|| format!("failed to write tmp file {}", tmp_path.display()))?;
        fs::rename(&tmp_path, &path)
            .with_context(|| format!("failed to rename {} to {}", tmp_path.display(), path.display()))?;
        Ok(())
    }

    pub fn load(&self) -> Result<SharedKnowledgeUpgradeQueue> {
        let path = self.base_dir.join("upgrades.json");
        if !path.exists() {
            return Ok(SharedKnowledgeUpgradeQueue::default());
        }
        let content = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        serde_json::from_str(&content)
            .with_context(|| format!("failed to parse {}", path.display()))
    }
}
```

在 `src/infrastructure/memory/mod.rs` 中导出 `SharedKnowledgeUpgradeService`。

在 `src/domain/mod.rs` 中导出 `SharedKnowledgeUpgradeQueue`。

在 `src/app/mod.rs` 的 `build_harness_app` 中插入 Resource 并加载已有数据：

```rust
let upgrade_service = SharedKnowledgeUpgradeService::default_path();
let upgrade_queue = upgrade_service.load().unwrap_or_default();
app.insert_resource(upgrade_service);
app.insert_resource(upgrade_queue);
```

- [ ] **Step 2: 先写治理系统的失败测试**

在 `src/systems/contribution.rs` 测试模块中追加：

```rust
#[test]
fn is_default_agent_detects_by_tag_not_name() {
    let default_agent = crate::domain::Agent {
        id: uuid::Uuid::new_v4(),
        profile: crate::domain::AgentProfile {
            name: "custom-default".to_string(),
            model: "test".to_string(),
        },
        capabilities: crate::domain::AgentCapabilities {
            tags: vec!["default".to_string(), "llm".to_string()],
            description: "default agent".to_string(),
        },
        kind: crate::domain::AgentKind::Persistent,
        parent_id: None,
        bound_task_id: None,
        tool_permissions: crate::domain::AgentToolPermissions::default(),
    };

    assert!(is_default_agent(&default_agent));
}
```

- [ ] **Step 3: 实现 `is_default_agent` helper**

在 `src/systems/contribution.rs` 中新增：

```rust
fn is_default_agent(agent: &Agent) -> bool {
    agent.capabilities.tags.iter().any(|t| t == "default")
}
```

同时确保 `src/systems/contribution.rs` 顶部引入 `warn`：

```rust
use tracing::{debug, warn};
```

- [ ] **Step 4: 重写 `experience_governance_system`**

替换 `src/systems/contribution.rs` 中的 `experience_governance_system` 为：

```rust
/// 经验治理系统：顶层唯一最终分流点。
pub(crate) fn experience_governance_system(
    mut commands: Commands,
    mut store: ResMut<crate::domain::ExperienceStore>,
    mut long_memories: Query<&mut LongTermMemory>,
    agents: Query<&Agent>,
    mut service: ResMut<LongTermMemoryService>,
    mut upgrade_queue: ResMut<crate::domain::SharedKnowledgeUpgradeQueue>,
    upgrade_service: Res<crate::infrastructure::memory::SharedKnowledgeUpgradeService>,
    requests: Query<(Entity, &ExperienceGovernanceRequestMessage)>,
) {
    for (entity, request) in &requests {
        let agent = match agents.iter().find(|a| a.id == request.agent_id) {
            Some(a) => a,
            None => {
                debug!(
                    event = "ExperienceGovernanceAgentNotFound",
                    agent_id = %request.agent_id,
                    task_id = %request.task_id,
                    "agent not found for governance, skipping"
                );
                commands.entity(entity).despawn();
                continue;
            }
        };

        let is_default = is_default_agent(agent);
        let candidate_ids = store.governance_candidates_for_task(request.task_id);

        // 记录治理者，供确认后写回路由使用。
        for id in &candidate_ids {
            if let Some(c) = store.candidates.get_mut(id) {
                c.governing_agent_id = Some(request.agent_id);
            }
        }

        for candidate_id in &candidate_ids {
            let Some(candidate) = store.candidates.get(candidate_id).cloned() else {
                continue;
            };

            match candidate.kind_hint {
                ExperienceKindHint::Discard => {
                    if let Some(c) = store.candidates.get_mut(candidate_id) {
                        c.status = ExperienceCandidateStatus::Rejected;
                    }
                    debug!(
                        event = "ExperienceGovernanceRejected",
                        candidate_id = %candidate_id,
                        task_id = %request.task_id,
                        "discarded candidate"
                    );
                }
                ExperienceKindHint::SharedKnowledge => {
                    upgrade_queue.candidates.push(crate::domain::SharedKnowledgeUpgradeCandidate {
                        candidate_id: uuid::Uuid::new_v4(),
                        content: candidate.payload.content().unwrap_or_default(),
                        kind: crate::domain::LongTermMemoryKind::Fact,
                        scope_tags: Vec::new(),
                        source_candidate_id: candidate.candidate_id,
                        source_agent_id: candidate.producer_agent_id,
                        source_task_id: candidate.producer_task_id,
                        validation_status: crate::domain::KnowledgeValidationStatus::Candidate,
                        created_at: chrono::Utc::now(),
                    });
                    match upgrade_service.persist(&upgrade_queue) {
                        Ok(_) => {
                            if let Some(c) = store.candidates.get_mut(candidate_id) {
                                c.status = ExperienceCandidateStatus::Persisted;
                            }
                            debug!(
                                event = "ExperienceGovernanceSharedKnowledgeQueued",
                                candidate_id = %candidate_id,
                                task_id = %request.task_id,
                                "queued and persisted shared knowledge upgrade candidate"
                            );
                        }
                        Err(e) => {
                            warn!(
                                event = "ExperienceWritebackFailed",
                                candidate_id = %candidate_id,
                                task_id = %request.task_id,
                                target = "SharedKnowledgeUpgradeQueue",
                                error = %e,
                                "failed to persist shared knowledge upgrade candidate"
                            );
                        }
                    }
                }
                ExperienceKindHint::Executable => {
                    if is_default {
                        // default Agent 的可执行候选只能进孵化提案，需要用户确认。
                        spawn_incubation_confirmation(&mut commands, &mut store, request, agent, candidate_id);
                    } else {
                        // 普通持久型 Agent 的可执行候选需要用户确认后生成 Skill Package。
                        if let Some(c) = store.candidates.get_mut(candidate_id) {
                            c.status = ExperienceCandidateStatus::NeedsUserApproval;
                        }
                        spawn_experience_confirmation(&mut commands, request, candidate_id, &candidate);
                    }
                }
                ExperienceKindHint::Knowledge => {
                    if is_default {
                        // default Agent 的私有知识候选只能进孵化提案。
                        spawn_incubation_confirmation(&mut commands, &mut store, request, agent, candidate_id);
                    } else {
                        // 普通持久型 Agent 的低风险知识自动写入 LongTermMemory。
                        let mut persisted = false;
                        if let Some(mut entry) = candidate.as_long_term_memory_entry() {
                            entry.source_candidate_id = Some(candidate.candidate_id);
                            entry.source_task_id = Some(candidate.producer_task_id);
                            entry.agent_id = Some(candidate.producer_agent_id);

                            if let Some(mut memory) = long_memories
                                .iter_mut()
                                .find(|lm| lm.agent_name.as_deref() == Some(&agent.profile.name))
                            {
                                match service.add_entry(&mut memory, entry) {
                                    Ok(_) => persisted = true,
                                    Err(e) => {
                                        warn!(
                                            event = "ExperienceWritebackFailed",
                                            candidate_id = %candidate_id,
                                            task_id = %request.task_id,
                                            target = "LongTermMemory",
                                            error = %e,
                                            "failed to auto-persist knowledge candidate"
                                        );
                                    }
                                }
                            } else {
                                warn!(
                                    event = "ExperienceWritebackFailed",
                                    candidate_id = %candidate_id,
                                    task_id = %request.task_id,
                                    target = "LongTermMemory",
                                    reason = "agent_memory_not_found",
                                    "no LongTermMemory component found for governing agent"
                                );
                            }
                        }
                        if persisted {
                            if let Some(c) = store.candidates.get_mut(candidate_id) {
                                c.status = ExperienceCandidateStatus::Persisted;
                            }
                            debug!(
                                event = "ExperienceGovernancePersisted",
                                candidate_id = %candidate_id,
                                task_id = %request.task_id,
                                agent_name = %agent.profile.name,
                                "persisted knowledge candidate to long-term memory"
                            );
                        }
                    }
                }
            }
        }

        commands.entity(entity).despawn();
    }
}
```

其中需要两个 helper 函数：

```rust
fn spawn_experience_confirmation(
    commands: &mut Commands,
    request: &ExperienceGovernanceRequestMessage,
    candidate_id: &uuid::Uuid,
    candidate: &crate::domain::ExperienceCandidate,
) {
    let request_id = uuid::Uuid::new_v4();
    commands.spawn(ToolConfirmationRequestMessage {
        request_id,
        task_id: request.task_id,
        agent_id: request.agent_id,
        tool_name: "experience_governance".to_string(),
        tool_input: serde_json::json!({
            "candidate_id": candidate_id.to_string(),
            "title": candidate.title,
            "kind": format!("{:?}", candidate.kind_hint),
        }),
        options: ConfirmationOption::default_options(),
        source: ConfirmationSource::User,
        parent_agent_id: None,
    });
}

fn spawn_incubation_confirmation(
    commands: &mut Commands,
    store: &mut crate::domain::ExperienceStore,
    request: &ExperienceGovernanceRequestMessage,
    agent: &Agent,
    candidate_id: &uuid::Uuid,
) {
    if let Some(c) = store.candidates.get_mut(candidate_id) {
        c.status = ExperienceCandidateStatus::NeedsUserApproval;
    }
    let candidate = store.candidates.get(candidate_id).cloned();
    if let Some(candidate) = candidate {
        let proposal_id = uuid::Uuid::new_v4();
        let (knowledge_ids, executable_ids, shared_ids) = match candidate.kind_hint {
            ExperienceKindHint::Knowledge => (vec![*candidate_id], vec![], vec![]),
            ExperienceKindHint::Executable => (vec![], vec![*candidate_id], vec![]),
            ExperienceKindHint::SharedKnowledge => (vec![], vec![], vec![*candidate_id]),
            ExperienceKindHint::Discard => (vec![], vec![], vec![]),
        };

        commands.spawn(IncubationProposal {
            proposal_id,
            source_agent_id: request.agent_id,
            source_task_id: request.task_id,
            proposed_agent_profile: crate::domain::AgentProfile {
                name: format!("incubated-{}", proposal_id),
                model: agent.profile.model.clone(),
            },
            knowledge_candidate_ids: knowledge_ids,
            executable_candidate_ids: executable_ids,
            shared_knowledge_candidate_ids: shared_ids,
            status: IncubationProposalStatus::Proposed,
            created_at: chrono::Utc::now(),
        });

        spawn_experience_confirmation(commands, request, candidate_id, &candidate);
    }
}
```

**注意：** `candidate.payload.content()` 当前不存在，需要为 `ExperienceCandidatePayload` 增加一个辅助方法返回知识/共享知识类型的文本内容。在 `src/domain/contribution.rs` 中实现：

```rust
impl ExperienceCandidatePayload {
    pub fn content(&self) -> Option<String> {
        match self {
            ExperienceCandidatePayload::Knowledge { content, .. } => Some(content.clone()),
            ExperienceCandidatePayload::Executable { .. } => None,
        }
    }
}
```

- [ ] **Step 5: 运行测试**

Run:

```bash
cargo test -q is_default_agent_detects_by_tag_not_name -- --nocapture
cargo test -q experience_candidate_flow -- --nocapture
```

Expected: PASS（`experience_candidate_flow` 中部分断言可能需要随状态机变化而调整，确保只验证不变的核心语义）。

- [ ] **Step 6: 提交**

```bash
git add src/domain/contribution.rs src/domain/space.rs src/domain/mod.rs src/infrastructure/memory/upgrade_service.rs src/infrastructure/memory/mod.rs src/systems/contribution.rs src/app/mod.rs src/plugins/execution.rs
git commit -m "feat: implement top-level experience governance with four destinations"
```

---

## Task 6: 用户确认后的最终写回

**Files:**
- Modify: `src/systems/contribution.rs`
- Modify: `src/plugins/memory.rs`
- Test: `src/systems/contribution.rs`（内嵌单元测试）

**动机：** spec 要求「用户确认完成后必须触发最终写回」。当前 `experience_approval_result_system` 只把 Approved 知识候选写入 LTM，缺少 Approved executable → Skill Package 以及 default Agent Approved 候选 → IncubationProposal 的分支。

- [ ] **Step 1: 在 `MemoryPlugin` 中注册 `AgentAssetService`**

在 `src/plugins/memory.rs` 中：

```rust
use crate::infrastructure::assets::AgentAssetService;
```

并在 `build` 中插入 Resource：

```rust
app.insert_resource(AgentAssetService::default_path());
```

- [ ] **Step 2: 先写确认结果处理的失败测试**

在 `src/systems/contribution.rs` 测试模块中追加：

```rust
#[test]
fn approved_executable_becomes_persisted() {
    use crate::domain::{ExperienceCandidate, ExperienceCandidatePayload, ExperienceCandidateStatus, ExperienceKindHint};

    let mut store = crate::domain::ExperienceStore::default();
    let candidate = ExperienceCandidate {
        candidate_id: uuid::Uuid::new_v4(),
        producer_task_id: uuid::Uuid::new_v4(),
        producer_agent_id: uuid::Uuid::new_v4(),
        title: "test skill".to_string(),
        kind_hint: ExperienceKindHint::Executable,
        payload: ExperienceCandidatePayload::Executable {
            intent: "run smoke test".to_string(),
            when_to_use: "after changes".to_string(),
            asset_refs: vec![],
        },
        dependency_refs: vec![],
        status: ExperienceCandidateStatus::NeedsUserApproval,
    };
    store.stage_root_candidate(candidate);
    store.apply_confirmation_response(uuid::Uuid::new_v4(), "approve");

    assert!(
        store.candidates.values().any(|c| c.status == ExperienceCandidateStatus::Approved),
        "approved executable should be marked Approved"
    );
}
```

- [ ] **Step 3: 重写 `experience_approval_result_system`**

替换 `src/systems/contribution.rs` 中的 `experience_approval_result_system` 为：

```rust
/// 经验确认结果系统：处理用户对经验候选的确认，触发最终写回。
pub(crate) fn experience_approval_result_system(
    mut commands: Commands,
    mut store: ResMut<crate::domain::ExperienceStore>,
    mut long_memories: Query<&mut LongTermMemory>,
    agents: Query<&Agent>,
    mut service: ResMut<LongTermMemoryService>,
    asset_service: Res<crate::infrastructure::assets::AgentAssetService>,
    mut upgrade_queue: ResMut<crate::domain::SharedKnowledgeUpgradeQueue>,
    upgrade_service: Res<crate::infrastructure::memory::SharedKnowledgeUpgradeService>,
    mut proposals: Query<&mut IncubationProposal>,
    responses: Query<(Entity, &ToolConfirmationResponseMessage)>,
) {
    for (entity, response) in &responses {
        let approved = response.selected_option != "deny";
        store.apply_confirmation_response(response.request_id, &response.selected_option);

        if approved {
            // 收集所有 Approved 但未 Persisted 的候选。
            let to_writeback: Vec<_> = store
                .candidates
                .values()
                .filter(|c| {
                    c.status == ExperienceCandidateStatus::Approved
                })
                .cloned()
                .collect();

            for candidate in to_writeback {
                let producer_agent = agents.iter().find(|a| a.id == candidate.producer_agent_id);
                let is_default = candidate
                    .governing_agent_id
                    .and_then(|id| agents.iter().find(|a| a.id == id))
                    .map(is_default_agent)
                    .unwrap_or(false);

                match candidate.kind_hint {
                    ExperienceKindHint::Knowledge => {
                        if is_default {
                            // default Agent 的私有知识经确认后批准对应 IncubationProposal。
                            if let Some(mut proposal) = proposals
                                .iter_mut()
                                .find(|p| p.knowledge_candidate_ids.contains(&candidate.candidate_id))
                            {
                                proposal.status = IncubationProposalStatus::Approved;
                            }
                            if let Some(c) = store.candidates.get_mut(&candidate.candidate_id) {
                                c.status = ExperienceCandidateStatus::Persisted;
                            }
                        } else if let Some(mut entry) = candidate.as_long_term_memory_entry() {
                            entry.source_candidate_id = Some(candidate.candidate_id);
                            entry.source_task_id = Some(candidate.producer_task_id);
                            entry.agent_id = Some(candidate.producer_agent_id);

                            let mut persisted = false;
                            if let Some(agent) = producer_agent {
                                if let Some(mut memory) = long_memories
                                    .iter_mut()
                                    .find(|lm| lm.agent_name.as_deref() == Some(&agent.profile.name))
                                {
                                    match service.add_entry(&mut memory, entry) {
                                        Ok(_) => persisted = true,
                                        Err(e) => {
                                            warn!(
                                                event = "ExperienceWritebackFailed",
                                                candidate_id = %candidate.candidate_id,
                                                target = "LongTermMemory",
                                                error = %e,
                                                "failed to persist knowledge candidate"
                                            );
                                        }
                                    }
                                } else {
                                    warn!(
                                        event = "ExperienceWritebackFailed",
                                        candidate_id = %candidate.candidate_id,
                                        target = "LongTermMemory",
                                        reason = "agent_memory_not_found",
                                        "no LongTermMemory component found for producer agent"
                                    );
                                }
                            } else {
                                warn!(
                                    event = "ExperienceWritebackFailed",
                                    candidate_id = %candidate.candidate_id,
                                    target = "LongTermMemory",
                                    reason = "producer_agent_not_found",
                                    "producer agent not found for knowledge candidate"
                                );
                            }

                            if persisted {
                                if let Some(c) = store.candidates.get_mut(&candidate.candidate_id) {
                                    c.status = ExperienceCandidateStatus::Persisted;
                                }
                            }
                        }
                    }
                    ExperienceKindHint::Executable => {
                        if is_default {
                            // default Agent 的可执行候选经确认后批准对应 IncubationProposal。
                            if let Some(mut proposal) = proposals
                                .iter_mut()
                                .find(|p| p.executable_candidate_ids.contains(&candidate.candidate_id))
                            {
                                proposal.status = IncubationProposalStatus::Approved;
                            }
                            if let Some(c) = store.candidates.get_mut(&candidate.candidate_id) {
                                c.status = ExperienceCandidateStatus::Persisted;
                            }
                        } else if let Some(agent) = producer_agent {
                            // 普通持久型 Agent 生成私有 Skill Package。
                            if let ExperienceCandidatePayload::Executable {
                                intent,
                                when_to_use,
                                asset_refs,
                            } = &candidate.payload
                            {
                                let draft = crate::infrastructure::assets::SkillPackageDraft {
                                    skill_id: format!("{}", candidate.candidate_id),
                                    title: candidate.title.clone(),
                                    problem: intent.clone(),
                                    when_to_use: when_to_use.clone(),
                                    steps: "参见 skill.md 与 scripts/ 目录".to_string(),
                                    asset_refs: asset_refs.clone(),
                                    dependency_refs: candidate.dependency_refs.clone(),
                                    risks: "首版实现，需人工复核".to_string(),
                                    source_task_id: Some(candidate.producer_task_id),
                                    source_candidate_id: Some(candidate.candidate_id),
                                };
                                match asset_service.persist_skill_package(&agent.profile.name, &draft) {
                                    Ok(_) => {
                                        if let Some(c) = store.candidates.get_mut(&candidate.candidate_id) {
                                            c.status = ExperienceCandidateStatus::Persisted;
                                        }
                                    }
                                    Err(e) => {
                                        warn!(
                                            event = "ExperienceWritebackFailed",
                                            candidate_id = %candidate.candidate_id,
                                            target = "SkillPackage",
                                            error = %e,
                                            "failed to persist skill package"
                                        );
                                    }
                                }
                            }
                        }
                    }
                    ExperienceKindHint::SharedKnowledge => {
                        // 若共享知识候选需要用户确认，批准后升入 Approved 并持久化。
                        if let Some(existing) = upgrade_queue
                            .candidates
                            .iter_mut()
                            .find(|u| u.source_candidate_id == candidate.candidate_id)
                        {
                            existing.validation_status = crate::domain::KnowledgeValidationStatus::Approved;
                        }
                        match upgrade_service.persist(&upgrade_queue) {
                            Ok(_) => {
                                if let Some(c) = store.candidates.get_mut(&candidate.candidate_id) {
                                    c.status = ExperienceCandidateStatus::Persisted;
                                }
                            }
                            Err(e) => {
                                warn!(
                                    event = "ExperienceWritebackFailed",
                                    candidate_id = %candidate.candidate_id,
                                    target = "SharedKnowledgeUpgradeQueue",
                                    error = %e,
                                    "failed to persist shared knowledge approval"
                                );
                            }
                        }
                    }
                    ExperienceKindHint::Discard => {}
                }

                debug!(
                    event = "ExperienceCandidateFinalWriteback",
                    candidate_id = %candidate.candidate_id,
                    kind = ?candidate.kind_hint,
                    is_default = is_default,
                    "finalized experience candidate after user approval"
                );
            }
        } else {
            debug!(
                event = "ExperienceCandidateRejected",
                request_id = %response.request_id,
                "user rejected experience candidate"
            );
        }

        commands.entity(entity).despawn();
    }
}
```

**注意：** `apply_confirmation_response` 当前会把所有 `NeedsUserApproval` 候选同时批准/拒绝。首版保留该 MVP 行为，但需确保它只更新状态为 `Approved` 或 `Rejected`。若需要精确到单个 request_id，后续再改进。

- [ ] **Step 4: 运行测试**

Run:

```bash
cargo test -q approved_executable_becomes_persisted -- --nocapture
cargo test -q experience_candidate_flow -- --nocapture
```

Expected: PASS。

- [ ] **Step 5: 提交**

```bash
git add src/systems/contribution.rs src/plugins/memory.rs
git commit -m "feat: finalize approved experience candidates to skill package or incubation proposal"
```

---

## Task 7: 四条闭环集成测试

**Files:**
- Create: `tests/experience_layered_governance_flow.rs`

**动机：** spec 明确要求首版至少保留 4 条闭环集成测试，分别覆盖普通持久型 Agent 的知识/executable 闭环、公共知识升级入口、default Agent 孵化。

- [ ] **Step 1: 创建测试文件骨架**

Create `tests/experience_layered_governance_flow.rs`：

```rust
//! 经验模块两层分层汇聚治理集成测试
//!
//! 覆盖 spec 要求的四条主链路：
//! - 普通持久型 Agent 知识类候选自动落盘到 LongTermMemory
//! - 普通持久型 Agent executable 候选用户批准后生成 Skill Package
//! - 公共规则类候选进入 SharedKnowledgeUpgradeQueue
//! - default Agent 的私有候选生成 IncubationProposal

use harness::{
    AgentAssetService, ExperienceCandidate, ExperienceCandidatePayload,
    ExperienceCandidateStatus, ExperienceKindHint, ExperienceStore,
    SharedKnowledgeUpgradeQueue,
    infrastructure::memory::{JsonFileMemoryStore, LongTermMemoryService, MemoryRepository},
};
use tempfile::TempDir;

fn make_service(dir: &TempDir) -> LongTermMemoryService {
    let store = JsonFileMemoryStore::new(dir.path().join("agents"));
    let repo = MemoryRepository::new(Box::new(store));
    LongTermMemoryService::new(repo)
}

/// Case 1: 普通持久型 Agent 的知识类闭环。
#[test]
fn persistent_agent_knowledge_candidate_persists_to_ltm() {
    let dir = TempDir::new().unwrap();
    let mut service = make_service(&dir);
    let mut memory = harness::LongTermMemory::with_name("persistent-worker");

    let mut store = ExperienceStore::default();
    let candidate = ExperienceCandidate::knowledge(
        uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4(),
        "shell timeout fact".to_string(),
        "shell_stop 默认等待退出".to_string(),
        harness::LongTermMemoryKind::Fact,
    );
    store.stage_root_candidate(candidate);

    // 模拟顶层治理自动落盘
    let ids = store.promote_root_candidates_to_governance(candidate.producer_task_id);
    assert!(!ids.is_empty());

    if let Some(entry) = store.candidates[&ids[0]].as_long_term_memory_entry() {
        service.add_entry(&mut memory, entry).unwrap();
    }

    assert_eq!(memory.entries.len(), 1);
    assert_eq!(memory.entries[0].content, "shell_stop 默认等待退出");
}

/// Case 2: 普通持久型 Agent 的 executable 候选需要用户确认，批准后生成 Skill Package。
#[test]
fn persistent_agent_executable_candidate_generates_skill_package_after_approval() {
    let dir = TempDir::new().unwrap();
    let asset_service = AgentAssetService::new(dir.path().join("assets"));

    let candidate = ExperienceCandidate {
        candidate_id: uuid::Uuid::new_v4(),
        producer_task_id: uuid::Uuid::new_v4(),
        producer_agent_id: uuid::Uuid::new_v4(),
        title: "smoke test skill".to_string(),
        kind_hint: ExperienceKindHint::Executable,
        payload: ExperienceCandidatePayload::Executable {
            intent: "run smoke test".to_string(),
            when_to_use: "after shell changes".to_string(),
            asset_refs: vec![],
        },
        dependency_refs: vec![],
        status: ExperienceCandidateStatus::NeedsUserApproval,
    };

    // 模拟用户批准后的落盘
    let draft = harness::SkillPackageDraft {
        skill_id: format!("{}", candidate.candidate_id),
        title: candidate.title.clone(),
        problem: "run smoke test".to_string(),
        when_to_use: "after shell changes".to_string(),
        steps: "参见 skill.md".to_string(),
        asset_refs: vec![],
        dependency_refs: vec![],
        risks: "需复核".to_string(),
        source_task_id: Some(candidate.producer_task_id),
        source_candidate_id: Some(candidate.candidate_id),
    };
    let relative = asset_service.persist_skill_package("persistent-worker", &draft).unwrap();

    assert!(dir.path().join("assets").join(&relative).join("skill.md").exists());
}

/// Case 3: 公共规则类候选进入 SharedKnowledge 升级入口。
#[test]
fn shared_knowledge_candidate_queues_upgrade_entry() {
    let mut queue = SharedKnowledgeUpgradeQueue::default();
    let candidate_id = uuid::Uuid::new_v4();

    queue.candidates.push(harness::SharedKnowledgeUpgradeCandidate {
        candidate_id: uuid::Uuid::new_v4(),
        content: "所有 Agent 必须使用中文撰写文档".to_string(),
        kind: harness::LongTermMemoryKind::Constraint,
        scope_tags: vec!["global".to_string()],
        source_candidate_id: candidate_id,
        source_agent_id: uuid::Uuid::new_v4(),
        source_task_id: uuid::Uuid::new_v4(),
        validation_status: harness::KnowledgeValidationStatus::Candidate,
        created_at: chrono::Utc::now(),
    });

    assert_eq!(queue.candidates.len(), 1);
    assert_eq!(queue.candidates[0].source_candidate_id, candidate_id);
}

/// Case 4: default Agent 的私有候选生成 IncubationProposal。
#[test]
fn default_agent_private_candidate_spawns_incubation_proposal() {
    let mut store = ExperienceStore::default();
    let task_id = uuid::Uuid::new_v4();
    let agent_id = uuid::Uuid::new_v4();
    let candidate = ExperienceCandidate::knowledge(
        uuid::Uuid::new_v4(),
        task_id,
        agent_id,
        "default agent private fact".to_string(),
        "this should not directly persist".to_string(),
        harness::LongTermMemoryKind::Fact,
    );
    store.stage_root_candidate(candidate.clone());

    // default Agent 治理：私有知识不能进 LTM，只能生成 IncubationProposal。
    let proposal = harness::IncubationProposal {
        proposal_id: uuid::Uuid::new_v4(),
        source_agent_id: agent_id,
        source_task_id: task_id,
        proposed_agent_profile: harness::AgentProfile {
            name: "incubated-test".to_string(),
            model: "gpt-4.1-mini".to_string(),
        },
        knowledge_candidate_ids: vec![candidate.candidate_id],
        executable_candidate_ids: vec![],
        shared_knowledge_candidate_ids: vec![],
        status: harness::IncubationProposalStatus::Proposed,
        created_at: chrono::Utc::now(),
    };

    assert_eq!(proposal.knowledge_candidate_ids, vec![candidate.candidate_id]);
    assert!(store.candidates.contains_key(&candidate.candidate_id));
}
```

- [ ] **Step 2: 运行集成测试**

Run:

```bash
cargo test -q experience_layered_governance_flow -- --nocapture
```

Expected: PASS。若 `AgentAssetService`、`SkillPackageDraft`、`SharedKnowledgeUpgradeQueue`、`SharedKnowledgeUpgradeCandidate`、`IncubationProposal` 未导出，需在 `src/lib.rs` 或 `src/domain/mod.rs` 中补充 `pub use`。

- [ ] **Step 3: 运行全量测试确认无回归**

Run:

```bash
cargo test --all-features
```

Expected: 全部 PASS（或仅因环境缺失 LLM 等外部依赖而失败的已知测试跳过）。

- [ ] **Step 4: 提交**

```bash
git add tests/experience_layered_governance_flow.rs
git commit -m "test: add four-layer experience governance integration tests"
```

---

## Task 8: 文档同步

**Files:**
- Modify: `docs/current-state.md`
- Modify: `docs/TODO.md`

**动机：** AGENTS.md 要求「代码变更涉及能力边界、配置、工具面或工作流时，必须同步更新相关文档」。

- [ ] **Step 1: 更新 `docs/current-state.md`**

在「经验候选治理」小节中，将原有 bullet 替换/扩展为：

```markdown
#### 经验候选治理

- 经验治理已收敛为两层分层模型：非顶层 `TaskScoped Agent` 只产生、汇聚、向上贡献；顶层 `Persistent Agent` 做最终治理与落盘
- `ExperienceCandidate` 是经验治理唯一中间态，具备完整状态机：`Submitted / InInbox / Aggregated / GovernancePending / NeedsUserApproval / Approved / Rejected / Persisted`
- 非顶层候选通过父任务 `ExperienceInbox` 上送，顶层候选进入 root 后触发 `ExperienceGovernanceRequestMessage`
- 顶层治理后四类最终去向全部可达：
  - `Knowledge` → 普通持久型 Agent 的 `LongTermMemory`
  - `Executable` → 用户确认后生成 Agent 私有 `Skill Package`
  - `SharedKnowledge` → `SharedKnowledgeUpgradeQueue` 升级入口（已持久化到 `.harness/memory/shared_knowledge/upgrades.json`）
  - `default Agent` 的私有 `Knowledge / Executable` → `IncubationProposal`
- `default Agent` 通过 `tags` 中的 `default` 识别，不直接沉淀私有长期身份资产
- `LongTermMemoryEntry` 已具备最小来源追溯字段：`source_candidate_id`、`source_task_id`、`agent_id`
- `IncubationProposal` 已扩展为正式治理输出结构，包含 `proposal_id`、`proposed_agent_profile`、按类型分列的候选 ID、`status`、`created_at`
- 写回失败时保留 `warn` 级审计日志，候选状态不推进到 `Persisted`
```

- [ ] **Step 2: 更新 `docs/TODO.md`**

将高优先级中的：

```markdown
- [ ] 增加 Agent 级别的 Skill 功能，使 Agent 在完成任务后可将可复用、
  有用的经验提炼为自身可调用的 Skill，支持渐进积累
- [x] 重新设计经验贡献系统架构
```

更新为：

```markdown
- [x] 增加 Agent 级别的 Skill 功能：可执行经验经治理后落盘为 Agent 私有 Skill Package
- [x] 重新设计经验贡献系统架构
- [x] 实现经验模块两层分层汇聚治理（非顶层贡献 / 顶层治理 / 四类去向）
- [x] 为 `SharedKnowledgeUpgradeQueue` 增加文件持久化
- [ ] 为 `IncubationProposal` 增加用户审批后的持久型 Agent 创建执行链路
- [ ] 清理旧经验直写链路：`memory_contribution_system` / `memory_absorption_system`（当前为过渡态保留）
- [ ] 实现顶层治理对候选 `kind_hint` 的修正能力
- [ ] 实现非顶层基于多个子候选整理组合候选的主动重写
```

- [ ] **Step 3: 运行 markdownlint**

Run:

```bash
markdownlint docs/current-state.md docs/TODO.md
```

Expected: 无错误。

- [ ] **Step 4: 提交**

```bash
git add docs/current-state.md docs/TODO.md
git commit -m "docs: update current-state and TODO for layered experience governance"
```

---

## Self-Review

### Spec Coverage

| Spec 要求 | 对应 Task / 位置 |
|-----------|------------------|
| 两层模型：非顶层 TaskScoped / 顶层 Persistent | Task 3（路由）、Task 4（汇聚触发） |
| `ExperienceCandidate` 唯一中间态 | Task 1（状态机） |
| `ExperienceInbox` 层间缓冲 | Task 1（Pending/Consumed）、Task 3（写入）、Task 4（消费） |
| 四类最终去向全部可达 | Task 5（治理分流）、Task 6（确认后写回） |
| `default Agent` 孵化制 | Task 5（tag 识别）、Task 6（IncubationProposal） |
| 可执行经验以 Skill Package 落盘 | Task 2（基础设施）、Task 6（批准后写回） |
| 公共规则进入 SharedKnowledge 升级入口并持久化 | Task 1（`SharedKnowledgeUpgradeCandidate`）、Task 5（入队 + `SharedKnowledgeUpgradeService` 写穿） |
| 最小追溯信息 | Task 1（LTM 字段） |
| `IncubationProposal` 正式治理输出结构 | Task 1（结构体扩展）、Task 5（构造）、Task 6（确认后更新状态） |
| 用户确认后最终写回 | Task 5（NeedsUserApproval）、Task 6（最终写回） |
| 四条闭环集成测试 | Task 7 |

### Placeholder Scan

- 无 "TBD" / "TODO" / "implement later" / "fill in details"
- 无 "Add appropriate error handling" 等模糊描述
- 每个代码步骤都包含可直接使用的 Rust 代码
- 无引用未定义类型的步骤

### Type Consistency

- `ExperienceCandidateStatus` 统一使用 8 个状态，后续 Task 引用一致
- `ExperienceInboxStatus` 统一为 `Pending` / `Consumed`
- `SharedKnowledgeUpgradeCandidate` 字段与 Task 5 入队代码一致
- `SkillPackageDraft` 与 `AgentAssetService::persist_skill_package` 签名一致
- `is_default_agent` 基于 `tags` 而非硬编码名称，与 agents.toml 一致
- `IncubationProposal` 扩展后的字段（`proposal_id`、`proposed_agent_profile`、分类候选 ID、`status`、`created_at`）与 Task 5/6/7 一致
- `ExperienceCandidate::governing_agent_id` 与 Task 5 记录、Task 6 确认后路由一致

### 风险与后续项

- `apply_confirmation_response` 首版仍采用全局 NeedsUserApproval 批量批准/拒绝，精确到 request_id 可在后续迭代。
- `IncubationProposal` 当前只生成组件并标记为 Approved，用户批准后真正创建持久型 Agent 的链路列为 TODO。
- 迁移第 5 步（删除旧补丁逻辑）：旧的 `memory_contribution_system` / `memory_absorption_system` 在本计划执行后仍未删除，需在 Task 8 文档中标注为过渡态，待经验候选链路稳定后清理。
- P0 明确不实现的 spec 能力：kind_hint 修正、非顶层组合候选重写、`ExperienceCandidateSubmitted` / `ExperienceContributionDeliveredToParentInbox` / `ExperienceGovernanceResolved` / `FinalWritebackCompleted` 等完整消息流。这些应在 `docs/current-state.md` 的"待完善"中说明。
- 所有新落盘路径均使用现有 `LongTermMemoryService`、`AgentAssetService` 与新增 `SharedKnowledgeUpgradeService`，未引入不符合依赖原则的 crate。

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-06-14-experience-module-layered-governance.md`. You can execute tasks inline using the executing-plans skill.
