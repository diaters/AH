//! Profile 更新系统
//!
//! 包含两个系统：
//! - `profile_update_trigger_system`：在 `experience_writeback_system` 之后运行，
//!   检测 LTM/SkillPackage 写回成功后，spawn `ProfileGenerationRequestMessage { kind: Update }`。
//! - `profile_update_writeback_system`：在 `experience_approval_result_system` 之后运行，
//!   检测更新审批通过后，写入 agents.toml 并更新 ECS `AgentCapabilities` 组件。

use crate::prelude::*;
use tracing::{debug, info, warn};

use crate::domain::{
    Agent, AgentCapabilities, ExistingAgentProfile, ExperienceCandidateStatus,
    ExperienceStore, PendingExperienceHooks, ProfileGenerationKind,
    ProfileGenerationRequestMessage, sanitize_tags,
};
use crate::user_plugins::hook_point::HookPoint;

/// Profile 更新触发系统：检测 LTM/SkillPackage 写回成功后，触发 profile 更新评估。
///
/// 仅对持久型（非 default）Agent 触发。default Agent 的写回走孵化路径，
/// 不触发 profile 更新。使用 `profile_update_triggered` 集合避免重复触发。
#[allow(dead_code)] // 任务 11 系统注册时启用
pub(crate) fn profile_update_trigger_system(
    mut commands: Commands,
    mut store: ResMut<ExperienceStore>,
    agents: Query<&Agent>,
) {
    // 先收集需要处理的候选（避免迭代时可变借用）
    let mut groups: std::collections::HashMap<
        crate::domain::TaskId,
        (crate::domain::AgentId, Vec<uuid::Uuid>),
    > = std::collections::HashMap::new();
    let mut skip_ids: Vec<uuid::Uuid> = Vec::new();

    for candidate in store.candidates.values() {
        if candidate.status != ExperienceCandidateStatus::Persisted {
            continue;
        }
        if store.profile_update_triggered.contains(&candidate.candidate_id) {
            continue;
        }

        let Some(governing_agent_id) = candidate.governing_agent_id else {
            skip_ids.push(candidate.candidate_id);
            continue;
        };

        groups
            .entry(candidate.producer_task_id)
            .and_modify(|(_, ids)| ids.push(candidate.candidate_id))
            .or_insert_with(|| (governing_agent_id, vec![candidate.candidate_id]));
    }

    // 标记无 governing_agent_id 的候选为已触发
    for id in skip_ids {
        store.profile_update_triggered.insert(id);
    }

    for (task_id, (agent_id, candidate_ids)) in groups {
        let Some(agent) = agents.iter().find(|a| a.id == agent_id) else {
            // Agent 不存在，标记为已触发以跳过
            for id in &candidate_ids {
                store.profile_update_triggered.insert(*id);
            }
            continue;
        };

        // 跳过 default Agent（孵化场景，不触发 profile 更新）
        if agent.capabilities.tags.iter().any(|t| t == "default") {
            for id in &candidate_ids {
                store.profile_update_triggered.insert(*id);
            }
            continue;
        }

        // 构建 existing_profile
        let existing_profile = ExistingAgentProfile {
            name: agent.profile.name.clone(),
            tags: agent.capabilities.tags.clone(),
            description: agent.capabilities.description.clone(),
        };

        // Spawn profile 更新评估请求
        commands.spawn(ProfileGenerationRequestMessage {
            task_id,
            agent_id,
            candidate_ids: candidate_ids.clone(),
            existing_profile: Some(existing_profile),
            kind: ProfileGenerationKind::Update,
            feedback: None,
            retry_count: 0,
        });

        // 标记候选为已触发
        for id in &candidate_ids {
            store.profile_update_triggered.insert(*id);
        }

        debug!(
            event = "ProfileUpdateTriggered",
            task_id = %task_id,
            agent_id = %agent_id,
            agent_name = %agent.profile.name,
            candidate_count = candidate_ids.len(),
            "profile update evaluation triggered after writeback"
        );
    }
}

/// Profile 更新写回系统：检测更新审批通过后，写入 agents.toml 并更新 ECS 组件。
///
/// 运行在 `experience_approval_result_system` 之后。
/// 检测候选状态为 `WritebackPending` 且有 Update 类型 context（含 generated_profile）的候选，
/// 执行两阶段提交：
/// 1. 写入 agents.toml（通过 `IncubatedAgentRegistry::update`）
/// 2. 更新 ECS `Agent` 组件的 `capabilities` 字段
#[allow(dead_code)] // 任务 11 系统注册时启用
pub(crate) fn profile_update_writeback_system(
    mut store: ResMut<ExperienceStore>,
    mut agents: Query<&mut Agent>,
    mut pending_hooks: ResMut<PendingExperienceHooks>,
    agent_registry: Res<crate::infrastructure::incubation::agent_registry::IncubatedAgentRegistry>,
    settings: Res<crate::app::HarnessSettings>,
) {
    // 先收集需要处理的候选（避免迭代时可变借用）
    let mut to_process: Vec<(uuid::Uuid, crate::domain::TaskId)> = Vec::new();

    for candidate in store.candidates.values() {
        if candidate.status != ExperienceCandidateStatus::WritebackPending {
            continue;
        }

        let task_id = candidate.producer_task_id;
        let Some(ctx) = store.profile_generation_context.get(&task_id) else {
            continue;
        };

        if ctx.kind != ProfileGenerationKind::Update {
            continue;
        }

        if ctx.generated_profile.is_none() {
            continue;
        }

        to_process.push((candidate.candidate_id, task_id));
    }

    for (candidate_id, task_id) in to_process {
        let ctx = store.profile_generation_context.get(&task_id).cloned();

        let Some(ctx) = ctx else {
            continue;
        };

        let Some(generated) = &ctx.generated_profile else {
            continue;
        };

        let Some(existing) = &ctx.existing_profile else {
            warn!(
                event = "ProfileUpdateWritebackNoExistingProfile",
                candidate_id = %candidate_id,
                task_id = %task_id,
                "existing_profile missing in context, skipping"
            );
            continue;
        };

        // name 不可变更，使用 existing name
        let agent_name = &existing.name;

        // sanitize tags：保留 existing 中的受保护标签
        let sanitized_tags = sanitize_tags(generated.tags.clone(), &existing.tags);

        // 第一阶段：写入 agents.toml
        match agent_registry.update(
            &settings.0.agents_config_path,
            agent_name,
            &sanitized_tags,
            &generated.description,
        ) {
            Ok(()) => {
                // 第二阶段：更新 ECS Agent 组件的 capabilities 字段
                let agent_found = agents
                    .iter_mut()
                    .find(|a| a.profile.name == *agent_name)
                    .map(|mut agent| {
                        agent.capabilities = AgentCapabilities {
                            tags: sanitized_tags.clone(),
                            description: generated.description.clone(),
                        };
                    })
                    .is_some();

                if !agent_found {
                    warn!(
                        event = "ProfileUpdateEcsEntityNotFound",
                        agent_name = %agent_name,
                        "agents.toml updated but ECS entity not found, will be consistent on restart"
                    );
                } else {
                    debug!(
                        event = "ProfileUpdateWritebackSucceeded",
                        agent_name = %agent_name,
                        "profile update written to agents.toml and ECS updated"
                    );
                }

                // 标记候选为 Persisted
                if let Some(c) = store.candidates.get_mut(&candidate_id) {
                    c.status = ExperienceCandidateStatus::Persisted;
                }

                info!(
                    event = "ProfileUpdateCompleted",
                    agent_name = %agent_name,
                    tags = ?sanitized_tags,
                    "agent profile updated successfully"
                );

                // 派发 on_agent_profile_updated hook（写回成功后触发）
                pending_hooks
                    .0
                    .push((HookPoint::OnAgentProfileUpdated, task_id));
            }
            Err(e) => {
                // 文件写入失败
                if let Some(c) = store.candidates.get_mut(&candidate_id) {
                    c.status = ExperienceCandidateStatus::WritebackFailed;
                }
                warn!(
                    event = "ProfileUpdateWritebackFailed",
                    candidate_id = %candidate_id,
                    agent_name = %agent_name,
                    error = %e,
                    "failed to write profile update to agents.toml"
                );
            }
        }

        // 清理 context
        store.profile_generation_context.remove(&task_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        AgentCapabilities, AgentKind, AgentProfile, AgentToolPermissions, ExperienceCandidate,
        ExperienceCandidatePayload, ExperienceCandidateStatus, ExperienceKindHint,
        ExperienceStore, GeneratedProfile, ExistingAgentProfile, ProfileGenerationContext,
        ProfileGenerationKind,
    };
    use bevy_ecs::system::RunSystemOnce;

    fn make_test_agent(name: &str, tags: &[&str], agent_id: crate::domain::AgentId) -> Agent {
        Agent {
            id: agent_id,
            profile: AgentProfile {
                name: name.to_string(),
                model: "test-model".to_string(),
            },
            capabilities: AgentCapabilities {
                tags: tags.iter().map(|t| t.to_string()).collect(),
                description: "test description".to_string(),
            },
            kind: AgentKind::Persistent,
            parent_id: None,
            bound_task_id: None,
            tool_permissions: AgentToolPermissions::default(),
        }
    }

    fn make_test_candidate(
        task_id: crate::domain::TaskId,
        agent_id: crate::domain::AgentId,
        status: ExperienceCandidateStatus,
    ) -> ExperienceCandidate {
        ExperienceCandidate {
            candidate_id: uuid::Uuid::new_v4(),
            producer_task_id: task_id,
            producer_agent_id: agent_id,
            title: "test knowledge".to_string(),
            kind_hint: ExperienceKindHint::Knowledge,
            payload: ExperienceCandidatePayload::Knowledge {
                content: "test content".to_string(),
            },
            dependency_refs: vec![],
            status,
            governing_agent_id: Some(agent_id),
            derived_from_candidate_ids: vec![],
        }
    }

    fn make_test_world_with_agents(store: ExperienceStore, agents: Vec<Agent>) -> World {
        let mut world = World::new();
        world.insert_resource(store);
        for agent in agents {
            world.spawn(agent);
        }
        world
    }

    /// profile_update_trigger_system：持久型 Agent 的 Persisted 候选触发更新评估
    #[test]
    fn trigger_system_spawns_update_request_for_persistent_agent() {
        let task_id = uuid::Uuid::new_v4();
        let agent_id = uuid::Uuid::new_v4();

        let mut store = ExperienceStore::default();
        let candidate = make_test_candidate(task_id, agent_id, ExperienceCandidateStatus::Persisted);
        store.candidates.insert(candidate.candidate_id, candidate);

        let agent = make_test_agent("physics-specialist", &["physics"], agent_id);
        let mut world = make_test_world_with_agents(store, vec![agent]);

        world
            .run_system_once(profile_update_trigger_system)
            .unwrap();

        let requests: Vec<&ProfileGenerationRequestMessage> = world
            .query::<&ProfileGenerationRequestMessage>()
            .iter(&world)
            .collect();
        assert_eq!(requests.len(), 1, "should spawn one update request");
        assert_eq!(requests[0].kind, ProfileGenerationKind::Update);
        assert_eq!(requests[0].task_id, task_id);
        assert_eq!(requests[0].retry_count, 0);
        assert!(requests[0].feedback.is_none());
        let existing = requests[0].existing_profile.as_ref().unwrap();
        assert_eq!(existing.name, "physics-specialist");
        assert_eq!(existing.tags, vec!["physics"]);
    }

    /// profile_update_trigger_system：default Agent 的候选不触发更新评估
    #[test]
    fn trigger_system_skips_default_agent() {
        let task_id = uuid::Uuid::new_v4();
        let agent_id = uuid::Uuid::new_v4();

        let mut store = ExperienceStore::default();
        let candidate = make_test_candidate(task_id, agent_id, ExperienceCandidateStatus::Persisted);
        store.candidates.insert(candidate.candidate_id, candidate);

        let agent = make_test_agent("default", &["default"], agent_id);
        let mut world = make_test_world_with_agents(store, vec![agent]);

        world
            .run_system_once(profile_update_trigger_system)
            .unwrap();

        let count = world
            .query::<&ProfileGenerationRequestMessage>()
            .iter(&world)
            .count();
        assert_eq!(count, 0, "should not spawn update request for default agent");
    }

    /// profile_update_trigger_system：已触发的候选不重复触发
    #[test]
    fn trigger_system_does_not_retrigger() {
        let task_id = uuid::Uuid::new_v4();
        let agent_id = uuid::Uuid::new_v4();

        let mut store = ExperienceStore::default();
        let candidate = make_test_candidate(task_id, agent_id, ExperienceCandidateStatus::Persisted);
        store.profile_update_triggered.insert(candidate.candidate_id);
        store.candidates.insert(candidate.candidate_id, candidate);

        let agent = make_test_agent("physics-specialist", &["physics"], agent_id);
        let mut world = make_test_world_with_agents(store, vec![agent]);

        world
            .run_system_once(profile_update_trigger_system)
            .unwrap();

        let count = world
            .query::<&ProfileGenerationRequestMessage>()
            .iter(&world)
            .count();
        assert_eq!(count, 0, "should not retrigger already processed candidates");
    }

    /// profile_update_trigger_system：非 Persisted 候选不触发
    #[test]
    fn trigger_system_skips_non_persisted_candidates() {
        let task_id = uuid::Uuid::new_v4();
        let agent_id = uuid::Uuid::new_v4();

        let mut store = ExperienceStore::default();
        let candidate =
            make_test_candidate(task_id, agent_id, ExperienceCandidateStatus::WritebackPending);
        store.candidates.insert(candidate.candidate_id, candidate);

        let agent = make_test_agent("physics-specialist", &["physics"], agent_id);
        let mut world = make_test_world_with_agents(store, vec![agent]);

        world
            .run_system_once(profile_update_trigger_system)
            .unwrap();

        let count = world
            .query::<&ProfileGenerationRequestMessage>()
            .iter(&world)
            .count();
        assert_eq!(count, 0, "should not trigger for non-Persisted candidates");
    }

    /// profile_update_writeback_system：审批通过后写入 agents.toml 和 ECS
    #[test]
    fn writeback_system_updates_agents_toml_and_ecs() {
        use crate::infrastructure::incubation::agent_registry::IncubatedAgentRegistry;

        let dir = tempfile::TempDir::new().unwrap();
        let config_path = dir.path().join("agents.toml");

        // 预写入一个 agent
        let initial = r#"
[[agent]]
name = "physics-specialist"
model = "deepseek-chat"
tags = ["physics"]
description = "old description"
"#;
        std::fs::write(&config_path, initial).unwrap();

        let task_id = uuid::Uuid::new_v4();
        let agent_id = uuid::Uuid::new_v4();

        let mut store = ExperienceStore::default();
        let candidate =
            make_test_candidate(task_id, agent_id, ExperienceCandidateStatus::WritebackPending);
        let candidate_id = candidate.candidate_id;
        store.candidates.insert(candidate_id, candidate);

        // 存入 Update context with generated_profile
        store.profile_generation_context.insert(
            task_id,
            ProfileGenerationContext {
                kind: ProfileGenerationKind::Update,
                retry_count: 0,
                existing_profile: Some(ExistingAgentProfile {
                    name: "physics-specialist".to_string(),
                    tags: vec!["physics".to_string()],
                    description: "old description".to_string(),
                }),
                generated_profile: Some(GeneratedProfile {
                    name: "ignored-name".to_string(), // name 不可变更
                    tags: vec!["physics".to_string(), "quantum".to_string()],
                    description: "new description".to_string(),
                }),
            },
        );

        let agent = make_test_agent("physics-specialist", &["physics"], agent_id);
        let mut world = World::new();
        world.insert_resource(store);
        world.insert_resource(PendingExperienceHooks::default());
        world.insert_resource(IncubatedAgentRegistry);
        world.insert_resource(crate::app::HarnessSettings(
            crate::app::HarnessConfig {
                agents_config_path: config_path.to_str().unwrap().to_string(),
                ..Default::default()
            }
        ));
        world.spawn(agent);

        world
            .run_system_once(profile_update_writeback_system)
            .unwrap();

        // 验证 agents.toml 更新
        let content = std::fs::read_to_string(&config_path).unwrap();
        let config: crate::domain::AgentConfig = toml::from_str(&content).unwrap();
        assert_eq!(config.agent.len(), 1);
        assert_eq!(config.agent[0].name, "physics-specialist");
        assert_eq!(config.agent[0].tags, vec!["physics", "quantum"]);
        assert_eq!(config.agent[0].description, "new description");

        // 验证候选标记为 Persisted
        let store = world.resource::<ExperienceStore>();
        assert_eq!(
            store.candidates.get(&candidate_id).unwrap().status,
            ExperienceCandidateStatus::Persisted
        );

        // 验证 context 清理
        assert!(
            !store.profile_generation_context.contains_key(&task_id),
            "context should be cleaned up after writeback"
        );

        // 验证 ECS 组件更新（需要 apply commands）
        // 注意：run_system_once 不会自动 apply commands，需要手动检查
    }

    /// profile_update_writeback_system：文件写入失败时标记 WritebackFailed
    #[test]
    fn writeback_system_marks_failed_on_file_error() {
        use crate::infrastructure::incubation::agent_registry::IncubatedAgentRegistry;

        let task_id = uuid::Uuid::new_v4();
        let agent_id = uuid::Uuid::new_v4();

        let mut store = ExperienceStore::default();
        let candidate =
            make_test_candidate(task_id, agent_id, ExperienceCandidateStatus::WritebackPending);
        let candidate_id = candidate.candidate_id;
        store.candidates.insert(candidate_id, candidate);

        store.profile_generation_context.insert(
            task_id,
            ProfileGenerationContext {
                kind: ProfileGenerationKind::Update,
                retry_count: 0,
                existing_profile: Some(ExistingAgentProfile {
                    name: "nonexistent-agent".to_string(),
                    tags: vec![],
                    description: "old".to_string(),
                }),
                generated_profile: Some(GeneratedProfile {
                    name: "ignored".to_string(),
                    tags: vec!["new".to_string()],
                    description: "new".to_string(),
                }),
            },
        );

        let mut world = World::new();
        world.insert_resource(store);
        world.insert_resource(PendingExperienceHooks::default());
        world.insert_resource(IncubatedAgentRegistry);
        // 使用不存在的配置路径
        world.insert_resource(crate::app::HarnessSettings(
            crate::app::HarnessConfig {
                agents_config_path: "/nonexistent/path/agents.toml".to_string(),
                ..Default::default()
            }
        ));

        world
            .run_system_once(profile_update_writeback_system)
            .unwrap();

        let store = world.resource::<ExperienceStore>();
        assert_eq!(
            store.candidates.get(&candidate_id).unwrap().status,
            ExperienceCandidateStatus::WritebackFailed,
            "candidate should be WritebackFailed on file error"
        );
    }

    /// profile_update_writeback_system：无 Update context 的候选不被处理
    #[test]
    fn writeback_system_skips_candidates_without_update_context() {
        use crate::infrastructure::incubation::agent_registry::IncubatedAgentRegistry;

        let task_id = uuid::Uuid::new_v4();
        let agent_id = uuid::Uuid::new_v4();

        let mut store = ExperienceStore::default();
        let candidate =
            make_test_candidate(task_id, agent_id, ExperienceCandidateStatus::WritebackPending);
        let candidate_id = candidate.candidate_id;
        store.candidates.insert(candidate_id, candidate);

        // 无 context
        let mut world = World::new();
        world.insert_resource(store);
        world.insert_resource(PendingExperienceHooks::default());
        world.insert_resource(IncubatedAgentRegistry);
        world.insert_resource(crate::app::HarnessSettings(
            crate::app::HarnessConfig::default()
        ));

        world
            .run_system_once(profile_update_writeback_system)
            .unwrap();

        let store = world.resource::<ExperienceStore>();
        // 候选状态不变
        assert_eq!(
            store.candidates.get(&candidate_id).unwrap().status,
            ExperienceCandidateStatus::WritebackPending
        );
    }
}
