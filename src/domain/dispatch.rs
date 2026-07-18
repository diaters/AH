//! 派发相关数据结构
//!
//! 定义统一的派发请求标记 Component 和相关类型。
//! 所有派发请求通过 `PendingDispatch` Component 流转，
//! 由单一的 `dispatch_system` 扫描处理。

use crate::domain::{AgentId, TaskId, WorkItemType};
use crate::infrastructure::skills::SkillId;
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

/// 派发策略
#[derive(Debug, Clone)]
pub enum DispatchStrategy {
    /// 走 Brain LLM 选 Agent + skill（默认）
    BrainLlm,
    /// Brain 决策后或显式指定，直接委派
    DirectDelegate,
}

/// 派发提示
#[derive(Debug, Clone)]
pub struct DispatchHint {
    pub strategy: DispatchStrategy,
    /// 显式指定的 Agent 名称（DirectDelegate 时必填）
    pub preferred_agent_name: Option<String>,
    /// 需要注入的 skill ID（可选）
    pub required_skill_id: Option<SkillId>,
    /// 需要 spawn 新 Agent 时携带的规格
    pub agent_spawn_spec: Option<AgentSpawnSpec>,
}

/// Agent 生成规格
#[derive(Debug, Clone)]
pub struct AgentSpawnSpec {
    pub name: String,
    pub model: Option<String>,
    pub allowed_tools: Vec<String>,
    pub parent_agent_id: Option<AgentId>,
}

/// Brain LLM 决策等待状态。
///
/// 由 `dispatch_system` 在 BrainLlm 策略下附加，
/// 由 `brain_decision_system` 处理 Brain 输出后移除。
#[derive(Component)]
pub struct AwaitingBrainDecision {
    pub task_id: TaskId,
    pub spawn_spec: Option<AgentSpawnSpec>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::WorkItemType;

    #[test]
    fn pending_dispatch_task_kind_construction() {
        let hint = DispatchHint {
            strategy: DispatchStrategy::BrainLlm,
            preferred_agent_name: None,
            required_skill_id: None,
            agent_spawn_spec: None,
        };
        let pending = PendingDispatch {
            kind: DispatchKind::Task,
            hint,
        };
        assert!(matches!(pending.kind, DispatchKind::Task));
        assert!(matches!(pending.hint.strategy, DispatchStrategy::BrainLlm));
    }

    #[test]
    fn pending_dispatch_workitem_kind_construction() {
        let hint = DispatchHint {
            strategy: DispatchStrategy::DirectDelegate,
            preferred_agent_name: None,
            required_skill_id: None,
            agent_spawn_spec: None,
        };
        let pending = PendingDispatch {
            kind: DispatchKind::WorkItem(WorkItemType::SkillUpdate),
            hint,
        };
        assert!(matches!(
            pending.kind,
            DispatchKind::WorkItem(WorkItemType::SkillUpdate)
        ));
    }

    #[test]
    fn awaiting_brain_decision_carries_spawn_spec() {
        let spec = AgentSpawnSpec {
            name: "child-agent".to_string(),
            model: None,
            allowed_tools: vec![],
            parent_agent_id: None,
        };
        let awaiting = AwaitingBrainDecision {
            task_id: uuid::Uuid::nil(),
            spawn_spec: Some(spec),
        };
        assert!(awaiting.spawn_spec.is_some());
    }
}
