use std::str::FromStr;

use thiserror::Error;

/// 单点定义全部 hook 点：枚举变体、`FromStr` 解析、`as_serialized` 序列化名、
/// 测试用全名单一来源。新增 hook 点只需在下方清单加一行（算核心契约变更，
/// 需要设计评审）。
macro_rules! define_hook_points {
    ($($variant:ident => $name:literal),* $(,)?) => {
        /// v1 暴露的固定 hook 点清单。
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum HookPoint {
            $($variant,)*
        }

        impl FromStr for HookPoint {
            type Err = HookPointParseError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                match s {
                    $($name => Ok(Self::$variant),)*
                    other => Err(HookPointParseError::Unknown(other.to_string())),
                }
            }
        }

        impl HookPoint {
            /// 全部 hook 点的序列化名（按声明序），供测试与清单展示使用。
            pub const ALL_NAMES: &'static [&'static str] = &[$($name),*];

            /// 序列化后的字符串形式（用于 manifest 比对）。
            pub fn as_serialized(&self) -> &'static str {
                match self {
                    $(Self::$variant => $name,)*
                }
            }
        }
    };
}

define_hook_points! {
    // 前 hook（可拒绝）
    OnToolCalled => "on_tool_called",
    // 后 hook（观察 + 受控修改）
    OnTaskCreated => "on_task_created",
    OnTaskCompleted => "on_task_completed",
    OnTaskFailed => "on_task_failed",
    OnWorkItemStarted => "on_workitem_started",
    OnWorkItemCompleted => "on_workitem_completed",
    OnWorkItemFailed => "on_workitem_failed",
    OnAgentStarted => "on_agent_started",
    OnAgentStopped => "on_agent_stopped",
    OnToolReturned => "on_tool_returned",
    OnMessageDispatched => "on_message_dispatched",
    OnMessageReceived => "on_message_received",
    OnLlmResponse => "on_llm_response",
    OnLongTermMemoryWrite => "on_long_term_memory_write",
    OnLongTermMemoryEvicted => "on_long_term_memory_evicted",
    OnSharedKnowledgeWrite => "on_shared_knowledge_write",
    OnExperienceCandidateSubmitted => "on_experience_candidate_submitted",
    OnExperienceCandidateApproved => "on_experience_candidate_approved",
    OnExperienceCandidateRejected => "on_experience_candidate_rejected",
    OnApprovalRequested => "on_approval_requested",
    OnApprovalResolved => "on_approval_resolved",
    OnAgentProfileGenerated => "on_agent_profile_generated",
    OnAgentProfileUpdated => "on_agent_profile_updated",
    OnAgentIncubated => "on_agent_incubated",
    OnAgentProfileGenerationFailed => "on_agent_profile_generation_failed",
}

#[derive(Debug, Error)]
pub enum HookPointParseError {
    #[error("unknown hook point: {0}")]
    Unknown(String),
}

impl HookPoint {
    /// 此 hook 点是否为"前 hook"，允许拒绝或修改入参。
    pub fn is_pre(&self) -> bool {
        matches!(self, Self::OnToolCalled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_known_points() {
        for s in HookPoint::ALL_NAMES {
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
