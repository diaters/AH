use bevy::prelude::{Component, Resource};
use serde::{Deserialize, Serialize};

use super::{AgentId, LongTermMemoryEntry, SharedKnowledgeEntry, TaskId};

/// 记忆写回结果。
#[derive(Debug, Clone, Default)]
pub struct MemoryWritebackBatch {
    pub accepted_long_term_memories: Vec<LongTermMemoryEntry>,
    pub shared_knowledge_candidates: Vec<SharedKnowledgeEntry>,
}

/// 记忆贡献请求消息
#[derive(Debug, Clone, Component)]
pub struct MemoryContributionRequestMessage {
    pub contributor_id: AgentId,
    pub contributor_name: String,
    pub parent_id: AgentId,
    pub memories: Vec<LongTermMemoryEntry>,
    pub task_summary: TaskSummary,
}

/// 任务摘要
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSummary {
    pub task_id: TaskId,
    pub goal: String,
    pub outcome: String,
}

/// 贡献评估结果（LLM 返回）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContributionEvaluation {
    pub absorb: Vec<AbsorbedMemory>,
    pub discard: Vec<DiscardedMemory>,
}

/// 被吸收的记忆
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AbsorbedMemory {
    pub content: String,
    pub reason: String,
}

/// 被丢弃的记忆
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscardedMemory {
    pub content: String,
    pub reason: String,
}

/// 记忆吸收消息（内部使用）
#[derive(Debug, Clone, Component)]
pub struct MemoryAbsorptionMessage {
    pub parent_id: AgentId,
    pub absorbed: Vec<LongTermMemoryEntry>,
}

/// 经验候选类型提示。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ExperienceKindHint {
    Knowledge,
    Executable,
    SharedKnowledge,
    Discard,
}

/// 经验候选状态。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ExperienceCandidateStatus {
    Submitted,
    InInbox,
    Aggregated,
    GovernancePending,
    NeedsUserApproval,
    Approved,
    Rejected,
    Persisted,
}

/// 经验候选载荷。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ExperienceCandidatePayload {
    Knowledge {
        content: String,
        memory_kind: super::LongTermMemoryKind,
    },
    Executable {
        intent: String,
        when_to_use: String,
        asset_refs: Vec<String>,
    },
}

impl ExperienceCandidatePayload {
    /// 返回知识类载荷的文本内容，可执行类返回 None。
    pub fn content(&self) -> Option<String> {
        match self {
            ExperienceCandidatePayload::Knowledge { content, .. } => Some(content.clone()),
            ExperienceCandidatePayload::Executable { .. } => None,
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
}

impl ExperienceCandidate {
    /// 创建知识类候选。
    pub fn knowledge(
        candidate_id: uuid::Uuid,
        producer_task_id: TaskId,
        producer_agent_id: AgentId,
        title: String,
        content: String,
        memory_kind: super::LongTermMemoryKind,
    ) -> Self {
        Self {
            candidate_id,
            producer_task_id,
            producer_agent_id,
            title,
            kind_hint: ExperienceKindHint::Knowledge,
            payload: ExperienceCandidatePayload::Knowledge {
                content,
                memory_kind,
            },
            dependency_refs: Vec::new(),
            status: ExperienceCandidateStatus::Submitted,
            governing_agent_id: None,
        }
    }

    /// 判断候选是否需要用户确认。
    ///
    /// 可执行类或带资产依赖的候选需要用户确认。
    pub fn requires_user_confirmation(&self) -> bool {
        matches!(self.kind_hint, ExperienceKindHint::Executable)
            || matches!(
                &self.payload,
                ExperienceCandidatePayload::Executable { asset_refs, .. } if !asset_refs.is_empty()
            )
    }

    /// 将知识类候选转换为长期记忆条目。
    ///
    /// 仅知识类候选可以转换，可执行类返回 `None`。
    pub fn as_long_term_memory_entry(&self) -> Option<super::LongTermMemoryEntry> {
        match &self.payload {
            ExperienceCandidatePayload::Knowledge {
                content,
                memory_kind,
            } => Some(super::LongTermMemoryEntry::new(
                *memory_kind,
                content.clone(),
            )),
            ExperienceCandidatePayload::Executable { .. } => None,
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

    /// 按 producer_task_id 查找候选。
    pub fn candidates_by_producer_task(&self, task_id: TaskId) -> Vec<&ExperienceCandidate> {
        self.candidates
            .values()
            .filter(|c| c.producer_task_id == task_id)
            .collect()
    }

    /// 获取指定任务中处于 GovernancePending 状态的候选。
    pub fn governance_candidates_for_task(&self, task_id: TaskId) -> Vec<uuid::Uuid> {
        self.root_candidates_for_task(task_id)
            .into_iter()
            .filter(|id| {
                self.candidates
                    .get(id)
                    .map(|c| c.status == ExperienceCandidateStatus::GovernancePending)
                    .unwrap_or(false)
            })
            .collect()
    }

    /// 根据确认请求 ID 应用确认结果。
    pub fn apply_confirmation_response(&mut self, request_id: uuid::Uuid, selected_option: &str) {
        // 找到对应的 NeedsUserApproval 候选并更新状态
        let approved = selected_option == "approve";
        for candidate in self.candidates.values_mut() {
            if candidate.status == ExperienceCandidateStatus::NeedsUserApproval {
                // 首版简单匹配：所有 NeedsUserApproval 候选根据用户选择更新
                candidate.status = if approved {
                    ExperienceCandidateStatus::Approved
                } else {
                    ExperienceCandidateStatus::Rejected
                };
            }
        }
        let _ = request_id; // 首版不精确匹配 request_id
    }
}

/// 经验收集请求消息。
#[derive(Debug, Clone, Component)]
pub struct ExperienceCollectionRequestMessage {
    pub task_id: TaskId,
    pub parent_task_id: Option<TaskId>,
    pub parent_agent_id: Option<AgentId>,
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
    Rejected,
}

/// 孵化提案：default Agent 的正式治理输出。
#[derive(Debug, Clone, Component)]
pub struct IncubationProposal {
    pub proposal_id: uuid::Uuid,
    pub source_agent_id: AgentId,
    pub source_task_id: TaskId,
    pub proposed_agent_profile: super::AgentProfile,
    pub knowledge_candidate_ids: Vec<uuid::Uuid>,
    pub executable_candidate_ids: Vec<uuid::Uuid>,
    pub shared_knowledge_candidate_ids: Vec<uuid::Uuid>,
    pub status: IncubationProposalStatus,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// 共享知识升级入口候选：已被顶层治理判定具备公共价值，但尚未成为最终共享知识正文。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SharedKnowledgeUpgradeCandidate {
    pub candidate_id: uuid::Uuid,
    pub content: String,
    pub kind: super::LongTermMemoryKind,
    pub scope_tags: Vec<String>,
    pub source_candidate_id: uuid::Uuid,
    pub source_agent_id: AgentId,
    pub source_task_id: TaskId,
    pub validation_status: super::KnowledgeValidationStatus,
    pub created_at: chrono::DateTime<chrono::Utc>,
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
            super::super::LongTermMemoryKind::Fact,
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
            ExperienceCandidateStatus::NeedsUserApproval,
            ExperienceCandidateStatus::Approved,
            ExperienceCandidateStatus::Rejected,
            ExperienceCandidateStatus::Persisted,
        ];
        assert_eq!(statuses.len(), 8);
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
            super::super::LongTermMemoryKind::Fact,
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
}
