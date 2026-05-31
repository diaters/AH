//! Planning 契约
//!
//! 定义规划相关的 trait 接口。

use crate::{
    contracts::dispatch::{AgentCapabilitySummary, TagSet},
    domain::{TaskId, WorkItemType},
};

/// 规划步骤
#[derive(Debug, Clone)]
pub struct PlanStep {
    pub name: String,
    pub description: String,
    pub estimated_complexity: Complexity,
}

/// 复杂度估计
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Complexity {
    Low,
    Medium,
    High,
}

/// 子任务规格
#[derive(Debug, Clone)]
pub struct SubtaskSpec {
    pub name: String,
    pub content: String,
    pub work_type: WorkItemType,
    pub tags: TagSet,
    pub required_tools: Vec<String>,
    pub depends_on: Vec<String>,
}

/// 规划结果
#[derive(Debug, Clone)]
pub struct PlanArtifact {
    pub steps: Vec<PlanStep>,
    pub subtasks: Vec<SubtaskSpec>,
    pub dependencies: Vec<(String, String)>, // (from, to)
}

/// 规划上下文
#[derive(Debug, Clone)]
pub struct PlanContext {
    pub task_id: TaskId,
    pub stm_entries: usize,
    pub available_agents: Vec<AgentCapabilitySummary>,
}

/// 重新规划事件
#[derive(Debug, Clone)]
pub enum ReplanEvent {
    SubtaskFailed {
        subtask_name: String,
        error: String,
    },
    SubtaskBlocked {
        subtask_name: String,
        reason: String,
    },
    ContextChanged {
        change: String,
    },
}

/// 规划错误
#[derive(Debug, thiserror::Error)]
pub enum PlanError {
    #[error("parse failed: {0}")]
    ParseFailed(String),
    #[error("invalid plan: {0}")]
    InvalidPlan(String),
}

/// 判断是否需要规划
pub trait PlanPolicy: Send + Sync + 'static {
    /// 判断任务是否需要规划
    fn should_plan(&self, task_content: &str, context: &PlanContext) -> bool;
}

/// 构建规划结果
pub trait PlanArtifactBuilder: Send + Sync + 'static {
    /// 从原始输出构建规划结果
    fn build(&self, raw_output: &str) -> Result<PlanArtifact, PlanError>;
}

/// 判断是否需要重新规划
pub trait ReplanPolicy: Send + Sync + 'static {
    /// 判断是否需要重新规划
    fn should_replan(&self, event: &ReplanEvent) -> bool;
}

/// 规划的工作项规格
#[derive(Debug, Clone)]
pub struct PlannedWorkItemSpec {
    pub name: String,
    pub work_type: WorkItemType,
    pub prompt: String,
    pub tags: TagSet,
    pub required_tools: Vec<String>,
    pub depends_on: Vec<String>,
}

/// 将规划结果转化为工作项草案
pub trait WorkItemDeriver: Send + Sync + 'static {
    /// 从规划结果派生工作项
    fn derive(&self, task_id: TaskId, artifact: &PlanArtifact) -> Vec<PlannedWorkItemSpec>;
}

/// 默认规划策略
///
/// 基于任务复杂度和长度判断是否需要规划。
#[derive(Debug, Clone, Copy)]
pub struct DefaultPlanPolicy {
    /// 触发规划的最低任务长度
    pub min_task_length: usize,
}

impl PlanPolicy for DefaultPlanPolicy {
    fn should_plan(&self, task_content: &str, _context: &PlanContext) -> bool {
        // 简单规则：任务长度超过阈值则认为需要规划
        task_content.len() > self.min_task_length
    }
}

impl Default for DefaultPlanPolicy {
    fn default() -> Self {
        Self {
            min_task_length: 200,
        }
    }
}

/// 默认重新规划策略
#[derive(Debug, Clone, Copy, Default)]
pub struct DefaultReplanPolicy;

impl ReplanPolicy for DefaultReplanPolicy {
    fn should_replan(&self, event: &ReplanEvent) -> bool {
        match event {
            ReplanEvent::SubtaskFailed { .. } => true,
            ReplanEvent::SubtaskBlocked { .. } => false, // 阻塞不一定需要重规划
            ReplanEvent::ContextChanged { .. } => true,
        }
    }
}

/// 默认工作项派生器
#[derive(Debug, Clone, Default)]
pub struct DefaultWorkItemDeriver;

impl WorkItemDeriver for DefaultWorkItemDeriver {
    fn derive(&self, _task_id: TaskId, artifact: &PlanArtifact) -> Vec<PlannedWorkItemSpec> {
        artifact
            .subtasks
            .iter()
            .map(|subtask| PlannedWorkItemSpec {
                name: subtask.name.clone(),
                work_type: subtask.work_type,
                prompt: subtask.content.clone(),
                tags: subtask.tags.clone(),
                required_tools: subtask.required_tools.clone(),
                depends_on: subtask.depends_on.clone(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_plan_policy_short_task() {
        let policy = DefaultPlanPolicy::default();
        let context = PlanContext {
            task_id: uuid::Uuid::nil(),
            stm_entries: 0,
            available_agents: vec![],
        };
        assert!(!policy.should_plan("short task", &context));
    }

    #[test]
    fn default_plan_policy_long_task() {
        let policy = DefaultPlanPolicy::default();
        let context = PlanContext {
            task_id: uuid::Uuid::nil(),
            stm_entries: 0,
            available_agents: vec![],
        };
        let long_task = "这是一个非常长的任务描述，包含了多个步骤和复杂的逻辑，需要进行详细的规划才能执行完成。".repeat(5);
        assert!(policy.should_plan(&long_task, &context));
    }

    #[test]
    fn default_replan_policy() {
        let policy = DefaultReplanPolicy;

        assert!(policy.should_replan(&ReplanEvent::SubtaskFailed {
            subtask_name: "step1".to_string(),
            error: "error".to_string(),
        }));

        assert!(!policy.should_replan(&ReplanEvent::SubtaskBlocked {
            subtask_name: "step1".to_string(),
            reason: "waiting".to_string(),
        }));
    }
}
