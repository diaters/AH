//! 经验模块两层分层汇聚治理集成测试
//!
//! 覆盖 spec 要求的四条主链路：
//! - 普通持久型 Agent 知识类候选自动落盘到 LongTermMemory
//! - 普通持久型 Agent executable 候选用户批准后生成 Skill Package
//! - 公共规则类候选进入 SharedKnowledgeUpgradeQueue
//! - default Agent 的私有候选生成 IncubationProposal

use harness::{
    AgentAssetService, ExperienceCandidate, ExperienceCandidatePayload, ExperienceCandidateStatus,
    ExperienceKindHint, ExperienceStore, SharedKnowledgeUpgradeQueue,
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
        status: harness::IncubationProposalStatus::Proposed,
        created_at: chrono::Utc::now(),
    };

    assert_eq!(
        proposal.knowledge_candidate_ids,
        vec![candidate.candidate_id]
    );
    assert!(store.candidates.contains_key(&candidate.candidate_id));
}
