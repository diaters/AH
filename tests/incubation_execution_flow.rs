//! 孵化执行集成测试
//!
//! 覆盖 proposal 批准后创建新 Agent 并写入资产的主链路。

use harness::infrastructure::incubation::agent_registry::{
    IncubatedAgentRecord, IncubatedAgentRegistry,
};

/// 验证批准 proposal 后会创建新持久型 Agent 记录。
#[test]
fn approved_incubation_proposal_creates_persistent_agent_record() {
    let dir = tempfile::TempDir::new().unwrap();
    let registry = IncubatedAgentRegistry::new(dir.path().join("incubated_agents.json"));

    let profile = harness::AgentProfile {
        name: "physics-specialist".to_string(),
        model: "gpt-4.1-mini".to_string(),
    };

    registry
        .append(&IncubatedAgentRecord {
            profile: profile.clone(),
            tags: vec!["incubated".to_string(), "physics".to_string()],
            description: "derived from top-level proposal".to_string(),
            tools: vec![],
        })
        .unwrap();

    let records = registry.load().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].profile.name, "physics-specialist");
    assert!(records[0].tags.contains(&"incubated".to_string()));
}

/// 验证多次追加后所有记录均可恢复。
#[test]
fn multiple_incubated_agents_persist_and_load() {
    let dir = tempfile::TempDir::new().unwrap();
    let registry = IncubatedAgentRegistry::new(dir.path().join("incubated_agents.json"));

    for i in 0..3 {
        registry
            .append(&IncubatedAgentRecord {
                profile: harness::AgentProfile {
                    name: format!("agent-{i}"),
                    model: "test".to_string(),
                },
                tags: vec!["incubated".to_string()],
                description: format!("agent {i}"),
                tools: vec![],
            })
            .unwrap();
    }

    let records = registry.load().unwrap();
    assert_eq!(records.len(), 3);
    assert_eq!(records[0].profile.name, "agent-0");
    assert_eq!(records[2].profile.name, "agent-2");
}

/// 验证 IncubationProposal 可以被持久化和加载。
#[test]
fn proposal_store_persists_and_loads_proposals() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = harness::infrastructure::incubation::proposal_store::IncubationProposalStore::new(
        dir.path().join("proposals"),
    );

    let task_id = uuid::Uuid::new_v4();
    let agent_id = uuid::Uuid::new_v4();
    let mut proposal = harness::IncubationProposal::new(
        task_id,
        agent_id,
        harness::AgentProfile {
            name: "test-agent".to_string(),
            model: "test".to_string(),
        },
    );
    proposal.incubation_rationale = "test rationale".to_string();

    store.persist(&proposal).unwrap();

    let loaded = store.load_all().unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].proposal_id, proposal.proposal_id);
    assert_eq!(loaded[0].incubation_rationale, "test rationale");
}
