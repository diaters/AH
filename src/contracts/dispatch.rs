//! Dispatch 契约
//!
//! 定义任务派发相关的 trait 接口。

use crate::domain::{AgentId, Task, TaskId};

/// 标签集合
#[derive(Debug, Clone, Default)]
pub struct TagSet {
    pub tags: Vec<String>,
}

impl TagSet {
    pub fn new(tags: Vec<String>) -> Self {
        Self { tags }
    }

    pub fn empty() -> Self {
        Self { tags: Vec::new() }
    }

    pub fn from_tags<I: IntoIterator<Item = impl Into<String>>>(iter: I) -> Self {
        Self {
            tags: iter.into_iter().map(|s| s.into()).collect(),
        }
    }
}

/// Agent 的可见能力摘要
#[derive(Debug, Clone)]
pub struct AgentCapabilitySummary {
    pub agent_id: AgentId,
    pub name: String,
    pub tags: Vec<String>,
    pub model: String,
}

impl AgentCapabilitySummary {
    pub fn new(agent_id: AgentId, name: String, tags: Vec<String>, model: String) -> Self {
        Self {
            agent_id,
            name,
            tags,
            model,
        }
    }
}

/// 派发上下文
#[derive(Debug, Clone)]
pub struct DispatchContext {
    pub task_id: TaskId,
    pub available_agents: Vec<AgentCapabilitySummary>,
}

impl DispatchContext {
    pub fn new(task_id: TaskId, available_agents: Vec<AgentCapabilitySummary>) -> Self {
        Self {
            task_id,
            available_agents,
        }
    }
}

/// 分配结果
#[derive(Debug, Clone)]
pub struct AssignmentResult {
    pub agent_id: AgentId,
    pub reasoning: String,
}

impl AssignmentResult {
    pub fn new(agent_id: AgentId, reasoning: String) -> Self {
        Self { agent_id, reasoning }
    }
}

/// 标签匹配器
///
/// 定义 Agent 标签与所需标签的匹配规则。
pub trait TagMatcher: Send + Sync + 'static {
    /// 检查 Agent 的标签是否满足所需的标签集合
    fn matches(&self, agent_tags: &[String], required_tags: &TagSet) -> bool;
}

/// 候选 Agent 选择器
///
/// 从可用 Agent 中筛选出符合条件的候选列表。
pub trait AgentSelector: Send + Sync + 'static {
    /// 从可用 Agent 中选择符合条件的候选
    fn select_candidates(
        &self,
        task: &Task,
        agents: &[AgentCapabilitySummary],
    ) -> Vec<AgentCapabilitySummary>;
}

/// 派发策略
///
/// 决定将任务分配给哪个 Agent。
pub trait DispatchPolicy: Send + Sync + 'static {
    /// 将任务分配给合适的 Agent
    fn assign(
        &self,
        task: &Task,
        context: &DispatchContext,
    ) -> Option<AssignmentResult>;
}

/// 默认标签匹配器：Agent tags 必须全部包含 required tags
#[derive(Debug, Clone, Copy, Default)]
pub struct AllMatchTagMatcher;

impl TagMatcher for AllMatchTagMatcher {
    fn matches(&self, agent_tags: &[String], required_tags: &TagSet) -> bool {
        required_tags
            .tags
            .iter()
            .all(|required| agent_tags.contains(required))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_set_from_tags() {
        let tags = TagSet::from_tags(["brain", "planning"]);
        assert_eq!(tags.tags, vec!["brain", "planning"]);
    }

    #[test]
    fn all_match_tag_matcher_positive() {
        let matcher = AllMatchTagMatcher;
        let agent_tags = vec!["brain".to_string(), "planning".to_string()];
        let required = TagSet::from_tags(["brain"]);
        assert!(matcher.matches(&agent_tags, &required));
    }

    #[test]
    fn all_match_tag_matcher_negative() {
        let matcher = AllMatchTagMatcher;
        let agent_tags = vec!["brain".to_string()];
        let required = TagSet::from_tags(["brain", "planning"]);
        assert!(!matcher.matches(&agent_tags, &required));
    }

    #[test]
    fn all_match_tag_matcher_empty_required() {
        let matcher = AllMatchTagMatcher;
        let agent_tags = vec!["brain".to_string()];
        let required = TagSet::empty();
        assert!(matcher.matches(&agent_tags, &required));
    }
}
