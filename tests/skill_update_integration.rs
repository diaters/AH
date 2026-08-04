//! Skill 更新与经验治理集成测试
//!
//! 覆盖 plan 任务 25-26：
//! - 任务 25：持久Agent 吸收路径（skill-updater / LTM / governance / parent inbox）
//! - 任务 26：skill 更新 apply / 失败保护 / self_updatable 降级 / kind_filter 循环防护
//!
//! 全部使用 `build_harness_app` 构造完整 ECS World，通过 `app.update()` 推进帧，
//! 验证端到端 system 链路行为。

use std::sync::Arc;

use bevy_ecs::entity::Entity;
use crossbeam_channel::unbounded;
use harness::infrastructure::skills::{SkillEntry, SkillId, SkillLoader, SkillRegistry};
use harness::{
    Agent, AgentCapabilities, AgentExecutionOutput, AgentExecutionRequest, AgentExecutor,
    AgentKind, AgentProfile, AgentRequestKind, AgentToolPermissions, ChannelId,
    ConversationMessage, EntityIndex, ExperienceCandidate, ExperienceCandidateStatus,
    ExperienceCollectionCompletedMessage, ExperienceGovernanceRequestMessage, ExperienceKindFilter,
    ExperienceKindHint, ExperienceStore, ExperienceWritebackDestination, FrontendKind,
    HarnessConfig, LongTermMemory, ShortTermMemory, SkillUpdateCompletedMessage,
    SkillUpdateContext, SkillUpdateOperation, Task, TaskExperiencePolicy, TaskInjectedSkill,
    TaskRoutingPolicy, TaskStatus, ToolCallingState, ToolDefinition, ToolExecutionRequestMessage,
    ToolPermission, WorkItem, WorkItemType, build_harness_app, llm::ExecutorRegistry,
};
use tempfile::TempDir;
use tokio::runtime::Runtime;
use uuid::Uuid;

// ============ 通用测试辅助 ============

fn default_channel() -> ChannelId {
    ChannelId {
        frontend: FrontendKind::Tui,
        user_id: "default".to_string(),
        thread_id: None,
    }
}

fn no_brain_test_config() -> HarnessConfig {
    HarnessConfig {
        agents_config_path: "/nonexistent_agents.toml".to_string(),
        providers_config_path: "/nonexistent_providers.toml".to_string(),
        ..HarnessConfig::default()
    }
}

struct NoOpExecutor;

impl AgentExecutor for NoOpExecutor {
    fn execute(&self, _request: AgentExecutionRequest) -> harness::ExecutorFuture {
        Box::pin(async move {
            Ok(AgentExecutionOutput {
                content: harness::OutputContent::Text("ok".to_string()),
                reasoning_content: None,
            })
        })
    }
}

fn make_persistent_agent(id: Uuid, name: &str, tags: Vec<&str>) -> Agent {
    Agent {
        id,
        profile: AgentProfile {
            name: name.to_string(),
            model: "test-model".to_string(),
        },
        capabilities: AgentCapabilities {
            tags: tags.into_iter().map(|t| t.to_string()).collect(),
            description: format!("persistent agent {}", name),
        },
        kind: AgentKind::Persistent,
        parent_id: None,
        bound_task_id: None,
        tool_permissions: AgentToolPermissions::default(),
        system_prompt: None,
    }
}

fn make_temporary_agent(id: Uuid, name: &str, parent_id: Uuid) -> Agent {
    Agent {
        id,
        profile: AgentProfile {
            name: name.to_string(),
            model: "test-model".to_string(),
        },
        capabilities: AgentCapabilities {
            tags: vec![],
            description: format!("temporary agent {}", name),
        },
        kind: AgentKind::TaskScoped,
        parent_id: Some(parent_id),
        bound_task_id: None,
        tool_permissions: AgentToolPermissions::default(),
        system_prompt: None,
    }
}

fn make_task(task_id: Uuid, delegate: Option<Uuid>, parent_task_id: Option<Uuid>) -> Task {
    let now = chrono::Utc::now();
    Task {
        id: task_id,
        content: "test task content".to_string(),
        creator: Uuid::nil(),
        delegate,
        status: TaskStatus::Done,
        pending_confirmation_id: None,
        input_summary: String::new(),
        result_summary: String::new(),
        priority: 0,
        created_at: now,
        updated_at: now,
        retry_count: 0,
        max_retries: 3,
        next_retry_at: None,
        last_error: None,
        multi_turn: false,
        parent_task_id,
        batch_id: None,
        origin_channel: Some(default_channel()),
        routing_policy: TaskRoutingPolicy::event(None, None),
        last_evaluated_turn: None,
    }
}

fn make_skill_entry(skill_id: SkillId, self_updatable: bool) -> SkillEntry {
    SkillEntry {
        owner_agent_name: skill_id.owner_agent_name.clone(),
        skill_id: skill_id.clone(),
        name: skill_id.skill_name.clone(),
        description: format!("desc for {}", skill_id.skill_name),
        instructions: format!("instructions for {}", skill_id.skill_name),
        version: 1,
        self_updatable,
    }
}

/// 构造一个已进入 GovernancePending 状态的 Skill 类候选。
fn make_governance_pending_skill_candidate(
    producer_task_id: Uuid,
    governing_agent_id: Uuid,
) -> ExperienceCandidate {
    let mut c = ExperienceCandidate::skill(
        Uuid::new_v4(),
        producer_task_id,
        governing_agent_id,
        "test skill".to_string(),
        "test-skill".to_string(),
        "desc".to_string(),
        "instructions".to_string(),
        Vec::new(),
    );
    c.status = ExperienceCandidateStatus::GovernancePending;
    c.governing_agent_id = Some(governing_agent_id);
    c
}

/// 构造一个 Submitted 状态的 Skill 类候选（用于持久Agent 吸收路径）。
fn make_submitted_skill_candidate(
    producer_task_id: Uuid,
    producer_agent_id: Uuid,
) -> ExperienceCandidate {
    ExperienceCandidate::skill(
        Uuid::new_v4(),
        producer_task_id,
        producer_agent_id,
        "test skill".to_string(),
        "test-skill".to_string(),
        "desc".to_string(),
        "instructions".to_string(),
        Vec::new(),
    )
}

/// 构造一个 Submitted 状态的 Knowledge 类候选。
fn make_submitted_knowledge_candidate(
    producer_task_id: Uuid,
    producer_agent_id: Uuid,
) -> ExperienceCandidate {
    ExperienceCandidate::knowledge(
        Uuid::new_v4(),
        producer_task_id,
        producer_agent_id,
        "test knowledge".to_string(),
        "test content".to_string(),
    )
}

/// 创建测试用 App（含 NoOpExecutor），返回已推进一帧的 App。
fn create_test_app(config: HarnessConfig) -> bevy_app::App {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(NoOpExecutor);
    let executor_registry = ExecutorRegistry::from_single_executor(executor, "default");
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(
        config,
        runtime,
        executor_registry,
        input_rx,
        vec![],
        harness::channels::ChannelManager::empty().0,
    );
    // 第一帧：运行 Startup 系统（load_agents / plugin_load）
    app.update();
    app
}

// ============ 任务 25：持久Agent 吸收路径 ============

/// 持久Agent + 注入 skill + Skill 类候选 → spawn SkillUpdateRequestMessage。
///
/// 验证：经验收集完成后，候选被路由到 skill-updater（SkillUpdateRequestMessage 被 spawn）。
/// 由于 skill_update_workitem_system 在同帧消费 SkillUpdateRequestMessage 并创建 WorkItem，
/// 通过验证 WorkItem 是否创建来间接确认 SkillUpdateRequestMessage 曾被 spawn。
#[test]
fn persistent_agent_with_skill_skill_kind_triggers_skill_updater() {
    let skill_id = SkillId::new("worker-agent", "coding");
    // Bug C 修复后 skill_update_workitem_system 需要读取 SKILL.md 完整内容构造 prompt，
    // 测试必须提供真实文件，否则系统会因读文件失败而 despawn 请求。
    let (tmp, loader) = setup_skill_dir(&skill_id, SAMPLE_SKILL_MD);

    let mut app = create_test_app(no_brain_test_config());
    // 覆盖 SkillLoader 指向临时目录
    app.insert_resource(loader.clone());

    let agent_id = Uuid::new_v4();
    let task_id = Uuid::new_v4();

    // Spawn 持久Agent（非 default，避免走 incubation 路径）
    let agent_entity = app
        .world_mut()
        .spawn(make_persistent_agent(agent_id, "worker-agent", vec!["llm"]))
        .id();
    // 测试夹具绕过 spawn_agent 封装直接 spawn，需手动写入 EntityIndex
    app.world_mut()
        .resource_mut::<EntityIndex>()
        .agents
        .insert(agent_id, agent_entity);

    // Spawn skill-updater Agent（让 skill_update_workitem_system 能找到 handler）
    let skill_updater_id = Uuid::new_v4();
    app.world_mut().spawn(make_persistent_agent(
        skill_updater_id,
        "skill-updater",
        vec!["skill-updater"],
    ));

    // Task 注入了 skill
    let task_entity = app
        .world_mut()
        .spawn((
            make_task(task_id, Some(agent_id), None),
            TaskInjectedSkill {
                skill_id: Some(skill_id.clone()),
            },
        ))
        .id();
    // 测试夹具绕过 spawn_task 封装直接 spawn，需手动写入 EntityIndex
    app.world_mut()
        .resource_mut::<EntityIndex>()
        .tasks
        .insert(task_id, task_entity);

    // 预置 Skill 类候选（Submitted 状态，stage 为 root candidate）
    let candidate = make_submitted_skill_candidate(task_id, agent_id);
    let candidate_id = candidate.candidate_id;
    app.world_mut()
        .resource_mut::<ExperienceStore>()
        .stage_root_candidate(candidate);

    // 预置 SkillRegistry
    app.world_mut()
        .resource_mut::<SkillRegistry>()
        .upsert(make_skill_entry(skill_id.clone(), true));

    // 触发经验收集完成
    app.world_mut().spawn(ExperienceCollectionCompletedMessage {
        task_id,
        parent_task_id: None,
        agent_id,
        governing_agent_id: agent_id,
    });

    app.update();

    // 验证：候选已被 collect_top_level_governance_candidates 推进到 GovernancePending，
    // 然后 route_persistent_agent_experience spawn 了 SkillUpdateRequestMessage，
    // skill_update_workitem_system 同帧消费并创建 SkillUpdate WorkItem。
    let has_skill_update_workitem = {
        let mut q = app.world_mut().query::<&WorkItem>();
        q.iter(app.world())
            .any(|wi| wi.work_type == WorkItemType::SkillUpdate)
    };
    assert!(
        has_skill_update_workitem,
        "should spawn SkillUpdate WorkItem (consumed SkillUpdateRequestMessage)"
    );

    // 候选状态：在当前实现下，spawn_skill_update_workitem 不修改候选状态，
    // 候选保持 GovernancePending（由 collect_top_level_governance_candidates 设置）。
    let store = app.world().resource::<ExperienceStore>();
    let candidate = store
        .candidates
        .get(&candidate_id)
        .expect("candidate exists");
    assert_eq!(
        candidate.status,
        ExperienceCandidateStatus::GovernancePending,
        "candidate status should be GovernancePending (skill-updater placeholder does not modify status)"
    );

    drop(tmp);
}

/// 持久Agent + 注入 skill + Knowledge 类候选 → 候选置 WritebackPending。
#[test]
fn persistent_agent_with_skill_knowledge_kind_writes_ltm() {
    let mut app = create_test_app(no_brain_test_config());

    let agent_id = Uuid::new_v4();
    let task_id = Uuid::new_v4();
    let skill_id = SkillId::new("worker-agent", "coding");

    let agent_entity = app
        .world_mut()
        .spawn(make_persistent_agent(agent_id, "worker-agent", vec!["llm"]))
        .id();
    // 测试夹具绕过 spawn_agent 封装直接 spawn，需手动写入 EntityIndex
    app.world_mut()
        .resource_mut::<EntityIndex>()
        .agents
        .insert(agent_id, agent_entity);
    let task_entity = app
        .world_mut()
        .spawn((
            make_task(task_id, Some(agent_id), None),
            TaskInjectedSkill {
                skill_id: Some(skill_id.clone()),
            },
        ))
        .id();
    // 测试夹具绕过 spawn_task 封装直接 spawn，需手动写入 EntityIndex
    app.world_mut()
        .resource_mut::<EntityIndex>()
        .tasks
        .insert(task_id, task_entity);

    let candidate = make_submitted_knowledge_candidate(task_id, agent_id);
    let candidate_id = candidate.candidate_id;
    app.world_mut()
        .resource_mut::<ExperienceStore>()
        .stage_root_candidate(candidate);

    app.world_mut()
        .resource_mut::<SkillRegistry>()
        .upsert(make_skill_entry(skill_id, true));

    app.world_mut().spawn(ExperienceCollectionCompletedMessage {
        task_id,
        parent_task_id: None,
        agent_id,
        governing_agent_id: agent_id,
    });

    app.update();

    let store = app.world().resource::<ExperienceStore>();
    let candidate = store
        .candidates
        .get(&candidate_id)
        .expect("candidate exists");
    assert_eq!(
        candidate.status,
        ExperienceCandidateStatus::WritebackPending,
        "knowledge candidate under injected skill path should be WritebackPending"
    );
}

/// 持久Agent + 未注入 skill → spawn ExperienceGovernanceRequestMessage。
///
/// 验证：候选进入 governance 路径（ExperienceGovernanceRequestMessage 被 spawn）。
/// 由于 experience_governance_system 在同帧消费该消息并推进候选状态，
/// 通过验证候选最终状态（NeedsUserApproval）和 SkillPackage destination 来确认 governance 路径。
#[test]
fn persistent_agent_without_skill_routes_to_governance() {
    let mut app = create_test_app(no_brain_test_config());

    let agent_id = Uuid::new_v4();
    let task_id = Uuid::new_v4();

    // 非默认持久Agent
    let agent_entity = app
        .world_mut()
        .spawn(make_persistent_agent(agent_id, "worker-agent", vec!["llm"]))
        .id();
    // 测试夹具绕过 spawn_agent 封装直接 spawn，需手动写入 EntityIndex
    app.world_mut()
        .resource_mut::<EntityIndex>()
        .agents
        .insert(agent_id, agent_entity);
    // Task 不注入 skill
    let task_entity = app
        .world_mut()
        .spawn(make_task(task_id, Some(agent_id), None))
        .id();
    // 测试夹具绕过 spawn_task 封装直接 spawn，需手动写入 EntityIndex
    app.world_mut()
        .resource_mut::<EntityIndex>()
        .tasks
        .insert(task_id, task_entity);

    let candidate = make_submitted_skill_candidate(task_id, agent_id);
    let candidate_id = candidate.candidate_id;
    app.world_mut()
        .resource_mut::<ExperienceStore>()
        .stage_root_candidate(candidate);

    app.world_mut().spawn(ExperienceCollectionCompletedMessage {
        task_id,
        parent_task_id: None,
        agent_id,
        governing_agent_id: agent_id,
    });

    app.update();

    // 验证：route_persistent_agent_experience 走 governance 分支
    //（spawn ExperienceGovernanceRequestMessage），
    // experience_governance_system 同帧消费并推进候选到 NeedsUserApproval（SkillPackage destination）。
    let store = app.world().resource::<ExperienceStore>();
    let candidate = store
        .candidates
        .get(&candidate_id)
        .expect("candidate exists");
    assert_eq!(
        candidate.status,
        ExperienceCandidateStatus::NeedsUserApproval,
        "candidate should be NeedsUserApproval after governance routes to SkillPackage"
    );

    // 验证 governance spawned ExperienceGovernanceDecision with SkillPackage destination
    let has_skill_package_decision = {
        let mut q = app
            .world_mut()
            .query::<&harness::ExperienceGovernanceDecision>();
        q.iter(app.world())
            .any(|d| d.destination == ExperienceWritebackDestination::SkillPackage)
    };
    assert!(
        has_skill_package_decision,
        "governance should spawn decision with SkillPackage destination"
    );

    // 验证 SkillUpdateRequestMessage 未被 spawn（未走 skill-updater 路径）
    let has_skill_update_request = {
        let mut q = app
            .world_mut()
            .query::<&harness::SkillUpdateRequestMessage>();
        q.iter(app.world()).count() > 0
    };
    assert!(
        !has_skill_update_request,
        "SkillUpdateRequestMessage should NOT be spawned without injected skill"
    );
}

/// 临时Agent + parent_task_id → 候选聚合到父任务 inbox。
///
/// 验证：非持久Agent 路径下，候选进入父任务 inbox 并被 aggregate。
#[test]
fn temporary_agent_routes_to_parent_inbox() {
    let mut app = create_test_app(no_brain_test_config());

    let parent_agent_id = Uuid::new_v4();
    let parent_task_id = Uuid::new_v4();
    let child_task_id = Uuid::new_v4();
    let child_agent_id = Uuid::new_v4();

    // Spawn 父 Agent（持久）和子 Agent（临时）
    app.world_mut().spawn(make_persistent_agent(
        parent_agent_id,
        "parent-agent",
        vec!["llm"],
    ));
    app.world_mut().spawn(make_temporary_agent(
        child_agent_id,
        "child-agent",
        parent_agent_id,
    ));

    // Spawn 子任务（delegate 是临时 Agent，parent_task_id 是父任务）
    app.world_mut().spawn(make_task(
        child_task_id,
        Some(child_agent_id),
        Some(parent_task_id),
    ));

    // 在父任务 inbox 中放入候选
    let candidate = make_submitted_knowledge_candidate(child_task_id, child_agent_id);
    let candidate_id = candidate.candidate_id;
    app.world_mut()
        .resource_mut::<ExperienceStore>()
        .queue_for_parent(parent_task_id, parent_agent_id, candidate);

    // 触发经验收集完成（带 parent_task_id）
    app.world_mut().spawn(ExperienceCollectionCompletedMessage {
        task_id: child_task_id,
        parent_task_id: Some(parent_task_id),
        agent_id: child_agent_id,
        governing_agent_id: parent_agent_id,
    });

    app.update();

    // 验证：候选已被 aggregate（status = Aggregated）
    let store = app.world().resource::<ExperienceStore>();
    let candidate = store
        .candidates
        .get(&candidate_id)
        .expect("candidate exists");
    assert_eq!(
        candidate.status,
        ExperienceCandidateStatus::Aggregated,
        "candidate should be Aggregated after temporary agent path"
    );

    // 验证 inbox 已被消费（status = Consumed）
    let inbox = store
        .inboxes
        .get(&parent_task_id)
        .expect("inbox should exist for parent task");
    assert_eq!(
        inbox.status,
        harness::ExperienceInboxStatus::Consumed,
        "inbox status should be Consumed after aggregation"
    );
}

// ============ 任务 26：skill 更新与循环防护 ============

/// 测试用 SKILL.md 模板（version=1，含 Usage 与 Examples 两个 section）。
const SAMPLE_SKILL_MD: &str = "---\nname: coding\ndescription: A coding skill\nversion: 1\nself_updatable: true\n---\n\n## Usage\n\nDo the thing.\n\n## Examples\n\nExample 1.\n";

/// v8 D19：包含三级标题（### Subsection）的样本 SKILL.md，
/// 用于 subsection 级 operation（replace_subsection / add_subsection / remove_subsection）测试。
const SAMPLE_SKILL_MD_WITH_SUBSECTIONS: &str = "---\nname: coding\ndescription: A coding skill with subsections\nversion: 1\nself_updatable: true\n---\n\n## Usage\n\nDo the thing.\n\n### Setup\n\nRun `make install`.\n\n### Execution\n\nRun `make run`.\n\n## Examples\n\nExample 1.\n";

/// 在临时目录下写入 SKILL.md，返回 (TempDir, SkillLoader)。
/// 保留 TempDir 句柄以避免目录被提前清理。
fn setup_skill_dir(skill_id: &SkillId, content: &str) -> (TempDir, SkillLoader) {
    let tmp = TempDir::new().unwrap();
    let loader = SkillLoader::new(tmp.path().to_path_buf());
    let skill_path = loader.skill_md_path(skill_id);
    std::fs::create_dir_all(skill_path.parent().unwrap()).unwrap();
    std::fs::write(&skill_path, content).unwrap();
    (tmp, loader)
}

/// 构造测试用 SkillUpdateContext + WorkItem（同 entity）并 spawn 到 world。
/// 返回 (work_item_id, work_item_entity)：work_item_id 用于日志/Uuid 匹配，
/// work_item_entity 用于把 SkillUpdateCompletedMessage / ToolExecutionRequestMessage.work_item_entity
/// 关联到同一 entity（Bug B 修复后的 insert 模式）。
fn spawn_work_item_with_context(
    world: &mut bevy_ecs::world::World,
    skill_id: SkillId,
    base_version: u32,
    candidate_id: Uuid,
) -> (Uuid, Entity) {
    let governing_agent_id = Uuid::new_v4();
    let task_id = Uuid::new_v4();
    let mut work_item = WorkItem::skill_update(
        task_id,
        "prompt".to_string(),
        Vec::<ConversationMessage>::new(),
        Vec::<ToolDefinition>::new(),
        governing_agent_id,
    );
    work_item.start();
    let work_item_id = work_item.id;
    let work_item_entity = world
        .spawn((
            work_item,
            SkillUpdateContext {
                skill_id: skill_id.clone(),
                base_version,
                experience_candidate_id: candidate_id,
                governing_agent_id,
            },
        ))
        .id();
    (work_item_id, work_item_entity)
}

/// 构造 SkillUpdateCompletedMessage。
fn make_completed_message(
    work_item_id: Uuid,
    skill_id: SkillId,
    base_version: u32,
    new_version: u32,
    operations: Vec<SkillUpdateOperation>,
) -> SkillUpdateCompletedMessage {
    SkillUpdateCompletedMessage {
        work_item_id,
        task_id: Uuid::new_v4(),
        agent_id: Uuid::new_v4(),
        skill_id,
        base_version,
        new_version,
        operations,
        rationale: "test rationale".to_string(),
    }
}

/// 在 ExperienceStore 中插入一个 GovernanceResolved 状态的候选，返回 candidate_id。
fn stage_resolved_candidate(store: &mut ExperienceStore, producer_task_id: Uuid) -> Uuid {
    let mut c = ExperienceCandidate::skill(
        Uuid::new_v4(),
        producer_task_id,
        Uuid::new_v4(),
        "title".to_string(),
        "skill-name".to_string(),
        "desc".to_string(),
        "instr".to_string(),
        Vec::new(),
    );
    c.status = ExperienceCandidateStatus::GovernanceResolved;
    let id = c.candidate_id;
    store.candidates.insert(id, c);
    id
}

/// skill 更新成功：SKILL.md 内容已更新、history/v1.md 备份存在、
/// SkillRegistry 中 version==2、候选状态为 Persisted。
#[test]
fn skill_update_increments_version_and_keeps_history() {
    let skill_id = SkillId::new("worker-agent", "coding");
    let (tmp, loader) = setup_skill_dir(&skill_id, SAMPLE_SKILL_MD);
    let skill_path = loader.skill_md_path(&skill_id);

    let mut app = create_test_app(no_brain_test_config());
    // 覆盖 SkillLoader 与 SkillRegistry，指向临时目录
    app.insert_resource(loader.clone());
    app.world_mut()
        .resource_mut::<SkillRegistry>()
        .upsert(make_skill_entry(skill_id.clone(), true));

    let producer_task_id = Uuid::new_v4();
    let candidate_id = stage_resolved_candidate(
        &mut app.world_mut().resource_mut::<ExperienceStore>(),
        producer_task_id,
    );

    let (work_item_id, work_item_entity) =
        spawn_work_item_with_context(app.world_mut(), skill_id.clone(), 1, candidate_id);

    // 用 ReplaceSection 操作替换 Usage section 内容
    let operations = vec![SkillUpdateOperation::ReplaceSection {
        section: "## Usage".to_string(),
        content: "New usage content.".to_string(),
    }];
    app.world_mut()
        .entity_mut(work_item_entity)
        .insert(make_completed_message(
            work_item_id,
            skill_id.clone(),
            1,
            2,
            operations,
        ));

    app.update();

    // 1. SKILL.md 已更新
    let new_content = std::fs::read_to_string(&skill_path).unwrap();
    assert!(
        new_content.contains("New usage content."),
        "SKILL.md should contain new content"
    );
    assert!(
        !new_content.contains("Do the thing."),
        "SKILL.md should not contain old content"
    );

    // 2. history v1.md 备份存在
    let history_dir = skill_path.parent().unwrap().join("history");
    let backup = std::fs::read_to_string(history_dir.join("v1.md")).unwrap();
    assert!(
        backup.contains("Do the thing."),
        "history/v1.md should contain original content"
    );

    // 3. SkillRegistry 已刷新（version=2）
    let registry = app.world().resource::<SkillRegistry>();
    let entry = registry
        .get(&skill_id)
        .expect("skill should be in registry");
    assert_eq!(entry.version, 2, "registry version should be 2");

    // 4. 候选状态为 Persisted
    let store = app.world().resource::<ExperienceStore>();
    let c = store
        .candidates
        .get(&candidate_id)
        .expect("candidate exists");
    assert_eq!(c.status, ExperienceCandidateStatus::Persisted);

    // 保留 TempDir 句柄避免目录被提前清理
    drop(tmp);
}

/// skill 更新 apply 失败：SKILL.md 内容不变，候选状态保持 GovernanceResolved。
#[test]
fn skill_update_apply_failure_preserves_state() {
    let skill_id = SkillId::new("worker-agent", "coding");
    let (tmp, loader) = setup_skill_dir(&skill_id, SAMPLE_SKILL_MD);
    let skill_path = loader.skill_md_path(&skill_id);

    let mut app = create_test_app(no_brain_test_config());
    app.insert_resource(loader.clone());
    app.world_mut()
        .resource_mut::<SkillRegistry>()
        .upsert(make_skill_entry(skill_id.clone(), true));

    let producer_task_id = Uuid::new_v4();
    let candidate_id = stage_resolved_candidate(
        &mut app.world_mut().resource_mut::<ExperienceStore>(),
        producer_task_id,
    );

    let (work_item_id, work_item_entity) =
        spawn_work_item_with_context(app.world_mut(), skill_id.clone(), 1, candidate_id);

    // 用不存在的 section 触发 ApplyError::SectionNotFound
    let operations = vec![SkillUpdateOperation::ReplaceSection {
        section: "## NonExistent".to_string(),
        content: "x".to_string(),
    }];
    app.world_mut()
        .entity_mut(work_item_entity)
        .insert(make_completed_message(
            work_item_id,
            skill_id.clone(),
            1,
            2,
            operations,
        ));

    app.update();

    // 候选状态保持 GovernanceResolved
    let store = app.world().resource::<ExperienceStore>();
    let c = store
        .candidates
        .get(&candidate_id)
        .expect("candidate exists");
    assert_eq!(
        c.status,
        ExperienceCandidateStatus::GovernanceResolved,
        "candidate status should be unchanged on apply failure"
    );

    // SKILL.md 未被修改（仍含原内容）
    let content = std::fs::read_to_string(&skill_path).unwrap();
    assert!(
        content.contains("Do the thing."),
        "SKILL.md should be unchanged on apply failure"
    );

    // SkillRegistry 版本未刷新
    let registry = app.world().resource::<SkillRegistry>();
    let entry = registry
        .get(&skill_id)
        .expect("skill should be in registry");
    assert_eq!(entry.version, 1, "registry version should remain 1");

    drop(tmp);
}

/// self_updatable=false：候选被标记 Discarded，未走 SkillUpdate 路径，也不产生 writeback 请求。
///
/// ADR-004 v6 D15：原设计"降级 kind_hint 为 Knowledge 并写入 LTM"会导致 payload 形态不匹配
/// （Skill payload 与 Knowledge payload 不同，writeback 失败）。修订为直接 Discarded + warn 日志，
/// 让 LLM 在下一轮重新评估。需要变更不可自更新 skill 的，应通过 IncubationProposal 提案新 skill。
#[test]
fn self_updatable_false_discards_candidate() {
    let mut app = create_test_app(no_brain_test_config());

    let agent_id = Uuid::new_v4();
    let task_id = Uuid::new_v4();
    let skill_id = SkillId::new("worker-agent", "locked-skill");

    // 非默认持久Agent
    let agent_name = "worker-agent".to_string();
    let agent_entity = app
        .world_mut()
        .spawn((
            make_persistent_agent(agent_id, &agent_name, vec!["llm"]),
            LongTermMemory::with_name(&agent_name),
        ))
        .id();
    // 测试夹具绕过 spawn_agent 封装直接 spawn，需手动写入 EntityIndex
    app.world_mut()
        .resource_mut::<EntityIndex>()
        .agents
        .insert(agent_id, agent_entity);

    // Task 注入了 self_updatable=false 的 skill
    let task_entity = app
        .world_mut()
        .spawn((
            make_task(task_id, Some(agent_id), None),
            TaskInjectedSkill {
                skill_id: Some(skill_id.clone()),
            },
        ))
        .id();
    // 测试夹具绕过 spawn_task 封装直接 spawn，需手动写入 EntityIndex
    app.world_mut()
        .resource_mut::<EntityIndex>()
        .tasks
        .insert(task_id, task_entity);

    // SkillRegistry 中 skill self_updatable=false
    app.world_mut()
        .resource_mut::<SkillRegistry>()
        .upsert(make_skill_entry(skill_id.clone(), false));

    // GovernancePending 状态的 Skill 类候选
    let candidate = make_governance_pending_skill_candidate(task_id, agent_id);
    let candidate_id = candidate.candidate_id;
    app.world_mut()
        .resource_mut::<ExperienceStore>()
        .candidates
        .insert(candidate_id, candidate);

    // 触发治理
    app.world_mut()
        .spawn(ExperienceGovernanceRequestMessage { task_id, agent_id });

    app.update();

    // 验证：没有 SkillUpdateRequestMessage 被 spawn（未走 skill-updater 路径）
    let has_skill_update_request = {
        let mut q = app
            .world_mut()
            .query::<&harness::SkillUpdateRequestMessage>();
        q.iter(app.world()).count() > 0
    };
    assert!(
        !has_skill_update_request,
        "SkillUpdateRequestMessage should NOT be spawned when self_updatable=false"
    );

    // 验证：候选 kind_hint 保持 Skill（不降级 payload，避免语义不一致）
    let store = app.world().resource::<ExperienceStore>();
    let candidate = store
        .candidates
        .get(&candidate_id)
        .expect("candidate exists");
    assert_eq!(
        candidate.kind_hint,
        ExperienceKindHint::Skill,
        "kind_hint should remain Skill; do not downgrade payload"
    );

    // 验证：候选被标记 Discarded（ADR-004 v6 D15：不强行降级，直接 Discarded + warn）
    assert_eq!(
        candidate.status,
        ExperienceCandidateStatus::Discarded,
        "candidate should be Discarded when self_updatable=false (ADR-004 v6 D15)"
    );
}

/// kind_filter=KnowledgeOnly：Skill 候选被 Discarded，Knowledge 候选保留并 WritebackPending。
#[test]
fn experience_kind_filter_knowledge_only_discards_skill() {
    let mut app = create_test_app(no_brain_test_config());

    let agent_id = Uuid::new_v4();
    let task_id = Uuid::new_v4();
    let skill_id = SkillId::new("worker-agent", "coding");

    let agent_entity = app
        .world_mut()
        .spawn(make_persistent_agent(agent_id, "worker-agent", vec!["llm"]))
        .id();
    // 测试夹具绕过 spawn_agent 封装直接 spawn，需手动写入 EntityIndex
    app.world_mut()
        .resource_mut::<EntityIndex>()
        .agents
        .insert(agent_id, agent_entity);
    let task_entity = app
        .world_mut()
        .spawn((
            make_task(task_id, Some(agent_id), None),
            TaskInjectedSkill {
                skill_id: Some(skill_id.clone()),
            },
            TaskExperiencePolicy {
                kind_filter: ExperienceKindFilter::KnowledgeOnly,
            },
        ))
        .id();
    // 测试夹具绕过 spawn_task 封装直接 spawn，需手动写入 EntityIndex
    app.world_mut()
        .resource_mut::<EntityIndex>()
        .tasks
        .insert(task_id, task_entity);

    app.world_mut()
        .resource_mut::<SkillRegistry>()
        .upsert(make_skill_entry(skill_id, true));

    // Skill 类候选 + Knowledge 类候选
    let skill_candidate = make_submitted_skill_candidate(task_id, agent_id);
    let skill_candidate_id = skill_candidate.candidate_id;
    let knowledge_candidate = make_submitted_knowledge_candidate(task_id, agent_id);
    let knowledge_candidate_id = knowledge_candidate.candidate_id;
    {
        let mut store = app.world_mut().resource_mut::<ExperienceStore>();
        store.stage_root_candidate(skill_candidate);
        store.stage_root_candidate(knowledge_candidate);
    }

    app.world_mut().spawn(ExperienceCollectionCompletedMessage {
        task_id,
        parent_task_id: None,
        agent_id,
        governing_agent_id: agent_id,
    });

    app.update();

    // Skill 候选被 Discarded
    let store = app.world().resource::<ExperienceStore>();
    let skill_c = store
        .candidates
        .get(&skill_candidate_id)
        .expect("skill candidate exists");
    assert_eq!(
        skill_c.status,
        ExperienceCandidateStatus::Discarded,
        "skill candidate should be Discarded under KnowledgeOnly filter"
    );

    // Knowledge 候选为 WritebackPending（route_persistent_agent_experience 走 LTM 占位分支）
    let knowledge_c = store
        .candidates
        .get(&knowledge_candidate_id)
        .expect("knowledge candidate exists");
    assert_eq!(
        knowledge_c.status,
        ExperienceCandidateStatus::WritebackPending,
        "knowledge candidate should be WritebackPending under injected skill path"
    );
}

// ============ Bug C：SubmitSkillUpdate dry-run 校验 ============

/// Bug C 修复：orchestrator 在 SubmitSkillUpdate 分支提前做 dry-run 校验。
///
/// 当 LLM 提交的 operations 引用不存在的 section 时，dry-run 应立即拒绝：
/// - 不 spawn SkillUpdateCompletedMessage
/// - ToolCallingState 的 pending_tool_call_ids 被清空（说明 error result 已被
///   tool_calling_orchestrator_system 消费，LLM 通过 conversation 收到了 error 反馈）
/// - SKILL.md 文件不变
/// - SkillRegistry version 仍是 1（completion_system 未被触发）
/// - 候选状态保持 GovernanceResolved（未推进到 Persisted）
#[test]
fn submit_skill_update_dry_run_rejects_nonexistent_section() {
    let skill_id = SkillId::new("worker-agent", "coding");
    // SKILL.md 仅含 `## Usage` section，operations 引用 `## NonExistent` 必然失败
    let (tmp, loader) = setup_skill_dir(&skill_id, SAMPLE_SKILL_MD);
    let skill_path = loader.skill_md_path(&skill_id);

    let mut app = create_test_app(no_brain_test_config());
    // 覆盖 SkillLoader 指向临时目录
    app.insert_resource(loader.clone());
    // 预置 SkillRegistry（让 skill_update_workitem_system / completion_system 都能查到）
    app.world_mut()
        .resource_mut::<SkillRegistry>()
        .upsert(make_skill_entry(skill_id.clone(), true));

    // 预置 GovernanceResolved 候选，让 completion_system 在 dry-run 通过路径下能推进到 Persisted。
    // dry-run 失败时候选状态应保持 GovernanceResolved。
    let producer_task_id = Uuid::new_v4();
    let candidate_id = stage_resolved_candidate(
        &mut app.world_mut().resource_mut::<ExperienceStore>(),
        producer_task_id,
    );

    // Spawn skill-updater Agent（持有 submit_skill_update 工具所需 tag）
    let agent_id = Uuid::new_v4();
    let agent_entity = app
        .world_mut()
        .spawn(Agent {
            id: agent_id,
            profile: AgentProfile {
                name: "skill-updater".to_string(),
                model: "test-model".to_string(),
            },
            capabilities: AgentCapabilities {
                tags: vec!["skill-updater".to_string()],
                description: "skill-updater agent".to_string(),
            },
            kind: AgentKind::Persistent,
            parent_id: None,
            bound_task_id: None,
            tool_permissions: AgentToolPermissions {
                default_permission: ToolPermission::Allow,
                default_permission_explicit: true,
                overrides: std::collections::HashMap::new(),
            },
            system_prompt: None,
        })
        .id();
    app.world_mut()
        .resource_mut::<EntityIndex>()
        .agents
        .insert(agent_id, agent_entity);

    // Spawn Task + ShortTermMemory（dispatch_system 会查询 task）
    let task_entity = app
        .world_mut()
        .spawn((
            Task::from_user_input_ready("skill update task", 3, default_channel()),
            ShortTermMemory::default(),
        ))
        .id();
    let task_id = app.world().get::<Task>(task_entity).unwrap().id;
    app.world_mut()
        .resource_mut::<EntityIndex>()
        .tasks
        .insert(task_id, task_entity);

    // Spawn WorkItem + SkillUpdateContext（让 orchestrator 能反查 context 注入 skill_id）
    let (work_item_id, work_item_entity) =
        spawn_work_item_with_context(app.world_mut(), skill_id.clone(), 1, candidate_id);

    // Spawn ToolCallingState（restore_task_after_tool / tool_calling_orchestrator_system 会查询）
    let calling_state_entity = app
        .world_mut()
        .spawn(ToolCallingState {
            task_id,
            agent_id,
            pending_tool_call_ids: vec!["call_test".to_string()],
            iteration: 1,
            max_iterations: 5,
            conversation: Vec::new(),
            tools: Vec::new(),
            request_kind: AgentRequestKind::ToolExecution {
                tool_name: "submit_skill_update".to_string(),
            },
            work_item_id: Some(work_item_id),
        })
        .id();

    // Spawn ToolExecutionRequestMessage：operations 引用不存在的 section
    app.world_mut().spawn(ToolExecutionRequestMessage {
        request: AgentExecutionRequest {
            task_id,
            agent_id,
            request_kind: AgentRequestKind::ToolExecution {
                tool_name: "submit_skill_update".to_string(),
            },
            prompt: String::new(),
            system_prompt: None,
            tools: Vec::new(),
            conversation: None,
            work_item_id: Some(work_item_id),
            model_override: None,
        },
        tool_name: "submit_skill_update".to_string(),
        tool_input: serde_json::json!({
            "operations": [
                {"action": "replace_section", "section": "## NonExistent", "content": "x"}
            ],
            "rationale": "test dry-run rejection"
        }),
        pending_confirmation_id: None,
        tool_call_id: Some("call_test".to_string()),
        pending_confirmation_options: None,
        work_item_entity: Some(work_item_entity),
        confirmed_once: false,
    });

    // 运行多帧让 dispatch → handle_tool_action → dry-run → tool_calling_orchestrator 完成
    for _ in 0..5 {
        app.update();
    }

    // 1. 不应 spawn SkillUpdateCompletedMessage（dry-run 拦截）
    let completed_count = {
        let mut q = app.world_mut().query::<&SkillUpdateCompletedMessage>();
        q.iter(app.world()).count()
    };
    assert_eq!(
        completed_count, 0,
        "dry-run should reject invalid operations; no SkillUpdateCompletedMessage spawned"
    );

    // 2. ToolCallingState 的 pending_tool_call_ids 应被清空。
    // 说明 tool_calling_orchestrator_system 收集到了 dry-run 失败的 error result，
    // 把它作为 Tool message 加到 conversation 中（LLM 即将收到错误反馈）。
    let calling_state = app
        .world()
        .get::<ToolCallingState>(calling_state_entity)
        .expect("ToolCallingState should still exist");
    assert!(
        calling_state.pending_tool_call_ids.is_empty(),
        "dry-run failure result should be consumed by tool_calling_orchestrator_system, clearing pending_tool_call_ids"
    );
    // conversation 中应包含一条 Tool 消息，其内容含 dry-run 失败说明
    let has_dry_run_error_msg = calling_state.conversation.iter().any(|m| match m {
        ConversationMessage::Tool { content, .. } => content.contains("dry-run failed"),
        _ => false,
    });
    assert!(
        has_dry_run_error_msg,
        "ToolCallingState conversation should contain dry-run failure message"
    );

    // 3. SKILL.md 内容未被修改（仍含原 Usage 内容）
    let content = std::fs::read_to_string(&skill_path).unwrap();
    assert!(
        content.contains("Do the thing."),
        "SKILL.md should be unchanged after dry-run rejection"
    );

    // 4. SkillRegistry version 仍是 1（completion_system 未被触发）
    let registry = app.world().resource::<SkillRegistry>();
    let entry = registry
        .get(&skill_id)
        .expect("skill should be in registry");
    assert_eq!(
        entry.version, 1,
        "registry version should remain 1 after dry-run rejection"
    );

    // 5. 候选状态保持 GovernanceResolved（未推进到 Persisted）
    let store = app.world().resource::<ExperienceStore>();
    let c = store
        .candidates
        .get(&candidate_id)
        .expect("candidate exists");
    assert_eq!(
        c.status,
        ExperienceCandidateStatus::GovernanceResolved,
        "candidate status should remain GovernanceResolved after dry-run rejection"
    );

    drop(tmp);
}

/// Bug C 修复：dry-run 通过时正常 spawn SkillUpdateCompletedMessage。
///
/// 验证 dry-run 不会误拒合法 operations：
/// - operations 引用存在的 section（## Usage）
/// - spawn SkillUpdateCompletedMessage
/// - completion_system 同帧消费并 apply 到 SKILL.md
#[test]
fn submit_skill_update_dry_run_accepts_valid_operations() {
    let skill_id = SkillId::new("worker-agent", "coding");
    let (tmp, loader) = setup_skill_dir(&skill_id, SAMPLE_SKILL_MD);
    let skill_path = loader.skill_md_path(&skill_id);

    let mut app = create_test_app(no_brain_test_config());
    app.insert_resource(loader.clone());
    app.world_mut()
        .resource_mut::<SkillRegistry>()
        .upsert(make_skill_entry(skill_id.clone(), true));

    // 预置 GovernanceResolved 候选，让 completion_system 能推进到 Persisted
    let producer_task_id = Uuid::new_v4();
    let candidate_id = stage_resolved_candidate(
        &mut app.world_mut().resource_mut::<ExperienceStore>(),
        producer_task_id,
    );

    let agent_id = Uuid::new_v4();
    let agent_entity = app
        .world_mut()
        .spawn(Agent {
            id: agent_id,
            profile: AgentProfile {
                name: "skill-updater".to_string(),
                model: "test-model".to_string(),
            },
            capabilities: AgentCapabilities {
                tags: vec!["skill-updater".to_string()],
                description: "skill-updater agent".to_string(),
            },
            kind: AgentKind::Persistent,
            parent_id: None,
            bound_task_id: None,
            tool_permissions: AgentToolPermissions {
                default_permission: ToolPermission::Allow,
                default_permission_explicit: true,
                overrides: std::collections::HashMap::new(),
            },
            system_prompt: None,
        })
        .id();
    app.world_mut()
        .resource_mut::<EntityIndex>()
        .agents
        .insert(agent_id, agent_entity);

    let task_entity = app
        .world_mut()
        .spawn((
            Task::from_user_input_ready("skill update task", 3, default_channel()),
            ShortTermMemory::default(),
        ))
        .id();
    let task_id = app.world().get::<Task>(task_entity).unwrap().id;
    app.world_mut()
        .resource_mut::<EntityIndex>()
        .tasks
        .insert(task_id, task_entity);

    let (work_item_id, work_item_entity) =
        spawn_work_item_with_context(app.world_mut(), skill_id.clone(), 1, candidate_id);

    app.world_mut().spawn(ToolCallingState {
        task_id,
        agent_id,
        pending_tool_call_ids: vec!["call_test".to_string()],
        iteration: 1,
        max_iterations: 5,
        conversation: Vec::new(),
        tools: Vec::new(),
        request_kind: AgentRequestKind::ToolExecution {
            tool_name: "submit_skill_update".to_string(),
        },
        work_item_id: Some(work_item_id),
    });

    // 合法 operations：引用实际存在的 ## Usage section
    app.world_mut().spawn(ToolExecutionRequestMessage {
        request: AgentExecutionRequest {
            task_id,
            agent_id,
            request_kind: AgentRequestKind::ToolExecution {
                tool_name: "submit_skill_update".to_string(),
            },
            prompt: String::new(),
            system_prompt: None,
            tools: Vec::new(),
            conversation: None,
            work_item_id: Some(work_item_id),
            model_override: None,
        },
        tool_name: "submit_skill_update".to_string(),
        tool_input: serde_json::json!({
            "operations": [
                {"action": "replace_section", "section": "## Usage", "content": "New usage content."}
            ],
            "rationale": "valid update"
        }),
        pending_confirmation_id: None,
        tool_call_id: Some("call_test".to_string()),
        pending_confirmation_options: None,
        work_item_entity: Some(work_item_entity),
        confirmed_once: false,
    });

    // 运行多帧：dispatch → dry-run 通过 → spawn SkillUpdateCompletedMessage → completion_system apply
    for _ in 0..5 {
        app.update();
    }

    // 1. SKILL.md 已被 apply 更新
    let content = std::fs::read_to_string(&skill_path).unwrap();
    assert!(
        content.contains("New usage content."),
        "dry-run should accept valid operations; SKILL.md should be updated"
    );
    assert!(
        !content.contains("Do the thing."),
        "old Usage content should be replaced"
    );

    // 2. SkillRegistry version 已刷新到 2
    let registry = app.world().resource::<SkillRegistry>();
    let entry = registry
        .get(&skill_id)
        .expect("skill should be in registry");
    assert_eq!(
        entry.version, 2,
        "registry version should be 2 after successful dry-run + apply"
    );

    // 3. 候选状态为 Persisted（completion_system 推进）
    let store = app.world().resource::<ExperienceStore>();
    let c = store
        .candidates
        .get(&candidate_id)
        .expect("candidate exists");
    assert_eq!(
        c.status,
        ExperienceCandidateStatus::Persisted,
        "candidate should be Persisted after successful dry-run + apply"
    );

    drop(tmp);
}

/// [次要-1] 回归测试：当 `ToolExecutionRequestMessage.work_item_entity` 为 None
/// 但 `request.work_item_id` 为 Some 时（不可达路径），orchestrator 应直接拒绝。
///
/// 验证 [重要-1] 修复后的行为：
/// - 不 spawn `SkillUpdateCompletedMessage`（既不 insert 到 WorkItem，也不 spawn 独立 entity）
/// - 通过 `spawn_tool_error` 返回错误给 LLM（`ToolCallingState.pending_tool_call_ids` 清空）
/// - SKILL.md 内容不变
/// - SkillRegistry version 仍是 1
/// - 候选状态保持 GovernanceResolved（未推进到 Persisted）
///
/// 该路径在实践中不可达：`llm_response.rs` 中 `work_item_entity` 是基于同一 WorkItem
/// query 设置的，反查 work_item_id 成功意味着 work_item_entity 也应为 Some。本测试
/// 通过手动构造异常请求验证 orchestrator 的防御性拒绝行为。
#[test]
fn submit_skill_update_rejects_when_work_item_entity_is_none() {
    let skill_id = SkillId::new("worker-agent", "coding");
    let (tmp, loader) = setup_skill_dir(&skill_id, SAMPLE_SKILL_MD);
    let skill_path = loader.skill_md_path(&skill_id);

    let mut app = create_test_app(no_brain_test_config());
    // 覆盖 SkillLoader 指向临时目录
    app.insert_resource(loader.clone());
    app.world_mut()
        .resource_mut::<SkillRegistry>()
        .upsert(make_skill_entry(skill_id.clone(), true));

    // 预置 GovernanceResolved 候选
    let producer_task_id = Uuid::new_v4();
    let candidate_id = stage_resolved_candidate(
        &mut app.world_mut().resource_mut::<ExperienceStore>(),
        producer_task_id,
    );

    // Spawn skill-updater Agent
    let agent_id = Uuid::new_v4();
    app.world_mut().spawn(Agent {
        id: agent_id,
        profile: AgentProfile {
            name: "skill-updater".to_string(),
            model: "test-model".to_string(),
        },
        capabilities: AgentCapabilities {
            tags: vec!["skill-updater".to_string()],
            description: "skill-updater agent".to_string(),
        },
        kind: AgentKind::Persistent,
        parent_id: None,
        bound_task_id: None,
        tool_permissions: AgentToolPermissions {
            default_permission: ToolPermission::Allow,
            default_permission_explicit: true,
            overrides: std::collections::HashMap::new(),
        },
        system_prompt: None,
    });

    // Spawn Task + ShortTermMemory
    let task_entity = app
        .world_mut()
        .spawn((
            Task::from_user_input_ready("skill update task", 3, default_channel()),
            ShortTermMemory::default(),
        ))
        .id();
    let task_id = app.world().get::<Task>(task_entity).unwrap().id;

    // Spawn WorkItem + SkillUpdateContext（让 work_item_id 存在但请求中不引用 entity）
    let (work_item_id, _work_item_entity) =
        spawn_work_item_with_context(app.world_mut(), skill_id.clone(), 1, candidate_id);

    // Spawn ToolCallingState
    let calling_state_entity = app
        .world_mut()
        .spawn(ToolCallingState {
            task_id,
            agent_id,
            pending_tool_call_ids: vec!["call_test".to_string()],
            iteration: 1,
            max_iterations: 5,
            conversation: Vec::new(),
            tools: Vec::new(),
            request_kind: AgentRequestKind::ToolExecution {
                tool_name: "submit_skill_update".to_string(),
            },
            work_item_id: Some(work_item_id),
        })
        .id();

    // 构造异常 ToolExecutionRequestMessage：work_item_id 为 Some（通过第一道检查），
    // 但 work_item_entity 为 None（触发 [重要-1] 修复后的 error! + spawn_tool_error 路径）。
    app.world_mut().spawn(ToolExecutionRequestMessage {
        request: AgentExecutionRequest {
            task_id,
            agent_id,
            request_kind: AgentRequestKind::ToolExecution {
                tool_name: "submit_skill_update".to_string(),
            },
            prompt: String::new(),
            system_prompt: None,
            tools: Vec::new(),
            conversation: None,
            work_item_id: Some(work_item_id),
            model_override: None,
        },
        tool_name: "submit_skill_update".to_string(),
        tool_input: serde_json::json!({
            "operations": [
                {"action": "replace_section", "section": "## Usage", "content": "New usage content."}
            ],
            "rationale": "should be rejected due to missing work_item_entity"
        }),
        pending_confirmation_id: None,
        tool_call_id: Some("call_test".to_string()),
        pending_confirmation_options: None,
        work_item_entity: None,
        confirmed_once: false,
    });

    // 运行多帧让 dispatch → handle_tool_action → error! + spawn_tool_error → tool_calling_orchestrator 完成
    for _ in 0..5 {
        app.update();
    }

    // 1. 不应 spawn 任何 SkillUpdateCompletedMessage（不 insert 到 WorkItem，也不 spawn 独立 entity）
    let completed_count = {
        let mut q = app.world_mut().query::<&SkillUpdateCompletedMessage>();
        q.iter(app.world()).count()
    };
    assert_eq!(
        completed_count, 0,
        "should not spawn SkillUpdateCompletedMessage when work_item_entity is None"
    );

    // 2. ToolCallingState 的 pending_tool_call_ids 应被清空（spawn_tool_error 已被消费）
    let calling_state = app
        .world()
        .get::<ToolCallingState>(calling_state_entity)
        .expect("ToolCallingState should still exist");
    assert!(
        calling_state.pending_tool_call_ids.is_empty(),
        "spawn_tool_error result should be consumed by tool_calling_orchestrator_system, \
         clearing pending_tool_call_ids"
    );

    // 3. SKILL.md 内容未被修改（仍含原 Usage 内容）
    let content = std::fs::read_to_string(&skill_path).unwrap();
    assert!(
        content.contains("Do the thing."),
        "SKILL.md should be unchanged when work_item_entity is None"
    );

    // 4. SkillRegistry version 仍是 1（completion_system 未被触发）
    let registry = app.world().resource::<SkillRegistry>();
    let entry = registry
        .get(&skill_id)
        .expect("skill should be in registry");
    assert_eq!(
        entry.version, 1,
        "registry version should remain 1 when work_item_entity is None"
    );

    // 5. 候选状态保持 GovernanceResolved（未推进到 Persisted）
    let store = app.world().resource::<ExperienceStore>();
    let c = store
        .candidates
        .get(&candidate_id)
        .expect("candidate exists");
    assert_eq!(
        c.status,
        ExperienceCandidateStatus::GovernanceResolved,
        "candidate status should remain GovernanceResolved when work_item_entity is None"
    );

    drop(tmp);
}

// ============ v8 D19：subsection 级 operation + replace_body + 结构校验 ============

/// v8 D19：subsection 级 operation（replace_subsection / add_subsection / remove_subsection）
/// 直接通过 `SkillUpdateCompletedMessage` 注入到 completion_system 验证 apply 行为。
///
/// 验证点：
/// - replace_subsection 替换 `### Setup` 内容
/// - add_subsection 在 `### Setup` 之后插入 `### Testing`
/// - remove_subsection 删除 `### Execution`
/// - SKILL.md 三级标题结构按预期变化
/// - SkillRegistry version 刷新到 2
/// - 候选状态推进到 Persisted
#[test]
fn skill_update_subsection_operations_apply_correctly() {
    let skill_id = SkillId::new("worker-agent", "coding");
    let (tmp, loader) = setup_skill_dir(&skill_id, SAMPLE_SKILL_MD_WITH_SUBSECTIONS);
    let skill_path = loader.skill_md_path(&skill_id);

    let mut app = create_test_app(no_brain_test_config());
    app.insert_resource(loader.clone());
    app.world_mut()
        .resource_mut::<SkillRegistry>()
        .upsert(make_skill_entry(skill_id.clone(), true));

    let producer_task_id = Uuid::new_v4();
    let candidate_id = stage_resolved_candidate(
        &mut app.world_mut().resource_mut::<ExperienceStore>(),
        producer_task_id,
    );

    let (work_item_id, work_item_entity) =
        spawn_work_item_with_context(app.world_mut(), skill_id.clone(), 1, candidate_id);

    let operations = vec![
        SkillUpdateOperation::ReplaceSubsection {
            section: "## Usage".to_string(),
            subsection: "### Setup".to_string(),
            content: "Run `make setup`.".to_string(),
        },
        SkillUpdateOperation::AddSubsection {
            section: "## Usage".to_string(),
            after: "### Setup".to_string(),
            subsection: "### Testing".to_string(),
            content: "Run `make test`.".to_string(),
        },
        SkillUpdateOperation::RemoveSubsection {
            section: "## Usage".to_string(),
            subsection: "### Execution".to_string(),
        },
    ];
    app.world_mut()
        .entity_mut(work_item_entity)
        .insert(make_completed_message(
            work_item_id,
            skill_id.clone(),
            1,
            2,
            operations,
        ));

    app.update();

    let new_content = std::fs::read_to_string(&skill_path).unwrap();

    // 1. Setup 内容已被替换
    assert!(
        new_content.contains("Run `make setup`."),
        "Setup subsection content should be replaced; got:\n{}",
        new_content
    );
    assert!(
        !new_content.contains("Run `make install`."),
        "old Setup content should be gone; got:\n{}",
        new_content
    );

    // 2. Testing subsection 已新增（在 Setup 之后）
    assert!(
        new_content.contains("### Testing"),
        "Testing subsection should be added; got:\n{}",
        new_content
    );
    assert!(
        new_content.contains("Run `make test`."),
        "Testing content should be present; got:\n{}",
        new_content
    );

    // 3. Execution subsection 已删除
    assert!(
        !new_content.contains("### Execution"),
        "Execution subsection should be removed; got:\n{}",
        new_content
    );
    assert!(
        !new_content.contains("Run `make run`."),
        "Execution content should be gone; got:\n{}",
        new_content
    );

    // 4. 顶层 section（## Usage / ## Examples）仍存在
    assert!(
        new_content.contains("## Usage") && new_content.contains("## Examples"),
        "top-level sections should remain; got:\n{}",
        new_content
    );

    // 5. SkillRegistry version 已刷新
    let registry = app.world().resource::<SkillRegistry>();
    let entry = registry
        .get(&skill_id)
        .expect("skill should be in registry");
    assert_eq!(
        entry.version, 2,
        "registry version should be 2 after subsection operations"
    );

    // 6. 候选状态为 Persisted
    let store = app.world().resource::<ExperienceStore>();
    let c = store
        .candidates
        .get(&candidate_id)
        .expect("candidate exists");
    assert_eq!(
        c.status,
        ExperienceCandidateStatus::Persisted,
        "candidate should be Persisted after successful subsection operations"
    );

    drop(tmp);
}

/// v8 D19：replace_body 兜底 operation 整体替换 body，frontmatter 保持不变。
///
/// 验证点：
/// - body 全部被替换为新内容
/// - frontmatter 仍含原 name / description / version / self_updatable
/// - SkillRegistry version 刷新到 2
/// - 候选状态推进到 Persisted
#[test]
fn skill_update_replace_body_keeps_frontmatter() {
    let skill_id = SkillId::new("worker-agent", "coding");
    let (tmp, loader) = setup_skill_dir(&skill_id, SAMPLE_SKILL_MD_WITH_SUBSECTIONS);
    let skill_path = loader.skill_md_path(&skill_id);

    let mut app = create_test_app(no_brain_test_config());
    app.insert_resource(loader.clone());
    app.world_mut()
        .resource_mut::<SkillRegistry>()
        .upsert(make_skill_entry(skill_id.clone(), true));

    let producer_task_id = Uuid::new_v4();
    let candidate_id = stage_resolved_candidate(
        &mut app.world_mut().resource_mut::<ExperienceStore>(),
        producer_task_id,
    );

    let (work_item_id, work_item_entity) =
        spawn_work_item_with_context(app.world_mut(), skill_id.clone(), 1, candidate_id);

    let new_body = "## Usage\n\nBrand new body content.\n";
    let operations = vec![SkillUpdateOperation::ReplaceBody {
        content: new_body.to_string(),
    }];
    app.world_mut()
        .entity_mut(work_item_entity)
        .insert(make_completed_message(
            work_item_id,
            skill_id.clone(),
            1,
            2,
            operations,
        ));

    app.update();

    let new_content = std::fs::read_to_string(&skill_path).unwrap();

    // 1. body 已被整体替换
    assert!(
        new_content.contains("Brand new body content."),
        "body should be replaced; got:\n{}",
        new_content
    );
    assert!(
        !new_content.contains("Do the thing."),
        "old body content should be gone; got:\n{}",
        new_content
    );
    assert!(
        !new_content.contains("### Setup"),
        "old subsections should be gone; got:\n{}",
        new_content
    );

    // 2. frontmatter 仍含原字段
    assert!(
        new_content.contains("name: coding") && new_content.contains("self_updatable: true"),
        "frontmatter should be preserved; got:\n{}",
        new_content
    );

    // 3. SkillRegistry version 已刷新
    let registry = app.world().resource::<SkillRegistry>();
    let entry = registry
        .get(&skill_id)
        .expect("skill should be in registry");
    assert_eq!(
        entry.version, 2,
        "registry version should be 2 after replace_body"
    );

    // 4. 候选状态为 Persisted
    let store = app.world().resource::<ExperienceStore>();
    let c = store
        .candidates
        .get(&candidate_id)
        .expect("candidate exists");
    assert_eq!(
        c.status,
        ExperienceCandidateStatus::Persisted,
        "candidate should be Persisted after replace_body"
    );

    drop(tmp);
}

/// v8 D19：post-apply `validate_skill_structure` 拦截结构破坏的 operations。
///
/// 场景：通过 `replace_body` 把 body 替换为没有任何 `##` 标题的纯文本，
/// 触发 `ApplyError::StructureInvalid`，整体回滚：
/// - SKILL.md 内容不变
/// - SkillRegistry version 仍是 1
/// - 候选状态保持 GovernanceResolved
#[test]
fn skill_update_invalid_structure_after_replace_body_rolls_back() {
    let skill_id = SkillId::new("worker-agent", "coding");
    let (tmp, loader) = setup_skill_dir(&skill_id, SAMPLE_SKILL_MD_WITH_SUBSECTIONS);
    let skill_path = loader.skill_md_path(&skill_id);

    let mut app = create_test_app(no_brain_test_config());
    app.insert_resource(loader.clone());
    app.world_mut()
        .resource_mut::<SkillRegistry>()
        .upsert(make_skill_entry(skill_id.clone(), true));

    let producer_task_id = Uuid::new_v4();
    let candidate_id = stage_resolved_candidate(
        &mut app.world_mut().resource_mut::<ExperienceStore>(),
        producer_task_id,
    );

    let (work_item_id, work_item_entity) =
        spawn_work_item_with_context(app.world_mut(), skill_id.clone(), 1, candidate_id);

    // 用 replace_body 写入没有任何 `##` 标题的纯文本，触发 StructureInvalid
    let operations = vec![SkillUpdateOperation::ReplaceBody {
        content: "no section heading here".to_string(),
    }];
    app.world_mut()
        .entity_mut(work_item_entity)
        .insert(make_completed_message(
            work_item_id,
            skill_id.clone(),
            1,
            2,
            operations,
        ));

    app.update();

    // 1. SKILL.md 未被修改（仍含原 Setup 内容）
    let content = std::fs::read_to_string(&skill_path).unwrap();
    assert!(
        content.contains("Run `make install`."),
        "SKILL.md should be unchanged after StructureInvalid rollback; got:\n{}",
        content
    );

    // 2. SkillRegistry version 仍是 1
    let registry = app.world().resource::<SkillRegistry>();
    let entry = registry
        .get(&skill_id)
        .expect("skill should be in registry");
    assert_eq!(
        entry.version, 1,
        "registry version should remain 1 after StructureInvalid rollback"
    );

    // 3. 候选状态保持 GovernanceResolved
    let store = app.world().resource::<ExperienceStore>();
    let c = store
        .candidates
        .get(&candidate_id)
        .expect("candidate exists");
    assert_eq!(
        c.status,
        ExperienceCandidateStatus::GovernanceResolved,
        "candidate status should remain GovernanceResolved after StructureInvalid rollback"
    );

    drop(tmp);
}

/// v8 D19：subsection 级 operation 的 dry-run 拦截。
///
/// 通过 `submit_skill_update` 工具调用路径，验证引用不存在的 subsection 时
/// orchestrator 的 dry-run 应立即拒绝：
/// - 不 spawn `SkillUpdateCompletedMessage`
/// - `ToolCallingState.pending_tool_call_ids` 被清空（错误 result 已被消费）
/// - SKILL.md 内容不变
/// - SkillRegistry version 仍是 1
/// - 候选状态保持 GovernanceResolved
#[test]
fn submit_skill_update_dry_run_rejects_nonexistent_subsection() {
    let skill_id = SkillId::new("worker-agent", "coding");
    let (tmp, loader) = setup_skill_dir(&skill_id, SAMPLE_SKILL_MD_WITH_SUBSECTIONS);
    let skill_path = loader.skill_md_path(&skill_id);

    let mut app = create_test_app(no_brain_test_config());
    app.insert_resource(loader.clone());
    app.world_mut()
        .resource_mut::<SkillRegistry>()
        .upsert(make_skill_entry(skill_id.clone(), true));

    let producer_task_id = Uuid::new_v4();
    let candidate_id = stage_resolved_candidate(
        &mut app.world_mut().resource_mut::<ExperienceStore>(),
        producer_task_id,
    );

    let agent_id = Uuid::new_v4();
    let agent_entity = app
        .world_mut()
        .spawn(Agent {
            id: agent_id,
            profile: AgentProfile {
                name: "skill-updater".to_string(),
                model: "test-model".to_string(),
            },
            capabilities: AgentCapabilities {
                tags: vec!["skill-updater".to_string()],
                description: "skill-updater agent".to_string(),
            },
            kind: AgentKind::Persistent,
            parent_id: None,
            bound_task_id: None,
            tool_permissions: AgentToolPermissions {
                default_permission: ToolPermission::Allow,
                default_permission_explicit: true,
                overrides: std::collections::HashMap::new(),
            },
            system_prompt: None,
        })
        .id();
    app.world_mut()
        .resource_mut::<EntityIndex>()
        .agents
        .insert(agent_id, agent_entity);

    let task_entity = app
        .world_mut()
        .spawn((
            Task::from_user_input_ready("skill update task", 3, default_channel()),
            ShortTermMemory::default(),
        ))
        .id();
    let task_id = app.world().get::<Task>(task_entity).unwrap().id;
    app.world_mut()
        .resource_mut::<EntityIndex>()
        .tasks
        .insert(task_id, task_entity);

    let (work_item_id, work_item_entity) =
        spawn_work_item_with_context(app.world_mut(), skill_id.clone(), 1, candidate_id);

    let calling_state_entity = app
        .world_mut()
        .spawn(ToolCallingState {
            task_id,
            agent_id,
            pending_tool_call_ids: vec!["call_test".to_string()],
            iteration: 1,
            max_iterations: 5,
            conversation: Vec::new(),
            tools: Vec::new(),
            request_kind: AgentRequestKind::ToolExecution {
                tool_name: "submit_skill_update".to_string(),
            },
            work_item_id: Some(work_item_id),
        })
        .id();

    // 引用不存在的 subsection：### NonExistent
    app.world_mut().spawn(ToolExecutionRequestMessage {
        request: AgentExecutionRequest {
            task_id,
            agent_id,
            request_kind: AgentRequestKind::ToolExecution {
                tool_name: "submit_skill_update".to_string(),
            },
            prompt: String::new(),
            system_prompt: None,
            tools: Vec::new(),
            conversation: None,
            work_item_id: Some(work_item_id),
            model_override: None,
        },
        tool_name: "submit_skill_update".to_string(),
        tool_input: serde_json::json!({
            "operations": [
                {"action": "replace_subsection", "section": "## Usage", "subsection": "### NonExistent", "content": "x"}
            ],
            "rationale": "test dry-run rejection for nonexistent subsection"
        }),
        pending_confirmation_id: None,
        tool_call_id: Some("call_test".to_string()),
        pending_confirmation_options: None,
        work_item_entity: Some(work_item_entity),
        confirmed_once: false,
    });

    for _ in 0..5 {
        app.update();
    }

    // 1. 不应 spawn SkillUpdateCompletedMessage
    let completed_count = {
        let mut q = app.world_mut().query::<&SkillUpdateCompletedMessage>();
        q.iter(app.world()).count()
    };
    assert_eq!(
        completed_count, 0,
        "dry-run should reject nonexistent subsection; no SkillUpdateCompletedMessage spawned"
    );

    // 2. ToolCallingState pending_tool_call_ids 应被清空
    let calling_state = app
        .world()
        .get::<ToolCallingState>(calling_state_entity)
        .expect("ToolCallingState should still exist");
    assert!(
        calling_state.pending_tool_call_ids.is_empty(),
        "dry-run failure result should be consumed by tool_calling_orchestrator_system"
    );
    let has_dry_run_error_msg = calling_state.conversation.iter().any(|m| match m {
        ConversationMessage::Tool { content, .. } => content.contains("dry-run failed"),
        _ => false,
    });
    assert!(
        has_dry_run_error_msg,
        "ToolCallingState conversation should contain dry-run failure message"
    );

    // 3. SKILL.md 内容不变
    let content = std::fs::read_to_string(&skill_path).unwrap();
    assert!(
        content.contains("Run `make install`."),
        "SKILL.md should be unchanged after dry-run rejection"
    );

    // 4. SkillRegistry version 仍是 1
    let registry = app.world().resource::<SkillRegistry>();
    let entry = registry
        .get(&skill_id)
        .expect("skill should be in registry");
    assert_eq!(
        entry.version, 1,
        "registry version should remain 1 after dry-run rejection"
    );

    // 5. 候选状态保持 GovernanceResolved
    let store = app.world().resource::<ExperienceStore>();
    let c = store
        .candidates
        .get(&candidate_id)
        .expect("candidate exists");
    assert_eq!(
        c.status,
        ExperienceCandidateStatus::GovernanceResolved,
        "candidate status should remain GovernanceResolved after dry-run rejection"
    );

    drop(tmp);
}

/// v8 D19：dry-run 通过的 subsection operation 应成功 apply。
///
/// 通过 `submit_skill_update` 工具调用路径，验证合法 subsection operation 不被误拒：
/// - SKILL.md 内容已更新
/// - SkillRegistry version 刷新到 2
/// - 候选状态推进到 Persisted
#[test]
fn submit_skill_update_dry_run_accepts_valid_subsection_operation() {
    let skill_id = SkillId::new("worker-agent", "coding");
    let (tmp, loader) = setup_skill_dir(&skill_id, SAMPLE_SKILL_MD_WITH_SUBSECTIONS);
    let skill_path = loader.skill_md_path(&skill_id);

    let mut app = create_test_app(no_brain_test_config());
    app.insert_resource(loader.clone());
    app.world_mut()
        .resource_mut::<SkillRegistry>()
        .upsert(make_skill_entry(skill_id.clone(), true));

    let producer_task_id = Uuid::new_v4();
    let candidate_id = stage_resolved_candidate(
        &mut app.world_mut().resource_mut::<ExperienceStore>(),
        producer_task_id,
    );

    let agent_id = Uuid::new_v4();
    let agent_entity = app
        .world_mut()
        .spawn(Agent {
            id: agent_id,
            profile: AgentProfile {
                name: "skill-updater".to_string(),
                model: "test-model".to_string(),
            },
            capabilities: AgentCapabilities {
                tags: vec!["skill-updater".to_string()],
                description: "skill-updater agent".to_string(),
            },
            kind: AgentKind::Persistent,
            parent_id: None,
            bound_task_id: None,
            tool_permissions: AgentToolPermissions {
                default_permission: ToolPermission::Allow,
                default_permission_explicit: true,
                overrides: std::collections::HashMap::new(),
            },
            system_prompt: None,
        })
        .id();
    app.world_mut()
        .resource_mut::<EntityIndex>()
        .agents
        .insert(agent_id, agent_entity);

    let task_entity = app
        .world_mut()
        .spawn((
            Task::from_user_input_ready("skill update task", 3, default_channel()),
            ShortTermMemory::default(),
        ))
        .id();
    let task_id = app.world().get::<Task>(task_entity).unwrap().id;
    app.world_mut()
        .resource_mut::<EntityIndex>()
        .tasks
        .insert(task_id, task_entity);

    let (work_item_id, work_item_entity) =
        spawn_work_item_with_context(app.world_mut(), skill_id.clone(), 1, candidate_id);

    app.world_mut().spawn(ToolCallingState {
        task_id,
        agent_id,
        pending_tool_call_ids: vec!["call_test".to_string()],
        iteration: 1,
        max_iterations: 5,
        conversation: Vec::new(),
        tools: Vec::new(),
        request_kind: AgentRequestKind::ToolExecution {
            tool_name: "submit_skill_update".to_string(),
        },
        work_item_id: Some(work_item_id),
    });

    // 合法 replace_subsection：引用实际存在的 ## Usage / ### Setup
    app.world_mut().spawn(ToolExecutionRequestMessage {
        request: AgentExecutionRequest {
            task_id,
            agent_id,
            request_kind: AgentRequestKind::ToolExecution {
                tool_name: "submit_skill_update".to_string(),
            },
            prompt: String::new(),
            system_prompt: None,
            tools: Vec::new(),
            conversation: None,
            work_item_id: Some(work_item_id),
            model_override: None,
        },
        tool_name: "submit_skill_update".to_string(),
        tool_input: serde_json::json!({
            "operations": [
                {"action": "replace_subsection", "section": "## Usage", "subsection": "### Setup", "content": "Run `make setup`."}
            ],
            "rationale": "valid subsection update"
        }),
        pending_confirmation_id: None,
        tool_call_id: Some("call_test".to_string()),
        pending_confirmation_options: None,
        work_item_entity: Some(work_item_entity),
        confirmed_once: false,
    });

    for _ in 0..5 {
        app.update();
    }

    // 1. SKILL.md 已更新
    let content = std::fs::read_to_string(&skill_path).unwrap();
    assert!(
        content.contains("Run `make setup`."),
        "dry-run should accept valid subsection operation; SKILL.md should be updated"
    );
    assert!(
        !content.contains("Run `make install`."),
        "old Setup content should be replaced"
    );

    // 2. SkillRegistry version 已刷新
    let registry = app.world().resource::<SkillRegistry>();
    let entry = registry
        .get(&skill_id)
        .expect("skill should be in registry");
    assert_eq!(
        entry.version, 2,
        "registry version should be 2 after successful subsection update"
    );

    // 3. 候选状态为 Persisted
    let store = app.world().resource::<ExperienceStore>();
    let c = store
        .candidates
        .get(&candidate_id)
        .expect("candidate exists");
    assert_eq!(
        c.status,
        ExperienceCandidateStatus::Persisted,
        "candidate should be Persisted after successful subsection update"
    );

    drop(tmp);
}
