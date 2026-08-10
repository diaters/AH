use crate::prelude::{Component, Resource};
use serde::{Deserialize, Serialize};
use tracing::debug;

use super::{AgentId, TaskId};
use crate::infrastructure::skills::SkillId;
use crate::user_plugins::hook_point::HookPoint;

/// 经验候选类型提示。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ExperienceKindHint {
    Knowledge,
    Skill,
}

/// Skill 关联文件角色。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SkillFileRole {
    Script,
    Reference,
    Asset,
}

/// Skill 关联文件引用。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillFileRef {
    pub path: String,
    pub role: SkillFileRole,
}

/// 经验候选状态。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ExperienceCandidateStatus {
    Submitted,
    InInbox,
    Aggregated,
    Superseded, // 被合并候选替代
    GovernancePending,
    GovernanceResolved,
    NeedsUserApproval,
    WritebackPending,
    Approved,
    Rejected,
    Persisted,
    WritebackFailed,
    /// profile 生成中：治理决议为孵化后，等待 LLM 生成 Agent profile。
    ProfileGenerationPending,
    /// profile 生成失败：LLM 连续异常达到上限，或 profile-designer Agent 缺失。
    ProfileGenerationFailed,
    /// 被 experience_kind_filter 过滤
    Discarded,
}

/// 经验写回目标：治理决议后的唯一最终去向。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ExperienceWritebackDestination {
    LongTermMemory,
    SkillPackage,
    IncubationProposal,
    Rejected,
    /// skill-updater 自我迭代：由任务 20 的 skill_update_workitem_system 处理。
    SkillUpdate,
    /// skill-creator 新建 skill：由 skill_creation_writeback_system 处理 rename 写回。
    SkillCreation,
}

/// 经验治理决议：顶层治理对单个候选给出的最终判定。
#[derive(Debug, Clone, Component, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExperienceGovernanceDecision {
    pub candidate_id: uuid::Uuid,
    pub destination: ExperienceWritebackDestination,
    pub requires_user_confirmation: bool,
    pub decision_rationale: String,
    pub source_task_id: TaskId,
}

/// 经验候选载荷。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ExperienceCandidatePayload {
    Knowledge {
        content: String,
    },
    Skill {
        name: String,
        description: String,
        instructions: String,
        file_refs: Vec<SkillFileRef>,
        #[serde(default)]
        is_new: bool,
    },
}

impl ExperienceCandidatePayload {
    /// 返回知识类载荷的文本内容，Skill 类返回 None。
    pub fn content(&self) -> Option<String> {
        match self {
            ExperienceCandidatePayload::Knowledge { content, .. } => Some(content.clone()),
            ExperienceCandidatePayload::Skill { .. } => None,
        }
    }
}

/// 经验候选：任务结束后产出的治理候选，不是正式长期资产。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExperienceCandidate {
    pub candidate_id: uuid::Uuid,
    pub producer_task_id: TaskId,
    pub producer_agent_id: AgentId,
    pub title: String,
    pub kind_hint: ExperienceKindHint,
    pub payload: ExperienceCandidatePayload,
    pub dependency_refs: Vec<String>,
    pub status: ExperienceCandidateStatus,
    /// 最终治理该候选的顶层 Agent ID，用于确认后的写回路由。
    pub governing_agent_id: Option<AgentId>,
    /// 若此候选由顶层基于多个候选重写出，记录来源候选 ID。
    pub derived_from_candidate_ids: Vec<uuid::Uuid>,
}

impl ExperienceCandidate {
    /// 创建知识类候选。
    pub fn knowledge(
        candidate_id: uuid::Uuid,
        producer_task_id: TaskId,
        producer_agent_id: AgentId,
        title: String,
        content: String,
    ) -> Self {
        Self {
            candidate_id,
            producer_task_id,
            producer_agent_id,
            title,
            kind_hint: ExperienceKindHint::Knowledge,
            payload: ExperienceCandidatePayload::Knowledge { content },
            dependency_refs: Vec::new(),
            status: ExperienceCandidateStatus::Submitted,
            governing_agent_id: None,
            derived_from_candidate_ids: Vec::new(),
        }
    }

    /// 创建 Skill 类候选。
    #[allow(clippy::too_many_arguments)]
    pub fn skill(
        candidate_id: uuid::Uuid,
        producer_task_id: TaskId,
        producer_agent_id: AgentId,
        title: String,
        name: String,
        description: String,
        instructions: String,
        file_refs: Vec<SkillFileRef>,
    ) -> Self {
        Self {
            candidate_id,
            producer_task_id,
            producer_agent_id,
            title,
            kind_hint: ExperienceKindHint::Skill,
            payload: ExperienceCandidatePayload::Skill {
                name,
                description,
                instructions,
                file_refs,
                is_new: false,
            },
            dependency_refs: Vec::new(),
            status: ExperienceCandidateStatus::Submitted,
            governing_agent_id: None,
            derived_from_candidate_ids: Vec::new(),
        }
    }

    /// 创建 Skill 类候选（新建 skill，is_new = true）。
    #[allow(clippy::too_many_arguments)]
    pub fn skill_new(
        candidate_id: uuid::Uuid,
        producer_task_id: TaskId,
        producer_agent_id: AgentId,
        title: String,
        name: String,
        description: String,
        instructions: String,
        file_refs: Vec<SkillFileRef>,
    ) -> Self {
        Self {
            candidate_id,
            producer_task_id,
            producer_agent_id,
            title,
            kind_hint: ExperienceKindHint::Skill,
            payload: ExperienceCandidatePayload::Skill {
                name,
                description,
                instructions,
                file_refs,
                is_new: true,
            },
            dependency_refs: Vec::new(),
            status: ExperienceCandidateStatus::Submitted,
            governing_agent_id: None,
            derived_from_candidate_ids: Vec::new(),
        }
    }

    /// 判断 Skill 载荷是否为新建 skill（is_new = true）。
    pub fn is_skill_new(payload: &ExperienceCandidatePayload) -> bool {
        matches!(payload, ExperienceCandidatePayload::Skill { is_new: true, .. })
    }

    /// 判断候选是否需要用户确认。
    ///
    /// Skill 类候选需要用户确认。
    pub fn requires_user_confirmation(&self) -> bool {
        matches!(self.kind_hint, ExperienceKindHint::Skill)
    }

    /// 将知识类候选转换为长期记忆条目。
    ///
    /// 仅知识类候选可以转换，Skill 类返回 `None`。
    pub fn as_long_term_memory_entry(&self) -> Option<super::LongTermMemoryEntry> {
        match &self.payload {
            ExperienceCandidatePayload::Knowledge { content } => {
                Some(super::LongTermMemoryEntry::new(content.clone()))
            }
            ExperienceCandidatePayload::Skill { .. } => None,
        }
    }
}

/// 待派发经验候选相关 hook 的事件队列。
///
/// 由于 `ExperienceCandidate` 存储在 `ExperienceStore` Resource 中而非 ECS Entity，
/// 无法附带 Component 标记，因此使用 scratch resource 记录待派发事件。
///
/// 写入系统（提交、批准、拒绝）将 `(HookPoint, candidate_id)` 推入队列，
/// companion 系统 `on_experience_hook_system` 逐条派发 hook 后清空队列。
#[derive(Resource, Default)]
pub struct PendingExperienceHooks(pub Vec<(HookPoint, uuid::Uuid)>);

/// 经验收件箱状态。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum ExperienceInboxStatus {
    #[default]
    Pending,
    Consumed,
}

/// 经验收件箱：父任务绑定的治理缓冲层。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExperienceInbox {
    pub owner_task_id: TaskId,
    pub owner_agent_id: AgentId,
    pub candidate_ids: Vec<uuid::Uuid>,
    pub status: ExperienceInboxStatus,
}

/// 经验候选存储：全局运行时资源。
#[derive(Resource, Debug, Clone, Default)]
pub struct ExperienceStore {
    pub candidates: std::collections::HashMap<uuid::Uuid, ExperienceCandidate>,
    pub inboxes: std::collections::HashMap<TaskId, ExperienceInbox>,
    /// 顶层候选（无父任务的 Agent 自身产生的候选）
    pub root_candidates: std::collections::HashMap<TaskId, Vec<uuid::Uuid>>,
    /// 任务级孵化提案：同一任务最多一个活跃 proposal。
    pub proposals: std::collections::HashMap<TaskId, IncubationProposal>,
    /// 审批请求 ID 到候选 ID 的精确绑定。
    approval_bindings: std::collections::HashMap<uuid::Uuid, uuid::Uuid>,
    /// 已触发 profile 更新评估的候选 ID 集合，避免重复触发。
    pub profile_update_triggered: std::collections::HashSet<uuid::Uuid>,
}

/// profile 生成运行时上下文 Component：附加在 WorkItem Entity 上，
/// 与 `SkillUpdateContext` 存储模型一致。
///
/// 由 `profile_generation_workitem_system` 在 spawn `WorkItemType::ProfileGeneration` workitem 时
/// 一并注入到同一 entity，供 orchestrator（工具执行）、completion_system（完成处理）与
/// approval_system（拒绝并反馈重试）通过 Query 读取。
///
/// `exception_count` 语义：仅累计 LLM 异常（未调工具 / 互斥冲突 / Err / 调用非相关工具）。
/// - LLM 成功调用 submit_profile_update 或 skip_profile_update 后，由 orchestrator 归 0。
/// - reject_with_feedback 不占用计数（透传不变）。
/// - 达到 `MAX_PROFILE_EXCEPTIONS` 后不再重试，走失败路径。
#[derive(Component, Debug, Clone, PartialEq, Eq)]
pub struct ProfileGenerationContext {
    pub kind: ProfileGenerationKind,
    pub exception_count: u32,
    /// 更新场景下保存现有 profile，供拒绝并反馈重试时重新构建请求。
    pub existing_profile: Option<ExistingAgentProfile>,
    /// LLM 生成的 profile；更新场景下由 completion_system 写入，
    /// 供 profile_update_writeback_system 在审批通过后读取。
    pub generated_profile: Option<GeneratedProfile>,
}

impl ExperienceStore {
    /// 将候选投入父任务的收件箱，状态置为 InInbox。
    pub fn queue_for_parent(
        &mut self,
        parent_task_id: TaskId,
        parent_agent_id: AgentId,
        mut candidate: ExperienceCandidate,
    ) {
        candidate.status = ExperienceCandidateStatus::InInbox;
        let candidate_id = candidate.candidate_id;
        self.candidates.insert(candidate_id, candidate);
        self.inboxes
            .entry(parent_task_id)
            .or_insert_with(|| ExperienceInbox {
                owner_task_id: parent_task_id,
                owner_agent_id: parent_agent_id,
                candidate_ids: Vec::new(),
                status: ExperienceInboxStatus::Pending,
            })
            .candidate_ids
            .push(candidate_id);
    }

    /// 将候选暂存为顶层候选（用于持久型 Agent 自身的经验沉淀）。
    pub fn stage_root_candidate(&mut self, candidate: ExperienceCandidate) {
        let task_id = candidate.producer_task_id;
        let candidate_id = candidate.candidate_id;
        self.candidates.insert(candidate_id, candidate);
        self.root_candidates
            .entry(task_id)
            .or_default()
            .push(candidate_id);
    }

    /// 获取指定任务的顶层候选列表。
    pub fn root_candidates_for_task(&self, task_id: TaskId) -> Vec<uuid::Uuid> {
        self.root_candidates
            .get(&task_id)
            .cloned()
            .unwrap_or_default()
    }

    /// 获取指定任务的收件箱中的候选摘要。
    pub fn list_for_task(&self, task_id: TaskId) -> Vec<&ExperienceCandidate> {
        self.inboxes
            .get(&task_id)
            .map(|inbox| {
                inbox
                    .candidate_ids
                    .iter()
                    .filter_map(|id| self.candidates.get(id))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// 消费指定任务的收件箱，返回其中候选 ID 并将候选状态置为 Aggregated。
    pub fn aggregate_inbox_for_task(&mut self, task_id: TaskId) -> Vec<uuid::Uuid> {
        let Some(inbox) = self.inboxes.get_mut(&task_id) else {
            return Vec::new();
        };
        inbox.status = ExperienceInboxStatus::Consumed;
        let ids = inbox.candidate_ids.clone();
        for id in &ids {
            if let Some(c) = self.candidates.get_mut(id) {
                c.status = ExperienceCandidateStatus::Aggregated;
            }
        }
        ids
    }

    /// 将指定任务的顶层候选置为 GovernancePending，准备进入顶层治理。
    pub fn promote_root_candidates_to_governance(&mut self, task_id: TaskId) -> Vec<uuid::Uuid> {
        let ids = self.root_candidates_for_task(task_id);
        for id in &ids {
            if let Some(c) = self.candidates.get_mut(id) {
                c.status = ExperienceCandidateStatus::GovernancePending;
            }
        }
        ids
    }

    /// 统一收束顶层治理输入：合并顶层自身候选与子层汇聚候选。
    ///
    /// 顶层治理只消费这一份统一输入，不再分别读取两个存储区域。
    pub fn collect_top_level_governance_candidates(&mut self, task_id: TaskId) -> Vec<uuid::Uuid> {
        let mut ids = self.root_candidates_for_task(task_id);

        if let Some(inbox) = self.inboxes.get(&task_id) {
            ids.extend(inbox.candidate_ids.iter().copied().filter(|id| {
                self.candidates
                    .get(id)
                    .is_some_and(|c| c.status == ExperienceCandidateStatus::Aggregated)
            }));
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

    /// 按 producer_task_id 查找候选。
    pub fn candidates_by_producer_task(&self, task_id: TaskId) -> Vec<&ExperienceCandidate> {
        self.candidates
            .values()
            .filter(|c| c.producer_task_id == task_id)
            .collect()
    }

    /// 获取指定任务中处于 GovernancePending 状态的候选（包含 root 和 aggregated）。
    pub fn governance_candidates_for_task(&self, task_id: TaskId) -> Vec<uuid::Uuid> {
        self.candidates
            .values()
            .filter(|c| {
                c.status == ExperienceCandidateStatus::GovernancePending
                    && (c.producer_task_id == task_id
                        || self
                            .inboxes
                            .get(&task_id)
                            .is_some_and(|inbox| inbox.candidate_ids.contains(&c.candidate_id)))
            })
            .map(|c| c.candidate_id)
            .collect()
    }

    /// 为审批请求绑定目标候选。
    pub fn bind_approval_request(&mut self, request_id: uuid::Uuid, candidate_id: uuid::Uuid) {
        self.approval_bindings.insert(request_id, candidate_id);
    }

    /// 根据审批请求 ID 查找候选 ID。
    pub fn candidate_id_for_request(&self, request_id: uuid::Uuid) -> Option<uuid::Uuid> {
        self.approval_bindings.get(&request_id).copied()
    }

    /// 根据确认请求 ID 精确应用确认结果，返回实际更新的候选 ID。
    pub fn apply_confirmation_response_precise(
        &mut self,
        request_id: uuid::Uuid,
        selected_option: &str,
    ) -> Option<uuid::Uuid> {
        let candidate_id = self.candidate_id_for_request(request_id)?;
        let approved = selected_option == "approve";
        if let Some(candidate) = self.candidates.get_mut(&candidate_id) {
            candidate.status = if approved {
                ExperienceCandidateStatus::Approved
            } else {
                ExperienceCandidateStatus::Rejected
            };
            Some(candidate_id)
        } else {
            None
        }
    }

    /// 根据确认请求 ID 应用确认结果（精确匹配）。
    pub fn apply_confirmation_response(&mut self, request_id: uuid::Uuid, selected_option: &str) {
        self.apply_confirmation_response_precise(request_id, selected_option);
    }

    /// 查找或创建任务级孵化提案。
    ///
    /// 同一任务最多一个活跃 proposal，后续候选 merge 到已有 proposal。
    /// 若 proposal 已存在，更新 `proposed_agent_profile` 以反映最新的 LLM 生成结果
    /// （支持拒绝并反馈后的重新生成场景）。
    pub fn find_or_create_proposal(
        &mut self,
        task_id: TaskId,
        agent_id: AgentId,
        profile: super::AgentProfile,
    ) -> &mut IncubationProposal {
        let proposal = self
            .proposals
            .entry(task_id)
            .or_insert_with(|| IncubationProposal::new(task_id, agent_id, profile.clone()));
        proposal.proposed_agent_profile = profile;
        proposal
    }

    /// 将候选合并到任务级提案中。若不存在则创建。
    pub fn merge_into_proposal(
        &mut self,
        task_id: TaskId,
        agent_id: AgentId,
        profile: super::AgentProfile,
        candidate: &ExperienceCandidate,
    ) {
        let proposal = self.find_or_create_proposal(task_id, agent_id, profile);
        proposal.merge_candidate(candidate);
        debug!(
            event = "IncubationProposalMerged",
            task_id = %task_id,
            candidate_id = %candidate.candidate_id,
            "candidate merged into incubation proposal"
        );
    }
}

/// 经验写回请求消息：治理决议后由统一写回层消费。
#[derive(Debug, Clone, Component)]
pub struct ExperienceWritebackRequestMessage {
    pub decision: ExperienceGovernanceDecision,
}

/// 经验收集请求消息。
#[derive(Debug, Clone, Component)]
pub struct ExperienceCollectionRequestMessage {
    pub task_id: TaskId,
    pub parent_task_id: Option<TaskId>,
    pub parent_agent_id: Option<AgentId>,
    /// 原任务治理者，负责后续顶层经验治理与落盘。
    pub governing_agent_id: AgentId,
}

/// 经验治理请求消息。
#[derive(Debug, Clone, Component)]
pub struct ExperienceGovernanceRequestMessage {
    pub task_id: TaskId,
    pub agent_id: AgentId,
}

/// profile 生成场景：孵化时生成新 profile，或对持久型 Agent 评估更新。
#[allow(dead_code)] // 任务 6 起使用
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileGenerationKind {
    /// 孵化场景：根据经验候选为新 Agent 生成 name/tags/description。
    Incubation,
    /// 更新场景：评估现有 Agent profile 是否需要根据新经验更新。
    Update,
}

/// profile 生成异常计数上限：LLM 连续异常达到此值后不再重试，走失败路径。
///
/// 注意：此上限仅针对 LLM 异常（未调工具 / 互斥冲突 / Err / 调用非相关工具），
/// 不限制用户 reject_with_feedback 次数（reject_with_feedback 不占用异常计数）。
#[allow(dead_code)]
pub const MAX_PROFILE_EXCEPTIONS: u32 = 3;

/// 现有 Agent profile：更新场景下作为 LLM 评估输入。
#[allow(dead_code)] // 任务 6 起使用
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExistingAgentProfile {
    pub name: String,
    pub tags: Vec<String>,
    pub description: String,
}

/// LLM 生成的 Agent profile。
#[allow(dead_code)] // 任务 6 起使用
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeneratedProfile {
    pub name: String,
    pub tags: Vec<String>,
    pub description: String,
}

/// profile 生成请求消息：由治理系统（孵化）、更新触发系统或异常重试 / 反馈重试发起。
#[allow(dead_code)] // 任务 6 起使用
#[derive(Debug, Clone, Component)]
pub struct ProfileGenerationRequestMessage {
    pub task_id: TaskId,
    pub agent_id: AgentId,
    pub candidate_ids: Vec<uuid::Uuid>,
    pub existing_profile: Option<ExistingAgentProfile>,
    pub kind: ProfileGenerationKind,
    /// 拒绝并反馈场景：用户评审反馈，注入 LLM 上下文驱动重新生成。
    /// 异常重试场景：系统提示（如"上一轮未调用工具"），与用户反馈复用同一字段。
    pub feedback: Option<String>,
    /// LLM 异常计数：仅累计 LLM 失误（未调工具 / 互斥冲突 / Err / 调用非相关工具）。
    /// reject_with_feedback 透传不变；LLM 成功调工具后由 orchestrator 归 0。
    pub exception_count: u32,
}

/// profile 生成完成消息：由 profile-designer 工具调用完成后触发。
#[allow(dead_code)] // 任务 6 起使用
#[derive(Debug, Clone, Component)]
pub struct ProfileGenerationCompletedMessage {
    pub task_id: TaskId,
    pub agent_id: AgentId,
    /// LLM 生成的 profile；None 表示 LLM 调用了 skip_profile_update 或回退。
    pub generated_profile: Option<GeneratedProfile>,
    pub kind: ProfileGenerationKind,
}

/// 受保护标签：LLM 不可直接生成，由系统注入。
#[allow(dead_code)] // 任务 7 起使用
const PROTECTED_TAGS: &[&str] = &["incubated", "default"];

/// 过滤 LLM 生成的 tags：
/// - 移除受保护标签（incubated、default）
/// - 从 existing_tags 中补回受保护标签
/// - 去重并排序
#[allow(dead_code)] // 任务 7 起使用
pub fn sanitize_tags(llm_tags: Vec<String>, existing_tags: &[String]) -> Vec<String> {
    let mut result: Vec<String> = llm_tags
        .into_iter()
        .filter(|t| !PROTECTED_TAGS.contains(&t.as_str()))
        .collect();

    for tag in existing_tags {
        if PROTECTED_TAGS.contains(&tag.as_str()) && !result.contains(tag) {
            result.push(tag.clone());
        }
    }

    result.sort_unstable();
    result.dedup();

    result
}

/// 孵化提案状态。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum IncubationProposalStatus {
    #[default]
    Proposed,
    Approved,
    Executing,
    Executed,
    ExecutionFailed,
    Rejected,
}

/// 孵化提案：default Agent 的任务级正式治理输出。
#[derive(Debug, Clone, Component, Serialize, Deserialize)]
pub struct IncubationProposal {
    pub proposal_id: uuid::Uuid,
    pub source_agent_id: AgentId,
    pub source_task_id: TaskId,
    pub proposed_agent_profile: super::AgentProfile,
    pub knowledge_candidate_ids: Vec<uuid::Uuid>,
    pub skill_candidate_ids: Vec<uuid::Uuid>,
    pub incubation_rationale: String,
    pub status: IncubationProposalStatus,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl IncubationProposal {
    /// 创建新的任务级孵化提案。
    pub fn new(
        source_task_id: TaskId,
        source_agent_id: AgentId,
        proposed_agent_profile: super::AgentProfile,
    ) -> Self {
        let now = chrono::Utc::now();
        Self {
            proposal_id: uuid::Uuid::new_v4(),
            source_agent_id,
            source_task_id,
            proposed_agent_profile,
            knowledge_candidate_ids: Vec::new(),
            skill_candidate_ids: Vec::new(),
            incubation_rationale: String::new(),
            status: IncubationProposalStatus::Proposed,
            created_at: now,
            updated_at: now,
        }
    }

    /// 将候选合并到提案中，按 kind_hint 分类，不允许重复。
    pub fn merge_candidate(&mut self, candidate: &ExperienceCandidate) {
        let ids = match candidate.kind_hint {
            ExperienceKindHint::Knowledge => &mut self.knowledge_candidate_ids,
            ExperienceKindHint::Skill => &mut self.skill_candidate_ids,
        };
        if !ids.contains(&candidate.candidate_id) {
            ids.push(candidate.candidate_id);
        }
        self.updated_at = chrono::Utc::now();
    }
}

/// skill-updater workitem 的上下文 Component
///
/// 由 orchestrator 在 spawn `WorkItemType::SkillUpdate` workitem 时一并注入到同一 entity，
/// skill-updater Agent 通过读取该 Component 获取待更新 skill 的基线版本与来源候选，
/// 完成后由 orchestrator 读取并构造 `SkillUpdateCompletedMessage`。
#[allow(dead_code)] // 由后续 orchestrator/skill-updater 链路使用
#[derive(Component, Debug, Clone)]
pub struct SkillUpdateContext {
    pub skill_id: SkillId,
    pub base_version: u32,
    pub experience_candidate_id: uuid::Uuid,
    pub governing_agent_id: AgentId,
}

/// skill 更新的结构化 diff 操作
#[allow(dead_code)] // 由后续 orchestrator/skill-updater 链路使用
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "action")]
pub enum SkillUpdateOperation {
    #[serde(rename = "replace_section")]
    ReplaceSection {
        section: String,
        content: String,
        /// 目标文件路径（相对于 skill 目录）。None → SKILL.md。
        /// 仅接受 .md 后缀，且文件必须已存在。
        #[serde(default)]
        path: Option<String>,
    },
    #[serde(rename = "add_section")]
    AddSection {
        after: String,
        section: String,
        content: String,
        /// 目标文件路径（相对于 skill 目录）。None → SKILL.md。
        #[serde(default)]
        path: Option<String>,
    },
    #[serde(rename = "remove_section")]
    RemoveSection {
        section: String,
        /// 目标文件路径（相对于 skill 目录）。None → SKILL.md。
        #[serde(default)]
        path: Option<String>,
    },
    #[serde(rename = "replace_frontmatter")]
    ReplaceFrontmatter { field: String, value: String },
    /// v8 D19：三级标题级 — 在 `## {section}` 范围内替换 `### {subsection}` 内容
    #[serde(rename = "replace_subsection")]
    ReplaceSubsection {
        section: String,
        subsection: String,
        content: String,
        /// 目标文件路径（相对于 skill 目录）。None → SKILL.md。
        #[serde(default)]
        path: Option<String>,
    },
    /// v8 D19：三级标题级 — 在 `## {section}` 范围内 `### {after}` 之后插入新 `### {subsection}`
    #[serde(rename = "add_subsection")]
    AddSubsection {
        section: String,
        after: String,
        subsection: String,
        content: String,
        /// 目标文件路径（相对于 skill 目录）。None → SKILL.md。
        #[serde(default)]
        path: Option<String>,
    },
    /// v8 D19：三级标题级 — 删除 `## {section}` 下的 `### {subsection}`
    #[serde(rename = "remove_subsection")]
    RemoveSubsection {
        section: String,
        subsection: String,
        /// 目标文件路径（相对于 skill 目录）。None → SKILL.md。
        #[serde(default)]
        path: Option<String>,
    },
    /// v8 D19：兜底 — 整体替换 body，frontmatter 不变
    #[serde(rename = "replace_body")]
    ReplaceBody {
        content: String,
        /// 目标文件路径（相对于 skill 目录）。None → SKILL.md。
        #[serde(default)]
        path: Option<String>,
    },
    /// ADR-006：整体替换指定文件的内容。禁止作用于 SKILL.md。
    #[serde(rename = "replace_file")]
    ReplaceFile { path: String, content: String },
    /// ADR-006：创建新文件并写入内容。文件必须不存在。
    #[serde(rename = "create_file")]
    CreateFile { path: String, content: String },
    /// ADR-006：删除指定文件。禁止作用于 SKILL.md。
    #[serde(rename = "delete_file")]
    DeleteFile { path: String },
}

/// skill-updater workitem 完成后由 orchestrator spawn
#[allow(dead_code)] // 由后续 orchestrator/skill-updater 链路使用
#[derive(Debug, Clone, Component)]
pub struct SkillUpdateCompletedMessage {
    pub work_item_id: uuid::Uuid,
    pub task_id: TaskId,
    pub agent_id: AgentId,
    pub skill_id: SkillId,
    pub base_version: u32,
    pub new_version: u32,
    pub operations: Vec<SkillUpdateOperation>,
    pub rationale: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn experience_store_queues_candidate_for_parent_task() {
        let owner_task_id = uuid::Uuid::new_v4();
        let owner_agent_id = uuid::Uuid::new_v4();
        let candidate = ExperienceCandidate::knowledge(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "shell timeout knowledge".to_string(),
            "shell_stop 默认会等待退出".to_string(),
        );

        let mut store = ExperienceStore::default();
        store.queue_for_parent(owner_task_id, owner_agent_id, candidate.clone());

        let inbox = store.inboxes.get(&owner_task_id).unwrap();
        assert_eq!(inbox.owner_agent_id, owner_agent_id);
        assert_eq!(inbox.candidate_ids, vec![candidate.candidate_id]);
        assert_eq!(
            store
                .candidates
                .get(&candidate.candidate_id)
                .unwrap()
                .status,
            ExperienceCandidateStatus::InInbox,
        );
        assert_eq!(inbox.status, ExperienceInboxStatus::Pending);
    }

    #[test]
    fn candidate_status_machine_has_required_states() {
        let statuses = [
            ExperienceCandidateStatus::Submitted,
            ExperienceCandidateStatus::InInbox,
            ExperienceCandidateStatus::Aggregated,
            ExperienceCandidateStatus::Superseded,
            ExperienceCandidateStatus::GovernancePending,
            ExperienceCandidateStatus::GovernanceResolved,
            ExperienceCandidateStatus::NeedsUserApproval,
            ExperienceCandidateStatus::WritebackPending,
            ExperienceCandidateStatus::Approved,
            ExperienceCandidateStatus::Rejected,
            ExperienceCandidateStatus::Persisted,
            ExperienceCandidateStatus::WritebackFailed,
            ExperienceCandidateStatus::ProfileGenerationPending,
        ];
        assert_eq!(statuses.len(), 13);
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

    #[test]
    fn inbox_has_pending_and_consumed_states() {
        let inbox = ExperienceInbox {
            owner_task_id: uuid::Uuid::new_v4(),
            owner_agent_id: uuid::Uuid::new_v4(),
            candidate_ids: vec![],
            status: ExperienceInboxStatus::Pending,
        };
        assert!(matches!(inbox.status, ExperienceInboxStatus::Pending));
    }

    #[test]
    fn experience_store_marks_inbox_consumed_and_aggregates() {
        let owner_task_id = uuid::Uuid::new_v4();
        let owner_agent_id = uuid::Uuid::new_v4();
        let producer_task_id = uuid::Uuid::new_v4();
        let candidate = ExperienceCandidate::knowledge(
            uuid::Uuid::new_v4(),
            producer_task_id,
            uuid::Uuid::new_v4(),
            "child fact".to_string(),
            "content".to_string(),
        );

        let mut store = ExperienceStore::default();
        store.queue_for_parent(owner_task_id, owner_agent_id, candidate.clone());
        let ids = store.aggregate_inbox_for_task(owner_task_id);

        assert_eq!(ids, vec![candidate.candidate_id]);
        assert_eq!(
            store
                .candidates
                .get(&candidate.candidate_id)
                .unwrap()
                .status,
            ExperienceCandidateStatus::Aggregated
        );
        assert_eq!(
            store.inboxes.get(&owner_task_id).unwrap().status,
            ExperienceInboxStatus::Consumed
        );
    }

    #[test]
    fn approval_binding_links_request_to_single_candidate() {
        let mut store = ExperienceStore::default();
        let request_id = uuid::Uuid::new_v4();
        let candidate = ExperienceCandidate::knowledge(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "bound fact".to_string(),
            "content".to_string(),
        );
        let candidate_id = candidate.candidate_id;
        store.stage_root_candidate(candidate);

        store.bind_approval_request(request_id, candidate_id);
        assert_eq!(
            store.candidate_id_for_request(request_id),
            Some(candidate_id)
        );
    }

    #[test]
    fn precise_confirmation_only_updates_bound_candidate() {
        let mut store = ExperienceStore::default();
        let request_id = uuid::Uuid::new_v4();

        let mut c1 = ExperienceCandidate::knowledge(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "first".to_string(),
            "content".to_string(),
        );
        c1.status = ExperienceCandidateStatus::NeedsUserApproval;
        let c1_id = c1.candidate_id;
        store.stage_root_candidate(c1);

        let mut c2 = ExperienceCandidate::knowledge(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "second".to_string(),
            "content".to_string(),
        );
        c2.status = ExperienceCandidateStatus::NeedsUserApproval;
        let c2_id = c2.candidate_id;
        store.stage_root_candidate(c2);

        store.bind_approval_request(request_id, c1_id);
        let updated = store.apply_confirmation_response_precise(request_id, "approve");

        assert_eq!(updated, Some(c1_id));
        assert_eq!(
            store.candidates.get(&c1_id).unwrap().status,
            ExperienceCandidateStatus::Approved
        );
        assert_eq!(
            store.candidates.get(&c2_id).unwrap().status,
            ExperienceCandidateStatus::NeedsUserApproval
        );
    }

    #[test]
    fn unbound_confirmation_does_not_affect_any_candidate() {
        let mut store = ExperienceStore::default();
        let mut candidate = ExperienceCandidate::knowledge(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "orphan".to_string(),
            "content".to_string(),
        );
        candidate.status = ExperienceCandidateStatus::NeedsUserApproval;
        let candidate_id = candidate.candidate_id;
        store.stage_root_candidate(candidate);

        let updated = store.apply_confirmation_response_precise(uuid::Uuid::new_v4(), "approve");

        assert_eq!(updated, None);
        assert_eq!(
            store.candidates.get(&candidate_id).unwrap().status,
            ExperienceCandidateStatus::NeedsUserApproval
        );
    }

    #[test]
    fn sanitize_tags_filters_protected_and_deduplicates() {
        use super::sanitize_tags;
        // LLM 输出包含受保护标签和重复标签
        let llm_tags = vec![
            "physics".to_string(),
            "default".to_string(),
            "incubated".to_string(),
            "physics".to_string(),
            "calculation".to_string(),
        ];
        let existing_tags = vec!["incubated".to_string()];

        let result = sanitize_tags(llm_tags, &existing_tags);

        // 受保护标签从 LLM 输出中过滤
        assert!(!result.contains(&"default".to_string()));
        // incubated 从 existing 中补回
        assert!(result.contains(&"incubated".to_string()));
        // 去重
        assert_eq!(
            result
                .iter()
                .filter(|t| t == &&"physics".to_string())
                .count(),
            1
        );
        // 保留非保护标签
        assert!(result.contains(&"calculation".to_string()));
    }

    #[test]
    fn sanitize_tags_empty_existing_for_incubation() {
        use super::sanitize_tags;
        let llm_tags = vec!["physics".to_string(), "calculation".to_string()];
        let result = sanitize_tags(llm_tags, &[]);
        assert!(result.contains(&"physics".to_string()));
        assert!(result.contains(&"calculation".to_string()));
        assert!(!result.contains(&"incubated".to_string()));
        // incubated 由写回逻辑注入，不在 sanitize_tags 中
    }

    #[test]
    fn skill_constructor_sets_is_new_false() {
        let candidate = ExperienceCandidate::skill(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "test skill".to_string(),
            "test-skill".to_string(),
            "description".to_string(),
            "instructions".to_string(),
            vec![],
        );
        assert!(
            matches!(candidate.payload, ExperienceCandidatePayload::Skill { is_new: false, .. }),
            "skill() should set is_new = false"
        );
        assert!(!ExperienceCandidate::is_skill_new(&candidate.payload));
    }

    #[test]
    fn skill_new_constructor_sets_is_new_true() {
        let candidate = ExperienceCandidate::skill_new(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "new skill".to_string(),
            "new-skill".to_string(),
            "description".to_string(),
            "instructions".to_string(),
            vec![],
        );
        assert!(
            matches!(candidate.payload, ExperienceCandidatePayload::Skill { is_new: true, .. }),
            "skill_new() should set is_new = true"
        );
        assert!(ExperienceCandidate::is_skill_new(&candidate.payload));
    }

    #[test]
    fn serde_default_for_is_new() {
        // JSON without is_new field should deserialize with is_new = false (serde default)
        let json = r#"{"Skill":{"name":"test","description":"d","instructions":"i","file_refs":[]}}"#;
        let payload: ExperienceCandidatePayload = serde_json::from_str(json).unwrap();
        match payload {
            ExperienceCandidatePayload::Skill { is_new, .. } => {
                assert!(!is_new, "is_new should default to false when absent in JSON");
            }
            _ => panic!("expected Skill payload"),
        }
    }
}

#[cfg(test)]
mod skill_update_operation_tests {
    use super::*;

    #[test]
    fn serialize_replace_section() {
        let op = SkillUpdateOperation::ReplaceSection {
            section: "## Steps".to_string(),
            content: "new content".to_string(),
            path: None,
        };
        let json = serde_json::to_string(&op).expect("serialize ReplaceSection");
        assert!(
            json.contains("\"replace_section\""),
            "expected JSON to contain replace_section tag, got: {json}"
        );
        assert!(json.contains("## Steps"));
        assert!(json.contains("new content"));
    }

    #[test]
    fn deserialize_add_section() {
        let json = "{\"action\":\"add_section\",\"after\":\"## Intro\",\"section\":\"## Tips\",\"content\":\"be careful\"}";
        let op: SkillUpdateOperation = serde_json::from_str(json).expect("deserialize AddSection");
        match op {
            SkillUpdateOperation::AddSection {
                after,
                section,
                content,
                path,
            } => {
                assert_eq!(after, "## Intro");
                assert_eq!(section, "## Tips");
                assert_eq!(content, "be careful");
                assert_eq!(
                    path, None,
                    "path should default to None when not provided in JSON"
                );
            }
            other => panic!("expected AddSection, got {other:?}"),
        }
    }

    #[test]
    fn replace_frontmatter_preserves_arbitrary_field_through_serde_roundtrip() {
        // 枚举本身允许任意 field 值；白名单检查应在 apply 层
        let op = SkillUpdateOperation::ReplaceFrontmatter {
            field: "arbitrary_field".to_string(),
            value: "arbitrary_value".to_string(),
        };
        let json = serde_json::to_string(&op).expect("serialize ReplaceFrontmatter");
        let de: SkillUpdateOperation =
            serde_json::from_str(&json).expect("deserialize ReplaceFrontmatter");
        match de {
            SkillUpdateOperation::ReplaceFrontmatter { field, value } => {
                assert_eq!(field, "arbitrary_field");
                assert_eq!(value, "arbitrary_value");
            }
            other => panic!("expected ReplaceFrontmatter, got {other:?}"),
        }
    }

    /// ADR-006：旧格式 JSON（无 `path` 字段）反序列化为 `path: None`。
    #[test]
    fn deserialize_legacy_section_op_without_path_defaults_to_none() {
        for json in [
            "{\"action\":\"replace_section\",\"section\":\"## Usage\",\"content\":\"x\"}",
            "{\"action\":\"add_section\",\"after\":\"## A\",\"section\":\"## B\",\"content\":\"x\"}",
            "{\"action\":\"remove_section\",\"section\":\"## Usage\"}",
            "{\"action\":\"replace_subsection\",\"section\":\"## A\",\"subsection\":\"### B\",\"content\":\"x\"}",
            "{\"action\":\"add_subsection\",\"section\":\"## A\",\"after\":\"### B\",\"subsection\":\"### C\",\"content\":\"x\"}",
            "{\"action\":\"remove_subsection\",\"section\":\"## A\",\"subsection\":\"### B\"}",
            "{\"action\":\"replace_body\",\"content\":\"x\"}",
        ] {
            let op: SkillUpdateOperation =
                serde_json::from_str(json).unwrap_or_else(|e| panic!("JSON `{json}` failed: {e}"));
            match &op {
                SkillUpdateOperation::ReplaceSection { path, .. }
                | SkillUpdateOperation::AddSection { path, .. }
                | SkillUpdateOperation::RemoveSection { path, .. }
                | SkillUpdateOperation::ReplaceSubsection { path, .. }
                | SkillUpdateOperation::AddSubsection { path, .. }
                | SkillUpdateOperation::RemoveSubsection { path, .. }
                | SkillUpdateOperation::ReplaceBody { path, .. } => {
                    assert_eq!(*path, None, "legacy JSON should default path to None");
                }
                _ => panic!("expected section-level op, got {op:?}"),
            }
        }
    }

    /// ADR-006：新格式 JSON（含 `path` 字段）反序列化保留路径。
    #[test]
    fn deserialize_section_op_with_path_keeps_path() {
        let json = "{\"action\":\"replace_section\",\"section\":\"## Usage\",\"content\":\"x\",\"path\":\"download.md\"}";
        let op: SkillUpdateOperation = serde_json::from_str(json).expect("deserialize with path");
        match op {
            SkillUpdateOperation::ReplaceSection { path, .. } => {
                assert_eq!(path.as_deref(), Some("download.md"));
            }
            other => panic!("expected ReplaceSection, got {other:?}"),
        }
    }

    /// ADR-006：3 种文件级操作序列化/反序列化 roundtrip。
    #[test]
    fn file_level_operations_serde_roundtrip() {
        let ops = [
            SkillUpdateOperation::ReplaceFile {
                path: "scripts/run.py".to_string(),
                content: "print('new')".to_string(),
            },
            SkillUpdateOperation::CreateFile {
                path: "templates/note.md".to_string(),
                content: "# Note".to_string(),
            },
            SkillUpdateOperation::DeleteFile {
                path: "obsolete.md".to_string(),
            },
        ];
        for op in ops {
            let json = serde_json::to_string(&op).expect("serialize");
            let de: SkillUpdateOperation = serde_json::from_str(&json).expect("deserialize");
            match (op, de) {
                (
                    SkillUpdateOperation::ReplaceFile {
                        path: p1,
                        content: c1,
                    },
                    SkillUpdateOperation::ReplaceFile { path, content },
                )
                | (
                    SkillUpdateOperation::CreateFile {
                        path: p1,
                        content: c1,
                    },
                    SkillUpdateOperation::CreateFile { path, content },
                ) => {
                    assert_eq!(path, p1);
                    assert_eq!(content, c1);
                }
                (
                    SkillUpdateOperation::DeleteFile { path: p1 },
                    SkillUpdateOperation::DeleteFile { path },
                ) => {
                    assert_eq!(path, p1);
                }
                (op, other) => panic!("mismatched roundtrip: {op:?} vs {other:?}"),
            }
        }
    }
}
