//! 派发相关数据结构
//!
//! 定义统一的派发请求标记 Component 和相关类型。
//! 所有派发请求通过 `PendingDispatch` Component 流转，
//! 由单一的 `dispatch_system` 扫描处理。

use crate::domain::{AgentId, WorkItemType};
use bevy_ecs::prelude::Component;

/// 派发请求标记 Component，附加在 Task 或 WorkItem Entity 上。
///
/// 由派发请求生成器（Task 创建入口 / WorkItem 创建器 / SubTask preparation system）
/// 附加，由 `dispatch_system` 消费后移除。
#[derive(Component)]
pub struct PendingDispatch {
    pub kind: DispatchKind,
    pub hint: DispatchHint,
}

/// 派发类型
#[derive(Debug, Clone)]
pub enum DispatchKind {
    /// 合并 TopLevelTask + SubTask
    Task,
    /// WorkItem 派发，按 work_type 分流
    WorkItem(WorkItemType),
}

/// 派发提示
#[derive(Debug, Clone, Default)]
pub struct DispatchHint {
    /// 指定的 Persistent Agent 名称；`None` 表示交给主 Agent（初次派发）。
    pub preferred_agent_name: Option<String>,
    /// 目标 Agent 不存在时 spawn 新 Agent 的规格
    pub agent_spawn_spec: Option<AgentSpawnSpec>,
}

impl DispatchHint {
    /// 交给主 Agent 的初次派发。
    pub fn main_agent() -> Self {
        Self::default()
    }

    /// 指定 Persistent Agent 承接。
    pub fn agent(name: impl Into<String>) -> Self {
        Self {
            preferred_agent_name: Some(name.into()),
            agent_spawn_spec: None,
        }
    }
}

/// Agent 生成规格
#[derive(Debug, Clone)]
pub struct AgentSpawnSpec {
    pub name: String,
    pub model: Option<String>,
    pub allowed_tools: Vec<String>,
    pub parent_agent_id: Option<AgentId>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn main_agent_hint_has_no_preference() {
        let hint = DispatchHint::main_agent();
        assert!(hint.preferred_agent_name.is_none());
        assert!(hint.agent_spawn_spec.is_none());
    }

    #[test]
    fn agent_hint_pins_name() {
        let pending = PendingDispatch {
            kind: DispatchKind::WorkItem(WorkItemType::Evaluation),
            hint: DispatchHint::agent("evaluator"),
        };
        assert_eq!(
            pending.hint.preferred_agent_name.as_deref(),
            Some("evaluator")
        );
        assert!(matches!(
            pending.kind,
            DispatchKind::WorkItem(WorkItemType::Evaluation)
        ));
    }
}
