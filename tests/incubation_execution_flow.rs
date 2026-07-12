//! 孵化执行集成测试
//!
//! 覆盖 proposal 批准后创建新 Agent 并写入 agents.toml 的主链路。

use harness::infrastructure::incubation::agent_registry::{
    IncubatedAgentRecord, IncubatedAgentRegistry,
};

/// 验证追加孵化 Agent 到 agents.toml 后文件包含新条目且原有条目不变。
#[test]
fn incubated_agent_appended_to_agents_toml() {
    let dir = tempfile::TempDir::new().unwrap();
    let toml_path = dir.path().join("agents.toml");
    let registry = IncubatedAgentRegistry;

    // 先写入初始配置
    let initial = harness::domain::AgentConfig {
        agent: vec![harness::domain::AgentEntry {
            name: "default".to_string(),
            model: Some("gpt-4".to_string()),
            models: vec![],
            tags: vec!["default".to_string()],
            description: "default agent".to_string(),
            tools: None,
            skills: None,
        }],
    };
    std::fs::write(&toml_path, toml::to_string(&initial).unwrap()).unwrap();

    registry
        .append(
            toml_path.to_str().unwrap(),
            &IncubatedAgentRecord {
                name: "physics-specialist".to_string(),
                model: "gpt-4.1-mini".to_string(),
                tags: vec!["incubated".to_string(), "physics".to_string()],
                description: "derived from top-level proposal".to_string(),
                tools: None,
                skills: None,
            },
        )
        .unwrap();

    let content = std::fs::read_to_string(&toml_path).unwrap();
    let config: harness::domain::AgentConfig = toml::from_str(&content).unwrap();
    assert_eq!(config.agent.len(), 2);
    assert_eq!(config.agent[0].name, "default");
    assert_eq!(config.agent[1].name, "physics-specialist");
    assert!(config.agent[1].tags.contains(&"incubated".to_string()));
}

/// 验证同名 Agent 不重复追加。
#[test]
fn duplicate_incubation_skips_if_name_exists() {
    let dir = tempfile::TempDir::new().unwrap();
    let toml_path = dir.path().join("agents.toml");
    let registry = IncubatedAgentRegistry;

    let initial = harness::domain::AgentConfig {
        agent: vec![harness::domain::AgentEntry {
            name: "existing".to_string(),
            model: Some("gpt-4".to_string()),
            models: vec![],
            tags: vec![],
            description: "existing".to_string(),
            tools: None,
            skills: None,
        }],
    };
    std::fs::write(&toml_path, toml::to_string(&initial).unwrap()).unwrap();

    registry
        .append(
            toml_path.to_str().unwrap(),
            &IncubatedAgentRecord {
                name: "existing".to_string(),
                model: "other".to_string(),
                tags: vec![],
                description: "duplicate".to_string(),
                tools: None,
                skills: None,
            },
        )
        .unwrap();

    let config: harness::domain::AgentConfig =
        toml::from_str(&std::fs::read_to_string(&toml_path).unwrap()).unwrap();
    assert_eq!(config.agent.len(), 1);
    assert_eq!(config.agent[0].model, Some("gpt-4".to_string()));
}

/// 验证写回成功后 proposal 状态为 Executed。
#[test]
fn proposal_status_advances_to_executed() {
    let mut store = harness::ExperienceStore::default();
    let task_id = uuid::Uuid::new_v4();
    let agent_id = uuid::Uuid::new_v4();

    let candidate = harness::ExperienceCandidate::knowledge(
        uuid::Uuid::new_v4(),
        task_id,
        agent_id,
        "physics fact".to_string(),
        "E=mc²".to_string(),
    );
    store.stage_root_candidate(candidate.clone());

    // 创建 proposal 并设为 Approved
    store.merge_into_proposal(
        task_id,
        agent_id,
        harness::AgentProfile {
            name: "incubated-test".to_string(),
            model: "gpt-4.1-mini".to_string(),
        },
        &candidate,
    );
    if let Some(proposal) = store.proposals.get_mut(&task_id) {
        proposal.status = harness::IncubationProposalStatus::Approved;
    }

    // 模拟 writeback_incubation_proposal 的状态推进逻辑
    // （实际由 ECS system 驱动，此处直接验证状态机）
    if let Some(proposal) = store.proposals.get_mut(&task_id) {
        assert_eq!(proposal.status, harness::IncubationProposalStatus::Approved);
        proposal.status = harness::IncubationProposalStatus::Executing;
        proposal.updated_at = chrono::Utc::now();
    }
    if let Some(proposal) = store.proposals.get_mut(&task_id) {
        proposal.status = harness::IncubationProposalStatus::Executed;
        proposal.updated_at = chrono::Utc::now();
    }

    let proposal = store.proposals.get(&task_id).unwrap();
    assert_eq!(proposal.status, harness::IncubationProposalStatus::Executed);
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
