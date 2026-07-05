> **状态：已归档** — 对应功能已合并到 main，归档于 2026-07-05

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` (recommended) or `superpowers:executing-plans` to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

# 经验治理子任务候选写回修复实施计划

**Goal:** 修复经验治理模块中子任务候选无法正确找到父任务 IncubationProposal、以及多候选写回状态竞争导致写回失败的问题。

**Architecture:** 在 `ExperienceGovernanceDecision` 中显式携带 `source_task_id`，使审批和写回链路都按统一的治理任务 ID 定位 proposal；`writeback_incubation_proposal` 改为按 task_id 索引并对 `Executing`/`Executed` 状态幂等。

**Tech Stack:** Rust, Bevy ECS, tempfile, tokio

---

## 文件变更清单

| 文件 | 职责 |
|---|---|
| `src/domain/contribution.rs` | `ExperienceGovernanceDecision` 增加 `source_task_id` |
| `src/systems/contribution.rs` | 构造 decision、审批定位、写回函数签名与逻辑 |
| `tests/experience_layered_governance_flow.rs` | 更新现有测试构造 + 新增子任务聚合多候选测试 |

---

## Task 1: `ExperienceGovernanceDecision` 增加 `source_task_id`

**Files:**
- Modify: `src/domain/contribution.rs:113-121`

- [ ] **Step 1: 修改结构体定义**

```rust
#[derive(Debug, Clone, Component, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExperienceGovernanceDecision {
    pub candidate_id: uuid::Uuid,
    pub destination: ExperienceWritebackDestination,
    pub confirmation_policy: ExperienceConfirmationPolicy,
    pub final_risk_level: ExperienceRiskLevel,
    pub risk_overridden: bool,
    pub decision_rationale: String,
    pub source_task_id: TaskId,
}
```

- [ ] **Step 2: Commit**

```bash
git add src/domain/contribution.rs
git commit -m "feat: add source_task_id to ExperienceGovernanceDecision"
```

---

## Task 2: 构造 decision 时填入 `source_task_id`

**Files:**
- Modify: `src/systems/contribution.rs:391-398, 403-410, 412-418, 422-446`

- [ ] **Step 1: 修改 SharedKnowledge 分支**

```rust
ExperienceGovernanceDecision {
    candidate_id: *candidate_id,
    destination: ExperienceWritebackDestination::SharedKnowledgeUpgrade,
    confirmation_policy,
    final_risk_level: candidate.risk_level,
    risk_overridden: false,
    decision_rationale: "shared knowledge candidate".to_string(),
    source_task_id: request.task_id,
}
```

- [ ] **Step 2: 修改 Executable → IncubationProposal 分支**

```rust
ExperienceGovernanceDecision {
    candidate_id: *candidate_id,
    destination: ExperienceWritebackDestination::IncubationProposal,
    confirmation_policy: ExperienceConfirmationPolicy::User,
    final_risk_level: candidate.risk_level,
    risk_overridden: false,
    decision_rationale: "default agent executable -> incubation".to_string(),
    source_task_id: request.task_id,
}
```

- [ ] **Step 3: 修改 Executable → SkillPackage 分支**

```rust
ExperienceGovernanceDecision {
    candidate_id: *candidate_id,
    destination: ExperienceWritebackDestination::SkillPackage,
    confirmation_policy: ExperienceConfirmationPolicy::User,
    final_risk_level: candidate.risk_level,
    risk_overridden: false,
    decision_rationale: "executable requires user confirmation".to_string(),
    source_task_id: request.task_id,
}
```

- [ ] **Step 4: 修改 Knowledge 两个分支**

```rust
// Knowledge -> IncubationProposal
ExperienceGovernanceDecision {
    candidate_id: *candidate_id,
    destination: ExperienceWritebackDestination::IncubationProposal,
    confirmation_policy: ExperienceConfirmationPolicy::User,
    final_risk_level: candidate.risk_level,
    risk_overridden: false,
    decision_rationale: "default agent knowledge -> incubation".to_string(),
    source_task_id: request.task_id,
}

// Knowledge -> LongTermMemory
ExperienceGovernanceDecision {
    candidate_id: *candidate_id,
    destination: ExperienceWritebackDestination::LongTermMemory,
    confirmation_policy,
    final_risk_level: candidate.risk_level,
    risk_overridden: false,
    decision_rationale: "persistent agent private knowledge".to_string(),
    source_task_id: request.task_id,
}
```

- [ ] **Step 5: Commit**

```bash
git add src/systems/contribution.rs
git commit -m "feat: populate source_task_id in experience governance decisions"
```

---

## Task 3: 审批结果系统按 `source_task_id` 定位 proposal

**Files:**
- Modify: `src/systems/contribution.rs:938-991`

- [ ] **Step 1: 替换 proposal 查找键**

将：

```rust
let task_id = store
    .candidates
    .get(&candidate_id)
    .map(|c| c.producer_task_id);
```

改为：

```rust
let task_id = Some(decision.source_task_id);
```

- [ ] **Step 2: 首次审批设置 Approved 同样使用 `source_task_id`**

确认以下代码块不变，但 `task_id` 现在来自 decision：

```rust
// 首次审批：设置 proposal 为 Approved
if let Some(task_id) = task_id
    && let Some(proposal) = store.proposals.get_mut(&task_id)
{
    proposal.status = IncubationProposalStatus::Approved;
    proposal.updated_at = chrono::Utc::now();
}
```

- [ ] **Step 3: Commit**

```bash
git add src/systems/contribution.rs
git commit -m "fix: use source_task_id to locate incubation proposal during approval"
```

---

## Task 4: `writeback_incubation_proposal` 按 task_id 索引并幂等

**Files:**
- Modify: `src/systems/contribution.rs:710-803`

- [ ] **Step 1: 修改函数签名**

```rust
fn writeback_incubation_proposal(
    task_id: TaskId,
    store: &mut crate::domain::ExperienceStore,
    proposal_store: &crate::infrastructure::incubation::proposal_store::IncubationProposalStore,
    agent_registry: &crate::infrastructure::incubation::agent_registry::IncubatedAgentRegistry,
    config_path: &str,
) -> Result<(), String> {
```

- [ ] **Step 2: 替换全局扫描为 task_id 索引**

将：

```rust
let (task_id, profile, rationale) = store
    .proposals
    .iter()
    .find(|(_, p)| p.status == crate::domain::IncubationProposalStatus::Approved)
    .map(|(tid, p)| {
        (
            *tid,
            p.proposed_agent_profile.clone(),
            p.incubation_rationale.clone(),
        )
    })
    .ok_or_else(|| "no Approved IncubationProposal found".to_string())?;
```

改为：

```rust
let proposal = store
    .proposals
    .get(&task_id)
    .cloned()
    .ok_or_else(|| format!("no IncubationProposal found for task {}", task_id))?;

let profile = proposal.proposed_agent_profile.clone();
let rationale = proposal.incubation_rationale.clone();

match proposal.status {
    crate::domain::IncubationProposalStatus::Executing => {
        debug!(
            event = "IncubationExecutionInProgress",
            task_id = %task_id,
            "incubation writeback already in progress"
        );
        return Ok(());
    }
    crate::domain::IncubationProposalStatus::Executed => {
        debug!(
            event = "IncubationExecutionAlreadyDone",
            task_id = %task_id,
            "incubation proposal already executed"
        );
        return Ok(());
    }
    crate::domain::IncubationProposalStatus::Approved => {
        // continue below
    }
    other => {
        return Err(format!(
            "incubation proposal for task {} is not approved (status: {:?})",
            task_id, other
        ));
    }
}
```

- [ ] **Step 3: 删除旧的去重块**

删除以下代码（已被上方的 `Executing`/`Executed` 分支替代）：

```rust
// 去重：若已 Executed，跳过
if let Some(proposal) = store.proposals.get(&task_id)
    && proposal.status == crate::domain::IncubationProposalStatus::Executed
{
    debug!(
        event = "IncubationExecutionSkipped",
        task_id = %task_id,
        "proposal already executed, skipping"
    );
    return Ok(());
}
```

- [ ] **Step 4: Commit**

```bash
git add src/systems/contribution.rs
git commit -m "fix: make incubation writeback task-scoped and idempotent"
```

---

## Task 5: 写回系统调用点传入 `source_task_id`

**Files:**
- Modify: `src/systems/contribution.rs:569-577`

- [ ] **Step 1: 修改调用点**

将：

```rust
ExperienceWritebackDestination::IncubationProposal => {
    writeback_incubation_proposal(
        &mut store,
        &proposal_store,
        &agent_registry,
        &settings.0.agents_config_path,
    )
}
```

改为：

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

- [ ] **Step 2: Commit**

```bash
git add src/systems/contribution.rs
git commit -m "fix: pass source_task_id to incubation writeback"
```

---

## Task 6: 修复编译错误（更新现有测试）

**Files:**
- Modify: `tests/experience_layered_governance_flow.rs:467-474, 574-581, 675-682`

- [ ] **Step 1: 更新 `approved_candidate_spawns_writeback_request` 测试**

在 `ExperienceGovernanceDecision` 构造处添加 `source_task_id: task_id,`：

```rust
app.world_mut().spawn(ExperienceGovernanceDecision {
    candidate_id: promoted_ids[0],
    destination: ExperienceWritebackDestination::LongTermMemory,
    confirmation_policy: harness::ExperienceConfirmationPolicy::default(),
    final_risk_level: harness::ExperienceRiskLevel::default(),
    risk_overridden: false,
    decision_rationale: "test".to_string(),
    source_task_id: task_id,
});
```

- [ ] **Step 2: 更新 `approval_to_writeback_completes_in_same_frame` 测试**

```rust
app.world_mut().spawn(ExperienceGovernanceDecision {
    candidate_id,
    destination: ExperienceWritebackDestination::IncubationProposal,
    confirmation_policy: harness::ExperienceConfirmationPolicy::default(),
    final_risk_level: harness::ExperienceRiskLevel::default(),
    risk_overridden: false,
    decision_rationale: "test".to_string(),
    source_task_id: task_id,
});
```

- [ ] **Step 3: 更新 `multiple_candidates_same_proposal_deduplicate_writeback` 测试**

```rust
app.world_mut().spawn(ExperienceGovernanceDecision {
    candidate_id: *cid,
    destination: ExperienceWritebackDestination::IncubationProposal,
    confirmation_policy: harness::ExperienceConfirmationPolicy::default(),
    final_risk_level: harness::ExperienceRiskLevel::default(),
    risk_overridden: false,
    decision_rationale: format!("test {i}"),
    source_task_id: task_id,
});
```

- [ ] **Step 4: 运行测试确认编译通过**

```bash
cargo test --test experience_layered_governance_flow
```

- [ ] **Step 5: Commit**

```bash
git add tests/experience_layered_governance_flow.rs
git commit -m "test: update existing governance tests with source_task_id"
```

---

## Task 7: 新增子任务候选聚合 + 多候选审批测试

**Files:**
- Modify: `tests/experience_layered_governance_flow.rs`

- [ ] **Step 1: 在文件末尾新增测试函数**

```rust
/// 验证子任务候选聚合到父任务 inbox 后，多候选审批写回正常。
///
/// 场景：父任务有一个自身候选 + 两个子任务候选；所有候选合并到同一 IncubationProposal；
/// 用户依次批准，首个候选触发执行，后续候选幂等，所有候选最终为 Persisted。
#[test]
fn aggregated_child_candidates_writeback_idempotently() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(NoOpExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, vec![]);
    app.update();

    let parent_task_id = uuid::Uuid::new_v4();
    let child_task_id_1 = uuid::Uuid::new_v4();
    let child_task_id_2 = uuid::Uuid::new_v4();
    let agent_id = uuid::Uuid::new_v4();

    let (root_id, child_id_1, child_id_2, request_ids): (
        uuid::Uuid,
        uuid::Uuid,
        uuid::Uuid,
        Vec<uuid::Uuid>,
    ) = {
        let mut store = app.world_mut().resource_mut::<ExperienceStore>();

        // 父任务自身候选
        let root = ExperienceCandidate::knowledge(
            uuid::Uuid::new_v4(),
            parent_task_id,
            agent_id,
            "root candidate".to_string(),
            "root content".to_string(),
            harness::LongTermMemoryKind::Fact,
        );
        let root_id = root.candidate_id;
        store.stage_root_candidate(root);

        // 子任务候选 1
        let child1 = ExperienceCandidate::knowledge(
            uuid::Uuid::new_v4(),
            child_task_id_1,
            agent_id,
            "child candidate 1".to_string(),
            "child content 1".to_string(),
            harness::LongTermMemoryKind::Fact,
        );
        let child_id_1 = child1.candidate_id;
        store.queue_for_parent(parent_task_id, agent_id, child1);

        // 子任务候选 2
        let child2 = ExperienceCandidate::knowledge(
            uuid::Uuid::new_v4(),
            child_task_id_2,
            agent_id,
            "child candidate 2".to_string(),
            "child content 2".to_string(),
            harness::LongTermMemoryKind::Fact,
        );
        let child_id_2 = child2.candidate_id;
        store.queue_for_parent(parent_task_id, agent_id, child2);

        // 消费 inbox，使子候选状态变为 Aggregated
        store.aggregate_inbox_for_task(parent_task_id);

        // 统一收束为 GovernancePending
        let ids = store.collect_top_level_governance_candidates(parent_task_id);
        assert!(ids.contains(&root_id));
        assert!(ids.contains(&child_id_1));
        assert!(ids.contains(&child_id_2));

        // 合并到同一 proposal
        let profile = harness::AgentProfile {
            name: "incubated-test".to_string(),
            model: "test".to_string(),
        };
        for id in &ids {
            let snapshot = store.candidates.get(id).cloned().unwrap();
            store.merge_into_proposal(parent_task_id, agent_id, profile.clone(), &snapshot);
        }

        // 绑定审批请求
        let request_ids: Vec<uuid::Uuid> = ids.iter().map(|_| uuid::Uuid::new_v4()).collect();
        for (id, req_id) in ids.iter().zip(request_ids.iter()) {
            store.bind_approval_request(*req_id, *id);
        }

        (root_id, child_id_1, child_id_2, request_ids)
    };

    let candidate_ids = vec![root_id, child_id_1, child_id_2];

    // 为每个候选生成治理决议和配对确认实体
    for (cid, req_id) in candidate_ids.iter().zip(request_ids.iter()) {
        app.world_mut().spawn(ExperienceGovernanceDecision {
            candidate_id: *cid,
            destination: ExperienceWritebackDestination::IncubationProposal,
            confirmation_policy: harness::ExperienceConfirmationPolicy::default(),
            final_risk_level: harness::ExperienceRiskLevel::default(),
            risk_overridden: false,
            decision_rationale: "test".to_string(),
            source_task_id: parent_task_id,
        });

        app.world_mut().spawn(ToolConfirmationRequestMessage {
            request_id: *req_id,
            task_id: parent_task_id,
            agent_id,
            tool_name: "experience_governance".to_string(),
            tool_input: serde_json::json!({"candidate_id": cid.to_string()}),
            options: harness::ConfirmationOption::default_options(),
            source: harness::ConfirmationSource::User,
            parent_agent_id: None,
        });
        app.world_mut().spawn(ToolExecutionRequestMessage {
            request: AgentExecutionRequest {
                task_id: parent_task_id,
                agent_id,
                request_kind: AgentRequestKind::ToolExecution {
                    tool_name: "experience_governance".to_string(),
                },
                prompt: String::new(),
                system_prompt: None,
                tools: vec![],
                conversation: None,
                work_item_id: None,
            },
            tool_name: "experience_governance".to_string(),
            tool_input: serde_json::json!({}),
            pending_confirmation_id: Some(*req_id),
            tool_call_id: None,
            pending_confirmation_options: Some(harness::ConfirmationOption::default_options()),
        });
    }

    // 逐个审批
    for req_id in &request_ids {
        app.world_mut().spawn(ToolConfirmationResponseMessage {
            request_id: *req_id,
            selected_option: "approve".to_string(),
        });
        app.update();
    }

    // 验证 proposal 最终为 Executed
    let store = app.world_mut().resource::<ExperienceStore>();
    let proposal = store.proposals.get(&parent_task_id).unwrap();
    assert_eq!(
        proposal.status,
        harness::IncubationProposalStatus::Executed,
        "proposal should be Executed after first successful writeback"
    );

    // 验证所有候选最终为 Persisted，且不为 WritebackFailed
    for cid in &candidate_ids {
        let candidate = store.candidates.get(cid).unwrap();
        assert_eq!(
            candidate.status,
            ExperienceCandidateStatus::Persisted,
            "candidate {} should be Persisted",
            cid
        );
    }
}
```

- [ ] **Step 2: 运行新增测试**

```bash
cargo test --test experience_layered_governance_flow aggregated_child_candidates_writeback_idempotently -- --nocapture
```

- [ ] **Step 3: 运行全部相关测试**

```bash
cargo test --test experience_layered_governance_flow
```

- [ ] **Step 4: Commit**

```bash
git add tests/experience_layered_governance_flow.rs
git commit -m "test: add aggregated child candidate idempotent writeback test"
```

---

## Task 8: 全量 CI 检查

- [ ] **Step 1: 格式化检查**

```bash
cargo fmt --all --check
```

- [ ] **Step 2: Clippy 检查**

```bash
cargo clippy --all-targets --all-features -- -D warnings
```

- [ ] **Step 3: 运行全部测试**

```bash
cargo test --all-features
```

- [ ] **Step 4: Commit（如仅有格式修复）**

```bash
git add .
git commit -m "chore: fix formatting and clippy warnings"
```

---

## 自检

- [ ] `ExperienceGovernanceDecision` 已新增 `source_task_id` 并在所有构造点赋值
- [ ] `experience_approval_result_system` 不再使用 `candidate.producer_task_id` 定位 proposal
- [ ] `writeback_incubation_proposal` 按 `task_id` 索引，对 `Executing`/`Executed` 幂等
- [ ] 现有测试已更新以包含 `source_task_id`
- [ ] 新增测试覆盖子任务候选聚合 + 多候选审批场景
- [ ] `cargo fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`cargo test --all-features` 全部通过
