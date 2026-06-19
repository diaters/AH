use bevy::prelude::{Component, Resource};
use serde::{Deserialize, Serialize};
use tracing::debug;

use super::{AgentId, TaskId};

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
    GovernancePending,
    GovernanceResolved,
    NeedsUserApproval,
    WritebackPending,
    Approved,
    Rejected,
    Persisted,
    WritebackFailed,
}

/// 经验写回目标：治理决议后的唯一最终去向。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ExperienceWritebackDestination {
    LongTermMemory,
    SkillPackage,
    IncubationProposal,
    Rejected,
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
    Knowledge { content: String },
    Skill {
        name: String,
        description: String,
        instructions: String,
        file_refs: Vec<SkillFileRef>,
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
            },
            dependency_refs: Vec::new(),
            status: ExperienceCandidateStatus::Submitted,
            governing_agent_id: None,
            derived_from_candidate_ids: Vec::new(),
        }
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
    pub fn find_or_create_proposal(
        &mut self,
        task_id: TaskId,
        agent_id: AgentId,
        profile: super::AgentProfile,
    ) -> &mut IncubationProposal {
        self.proposals
            .entry(task_id)
            .or_insert_with(|| IncubationProposal::new(task_id, agent_id, profile));
        self.proposals.get_mut(&task_id).unwrap()
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
            ExperienceCandidateStatus::GovernancePending,
            ExperienceCandidateStatus::GovernanceResolved,
            ExperienceCandidateStatus::NeedsUserApproval,
            ExperienceCandidateStatus::WritebackPending,
            ExperienceCandidateStatus::Approved,
            ExperienceCandidateStatus::Rejected,
            ExperienceCandidateStatus::Persisted,
            ExperienceCandidateStatus::WritebackFailed,
        ];
        assert_eq!(statuses.len(), 11);
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
}
