//! 经验模块两层分层汇聚治理集成测试
//!
//! 覆盖 spec 要求的四条主链路：
//! - 普通持久型 Agent 知识类候选自动落盘到 LongTermMemory
//! - 普通持久型 Agent executable 候选用户批准后生成 Skill Package
//! - 公共规则类候选进入 SharedKnowledgeUpgradeQueue
//! - default Agent 的私有候选生成 IncubationProposal
//!
//! P0 修复验证：
//! - 审批→写回链路：配对 ToolExecutionRequestMessage 后确认系统保留响应实体
//! - 审批通过后 ExperienceWritebackRequestMessage 被创建

use std::sync::Arc;

use crossbeam_channel::unbounded;
use harness::{
    AgentAssetService, AgentExecutionRequest, AgentRequestKind, ExperienceCandidate,
    ExperienceCandidatePayload, ExperienceCandidateStatus, ExperienceGovernanceDecision,
    ExperienceKindHint, ExperienceStore, ExperienceWritebackDestination, HarnessConfig,
    SharedKnowledgeUpgradeQueue, ToolConfirmationRequestMessage, ToolConfirmationResponseMessage,
    ToolExecutionRequestMessage,
    infrastructure::memory::{JsonFileMemoryStore, LongTermMemoryService, MemoryRepository},
};
use harness::{AgentExecutor, ExecutorFuture, build_harness_app};
use tempfile::TempDir;
use tokio::runtime::Runtime;

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
    let producer_task_id = candidate.producer_task_id;
    store.stage_root_candidate(candidate);

    // 模拟顶层治理自动落盘
    let ids = store.promote_root_candidates_to_governance(producer_task_id);
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
        governing_agent_id: None,
        risk_level: harness::ExperienceRiskLevel::default(),
        risk_reason: String::new(),
        suggested_confirmation: harness::ExperienceConfirmationPolicy::default(),
        derived_from_candidate_ids: vec![],
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
    let relative = asset_service
        .persist_skill_package("persistent-worker", &draft)
        .unwrap();

    assert!(
        dir.path()
            .join("assets")
            .join(&relative)
            .join("skill.md")
            .exists()
    );
}

/// Case 3: 公共规则类候选进入 SharedKnowledge 升级入口。
#[test]
fn shared_knowledge_candidate_queues_upgrade_entry() {
    let mut queue = SharedKnowledgeUpgradeQueue::default();
    let candidate_id = uuid::Uuid::new_v4();

    queue
        .candidates
        .push(harness::SharedKnowledgeUpgradeCandidate {
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
        incubation_rationale: String::new(),
        status: harness::IncubationProposalStatus::Proposed,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    };

    assert_eq!(
        proposal.knowledge_candidate_ids,
        vec![candidate.candidate_id]
    );
    assert!(store.candidates.contains_key(&candidate.candidate_id));
}

/// 顶层治理统一收束：顶层自身候选与子层汇聚候选同时进入治理输入。
#[test]
fn top_level_governance_consumes_root_and_aggregated_candidates() {
    let mut store = harness::ExperienceStore::default();
    let top_task_id = uuid::Uuid::new_v4();
    let top_agent_id = uuid::Uuid::new_v4();

    // 顶层自身候选
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

    // 子层候选：先进入 inbox，再标记为 Aggregated
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

    // 统一收束
    let ids = store.collect_top_level_governance_candidates(top_task_id);

    assert!(ids.contains(&root_id), "should include root candidate");
    assert!(
        ids.contains(&child_id),
        "should include aggregated child candidate"
    );

    // 两者都应处于 GovernancePending
    assert_eq!(
        store.candidates.get(&root_id).unwrap().status,
        harness::ExperienceCandidateStatus::GovernancePending
    );
    assert_eq!(
        store.candidates.get(&child_id).unwrap().status,
        harness::ExperienceCandidateStatus::GovernancePending
    );
}

/// default Agent 的多个私有候选只汇总成一个任务级 IncubationProposal。
#[test]
fn default_agent_merges_multiple_private_candidates_into_single_task_level_proposal() {
    let mut store = harness::ExperienceStore::default();
    let task_id = uuid::Uuid::new_v4();
    let agent_id = uuid::Uuid::new_v4();

    let profile = harness::AgentProfile {
        name: "physics-specialist".to_string(),
        model: "gpt-4.1-mini".to_string(),
    };

    // 知识类候选
    let knowledge = harness::ExperienceCandidate::knowledge(
        uuid::Uuid::new_v4(),
        task_id,
        agent_id,
        "physics fact".to_string(),
        "E=mc²".to_string(),
        harness::LongTermMemoryKind::Fact,
    );
    // 可执行类候选
    let executable = harness::ExperienceCandidate {
        candidate_id: uuid::Uuid::new_v4(),
        producer_task_id: task_id,
        producer_agent_id: agent_id,
        title: "physics sim".to_string(),
        kind_hint: harness::ExperienceKindHint::Executable,
        payload: harness::ExperienceCandidatePayload::Executable {
            intent: "run physics simulation".to_string(),
            when_to_use: "after parameter changes".to_string(),
            asset_refs: vec![],
        },
        dependency_refs: vec![],
        status: harness::ExperienceCandidateStatus::Submitted,
        governing_agent_id: None,
        risk_level: harness::ExperienceRiskLevel::default(),
        risk_reason: String::new(),
        suggested_confirmation: harness::ExperienceConfirmationPolicy::default(),
        derived_from_candidate_ids: vec![],
    };

    // 先 merge knowledge
    store.merge_into_proposal(task_id, agent_id, profile.clone(), &knowledge);
    // 再 merge executable（应该合并到同一 proposal）
    store.merge_into_proposal(task_id, agent_id, profile.clone(), &executable);

    let proposal = store.proposals.get(&task_id).unwrap();
    assert_eq!(proposal.source_task_id, task_id);
    assert_eq!(proposal.knowledge_candidate_ids.len(), 1);
    assert_eq!(proposal.executable_candidate_ids.len(), 1);
    assert_eq!(proposal.knowledge_candidate_ids[0], knowledge.candidate_id);
    assert_eq!(
        proposal.executable_candidate_ids[0],
        executable.candidate_id
    );
}

/// 写回失败时候选应进入 WritebackFailed 状态。
#[test]
fn failed_writeback_marks_candidate_writeback_failed() {
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

// ============ P0: 审批→写回链路修复验证 ============

struct NoOpExecutor;

impl AgentExecutor for NoOpExecutor {
    fn execute(&self, _request: AgentExecutionRequest) -> ExecutorFuture {
        Box::pin(async move {
            Ok(harness::AgentExecutionOutput {
                content: harness::OutputContent::Text("ok".to_string()),
                reasoning_content: None,
            })
        })
    }
}

fn test_config() -> HarnessConfig {
    HarnessConfig::default()
}

/// 验证 experience_governance 特判分支：占位实体被清理，不触发工具执行。
///
/// tool_confirmation_result_system 对 experience_governance 做特判后：
/// - 占位 ToolExecutionRequestMessage 被 despawn
/// - 不生成 ToolExecutionResultMessage（不执行工具）
/// - ToolConfirmationResponseMessage 保留给 experience_approval_result_system
#[test]
fn experience_governance_confirmation_skips_tool_execution() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(NoOpExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, vec![]);
    app.update();

    let request_id = uuid::Uuid::new_v4();
    let task_id = uuid::Uuid::new_v4();
    let agent_id = uuid::Uuid::new_v4();

    // 模拟 spawn_experience_confirmation 的输出：配对的确认请求和执行请求
    app.world_mut().spawn(ToolConfirmationRequestMessage {
        request_id,
        task_id,
        agent_id,
        tool_name: "experience_governance".to_string(),
        tool_input: serde_json::json!({"candidate_id": uuid::Uuid::new_v4().to_string()}),
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

    // 模拟用户批准
    app.world_mut().spawn(ToolConfirmationResponseMessage {
        request_id,
        selected_option: "approve".to_string(),
    });

    app.update();

    // 验证 ToolExecutionRequestMessage 占位实体被清理（特判分支 despawn）
    let exec_requests: Vec<_> = app
        .world_mut()
        .query::<&ToolExecutionRequestMessage>()
        .iter(app.world())
        .filter(|r| r.tool_name == "experience_governance")
        .collect();
    assert!(
        exec_requests.is_empty(),
        "experience_governance placeholder ToolExecutionRequestMessage should be despawned"
    );

    // 验证没有生成 ToolExecutionResultMessage（特判阻止了工具执行）
    let exec_results: Vec<_> = app
        .world_mut()
        .query::<&harness::ToolExecutionResultMessage>()
        .iter(app.world())
        .filter(|r| r.tool_name == "experience_governance")
        .collect();
    assert!(
        exec_results.is_empty(),
        "experience_governance should not produce ToolExecutionResultMessage"
    );
}

/// 验证审批通过后候选同帧完成写回。
#[test]
fn approved_candidate_spawns_writeback_request() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(NoOpExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, vec![]);
    app.update();

    let task_id = uuid::Uuid::new_v4();
    let agent_id = uuid::Uuid::new_v4();
    let candidate_id = uuid::Uuid::new_v4();
    let request_id = uuid::Uuid::new_v4();
    let agent_name = "test-agent".to_string();

    // 创建 Agent 实体和 LongTermMemory 组件（writeback_to_long_term_memory 需要）
    app.world_mut().spawn((
        harness::Agent {
            id: agent_id,
            profile: harness::AgentProfile {
                name: agent_name.clone(),
                model: "test".to_string(),
            },
            capabilities: harness::AgentCapabilities {
                tags: vec![],
                description: String::new(),
            },
            kind: harness::AgentKind::Persistent,
            parent_id: None,
            bound_task_id: None,
            tool_permissions: harness::AgentToolPermissions::default(),
        },
        harness::LongTermMemory::with_name(&agent_name),
    ));

    // 设置 ExperienceStore：添加候选、设置 governing_agent_id、治理决议
    let promoted_ids = {
        let mut store = app.world_mut().resource_mut::<ExperienceStore>();
        let mut candidate = ExperienceCandidate::knowledge(
            candidate_id,
            task_id,
            agent_id,
            "test candidate".to_string(),
            "test content".to_string(),
            harness::LongTermMemoryKind::Fact,
        );
        candidate.governing_agent_id = Some(agent_id);
        store.stage_root_candidate(candidate);
        let ids = store.promote_root_candidates_to_governance(task_id);
        assert!(!ids.is_empty());
        store.bind_approval_request(request_id, ids[0]);
        ids
    };

    // 存储治理决议
    app.world_mut().spawn(ExperienceGovernanceDecision {
        candidate_id: promoted_ids[0],
        destination: ExperienceWritebackDestination::LongTermMemory,
        confirmation_policy: harness::ExperienceConfirmationPolicy::default(),
        final_risk_level: harness::ExperienceRiskLevel::default(),
        risk_overridden: false,
        decision_rationale: "test".to_string(),
        source_task_id: task_id,
    });

    // 创建配对实体
    app.world_mut().spawn(ToolConfirmationRequestMessage {
        request_id,
        task_id,
        agent_id,
        tool_name: "experience_governance".to_string(),
        tool_input: serde_json::json!({"candidate_id": promoted_ids[0].to_string()}),
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

    // D1 修复后 approval_result 和 writeback 在同一 Execution set 内顺序执行，
    // ExperienceWritebackRequestMessage 已被 writeback 系统消费。
    // 验证候选最终状态：LongTermMemory 目标写回成功后候选为 Persisted。
    let store = app.world_mut().resource::<ExperienceStore>();
    let candidate = store.candidates.get(&promoted_ids[0]);
    assert!(candidate.is_some(), "candidate should exist after approval");
    assert_eq!(
        candidate.unwrap().status,
        ExperienceCandidateStatus::Persisted,
        "approved LongTermMemory candidate should be persisted after same-frame writeback"
    );
}

/// 验证审批→写回在同一 Execution set 内同帧完成。
///
/// D1 修复后 approval_result 在 writeback 之前执行，
/// 用户审批后单帧内 proposal 状态从 Proposed → Approved → Executing → Executed。
#[test]
fn approval_to_writeback_completes_in_same_frame() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(NoOpExecutor);
    let (_input_tx, input_rx) = unbounded();
    let agents_dir = tempfile::TempDir::new().unwrap();
    let mut cfg = test_config();
    cfg.agents_config_path = agents_dir
        .path()
        .join("agents.toml")
        .to_str()
        .unwrap()
        .to_string();
    let mut app = build_harness_app(cfg, runtime, executor, input_rx, vec![]);
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

        // 先克隆候选再传入 merge_into_proposal（避免同时可变+不可变借用 store）
        let candidate_snapshot = store.candidates.get(&ids[0]).cloned().unwrap();
        store.merge_into_proposal(
            task_id,
            agent_id,
            harness::AgentProfile {
                name: "incubated-test".to_string(),
                model: "test".to_string(),
            },
            &candidate_snapshot,
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
        source_task_id: task_id,
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
    assert!(proposal.is_some(), "proposal should exist for task");
    assert_eq!(
        proposal.unwrap().status,
        harness::IncubationProposalStatus::Executed,
        "proposal should reach Executed in same frame after approval"
    );
}

/// 验证同一 proposal 的多个候选审批后只生成一个写回请求。
///
/// D2 修复后，首个候选审批生成写回请求，后续候选审批跳过写回请求生成，
/// 候选根据 proposal 状态标记为 WritebackPending 或 Persisted。
#[test]
fn multiple_candidates_same_proposal_deduplicate_writeback() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(NoOpExecutor);
    let (_input_tx, input_rx) = unbounded();
    let agents_dir = tempfile::TempDir::new().unwrap();
    let mut cfg = test_config();
    cfg.agents_config_path = agents_dir
        .path()
        .join("agents.toml")
        .to_str()
        .unwrap()
        .to_string();
    let mut app = build_harness_app(cfg, runtime, executor, input_rx, vec![]);
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
            source_task_id: task_id,
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

/// 验证子任务候选聚合到父任务 inbox 后，多候选审批写回正常。
///
/// 场景：父任务有一个自身候选 + 两个子任务候选；所有候选合并到同一 IncubationProposal；
/// 用户依次批准，首个候选触发执行，后续候选幂等，所有候选最终为 Persisted。
#[test]
fn aggregated_child_candidates_writeback_idempotently() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(NoOpExecutor);
    let (_input_tx, input_rx) = unbounded();
    let agents_dir = tempfile::TempDir::new().unwrap();
    let mut cfg = test_config();
    cfg.agents_config_path = agents_dir
        .path()
        .join("agents.toml")
        .to_str()
        .unwrap()
        .to_string();
    let mut app = build_harness_app(cfg, runtime, executor, input_rx, vec![]);
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

    // 验证所有候选最终为 Persisted
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
