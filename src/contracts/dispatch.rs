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

/// 基于 Tag 的 Agent 选择策略
///
/// 从带指定标签的 Agent 中选择第一个（配置中最前的）。
pub trait TagBasedSelector: Send + Sync + 'static {
    /// 从符合条件的 Agent 中选择一个
    fn select_by_tag(&self, agents: &[AgentCapabilitySummary], tag: &str) -> Option<AgentId>;
}

/// 默认实现：选择配置中最前的 Agent
#[derive(Debug, Clone, Copy, Default)]
pub struct FirstByTagPolicy;

impl TagBasedSelector for FirstByTagPolicy {
    fn select_by_tag(&self, agents: &[AgentCapabilitySummary], tag: &str) -> Option<AgentId> {
        agents
            .iter()
            .find(|a| a.has_tag(tag))
            .map(|a| a.agent_id)
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

/// Summarizer 选择策略
///
/// 定义如何选择 Summarizer Agent。
pub trait SummarizerSelectionPolicy: Send + Sync + 'static {
    /// 从可用 Agent 中选择 Summarizer
    fn select_summarizer(&self, agents: &[AgentCapabilitySummary]) -> Option<AgentId>;
}

/// 默认 Summarizer 选择策略：选择配置中最前的带 "summarization" 标签的 Agent
#[derive(Debug, Clone, Copy, Default)]
pub struct FirstSummarizerPolicy;

impl SummarizerSelectionPolicy for FirstSummarizerPolicy {
    fn select_summarizer(&self, agents: &[AgentCapabilitySummary]) -> Option<AgentId> {
        agents
            .iter()
            .find(|a| a.has_tag("summarization"))
            .map(|a| a.agent_id)
    }
}

/// 默认派发策略
///
/// 基于任务内容与 Agent 标签的匹配分数选择 Agent。
/// 支持以下规则：
/// 1. 排除 brain 标签的 Agent（由 BrainDispatch 专门处理）
/// 2. 计算任务内容与 Agent 标签的匹配分数
/// 3. 选择分数最高的 Agent
/// 4. 如果所有分数为 0，选择带有 "default" 标签的 Agent 作为 fallback
#[derive(Debug, Clone, Default)]
pub struct DefaultDispatchPolicy {
    tag_matcher: AllMatchTagMatcher,
}

impl DefaultDispatchPolicy {
    pub fn new() -> Self {
        Self {
            tag_matcher: AllMatchTagMatcher,
        }
    }

    /// 计算任务内容与 Agent 标签的匹配分数
    fn match_score(&self, agent: &AgentCapabilitySummary, task_content: &str) -> usize {
        let lower = task_content.to_lowercase();
        agent
            .tags
            .iter()
            .filter(|tag| lower.contains(&tag.to_lowercase()))
            .count()
    }
}

impl DispatchPolicy for DefaultDispatchPolicy {
    fn assign(
        &self,
        task: &Task,
        context: &DispatchContext,
    ) -> Option<AssignmentResult> {
        // 过滤出可用候选（排除 brain Agent）
        let candidates: Vec<_> = context
            .available_agents
            .iter()
            .filter(|a| !a.has_tag("brain"))
            .collect();

        if candidates.is_empty() {
            return None;
        }

        // 计算匹配分数
        let max_score = candidates
            .iter()
            .map(|a| self.match_score(a, &task.content))
            .max()
            .unwrap_or(0);

        // 选择 Agent
        let selected = if max_score > 0 {
            // 有正向匹配：选最高分，同分时优先 "default" tag
            candidates
                .iter()
                .filter(|a| self.match_score(a, &task.content) == max_score)
                .max_by_key(|a| a.has_tag("default") as usize)
        } else {
            // 全部评分为 0：fallback 到带 "default" tag 的 agent
            candidates
                .iter()
                .filter(|a| a.has_tag("default"))
                .max_by_key(|a| a.tags.len())
        };

        match selected {
            Some(agent) => Some(AssignmentResult::new(
                agent.agent_id,
                format!(
                    "Selected {} with score {}",
                    agent.name,
                    if max_score > 0 { max_score } else { 0 }
                ),
            )),
            None => {
                // 无 "default" tag 的 fallback：选第一个候选
                candidates.first().map(|agent| {
                    AssignmentResult::new(
                        agent.agent_id,
                        format!("Selected {} as fallback (no default found)", agent.name),
                    )
                })
            }
        }
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
