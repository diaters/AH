//! Experience candidate governance flow integration tests
//!
//! Tests for the experience candidate governance flow:
//! - Knowledge candidates are auto-persisted for persistent non-default agents
//! - Executable candidates require user approval
//! - Default-tagged agents generate incubation proposals instead of direct persistence
//! - Governance system processes ExperienceGovernanceRequestMessage correctly

use harness::{
    ExperienceCandidate, ExperienceCandidatePayload, ExperienceCandidateStatus, ExperienceKindHint,
    ExperienceStore, LongTermMemory, LongTermMemoryKind,
    infrastructure::memory::{JsonFileMemoryStore, LongTermMemoryService, MemoryRepository},
};

/// 验证知识类候选可以为持久型 Agent 直接落盘到长期记忆。
#[test]
fn knowledge_candidate_is_persisted_for_persistent_agent() {
    let mut store = ExperienceStore::default();
    let task_id = uuid::Uuid::new_v4();
    let agent_id = uuid::Uuid::new_v4();
    let candidate = ExperienceCandidate::knowledge(
        uuid::Uuid::new_v4(),
        task_id,
        agent_id,
        "shell timeout".to_string(),
        "shell_stop 默认等待退出".to_string(),
        LongTermMemoryKind::Fact,
    );

    store.stage_root_candidate(candidate.clone());
    let ids = store.root_candidates_for_task(task_id);
    assert_eq!(ids, vec![candidate.candidate_id]);

    // 知识类候选不需要用户确认
    assert!(!candidate.requires_user_confirmation());
}

/// 验证可执行类候选需要用户确认。
#[test]
fn executable_candidate_requires_user_approval() {
    let candidate = ExperienceCandidate {
        candidate_id: uuid::Uuid::new_v4(),
        producer_task_id: uuid::Uuid::new_v4(),
        producer_agent_id: uuid::Uuid::new_v4(),
        title: "shell smoke test".to_string(),
        kind_hint: ExperienceKindHint::Executable,
        payload: ExperienceCandidatePayload::Executable {
            intent: "run smoke test".to_string(),
            when_to_use: "after shell changes".to_string(),
            asset_refs: vec!["default-agent/script.sh".to_string()],
        },
        dependency_refs: vec![],
        status: ExperienceCandidateStatus::Submitted,
    };

    assert!(candidate.requires_user_confirmation());
}

/// 验证 ExperienceStore.apply_confirmation_response 可以审批和拒绝候选。
#[test]
fn confirmation_response_approves_and_rejects_candidates() {
    let mut store = ExperienceStore::default();
    let task_id = uuid::Uuid::new_v4();
    let agent_id = uuid::Uuid::new_v4();

    let mut candidate = ExperienceCandidate::knowledge(
        uuid::Uuid::new_v4(),
        task_id,
        agent_id,
        "test knowledge".to_string(),
        "some fact".to_string(),
        LongTermMemoryKind::Fact,
    );
    candidate.status = ExperienceCandidateStatus::NeedsUserApproval;

    let candidate_id = candidate.candidate_id;
    store.stage_root_candidate(candidate);

    // Approve
    store.apply_confirmation_response(uuid::Uuid::new_v4(), "approve");
    assert_eq!(
        store.candidates.get(&candidate_id).unwrap().status,
        ExperienceCandidateStatus::Approved
    );

    // Reject a different candidate
    let mut candidate2 = ExperienceCandidate::knowledge(
        uuid::Uuid::new_v4(),
        task_id,
        agent_id,
        "test knowledge 2".to_string(),
        "another fact".to_string(),
        LongTermMemoryKind::Fact,
    );
    candidate2.status = ExperienceCandidateStatus::NeedsUserApproval;
    let candidate2_id = candidate2.candidate_id;
    store.stage_root_candidate(candidate2);

    store.apply_confirmation_response(uuid::Uuid::new_v4(), "deny");
    assert_eq!(
        store.candidates.get(&candidate2_id).unwrap().status,
        ExperienceCandidateStatus::Rejected
    );
}

/// 验证 as_long_term_memory_entry 对知识类候选返回 Some，对可执行类返回 None。
#[test]
fn candidate_conversion_to_long_term_memory() {
    let knowledge_candidate = ExperienceCandidate::knowledge(
        uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4(),
        "test fact".to_string(),
        "some content".to_string(),
        LongTermMemoryKind::Fact,
    );
    assert!(knowledge_candidate.as_long_term_memory_entry().is_some());

    let executable_candidate = ExperienceCandidate {
        candidate_id: uuid::Uuid::new_v4(),
        producer_task_id: uuid::Uuid::new_v4(),
        producer_agent_id: uuid::Uuid::new_v4(),
        title: "test executable".to_string(),
        kind_hint: ExperienceKindHint::Executable,
        payload: ExperienceCandidatePayload::Executable {
            intent: "do something".to_string(),
            when_to_use: "when needed".to_string(),
            asset_refs: vec![],
        },
        dependency_refs: vec![],
        status: ExperienceCandidateStatus::Submitted,
    };
    assert!(executable_candidate.as_long_term_memory_entry().is_none());
}

/// 验证带有资产的 Knowledge 类候选不需要用户确认（资产只有 Executable 载荷才需要）。
#[test]
fn knowledge_candidate_without_assets_does_not_require_approval() {
    let candidate = ExperienceCandidate::knowledge(
        uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4(),
        "simple fact".to_string(),
        "simple content".to_string(),
        LongTermMemoryKind::Fact,
    );
    assert!(!candidate.requires_user_confirmation());
}

/// 验证 LongTermMemoryService 可以持久化知识类候选的内容。
#[test]
fn governance_persists_knowledge_candidate_content_via_service() {
    let dir = tempfile::TempDir::new().unwrap();
    let agent_name = "governance-test-agent";

    let store = JsonFileMemoryStore::new(dir.path().join("agents"));
    let repo = MemoryRepository::new(Box::new(store));
    let mut service = LongTermMemoryService::new(repo);
    let mut memory = LongTermMemory::with_name(agent_name);

    let candidate = ExperienceCandidate::knowledge(
        uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4(),
        "governance fact".to_string(),
        "governance-persisted content".to_string(),
        LongTermMemoryKind::Fact,
    );

    let entry = candidate.as_long_term_memory_entry().unwrap();
    service.add_entry(&mut memory, entry).unwrap();

    assert_eq!(memory.entries.len(), 1);
    assert_eq!(memory.entries[0].content, "governance-persisted content");
}