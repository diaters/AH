# 经验治理模块运行时缺陷修复实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 修复经验治理模块 3 个运行时缺陷：系统集执行顺序、多候选重复写回、Deny 描述错误

**Architecture:** D1 调整 `experience_approval_result_system` 系统集归属到 Execution，D2 在审批源头按 proposal 状态去重，D3 特判 deny 选项描述

**Tech Stack:** Rust, Bevy ECS, ratatui

---

### Task 1: D1 — 调整 experience_approval_result_system 系统集归属

**Files:**
- Modify: `src/plugins/execution.rs:56-59`

- [ ] **Step 1: 修改系统集归属和排序约束**

将 `experience_approval_result_system` 从 `HarnessSet::Maintenance` 移到 `HarnessSet::Execution`，
添加 `.after(experience_governance_system)` 和 `.before(experience_writeback_system)`，
保留 `.after(tool_confirmation_result_system)` 跨集约束。

在 `src/plugins/execution.rs` 中，替换：

```rust
                // 经验确认结果：处理用户对经验候选的确认
                experience_approval_result_system
                    .in_set(HarnessSet::Maintenance)
                    .after(crate::systems::tool_confirmation_result_system),
```

为：

```rust
                // 经验确认结果：处理用户对经验候选的确认
                experience_approval_result_system
                    .in_set(HarnessSet::Execution)
                    .after(crate::systems::tool_confirmation_result_system)
                    .after(experience_governance_system)
                    .before(experience_writeback_system),
```

- [ ] **Step 2: 运行 cargo check 验证编译**

Run: `cargo check 2>&1 | tail -3`
Expected: `Finished` 无 error

- [ ] **Step 3: 运行现有测试验证不破坏**

Run: `cargo test --test experience_layered_governance_flow -- --nocapture 2>&1 | tail -20`
Expected: `experience_governance_confirmation_skips_tool_execution` 通过，
`approved_candidate_spawns_writeback_request` **可能失败**（同一帧 writeback 消费消息）

- [ ] **Step 4: 调整 approved_candidate_spawns_writeback_request 断言**

D1 实施后 `experience_writeback_system` 在同一帧内消费 `ExperienceWritebackRequestMessage`
并 despawn，原断言 `ExperienceWritebackRequestMessage` 仍存在将失败。改为断言候选最终状态。

在 `tests/experience_layered_governance_flow.rs` 中，替换 `approved_candidate_spawns_writeback_request`
函数末尾的断言块：

```rust
    app.update();

    // 验证 ExperienceWritebackRequestMessage 被创建
    let writeback_requests: Vec<_> = app
        .world_mut()
        .query::<&ExperienceWritebackRequestMessage>()
        .iter(app.world())
        .collect();
    assert!(
        writeback_requests
            .iter()
            .any(|r| r.decision.candidate_id == promoted_ids[0]),
        "approval should spawn ExperienceWritebackRequestMessage"
    );
}
```

为：

```rust
    app.update();

    // D1 修复后 approval_result 和 writeback 在同一 Execution set 内顺序执行，
    // ExperienceWritebackRequestMessage 已被 writeback 系统消费。
    // 验证候选最终状态：LongTermMemory 目标写回成功后候选为 Persisted。
    let store = app.world_mut().resource::<ExperienceStore>();
    let candidate = store.candidates.get(&promoted_ids[0]);
    assert!(
        candidate.is_some(),
        "candidate should exist after approval"
    );
    assert_eq!(
        candidate.unwrap().status,
        ExperienceCandidateStatus::Persisted,
        "approved LongTermMemory candidate should be persisted after same-frame writeback"
    );
}
```

- [ ] **Step 5: 运行测试验证**

Run: `cargo test --test experience_layered_governance_flow -- --nocapture 2>&1 | tail -15`
Expected: 全部通过

- [ ] **Step 6: 提交**

```bash
git add src/plugins/execution.rs tests/experience_layered_governance_flow.rs
git commit -m "fix: move experience_approval_result_system to Execution set before writeback"
```

---

### Task 2: D2 — 审批源头去重

**Files:**
- Modify: `src/systems/contribution.rs:937-949`

- [ ] **Step 1: 修改 IncubationProposal 分支添加状态检查**

在 `experience_approval_result_system` 中，IncubationProposal 分支设置 proposal 状态为 `Approved`
之前，检查 proposal 当前状态。若已是 `Approved`/`Executing`/`Executed`，跳过生成写回请求，
并根据 proposal 状态决定候选最终状态。

在 `src/systems/contribution.rs` 中，替换：

```rust
                // 对于 IncubationProposal 目标，更新 store 中 proposal 状态
                if decision.destination == ExperienceWritebackDestination::IncubationProposal {
                    let task_id = store
                        .candidates
                        .get(&candidate_id)
                        .map(|c| c.producer_task_id);
                    if let Some(task_id) = task_id
                        && let Some(proposal) = store.proposals.get_mut(&task_id)
                    {
                        proposal.status = IncubationProposalStatus::Approved;
                        proposal.updated_at = chrono::Utc::now();
                    }
                }

                // 生成写回请求
                commands.spawn(ExperienceWritebackRequestMessage {
                    decision: decision.clone(),
                });
                commands.entity(decision_entity).despawn();
```

为：

```rust
                // 对于 IncubationProposal 目标，检查 proposal 状态做源头去重
                if decision.destination == ExperienceWritebackDestination::IncubationProposal {
                    let task_id = store
                        .candidates
                        .get(&candidate_id)
                        .map(|c| c.producer_task_id);

                    if let Some(task_id) = task_id
                        && let Some(proposal) = store.proposals.get(&task_id)
                    {
                        match proposal.status {
                            IncubationProposalStatus::Approved
                            | IncubationProposalStatus::Executing => {
                                // 已有写回请求在途，候选等待完成
                                if let Some(c) = store.candidates.get_mut(&candidate_id) {
                                    c.status = ExperienceCandidateStatus::WritebackPending;
                                }
                                debug!(
                                    event = "ExperienceApprovalDeduplicated",
                                    candidate_id = %candidate_id,
                                    proposal_status = ?proposal.status,
                                    "proposal already has writeback in progress, skipping"
                                );
                                commands.entity(decision_entity).despawn();
                                commands.entity(entity).despawn();
                                continue;
                            }
                            IncubationProposalStatus::Executed => {
                                // 已写回完成，候选直接标记为 Persisted
                                if let Some(c) = store.candidates.get_mut(&candidate_id) {
                                    c.status = ExperienceCandidateStatus::Persisted;
                                }
                                debug!(
                                    event = "ExperienceApprovalDeduplicated",
                                    candidate_id = %candidate_id,
                                    proposal_status = ?proposal.status,
                                    "proposal already executed, marking candidate as persisted"
                                );
                                commands.entity(decision_entity).despawn();
                                commands.entity(entity).despawn();
                                continue;
                            }
                            _ => {}
                        }
                    }

                    // 首次审批：设置 proposal 为 Approved
                    if let Some(task_id) = task_id
                        && let Some(proposal) = store.proposals.get_mut(&task_id)
                    {
                        proposal.status = IncubationProposalStatus::Approved;
                        proposal.updated_at = chrono::Utc::now();
                    }
                }

                // 生成写回请求
                commands.spawn(ExperienceWritebackRequestMessage {
                    decision: decision.clone(),
                });
                commands.entity(decision_entity).despawn();
```

- [ ] **Step 2: 运行 cargo check 验证编译**

Run: `cargo check 2>&1 | tail -3`
Expected: `Finished` 无 error

- [ ] **Step 3: 运行现有测试验证**

Run: `cargo test --test experience_layered_governance_flow -- --nocapture 2>&1 | tail -15`
Expected: 全部通过

- [ ] **Step 4: 提交**

```bash
git add src/systems/contribution.rs
git commit -m "fix: deduplicate IncubationProposal writeback requests at approval source"
```

---

### Task 3: D3 — 修正 Deny 选项描述

**Files:**
- Modify: `src/systems/frontend_output.rs:115-118`

- [ ] **Step 1: 修改选项描述映射，对 deny 特判**

在 `src/systems/frontend_output.rs` 中，替换：

```rust
                description: match opt.mode {
                    crate::domain::GrantMode::Once => "仅本次允许".to_string(),
                    crate::domain::GrantMode::Permanent => "永久允许此工具".to_string(),
                },
```

为：

```rust
                description: if opt.id == "deny" {
                    "拒绝".to_string()
                } else {
                    match opt.mode {
                        crate::domain::GrantMode::Once => "仅本次允许".to_string(),
                        crate::domain::GrantMode::Permanent => "永久允许此工具".to_string(),
                    }
                },
```

- [ ] **Step 2: 运行 cargo check 验证编译**

Run: `cargo check 2>&1 | tail -3`
Expected: `Finished` 无 error

- [ ] **Step 3: 提交**

```bash
git add src/systems/frontend_output.rs
git commit -m "fix: correct Deny option description from '仅本次允许' to '拒绝'"
```

---

### Task 4: 新增测试 — 审批→写回同帧完成

**Files:**
- Modify: `tests/experience_layered_governance_flow.rs`

- [ ] **Step 1: 在 `tests/experience_layered_governance_flow.rs` 末尾添加测试**

```rust
/// 验证审批→写回在同一 Execution set 内同帧完成。
///
/// D1 修复后 approval_result 在 writeback 之前执行，
/// 用户审批后单帧内 proposal 状态从 Proposed → Approved → Executing → Executed。
#[test]
fn approval_to_writeback_completes_in_same_frame() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(NoOpExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, vec![]);
    app.update();

    let task_id = uuid::Uuid::new_v4();
    let agent_id = uuid::Uuid::new_v4();
    let request_id = uuid::Uuid::new_v4();

    // 设置候选和 proposal
    let candidate_id = {
        let mut store = app.world_mut().resource_mut::<ExperienceStore>();
        let candidate = ExperienceCandidate::knowledge(
            uuid::Uuid::new_v4(),
            task_id,
            agent_id,
            "incubation test".to_string(),
            "content".to_string(),
            harness::LongTermMemoryKind::Fact,
        );
        let cid = candidate.candidate_id;
        store.stage_root_candidate(candidate);
        let ids = store.promote_root_candidates_to_governance(task_id);
        assert!(!ids.is_empty());
        store.bind_approval_request(request_id, ids[0]);

        // 创建 IncubationProposal 并设置治理决议
        store.merge_into_proposal(
            task_id,
            agent_id,
            harness::AgentProfile {
                name: "incubated-test".to_string(),
                model: "test".to_string(),
            },
            store.candidates.get(&ids[0]).unwrap(),
        );
        cid
    };

    // 存储治理决议：目标是 IncubationProposal
    app.world_mut().spawn(ExperienceGovernanceDecision {
        candidate_id,
        destination: ExperienceWritebackDestination::IncubationProposal,
        confirmation_policy: harness::ExperienceConfirmationPolicy::default(),
        final_risk_level: harness::ExperienceRiskLevel::default(),
        risk_overridden: false,
        decision_rationale: "test".to_string(),
    });

    // 创建配对实体和审批响应
    app.world_mut().spawn(ToolConfirmationRequestMessage {
        request_id,
        task_id,
        agent_id,
        tool_name: "experience_governance".to_string(),
        tool_input: serde_json::json!({"candidate_id": candidate_id.to_string()}),
        options: harness::ConfirmationOption::default_options(),
        source: harness::ConfirmationSource::User,
        parent_agent_id: None,
    });
    app.world_mut().spawn(ToolExecutionRequestMessage {
        request: AgentExecutionRequest {
            task_id,
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
        pending_confirmation_id: Some(request_id),
        tool_call_id: None,
        pending_confirmation_options: Some(harness::ConfirmationOption::default_options()),
    });
    app.world_mut().spawn(ToolConfirmationResponseMessage {
        request_id,
        selected_option: "approve".to_string(),
    });

    app.update();

    // 验证单帧内 proposal 状态推进到 Executed
    let store = app.world_mut().resource::<ExperienceStore>();
    let proposal = store.proposals.get(&task_id);
    assert!(
        proposal.is_some(),
        "proposal should exist for task"
    );
    assert_eq!(
        proposal.unwrap().status,
        harness::IncubationProposalStatus::Executed,
        "proposal should reach Executed in same frame after approval"
    );
}
```

- [ ] **Step 2: 运行测试验证**

Run: `cargo test --test experience_layered_governance_flow approval_to_writeback_completes -- --nocapture 2>&1 | tail -5`
Expected: PASS

- [ ] **Step 3: 提交**

```bash
git add tests/experience_layered_governance_flow.rs
git commit -m "test: add same-frame approval-to-writeback verification"
```

---

### Task 5: 新增测试 — 多候选 IncubationProposal 审批去重

**Files:**
- Modify: `tests/experience_layered_governance_flow.rs`

- [ ] **Step 1: 在 `tests/experience_layered_governance_flow.rs` 末尾添加测试**

```rust
/// 验证同一 proposal 的多个候选审批后只生成一个写回请求。
///
/// D2 修复后，首个候选审批生成写回请求，后续候选审批跳过写回请求生成，
/// 候选根据 proposal 状态标记为 WritebackPending 或 Persisted。
#[test]
fn multiple_candidates_same_proposal_deduplicate_writeback() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(NoOpExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, vec![]);
    app.update();

    let task_id = uuid::Uuid::new_v4();
    let agent_id = uuid::Uuid::new_v4();

    // 创建 3 个候选绑定到同一 proposal
    let candidate_ids: Vec<uuid::Uuid> = {
        let mut store = app.world_mut().resource_mut::<ExperienceStore>();
        let profile = harness::AgentProfile {
            name: "incubated-test".to_string(),
            model: "test".to_string(),
        };
        let mut ids = Vec::new();
        for i in 0..3 {
            let candidate = ExperienceCandidate::knowledge(
                uuid::Uuid::new_v4(),
                task_id,
                agent_id,
                format!("candidate {i}"),
                format!("content {i}"),
                harness::LongTermMemoryKind::Fact,
            );
            let cid = candidate.candidate_id;
            store.stage_root_candidate(candidate.clone());
            store.merge_into_proposal(task_id, agent_id, profile.clone(), &candidate);
            ids.push(cid);
        }
        ids
    };

    // 为每个候选创建治理决议和审批请求
    let request_ids: Vec<uuid::Uuid> = (0..3).map(|_| uuid::Uuid::new_v4()).collect();

    for (i, (cid, req_id)) in candidate_ids.iter().zip(request_ids.iter()).enumerate() {
        app.world_mut().spawn(ExperienceGovernanceDecision {
            candidate_id: *cid,
            destination: ExperienceWritebackDestination::IncubationProposal,
            confirmation_policy: harness::ExperienceConfirmationPolicy::default(),
            final_risk_level: harness::ExperienceRiskLevel::default(),
            risk_overridden: false,
            decision_rationale: format!("test {i}"),
        });

        {
            let mut store = app.world_mut().resource_mut::<ExperienceStore>();
            store.bind_approval_request(*req_id, *cid);
        }

        app.world_mut().spawn(ToolConfirmationRequestMessage {
            request_id: *req_id,
            task_id,
            agent_id,
            tool_name: "experience_governance".to_string(),
            tool_input: serde_json::json!({"candidate_id": cid.to_string()}),
            options: harness::ConfirmationOption::default_options(),
            source: harness::ConfirmationSource::User,
            parent_agent_id: None,
        });
        app.world_mut().spawn(ToolExecutionRequestMessage {
            request: AgentExecutionRequest {
                task_id,
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

    // 逐个审批每个候选
    for req_id in &request_ids {
        app.world_mut().spawn(ToolConfirmationResponseMessage {
            request_id: *req_id,
            selected_option: "approve".to_string(),
        });
        app.update();
    }

    // 验证 proposal 只被执行一次
    let store = app.world_mut().resource::<ExperienceStore>();
    let proposal = store.proposals.get(&task_id).unwrap();
    assert_eq!(
        proposal.status,
        harness::IncubationProposalStatus::Executed,
        "proposal should be Executed after first candidate writeback"
    );

    // 验证所有候选最终状态不为 WritebackFailed
    for cid in &candidate_ids {
        let candidate = store.candidates.get(cid).unwrap();
        assert_ne!(
            candidate.status,
            ExperienceCandidateStatus::WritebackFailed,
            "candidate {} should not be WritebackFailed",
            cid
        );
    }
}
```

- [ ] **Step 2: 运行测试验证**

Run: `cargo test --test experience_layered_governance_flow multiple_candidates_same_proposal -- --nocapture 2>&1 | tail -5`
Expected: PASS

- [ ] **Step 3: 提交**

```bash
git add tests/experience_layered_governance_flow.rs
git commit -m "test: add multi-candidate IncubationProposal dedup verification"
```

---

### Task 6: 全量回归验证

**Files:** 无代码变更

- [ ] **Step 1: 运行全量测试**

Run: `cargo test --all-features 2>&1 | grep -E "^test result:|FAILED" | head -20`
Expected: 全部 `ok`，0 FAILED

- [ ] **Step 2: 运行 clippy**

Run: `cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -3`
Expected: `Finished` 无 warning

- [ ] **Step 3: 运行 fmt**

Run: `cargo fmt --all --check`
Expected: 无输出（格式正确）
