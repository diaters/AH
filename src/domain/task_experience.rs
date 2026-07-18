use crate::infrastructure::skills::SkillId;
use crate::prelude::Component;

/// 标记 Task 注入的 skill（由 brain 派发时写入）
#[derive(Component, Debug, Clone, Default)]
pub struct TaskInjectedSkill {
    pub skill_id: Option<SkillId>,
}

/// 标记 Task 的经验治理过滤策略（仅 skill-updater 等特殊 Agent 需要）
#[derive(Component, Debug, Clone, Default)]
pub struct TaskExperiencePolicy {
    pub kind_filter: ExperienceKindFilter,
}

/// 经验类型过滤策略
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExperienceKindFilter {
    /// 允许所有类型（默认）
    #[default]
    All,
    /// 仅允许 knowledge 类（skill 候选被丢弃）
    KnowledgeOnly,
    /// 仅允许 skill 类（knowledge 候选被丢弃）
    SkillOnly,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_filter_is_all() {
        assert_eq!(ExperienceKindFilter::default(), ExperienceKindFilter::All);
    }

    #[test]
    fn task_injected_skill_default_is_none() {
        let t = TaskInjectedSkill::default();
        assert!(t.skill_id.is_none());
    }
}
