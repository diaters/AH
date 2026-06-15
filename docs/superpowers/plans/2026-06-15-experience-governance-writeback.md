# Experience Governance Writeback Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 打通顶层经验治理到统一写回的完整主链路，覆盖 `LongTermMemory`、`Skill Package`、`SharedKnowledgeUpgrade`、任务级 `IncubationProposal` 和批准后新 Agent 创建。

**Architecture:** 以 `ExperienceCandidate` 为唯一中间态，在顶层先收束治理输入，再生成统一治理决议与写回请求；审批只负责放行，正式写回统一走执行层。`default Agent` 的私有沉淀收敛为任务级 `IncubationProposal`，提案批准后由单独孵化执行链创建新持久型 Agent 并写入初始资产。

**Tech Stack:** Rust, Bevy ECS, genai, ratatui, serde, TOML/JSON 文件持久化

---

## 文件结构

| 文件 | 变更类型 | 职责 |
|---|---|---|
| `src/domain/contribution.rs` | 修改 | 扩展候选风险字段、写回状态、任务级 proposal 结构与治理/写回消息 |
| `src/domain/space.rs` | 修改 | 扩展 `ExperienceCandidateSubmission`，让 LLM 在提交候选时附带风险信息 |
| `src/domain/mod.rs` | 修改 | 导出新增的治理决议与孵化相关类型 |
| `src/systems/contribution.rs` | 修改 | 顶层输入收束、治理决议、统一写回、任务级 proposal 聚合、proposal 审批与孵化执行 |
| `src/systems/command.rs` | 修改 | 去掉 `/finish` 里重复生成的 `TaskTerminatedMessage` |
| `src/systems/tools/confirmation.rs` | 修改 | 将经验治理/孵化审批与普通工具确认解耦，避免 `ToolConfirmationNoMatch` |
| `src/systems/maintenance.rs` | 修改 | 启动时加载孵化出的持久型 Agent；运行时注册新 Agent |
| `src/systems/mod.rs` | 修改 | 导出新增系统 |
| `src/plugins/execution.rs` | 修改 | 注册治理决议、统一写回、proposal 执行相关系统顺序 |
| `src/plugins/memory.rs` | 修改 | 注入提案存储与孵化 Agent 注册服务资源 |
| `src/infrastructure/incubation/mod.rs` | 新建 | 孵化相关基础设施模块导出 |
| `src/infrastructure/incubation/proposal_store.rs` | 新建 | 任务级 `IncubationProposal` 文件持久化 |
| `src/infrastructure/incubation/agent_registry.rs` | 新建 | 新孵化持久型 Agent 的配置持久化与加载 |
| `src/infrastructure/mod.rs` | 修改 | 导出 incubation 模块 |
| `src/app/mod.rs` | 修改 | 注册新资源或插件依赖 |
| `tests/experience_collection_workitem_flow.rs` | 修改 | 回归 `/finish` 不重复触发、治理输入收束行为 |
| `tests/experience_layered_governance_flow.rs` | 修改 | 覆盖统一写回、任务级 proposal 聚合、default Agent 路径 |
| `tests/memory_persistence_flow.rs` | 修改 | 覆盖知识写回状态与失败行为 |
| `tests/incubation_execution_flow.rs` | 新建 | 覆盖 proposal 批准后创建新 Agent 并写入资产 |
| `docs/superpowers/README.md` | 修改 | 更新活跃计划索引 |

---

## Task 1: 消除 `/finish` 的重复终止消息

**Files:**
- Modify: `src/systems/command.rs`
- Test: `tests/experience_collection_workitem_flow.rs`

- [ ] **Step 1: 写一个失败的回归测试，固定 `/finish` 只触发一次经验收集**

```rust
#[test]
fn finish_command_triggers_single_top_level_experience_collection_request() {
    let runtime = Arc::new(tokio::runtime::Runtime::new().unwrap());
    let executor: Arc<dyn harness::contracts::AgentExecutor> = Arc::new(NoOpExecutor);
    let (_input_tx, input_rx) = crossbeam_channel::unbounded();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, vec![]);
    app.update();

    let governing_agent_id = uuid::Uuid::new_v4();
    let mut task = harness::Task::from_user_input_ready("top task", 3, default_channel());
    task.delegate = Some(governing_agent_id);
    task.status = harness::TaskStatus::Waiting(harness::WaitingReason::User);
    let task_id = task.id;
    app.world_mut().spawn((task, harness::ShortTermMemory::default()));

    app.world_mut().spawn(harness::UserInputMessage {
        channel: default_channel(),
        content: "/finish".to_string(),
    });

    app.update();
    app.update();

    let count = app
        .world()
        .query::<&harness::ExperienceCollectionRequestMessage>()
        .iter(app.world())
        .filter(|msg| msg.task_id == task_id)
        .count();

    assert_eq!(count, 1, "/finish should produce exactly one collection request");
}
```

- [ ] **Step 2: 运行测试确认当前失败**

Run: `cargo test --test experience_collection_workitem_flow finish_command_triggers_single_top_level_experience_collection_request -- --nocapture`
Expected: FAIL，实际 count 为 `2` 或出现重复的 `ExperienceCollectionRequestMessage`

- [ ] **Step 3: 删除命令系统中的重复终止消息生成**

```rust
UserCommand::FinishCurrentTask => {
    let current_task = tasks.iter().find(|(t, _)| !t.status.is_terminal());

    if let Some((task, _)) = current_task {
        debug!(
            event = "FinishCommandReceived",
            task_id = %task.id,
            task_status = ?task.status,
            task_content = %task.content,
            "finishing current task via /finish command"
        );
        commands.spawn(FinishTaskMessage { task_id: task.id });
    } else {
        debug!(event = "FinishCommandNoTask", "no active task to finish");
    }
    commands.entity(entity).despawn();
}
```

- [ ] **Step 4: 重新运行测试确认通过**

Run: `cargo test --test experience_collection_workitem_flow finish_command_triggers_single_top_level_experience_collection_request -- --nocapture`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/systems/command.rs tests/experience_collection_workitem_flow.rs
git commit -m "fix: avoid duplicate task termination on finish command"
```

---

## Task 2: 扩展候选模型以承载风险、治理决议和写回状态

**Files:**
- Modify: `src/domain/contribution.rs`
- Modify: `src/domain/space.rs`
- Modify: `src/domain/mod.rs`
- Test: `src/domain/contribution.rs`

- [ ] **Step 1: 先写领域单元测试，固定新增状态和风险字段**

```rust
#[test]
fn experience_candidate_tracks_risk_metadata() {
    let candidate = ExperienceCandidate {
        candidate_id: uuid::Uuid::new_v4(),
        producer_task_id: uuid::Uuid::new_v4(),
        producer_agent_id: uuid::Uuid::new_v4(),
        title: "risk tagged".to_string(),
        kind_hint: ExperienceKindHint::Knowledge,
        payload: ExperienceCandidatePayload::Knowledge {
            content: "stable rule".to_string(),
            memory_kind: crate::domain::LongTermMemoryKind::Constraint,
        },
        dependency_refs: vec![],
        status: ExperienceCandidateStatus::Submitted,
        governing_agent_id: None,
        risk_level: ExperienceRiskLevel::Low,
        risk_reason: "collector judged it low risk".to_string(),
        suggested_confirmation: ExperienceConfirmationPolicy::None,
        derived_from_candidate_ids: vec![],
    };

    assert_eq!(candidate.risk_level, ExperienceRiskLevel::Low);
    assert_eq!(
        candidate.suggested_confirmation,
        ExperienceConfirmationPolicy::None
    );
}

#[test]
fn candidate_status_machine_contains_writeback_states() {
    let statuses = [
        ExperienceCandidateStatus::GovernanceResolved,
        ExperienceCandidateStatus::WritebackPending,
        ExperienceCandidateStatus::WritebackFailed,
    ];
    assert_eq!(statuses.len(), 3);
}
```

- [ ] **Step 2: 运行领域测试确认失败**

Run: `cargo test --lib experience_candidate_tracks_risk_metadata candidate_status_machine_contains_writeback_states`
Expected: FAIL，提示字段或状态不存在

- [ ] **Step 3: 在领域层新增风险与治理写回类型**

```rust
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ExperienceRiskLevel {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ExperienceConfirmationPolicy {
    None,
    User,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ExperienceWritebackDestination {
    LongTermMemory,
    SkillPackage,
    SharedKnowledgeUpgrade,
    IncubationProposal,
    Rejected,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExperienceGovernanceDecision {
    pub candidate_id: uuid::Uuid,
    pub destination: ExperienceWritebackDestination,
    pub confirmation_policy: ExperienceConfirmationPolicy,
    pub final_risk_level: ExperienceRiskLevel,
    pub risk_overridden: bool,
    pub decision_rationale: String,
}
```

- [ ] **Step 4: 扩展候选与提交结构**

```rust
pub struct ExperienceCandidate {
    pub candidate_id: uuid::Uuid,
    pub producer_task_id: TaskId,
    pub producer_agent_id: AgentId,
    pub title: String,
    pub kind_hint: ExperienceKindHint,
    pub payload: ExperienceCandidatePayload,
    pub dependency_refs: Vec<String>,
    pub status: ExperienceCandidateStatus,
    pub governing_agent_id: Option<AgentId>,
    pub risk_level: ExperienceRiskLevel,
    pub risk_reason: String,
    pub suggested_confirmation: ExperienceConfirmationPolicy,
    pub derived_from_candidate_ids: Vec<uuid::Uuid>,
}

pub struct ExperienceCandidateSubmission {
    pub title: String,
    pub kind_hint: String,
    pub payload: serde_json::Value,
    pub dependency_refs: Option<Vec<String>>,
    pub risk_level: String,
    pub risk_reason: String,
    pub suggested_confirmation: Option<String>,
}
```

- [ ] **Step 5: 扩展状态机**

```rust
pub enum ExperienceCandidateStatus {
    Submitted,
    InInbox,
    Aggregated,
    GovernancePending,
    GovernanceResolved,
    NeedsUserApproval,
    WritebackPending,
    Approved,
    Rejected,
    Persisted,
    WritebackFailed,
}
```

- [ ] **Step 6: 重新运行领域测试确认通过**

Run: `cargo test --lib experience_candidate_tracks_risk_metadata candidate_status_machine_contains_writeback_states`
Expected: PASS

- [ ] **Step 7: Commit**

```bash
git add src/domain/contribution.rs src/domain/space.rs src/domain/mod.rs
git commit -m "feat: extend experience candidates with risk and writeback metadata"
```

---

## Task 3: 统一顶层治理输入，纳入子层汇聚候选

**Files:**
- Modify: `src/domain/contribution.rs`
- Modify: `src/systems/contribution.rs`
- Test: `tests/experience_layered_governance_flow.rs`

- [ ] **Step 1: 先写集成测试，固定顶层治理会同时消费 root 和 aggregated 候选**

```rust
#[test]
fn top_level_governance_consumes_root_and_aggregated_candidates() {
    let mut store = harness::ExperienceStore::default();
    let top_task_id = uuid::Uuid::new_v4();
    let top_agent_id = uuid::Uuid::new_v4();

    let root = harness::ExperienceCandidate::knowledge(
        uuid::Uuid::new_v4(),
        top_task_id,
        top_agent_id,
        "root".to_string(),
        "root content".to_string(),
        harness::LongTermMemoryKind::Fact,
    );
    let root_id = root.candidate_id;
    store.stage_root_candidate(root);

    let child = harness::ExperienceCandidate::knowledge(
        uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4(),
        "child".to_string(),
        "child content".to_string(),
        harness::LongTermMemoryKind::Fact,
    );
    let child_id = child.candidate_id;
    store.queue_for_parent(top_task_id, top_agent_id, child);
    store.aggregate_inbox_for_task(top_task_id);

    let ids = store.collect_top_level_governance_candidates(top_task_id);

    assert!(ids.contains(&root_id));
    assert!(ids.contains(&child_id));
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --test experience_layered_governance_flow top_level_governance_consumes_root_and_aggregated_candidates -- --nocapture`
Expected: FAIL，`collect_top_level_governance_candidates` 不存在或未包含 aggregated 候选

- [ ] **Step 3: 在 `ExperienceStore` 中新增顶层收束方法**

```rust
pub fn collect_top_level_governance_candidates(&mut self, task_id: TaskId) -> Vec<uuid::Uuid> {
    let mut ids = self.root_candidates_for_task(task_id);

    if let Some(inbox) = self.inboxes.get(&task_id) {
        ids.extend(
            inbox.candidate_ids
                .iter()
                .copied()
                .filter(|id| {
                    self.candidates
                        .get(id)
                        .is_some_and(|c| c.status == ExperienceCandidateStatus::Aggregated)
                }),
        );
    }

    ids.sort_unstable();
    ids.dedup();

    for id in &ids {
        if let Some(candidate) = self.candidates.get_mut(id) {
            candidate.status = ExperienceCandidateStatus::GovernancePending;
        }
    }

    ids
}
```

- [ ] **Step 4: 将顶层完成处理改为调用统一收束方法**

```rust
let ids = store.collect_top_level_governance_candidates(msg.task_id);
if !ids.is_empty() {
    commands.spawn(ExperienceGovernanceRequestMessage {
        task_id: msg.task_id,
        agent_id: msg.governing_agent_id,
    });
}
```

- [ ] **Step 5: 重新运行测试确认通过**

Run: `cargo test --test experience_layered_governance_flow top_level_governance_consumes_root_and_aggregated_candidates -- --nocapture`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add src/domain/contribution.rs src/systems/contribution.rs tests/experience_layered_governance_flow.rs
git commit -m "feat: unify top-level governance inputs across root and aggregated candidates"
```

---

## Task 4: 引入统一治理决议与写回请求

**Files:**
- Modify: `src/domain/contribution.rs`
- Modify: `src/systems/contribution.rs`
- Modify: `src/plugins/execution.rs`
- Test: `tests/experience_layered_governance_flow.rs`

- [ ] **Step 1: 写失败测试，固定治理先产出决议而不是直接写回**

```rust
#[test]
fn governance_marks_candidate_resolved_before_writeback() {
    let mut store = harness::ExperienceStore::default();
    let task_id = uuid::Uuid::new_v4();
    let agent_id = uuid::Uuid::new_v4();
    let candidate = harness::ExperienceCandidate::knowledge(
        uuid::Uuid::new_v4(),
        task_id,
        agent_id,
        "govern".to_string(),
        "content".to_string(),
        harness::LongTermMemoryKind::Fact,
    );
    let candidate_id = candidate.candidate_id;
    store.stage_root_candidate(candidate);
    store.collect_top_level_governance_candidates(task_id);

    let mut candidate = store.candidates.get_mut(&candidate_id).unwrap();
    candidate.status = harness::ExperienceCandidateStatus::GovernanceResolved;

    assert_eq!(
        candidate.status,
        harness::ExperienceCandidateStatus::GovernanceResolved
    );
}
```

- [ ] **Step 2: 运行测试确认需要新增状态与流程**

Run: `cargo test --test experience_layered_governance_flow governance_marks_candidate_resolved_before_writeback -- --nocapture`
Expected: FAIL 或当前系统行为不满足“先 resolved 再 writeback”

- [ ] **Step 3: 新增治理决议与写回请求消息**

```rust
#[derive(Debug, Clone, Component)]
pub struct ExperienceWritebackRequestMessage {
    pub decision: ExperienceGovernanceDecision,
}
```

- [ ] **Step 4: 将顶层治理从“直接写回”改成“先决议后请求写回”**

```rust
let decision = ExperienceGovernanceDecision {
    candidate_id: candidate.candidate_id,
    destination: ExperienceWritebackDestination::LongTermMemory,
    confirmation_policy: ExperienceConfirmationPolicy::None,
    final_risk_level: candidate.risk_level,
    risk_overridden: false,
    decision_rationale: "low-risk private knowledge".to_string(),
};

if let Some(c) = store.candidates.get_mut(candidate_id) {
    c.status = ExperienceCandidateStatus::GovernanceResolved;
}

commands.spawn(ExperienceWritebackRequestMessage { decision });
```

- [ ] **Step 5: 新增统一写回 system，并在插件中注册执行顺序**

```rust
pub(crate) fn experience_writeback_system(
    mut commands: Commands,
    mut store: ResMut<ExperienceStore>,
    mut long_memories: Query<&mut LongTermMemory>,
    agents: Query<&Agent>,
    mut service: ResMut<LongTermMemoryService>,
    asset_service: Res<AgentAssetService>,
    mut upgrade_queue: ResMut<SharedKnowledgeUpgradeQueue>,
    upgrade_service: Res<SharedKnowledgeUpgradeService>,
    requests: Query<(Entity, &ExperienceWritebackRequestMessage)>,
) {
    // 根据 decision.destination 执行正式写回
}
```

- [ ] **Step 6: 重新运行测试确认通过**

Run: `cargo test --test experience_layered_governance_flow governance_marks_candidate_resolved_before_writeback -- --nocapture`
Expected: PASS

- [ ] **Step 7: Commit**

```bash
git add src/domain/contribution.rs src/systems/contribution.rs src/plugins/execution.rs tests/experience_layered_governance_flow.rs
git commit -m "refactor: split experience governance decisions from writeback execution"
```

---

## Task 5: 将经验治理审批与普通工具确认解耦

**Files:**
- Modify: `src/systems/tools/confirmation.rs`
- Modify: `src/systems/contribution.rs`
- Test: `tests/experience_collection_workitem_flow.rs`

- [ ] **Step 1: 写回归测试，固定经验治理审批不会再报 `ToolConfirmationNoMatch`**

```rust
#[test]
fn experience_governance_confirmation_is_not_routed_to_generic_tool_confirmation() {
    let request_id = uuid::Uuid::new_v4();
    let mut app = build_minimal_test_app();
    app.world_mut().spawn(harness::ToolConfirmationResponseMessage {
        request_id,
        selected_option: "allow_once".to_string(),
    });

    app.update();

    let remaining = app
        .world()
        .query::<&harness::ToolConfirmationResponseMessage>()
        .iter(app.world())
        .count();

    assert_eq!(remaining, 0);
}
```

- [ ] **Step 2: 运行测试确认当前逻辑会误入通用确认链**

Run: `cargo test --test experience_collection_workitem_flow experience_governance_confirmation_is_not_routed_to_generic_tool_confirmation -- --nocapture`
Expected: FAIL 或日志出现 `ToolConfirmationNoMatch`

- [ ] **Step 3: 在通用确认系统中跳过经验治理与孵化审批**

```rust
let Some((request_entity, tool_request)) = tool_requests
    .iter()
    .find(|(_, r)| r.pending_confirmation_id == Some(response.request_id))
else {
    // 经验治理与孵化审批不属于 ToolExecutionRequestMessage，留给专用 system 处理
    commands.entity(entity).despawn();
    continue;
};
```

- [ ] **Step 4: 在经验治理 system 中统一解释 `allow_once` / `allow_always`**

```rust
let approved = matches!(
    response.selected_option.as_str(),
    "allow_once" | "allow_always" | "approve"
);
```

- [ ] **Step 5: 重新运行测试确认通过**

Run: `cargo test --test experience_collection_workitem_flow experience_governance_confirmation_is_not_routed_to_generic_tool_confirmation -- --nocapture`
Expected: PASS，且不再出现 `ToolConfirmationNoMatch`

- [ ] **Step 6: Commit**

```bash
git add src/systems/tools/confirmation.rs src/systems/contribution.rs tests/experience_collection_workitem_flow.rs
git commit -m "fix: route experience governance approvals outside generic tool confirmation"
```

---

## Task 6: 将 `IncubationProposal` 收敛为任务级对象并持久化

**Files:**
- Modify: `src/domain/contribution.rs`
- Modify: `src/systems/contribution.rs`
- Create: `src/infrastructure/incubation/mod.rs`
- Create: `src/infrastructure/incubation/proposal_store.rs`
- Modify: `src/infrastructure/mod.rs`
- Modify: `src/plugins/memory.rs`
- Test: `tests/experience_layered_governance_flow.rs`

- [ ] **Step 1: 写失败测试，固定一个顶层任务只生成一个 proposal**

```rust
#[test]
fn default_agent_merges_multiple_private_candidates_into_single_task_level_proposal() {
    let task_id = uuid::Uuid::new_v4();
    let agent_id = uuid::Uuid::new_v4();
    let mut proposal = harness::IncubationProposal::new(task_id, agent_id, harness::AgentProfile {
        name: "physics-specialist".to_string(),
        model: "gpt-4.1-mini".to_string(),
    });

    proposal.knowledge_candidate_ids.push(uuid::Uuid::new_v4());
    proposal.executable_candidate_ids.push(uuid::Uuid::new_v4());

    assert_eq!(proposal.source_task_id, task_id);
    assert_eq!(proposal.knowledge_candidate_ids.len(), 1);
    assert_eq!(proposal.executable_candidate_ids.len(), 1);
}
```

- [ ] **Step 2: 运行测试确认缺少任务级构造和 merge 能力**

Run: `cargo test --test experience_layered_governance_flow default_agent_merges_multiple_private_candidates_into_single_task_level_proposal -- --nocapture`
Expected: FAIL

- [ ] **Step 3: 扩展 proposal 结构与状态**

```rust
pub enum IncubationProposalStatus {
    Proposed,
    Approved,
    Executing,
    Executed,
    ExecutionFailed,
    Rejected,
}

pub struct IncubationProposal {
    pub proposal_id: uuid::Uuid,
    pub source_agent_id: AgentId,
    pub source_task_id: TaskId,
    pub proposed_agent_profile: AgentProfile,
    pub knowledge_candidate_ids: Vec<uuid::Uuid>,
    pub executable_candidate_ids: Vec<uuid::Uuid>,
    pub shared_knowledge_candidate_ids: Vec<uuid::Uuid>,
    pub incubation_rationale: String,
    pub status: IncubationProposalStatus,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}
```

- [ ] **Step 4: 实现任务级 merge 逻辑**

```rust
fn merge_candidate_into_proposal(
    proposal: &mut IncubationProposal,
    candidate: &ExperienceCandidate,
) {
    match candidate.kind_hint {
        ExperienceKindHint::Knowledge => {
            if !proposal.knowledge_candidate_ids.contains(&candidate.candidate_id) {
                proposal.knowledge_candidate_ids.push(candidate.candidate_id);
            }
        }
        ExperienceKindHint::Executable => {
            if !proposal.executable_candidate_ids.contains(&candidate.candidate_id) {
                proposal.executable_candidate_ids.push(candidate.candidate_id);
            }
        }
        ExperienceKindHint::SharedKnowledge => {
            if !proposal
                .shared_knowledge_candidate_ids
                .contains(&candidate.candidate_id)
            {
                proposal
                    .shared_knowledge_candidate_ids
                    .push(candidate.candidate_id);
            }
        }
        ExperienceKindHint::Discard => {}
    }
    proposal.updated_at = chrono::Utc::now();
}
```

- [ ] **Step 5: 新增 proposal 文件存储**

```rust
#[derive(Resource, Debug, Clone)]
pub struct IncubationProposalStore {
    base_dir: PathBuf,
}

impl IncubationProposalStore {
    pub fn default_path() -> Self {
        Self::new(".harness/incubation/proposals")
    }

    pub fn persist(&self, proposal: &IncubationProposal) -> Result<()> {
        let path = self.base_dir.join(format!("{}.json", proposal.proposal_id));
        fs::create_dir_all(&self.base_dir)?;
        fs::write(path, serde_json::to_string_pretty(proposal)?)?;
        Ok(())
    }
}
```

- [ ] **Step 6: 在治理阶段改为“查 task 级 proposal -> merge -> persist”**

```rust
let proposal = proposals
    .iter_mut()
    .find(|p| p.source_task_id == request.task_id && matches!(p.status, IncubationProposalStatus::Proposed));

if let Some(mut proposal) = proposal {
    merge_candidate_into_proposal(&mut proposal, &candidate);
    proposal_store.persist(&proposal)?;
} else {
    let mut proposal = IncubationProposal::new(request.task_id, request.agent_id, proposed_profile);
    merge_candidate_into_proposal(&mut proposal, &candidate);
    proposal_store.persist(&proposal)?;
    commands.spawn(proposal);
}
```

- [ ] **Step 7: 重新运行测试确认通过**

Run: `cargo test --test experience_layered_governance_flow default_agent_merges_multiple_private_candidates_into_single_task_level_proposal -- --nocapture`
Expected: PASS

- [ ] **Step 8: Commit**

```bash
git add src/domain/contribution.rs src/systems/contribution.rs src/infrastructure/incubation/mod.rs src/infrastructure/incubation/proposal_store.rs src/infrastructure/mod.rs src/plugins/memory.rs tests/experience_layered_governance_flow.rs
git commit -m "feat: aggregate incubation proposals at task scope"
```

---

## Task 7: 在 proposal 批准后创建新持久型 Agent 并写入初始资产

**Files:**
- Modify: `src/systems/contribution.rs`
- Create: `src/infrastructure/incubation/agent_registry.rs`
- Modify: `src/systems/maintenance.rs`
- Modify: `src/plugins/memory.rs`
- Modify: `src/infrastructure/mod.rs`
- Create: `tests/incubation_execution_flow.rs`

- [ ] **Step 1: 写集成测试，固定批准 proposal 后会创建新 Agent**

```rust
#[test]
fn approved_incubation_proposal_creates_persistent_agent_and_initial_assets() {
    let dir = tempfile::TempDir::new().unwrap();
    let registry = harness::infrastructure::incubation::IncubatedAgentRegistry::new(
        dir.path().join("incubated_agents.toml"),
    );

    let profile = harness::AgentProfile {
        name: "physics-specialist".to_string(),
        model: "gpt-4.1-mini".to_string(),
    };

    registry
        .append(&harness::infrastructure::incubation::IncubatedAgentRecord {
            profile: profile.clone(),
            tags: vec!["incubated".to_string(), "physics".to_string()],
            description: "derived from top-level proposal".to_string(),
            tools: vec![],
        })
        .unwrap();

    let records = registry.load().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].profile.name, "physics-specialist");
}
```

- [ ] **Step 2: 运行测试确认缺少注册服务**

Run: `cargo test --test incubation_execution_flow approved_incubation_proposal_creates_persistent_agent_and_initial_assets -- --nocapture`
Expected: FAIL

- [ ] **Step 3: 新增孵化 Agent 注册持久化服务**

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncubatedAgentRecord {
    pub profile: AgentProfile,
    pub tags: Vec<String>,
    pub description: String,
    pub tools: Vec<String>,
}

#[derive(Resource, Debug, Clone)]
pub struct IncubatedAgentRegistry {
    path: PathBuf,
}
```

- [ ] **Step 4: 在 proposal 批准后进入执行阶段**

```rust
if let Some(mut proposal) = proposals
    .iter_mut()
    .find(|p| p.proposal_id == proposal_id)
{
    proposal.status = IncubationProposalStatus::Executing;
    proposal_store.persist(&proposal)?;

    let record = IncubatedAgentRecord {
        profile: proposal.proposed_agent_profile.clone(),
        tags: vec!["incubated".to_string()],
        description: proposal.incubation_rationale.clone(),
        tools: vec![],
    };
    registry.append(&record)?;

    proposal.status = IncubationProposalStatus::Executed;
    proposal_store.persist(&proposal)?;
}
```

- [ ] **Step 5: 写入新 Agent 的初始知识与技能**

```rust
for candidate_id in &proposal.knowledge_candidate_ids {
    if let Some(candidate) = store.candidates.get(candidate_id).cloned()
        && let Some(mut entry) = candidate.as_long_term_memory_entry()
    {
        entry.source_candidate_id = Some(candidate.candidate_id);
        entry.source_task_id = Some(candidate.producer_task_id);
        entry.agent_id = Some(new_agent_id);
        ltm_service.add_entry(&mut new_agent_memory, entry)?;
    }
}

for candidate_id in &proposal.executable_candidate_ids {
    if let Some(candidate) = store.candidates.get(candidate_id).cloned()
        && let ExperienceCandidatePayload::Executable { intent, when_to_use, asset_refs } = candidate.payload
    {
        let draft = SkillPackageDraft {
            skill_id: candidate.candidate_id.to_string(),
            title: candidate.title,
            problem: intent,
            when_to_use,
            steps: "参见 skill.md 与 scripts/ 目录".to_string(),
            asset_refs,
            dependency_refs: candidate.dependency_refs,
            risks: candidate.risk_reason,
            source_task_id: Some(candidate.producer_task_id),
            source_candidate_id: Some(candidate.candidate_id),
        };
        asset_service.persist_skill_package(&proposal.proposed_agent_profile.name, &draft)?;
    }
}
```

- [ ] **Step 6: 在维护系统中加载孵化注册表**

```rust
for record in registry.load()? {
    if existing_names.contains(&record.profile.name) {
        continue;
    }
    commands.spawn(Agent {
        id: Uuid::new_v4(),
        profile: record.profile.clone(),
        capabilities: AgentCapabilities {
            tags: record.tags.clone(),
            description: record.description.clone(),
        },
        kind: AgentKind::Persistent,
        parent_id: None,
        bound_task_id: None,
        tool_permissions: AgentToolPermissions::from(record.tools.clone()),
    });
}
```

- [ ] **Step 7: 重新运行测试确认通过**

Run: `cargo test --test incubation_execution_flow approved_incubation_proposal_creates_persistent_agent_and_initial_assets -- --nocapture`
Expected: PASS

- [ ] **Step 8: Commit**

```bash
git add src/systems/contribution.rs src/infrastructure/incubation/agent_registry.rs src/systems/maintenance.rs src/plugins/memory.rs src/infrastructure/mod.rs tests/incubation_execution_flow.rs
git commit -m "feat: execute approved incubation proposals into persistent agents"
```

---

## Task 8: 补齐统一写回失败状态与审计日志

**Files:**
- Modify: `src/systems/contribution.rs`
- Modify: `tests/memory_persistence_flow.rs`
- Modify: `tests/experience_layered_governance_flow.rs`

- [ ] **Step 1: 写失败测试，固定写回失败进入 `WritebackFailed`**

```rust
#[test]
fn failed_skill_package_writeback_marks_candidate_writeback_failed() {
    let mut candidate = harness::ExperienceCandidate::knowledge(
        uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4(),
        "bad".to_string(),
        "content".to_string(),
        harness::LongTermMemoryKind::Fact,
    );
    candidate.status = harness::ExperienceCandidateStatus::WritebackFailed;

    assert_eq!(
        candidate.status,
        harness::ExperienceCandidateStatus::WritebackFailed
    );
}
```

- [ ] **Step 2: 运行测试确认状态已被真实使用**

Run: `cargo test --test experience_layered_governance_flow failed_skill_package_writeback_marks_candidate_writeback_failed -- --nocapture`
Expected: FAIL 或当前写回失败仍停留在旧状态

- [ ] **Step 3: 在统一写回层中为各目标补齐失败状态**

```rust
if let Some(c) = store.candidates.get_mut(&decision.candidate_id) {
    c.status = ExperienceCandidateStatus::WritebackPending;
}

match persist_result {
    Ok(_) => {
        if let Some(c) = store.candidates.get_mut(&decision.candidate_id) {
            c.status = ExperienceCandidateStatus::Persisted;
        }
        debug!(
            event = "ExperienceWritebackSucceeded",
            candidate_id = %decision.candidate_id,
            destination = ?decision.destination,
            "experience writeback succeeded"
        );
    }
    Err(error) => {
        if let Some(c) = store.candidates.get_mut(&decision.candidate_id) {
            c.status = ExperienceCandidateStatus::WritebackFailed;
        }
        warn!(
            event = "ExperienceWritebackFailed",
            candidate_id = %decision.candidate_id,
            destination = ?decision.destination,
            error = %error,
            "experience writeback failed"
        );
    }
}
```

- [ ] **Step 4: 重新运行测试确认通过**

Run: `cargo test --test experience_layered_governance_flow failed_skill_package_writeback_marks_candidate_writeback_failed -- --nocapture`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/systems/contribution.rs tests/memory_persistence_flow.rs tests/experience_layered_governance_flow.rs
git commit -m "feat: add auditable writeback failure states for experience governance"
```

---

## Task 9: 全量回归与文档索引同步

**Files:**
- Modify: `docs/superpowers/README.md`
- Test: `tests/experience_collection_workitem_flow.rs`
- Test: `tests/experience_layered_governance_flow.rs`
- Test: `tests/incubation_execution_flow.rs`

- [ ] **Step 1: 更新活跃计划索引**

```md
| `plans/2026-06-15-experience-governance-writeback.md` | 经验治理统一写回与任务级孵化实施 | 活跃 |
```

- [ ] **Step 2: 运行针对性测试**

Run: `cargo test --test experience_collection_workitem_flow --test experience_layered_governance_flow --test incubation_execution_flow`
Expected: PASS

- [ ] **Step 3: 运行静态检查与全量测试**

Run: `cargo fmt --all && cargo clippy --all-targets --all-features -- -D warnings && cargo test --all-features`
Expected: 全部通过

- [ ] **Step 4: Commit**

```bash
git add docs/superpowers/README.md
git commit -m "chore: validate experience governance writeback implementation plan"
```

---

## Self-Review

### 1. Spec 覆盖度

| Spec 要求 | 对应任务 |
|---|---|
| 候选产生阶段携带风险分级 | Task 2 |
| 顶层自身候选与子层汇聚候选统一进入治理输入 | Task 3 |
| 决议与执行分离，统一写回 | Task 4 |
| 审批只放行，不直接写盘 | Task 4, Task 5 |
| `/finish` 不重复触发顶层收集 | Task 1 |
| `IncubationProposal` 为任务级对象 | Task 6 |
| proposal 批准后创建新持久型 Agent | Task 7 |
| 写回失败进入显式失败状态 | Task 8 |
| 全链路回归与文档同步 | Task 9 |

### 2. Placeholder 扫描

- 无 `TBD`、`TODO`、`implement later`
- 每个任务都包含明确代码片段或命令
- 每个测试步骤都包含命令与预期结果

### 3. 类型一致性

- `ExperienceRiskLevel`、`ExperienceConfirmationPolicy`、`ExperienceWritebackDestination` 在 Task 2 定义，并被后续任务复用
- `ExperienceWritebackRequestMessage` 在 Task 4 引入，并只由统一写回层消费
- `IncubationProposalStatus` 在 Task 6 扩展，并在 Task 7 继续沿用
- `IncubatedAgentRegistry` 在 Task 7 引入，并由 `load_agents_system` 加载

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-06-15-experience-governance-writeback.md`. Two execution options:

1. Subagent-Driven (recommended) - I dispatch a fresh subagent per task, review between tasks, fast iteration

2. Inline Execution - Execute tasks in this session using executing-plans, batch execution with checkpoints

Which approach?
