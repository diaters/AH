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
    Queued,
    NeedsUserApproval,
    Approved,
    Rejected,
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
            } => Some(super::LongTermMemoryEntry::new(*memory_kind, content.clone())),
            ExperienceCandidatePayload::Executable { .. } => None,
        }
    }
}

/// 经验收件箱：父任务绑定的治理缓冲层。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExperienceInbox {
    pub owner_task_id: TaskId,
    pub owner_agent_id: AgentId,
    pub candidate_ids: Vec<uuid::Uuid>,
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
    /// 将候选投入父任务的收件箱。
    pub fn queue_for_parent(
        &mut self,
        parent_task_id: TaskId,
        parent_agent_id: AgentId,
        candidate: ExperienceCandidate,
    ) {
        let candidate_id = candidate.candidate_id;
        self.candidates.insert(candidate_id, candidate);
        self.inboxes
            .entry(parent_task_id)
            .or_insert_with(|| ExperienceInbox {
                owner_task_id: parent_task_id,
                owner_agent_id: parent_agent_id,
                candidate_ids: Vec::new(),
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
        self.root_candidates.get(&task_id).cloned().unwrap_or_default()
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

    /// 根据确认请求 ID 应用确认结果。
    pub fn apply_confirmation_response(
        &mut self,
        request_id: uuid::Uuid,
        selected_option: &str,
    ) {
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
    pub agent_id: AgentId,
    pub parent_task_id: Option<TaskId>,
    pub parent_agent_id: Option<AgentId>,
}

/// 经验治理请求消息。
#[derive(Debug, Clone, Component)]
pub struct ExperienceGovernanceRequestMessage {
    pub task_id: TaskId,
    pub agent_id: AgentId,
}

/// 孵化提案：default Agent 顶层任务结束后生成。
#[derive(Debug, Clone, Component)]
pub struct IncubationProposal {
    pub task_id: TaskId,
    pub agent_id: AgentId,
    pub candidate_ids: Vec<uuid::Uuid>,
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
            store.candidates.get(&candidate.candidate_id).unwrap().status,
            ExperienceCandidateStatus::Submitted,
        );
    }
}
