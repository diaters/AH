//! Agent 选择辅助函数
//!
//! 提供 Agent 匹配和评分功能。

use tracing::debug;

use crate::domain::{Agent, AgentKind, LongTermMemory};
use crate::infrastructure::skills::{SkillId, SkillRegistry};

/// 计算 Agent 与任务内容的匹配分数
pub fn match_score(agent: &Agent, task_content: &str) -> usize {
    let lower = task_content.to_lowercase();
    agent
        .capabilities
        .tags
        .iter()
        .filter(|tag| lower.contains(&tag.to_lowercase()))
        .count()
}

/// 选择最适合任务的 Agent
///
/// 评分逻辑与 `select_agent_for_sub_task` 保持一致：
/// 1. 按 task content 与 agent tags 的匹配度评分；
/// 2. 所有评分为 0 时，fallback 到带 "default" tag 的 agent；
/// 3. 同分或均无匹配时，优先 tag 数量更多的 agent；若仍平局，则保留后出现的最大元素（`max_by_key` 行为）。
pub fn select_agent_with_memory<'a>(
    agents: impl Iterator<Item = (&'a Agent, Option<&'a LongTermMemory>)>,
    task_content: &str,
) -> Option<(&'a Agent, Option<&'a LongTermMemory>)> {
    let candidates: Vec<_> = agents
        .filter(|(a, _)| a.kind == AgentKind::Persistent)
        .filter(|(a, _)| !a.capabilities.tags.contains(&"brain".to_string()))
        .collect();

    if candidates.is_empty() {
        return None;
    }

    let max_score = candidates
        .iter()
        .map(|(a, _)| match_score(a, task_content))
        .max()
        .unwrap_or(0);

    let selected = if max_score > 0 {
        // 有正向匹配：选最高分，同分时优先 "default" tag，再按 tag 数量 tie-break
        candidates
            .iter()
            .filter(|(a, _)| match_score(a, task_content) == max_score)
            .max_by_key(|(a, _)| {
                (
                    a.capabilities.tags.contains(&"default".to_string()) as usize,
                    a.capabilities.tags.len(),
                )
            })
    } else {
        // 全部评分为 0：fallback 到带 "default" tag 的 agent
        candidates
            .iter()
            .filter(|(a, _)| a.capabilities.tags.contains(&"default".to_string()))
            .max_by_key(|(a, _)| a.capabilities.tags.len())
    };

    if let Some((agent, ltm)) = selected {
        let score = match_score(agent, task_content);
        let all_scores: Vec<_> = candidates
            .iter()
            .map(|(a, _)| (a.profile.name.clone(), match_score(a, task_content)))
            .collect();
        debug!(
            event = "AgentScoring",
            selected_agent = %agent.profile.name,
            selected_score = score,
            all_candidates_scores = ?all_scores,
            task_content_preview = %task_content.chars().take(100).collect::<String>(),
            fallback = (max_score == 0),
            "agent scoring completed"
        );
        Some((*agent, *ltm))
    } else {
        // 无 "default" tag 的 fallback：选第一个候选
        let (agent, ltm) = candidates.into_iter().next()?;
        debug!(
            event = "AgentScoring",
            selected_agent = %agent.profile.name,
            selected_score = 0,
            fallback = true,
            "agent selected as last resort (no default tag found)"
        );
        Some((agent, ltm))
    }
}

/// 为子任务选择 Agent：基于 task content 与 agent tags 匹配评分，
/// 所有评分为 0 时优先选择带 "default" tag 的 agent 作为 fallback
pub fn select_agent_for_sub_task<'a>(
    agents: impl Iterator<Item = (&'a Agent, Option<&'a LongTermMemory>)>,
    task_content: &str,
) -> Option<(&'a Agent, Option<&'a LongTermMemory>)> {
    let candidates: Vec<_> = agents
        .filter(|(a, _)| a.kind == AgentKind::Persistent)
        .filter(|(a, _)| !a.capabilities.tags.contains(&"brain".to_string()))
        .collect();

    if candidates.is_empty() {
        return None;
    }

    let max_score = candidates
        .iter()
        .map(|(a, _)| match_score(a, task_content))
        .max()
        .unwrap_or(0);

    let selected = if max_score > 0 {
        // 有正向匹配：选最高分，同分时优先 "default" tag，再按 tag 数量 tie-break
        candidates
            .iter()
            .filter(|(a, _)| match_score(a, task_content) == max_score)
            .max_by_key(|(a, _)| {
                (
                    a.capabilities.tags.contains(&"default".to_string()) as usize,
                    a.capabilities.tags.len(),
                )
            })
    } else {
        // 全部评分为 0：fallback 到带 "default" tag 的 agent
        candidates
            .iter()
            .filter(|(a, _)| a.capabilities.tags.contains(&"default".to_string()))
            .max_by_key(|(a, _)| a.capabilities.tags.len())
    };

    if let Some((agent, ltm)) = selected {
        let score = match_score(agent, task_content);
        let all_scores: Vec<_> = candidates
            .iter()
            .map(|(a, _)| (a.profile.name.clone(), match_score(a, task_content)))
            .collect();
        debug!(
            event = "SubTaskAgentScoring",
            selected_agent = %agent.profile.name,
            selected_score = score,
            all_candidates_scores = ?all_scores,
            task_content_preview = %task_content.chars().take(100).collect::<String>(),
            fallback = (max_score == 0),
            "sub-task agent scoring completed"
        );
        Some((*agent, *ltm))
    } else {
        // 无 "default" tag 的 fallback：选第一个候选
        let (agent, ltm) = candidates.into_iter().next()?;
        debug!(
            event = "SubTaskAgentScoring",
            selected_agent = %agent.profile.name,
            selected_score = 0,
            fallback = true,
            "sub-task agent selected as last resort (no default tag found)"
        );
        Some((agent, ltm))
    }
}

/// 为子任务选择 Agent 并附带可能的 skill
///
/// 复用 `select_agent_for_sub_task` 选出 agent，然后从 `skill_registry` 列出该 agent 拥有的 skills。
/// 当前实现采用简单启发式占位（取第一个 skill），未来由 brain_dispatch 系统替换为真实 LLM 调用，
/// 让 LLM 基于 task_content 和 skill descriptions 选择最合适的 skill。
#[allow(dead_code)] // 后续 brain_dispatch 改造任务接入
pub fn select_agent_for_sub_task_with_skill<'a>(
    agents: impl Iterator<Item = (&'a Agent, Option<&'a LongTermMemory>)>,
    task_content: &str,
    skill_registry: &SkillRegistry,
) -> Option<(&'a Agent, Option<&'a LongTermMemory>, Option<SkillId>)> {
    // 复用现有 select_agent_for_sub_task 的候选过滤逻辑，确保两次 filter 幂等
    let candidates: Vec<_> = agents
        .filter(|(a, _)| a.kind == AgentKind::Persistent)
        .filter(|(a, _)| !a.capabilities.tags.contains(&"brain".to_string()))
        .collect();
    if candidates.is_empty() {
        return None;
    }
    let selected = select_agent_for_sub_task(candidates.into_iter(), task_content)?;
    // 对选中的 agent，从 skill_registry 列出其 skills
    let owner_skills = skill_registry.list_by_owner(&selected.0.profile.name);
    if owner_skills.is_empty() {
        return Some((selected.0, selected.1, None));
    }
    // TODO(skill-selection): 替换为真实 LLM 调用，让 LLM 基于 task_content 和 skill descriptions 选择最合适的 skill
    // 当前启发式：取第一个 skill 作为占位
    let skill_id = owner_skills.first().map(|e| e.skill_id.clone());
    Some((selected.0, selected.1, skill_id))
}

#[cfg(test)]
mod tests {
    use crate::domain::{
        Agent, AgentCapabilities, AgentKind, AgentProfile, AgentToolPermissions, LongTermMemory,
    };
    use uuid::Uuid;

    use super::{
        select_agent_for_sub_task, select_agent_for_sub_task_with_skill, select_agent_with_memory,
    };

    fn make_agent(name: &str, tags: Vec<&str>) -> Agent {
        Agent {
            id: Uuid::new_v4(),
            profile: AgentProfile {
                name: name.to_string(),
                model: "gpt-4.1-mini".to_string(),
            },
            capabilities: AgentCapabilities {
                tags: tags.into_iter().map(|t| t.to_string()).collect(),
                description: String::new(),
            },
            kind: AgentKind::Persistent,
            parent_id: None,
            bound_task_id: None,
            tool_permissions: AgentToolPermissions::default(),
            system_prompt: None,
        }
    }

    #[test]
    fn sub_task_prefers_default_on_no_tag_match() {
        let default_agent = make_agent("default-llm-agent", vec!["llm", "default", "general"]);
        let summarizer = make_agent("summarizer", vec!["summarization", "memory"]);

        let agents = vec![
            (&default_agent, None as Option<&LongTermMemory>),
            (&summarizer, None as Option<&LongTermMemory>),
        ];

        let selected = select_agent_for_sub_task(agents.into_iter(), "请计算兔子的繁衍数量");
        assert!(selected.is_some());
        let (agent, _) = selected.unwrap();
        assert_eq!(agent.profile.name, "default-llm-agent");
    }

    #[test]
    fn sub_task_prefers_higher_tag_score() {
        let default_agent = make_agent("default-llm-agent", vec!["llm", "default", "general"]);
        let summarizer = make_agent("summarizer", vec!["summarization", "memory"]);

        let agents = vec![
            (&default_agent, None as Option<&LongTermMemory>),
            (&summarizer, None as Option<&LongTermMemory>),
        ];

        let selected =
            select_agent_for_sub_task(agents.into_iter(), "Please perform summarization");
        assert!(selected.is_some());
        let (agent, _) = selected.unwrap();
        assert_eq!(agent.profile.name, "summarizer");
    }

    #[test]
    fn sub_task_default_tag_ties_break() {
        let agent_a = make_agent("agent-a", vec!["default"]);
        let agent_b = make_agent("agent-b", vec!["llm", "default", "general"]);

        let agents = vec![
            (&agent_a, None as Option<&LongTermMemory>),
            (&agent_b, None as Option<&LongTermMemory>),
        ];

        let selected = select_agent_for_sub_task(agents.into_iter(), "无关键词匹配");
        assert!(selected.is_some());
        let (agent, _) = selected.unwrap();
        // Both have "default" tag, agent_b has more tags
        assert_eq!(agent.profile.name, "agent-b");
    }

    #[test]
    fn with_memory_falls_back_to_default_on_no_match() {
        let default_agent = make_agent("default-llm-agent", vec!["llm", "default", "general"]);
        let summarizer = make_agent("summarizer", vec!["summarization", "memory"]);

        let agents = vec![
            (&default_agent, None as Option<&LongTermMemory>),
            (&summarizer, None as Option<&LongTermMemory>),
        ];

        let selected = select_agent_with_memory(agents.into_iter(), "请计算兔子的繁衍数量");
        assert!(selected.is_some());
        let (agent, _) = selected.unwrap();
        assert_eq!(agent.profile.name, "default-llm-agent");
    }

    #[test]
    fn with_memory_uses_default_and_tag_count_tie_break() {
        let agent_a = make_agent("agent-a", vec!["summarization", "default"]);
        let agent_b = make_agent("agent-b", vec!["summarization", "default", "memory"]);

        let agents = vec![
            (&agent_a, None as Option<&LongTermMemory>),
            (&agent_b, None as Option<&LongTermMemory>),
        ];

        let selected = select_agent_with_memory(agents.into_iter(), "Please perform summarization");
        assert!(selected.is_some());
        let (agent, _) = selected.unwrap();
        // Same score, both have "default" tag, agent_b has more tags
        assert_eq!(agent.profile.name, "agent-b");
    }

    #[test]
    fn with_memory_selects_first_candidate_when_no_default() {
        let agent_a = make_agent("agent-a", vec!["llm", "general"]);
        let agent_b = make_agent("agent-b", vec!["summarization", "memory"]);

        let agents = vec![
            (&agent_a, None as Option<&LongTermMemory>),
            (&agent_b, None as Option<&LongTermMemory>),
        ];

        let selected = select_agent_with_memory(agents.into_iter(), "无关键词匹配");
        assert!(selected.is_some());
        let (agent, _) = selected.unwrap();
        assert_eq!(agent.profile.name, "agent-a");
    }

    mod skill_selection_tests {
        use super::*;
        use crate::infrastructure::skills::{SkillEntry, SkillId, SkillRegistry};

        fn make_skill_registry(agent: &Agent, skill_name: &str) -> SkillRegistry {
            let mut reg = SkillRegistry::default();
            reg.upsert(SkillEntry {
                skill_id: SkillId::new(&agent.profile.name, skill_name),
                name: skill_name.to_string(),
                description: format!("desc for {}", skill_name),
                instructions: "instructions".to_string(),
                version: 1,
                owner_agent_name: agent.profile.name.clone(),
                self_updatable: true,
            });
            reg
        }

        #[test]
        fn select_agent_with_skill_returns_skill_id() {
            let agent = make_agent("agent-a", vec!["default"]);
            let reg = make_skill_registry(&agent, "coding");
            let agents = vec![(&agent, None as Option<&LongTermMemory>)];
            let result =
                select_agent_for_sub_task_with_skill(agents.into_iter(), "需要写代码的任务", &reg);
            assert!(result.is_some());
            let (_, _, skill) = result.unwrap();
            assert!(skill.is_some());
            assert_eq!(skill.unwrap(), SkillId::new("agent-a", "coding"));
        }

        #[test]
        fn select_agent_without_skills_returns_none_skill() {
            let agent = make_agent("agent-b", vec!["default"]);
            let reg = SkillRegistry::default();
            let agents = vec![(&agent, None as Option<&LongTermMemory>)];
            let result = select_agent_for_sub_task_with_skill(agents.into_iter(), "任意任务", &reg);
            assert!(result.is_some());
            let (_, _, skill) = result.unwrap();
            assert!(skill.is_none());
        }
    }
}
