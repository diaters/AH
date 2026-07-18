//! Dispatch 契约
//!
//! 定义任务派发相关的 trait 接口。

use crate::domain::AgentId;

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

    /// 检查是否为空
    pub fn is_empty(&self) -> bool {
        self.tags.is_empty()
    }

    /// 检查是否包含指定标签
    pub fn contains(&self, tag: &str) -> bool {
        self.tags.contains(&tag.to_string())
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

    /// 从 Agent 创建摘要
    pub fn from_agent(agent: &crate::domain::Agent) -> Self {
        Self {
            agent_id: agent.id,
            name: agent.profile.name.clone(),
            tags: agent.capabilities.tags.clone(),
            model: agent.profile.model.clone(),
        }
    }

    /// 检查是否包含所有指定标签
    pub fn has_all_tags(&self, required: &TagSet) -> bool {
        required.tags.iter().all(|t| self.tags.contains(t))
    }

    /// 检查是否包含指定标签
    pub fn has_tag(&self, tag: &str) -> bool {
        self.tags.contains(&tag.to_string())
    }
}

/// Brain 选择策略
///
/// 定义如何从多个带 "brain" 标签的 Agent 中选择一个。
pub trait BrainSelectionPolicy: Send + Sync + 'static {
    /// 从带 brain 标签的 Agent 中选择一个
    fn select_brain(&self, brain_agents: &[AgentCapabilitySummary]) -> Option<AgentId>;
}

/// 默认 Brain 选择策略：选择配置中最前的 Brain Agent
///
/// 按列表顺序选择第一个带 "brain" 标签的 Agent。
#[derive(Debug, Clone, Copy, Default)]
pub struct FirstBrainPolicy;

impl BrainSelectionPolicy for FirstBrainPolicy {
    fn select_brain(&self, brain_agents: &[AgentCapabilitySummary]) -> Option<AgentId> {
        brain_agents.first().map(|a| a.agent_id)
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
}
