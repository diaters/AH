use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// v1 暴露的固定 hook 点清单。
///
/// 新增 hook 点算核心契约变更，需要设计评审。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookPoint {
    // 前 hook（可拒绝）
    OnToolCalled,
    // 后 hook（观察 + 受控修改）
    OnTaskCreated,
    OnTaskCompleted,
    OnTaskFailed,
    OnWorkItemStarted,
    OnWorkItemCompleted,
    OnWorkItemFailed,
    OnAgentStarted,
    OnAgentStopped,
    OnToolReturned,
    OnMessageDispatched,
    OnMessageReceived,
    OnLlmResponse,
    OnLongTermMemoryWrite,
    OnLongTermMemoryEvicted,
    OnSharedKnowledgeWrite,
    OnExperienceCandidateSubmitted,
    OnExperienceCandidateApproved,
    OnExperienceCandidateRejected,
    OnApprovalRequested,
    OnApprovalResolved,
    OnAgentProfileGenerated,
    OnAgentProfileUpdated,
    OnAgentIncubated,
    OnAgentProfileGenerationFailed,
}

#[derive(Debug, Error)]
pub enum HookPointParseError {
    #[error("unknown hook point: {0}")]
    Unknown(String),
}

impl FromStr for HookPoint {
    type Err = HookPointParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "on_tool_called" => Ok(Self::OnToolCalled),
            "on_task_created" => Ok(Self::OnTaskCreated),
            "on_task_completed" => Ok(Self::OnTaskCompleted),
            "on_task_failed" => Ok(Self::OnTaskFailed),
            "on_workitem_started" => Ok(Self::OnWorkItemStarted),
            "on_workitem_completed" => Ok(Self::OnWorkItemCompleted),
            "on_workitem_failed" => Ok(Self::OnWorkItemFailed),
            "on_agent_started" => Ok(Self::OnAgentStarted),
            "on_agent_stopped" => Ok(Self::OnAgentStopped),
            "on_tool_returned" => Ok(Self::OnToolReturned),
            "on_message_dispatched" => Ok(Self::OnMessageDispatched),
            "on_message_received" => Ok(Self::OnMessageReceived),
            "on_llm_response" => Ok(Self::OnLlmResponse),
            "on_long_term_memory_write" => Ok(Self::OnLongTermMemoryWrite),
            "on_long_term_memory_evicted" => Ok(Self::OnLongTermMemoryEvicted),
            "on_shared_knowledge_write" => Ok(Self::OnSharedKnowledgeWrite),
            "on_experience_candidate_submitted" => Ok(Self::OnExperienceCandidateSubmitted),
            "on_experience_candidate_approved" => Ok(Self::OnExperienceCandidateApproved),
            "on_experience_candidate_rejected" => Ok(Self::OnExperienceCandidateRejected),
            "on_approval_requested" => Ok(Self::OnApprovalRequested),
            "on_approval_resolved" => Ok(Self::OnApprovalResolved),
            "on_agent_profile_generated" => Ok(Self::OnAgentProfileGenerated),
            "on_agent_profile_updated" => Ok(Self::OnAgentProfileUpdated),
            "on_agent_incubated" => Ok(Self::OnAgentIncubated),
            "on_agent_profile_generation_failed" => Ok(Self::OnAgentProfileGenerationFailed),
            other => Err(HookPointParseError::Unknown(other.to_string())),
        }
    }
}

impl HookPoint {
    /// 此 hook 点是否为"前 hook"，允许拒绝或修改入参。
    pub fn is_pre(&self) -> bool {
        matches!(self, Self::OnToolCalled)
    }

    /// 序列化后的字符串形式（用于 manifest 比对）。
    pub fn as_serialized(&self) -> &'static str {
        match self {
            Self::OnToolCalled => "on_tool_called",
            Self::OnTaskCreated => "on_task_created",
            Self::OnTaskCompleted => "on_task_completed",
            Self::OnTaskFailed => "on_task_failed",
            Self::OnWorkItemStarted => "on_workitem_started",
            Self::OnWorkItemCompleted => "on_workitem_completed",
            Self::OnWorkItemFailed => "on_workitem_failed",
            Self::OnAgentStarted => "on_agent_started",
            Self::OnAgentStopped => "on_agent_stopped",
            Self::OnToolReturned => "on_tool_returned",
            Self::OnMessageDispatched => "on_message_dispatched",
            Self::OnMessageReceived => "on_message_received",
            Self::OnLlmResponse => "on_llm_response",
            Self::OnLongTermMemoryWrite => "on_long_term_memory_write",
            Self::OnLongTermMemoryEvicted => "on_long_term_memory_evicted",
            Self::OnSharedKnowledgeWrite => "on_shared_knowledge_write",
            Self::OnExperienceCandidateSubmitted => "on_experience_candidate_submitted",
            Self::OnExperienceCandidateApproved => "on_experience_candidate_approved",
            Self::OnExperienceCandidateRejected => "on_experience_candidate_rejected",
            Self::OnApprovalRequested => "on_approval_requested",
            Self::OnApprovalResolved => "on_approval_resolved",
            Self::OnAgentProfileGenerated => "on_agent_profile_generated",
            Self::OnAgentProfileUpdated => "on_agent_profile_updated",
            Self::OnAgentIncubated => "on_agent_incubated",
            Self::OnAgentProfileGenerationFailed => "on_agent_profile_generation_failed",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_known_points() {
        for s in [
            "on_tool_called",
            "on_task_created",
            "on_task_completed",
            "on_task_failed",
            "on_workitem_started",
            "on_workitem_completed",
            "on_workitem_failed",
            "on_agent_started",
            "on_agent_stopped",
            "on_tool_returned",
            "on_message_dispatched",
            "on_message_received",
            "on_llm_response",
            "on_long_term_memory_write",
            "on_long_term_memory_evicted",
            "on_shared_knowledge_write",
            "on_experience_candidate_submitted",
            "on_experience_candidate_approved",
            "on_experience_candidate_rejected",
            "on_approval_requested",
            "on_approval_resolved",
            "on_agent_profile_generated",
            "on_agent_profile_updated",
            "on_agent_incubated",
            "on_agent_profile_generation_failed",
        ] {
            assert!(HookPoint::from_str(s).is_ok(), "failed to parse {s}");
        }
    }

    #[test]
    fn rejects_unknown_point() {
        assert!(HookPoint::from_str("on_unknown_thing").is_err());
    }

    #[test]
    fn only_on_tool_called_is_pre() {
        assert!(HookPoint::OnToolCalled.is_pre());
        assert!(!HookPoint::OnTaskCreated.is_pre());
    }
}
