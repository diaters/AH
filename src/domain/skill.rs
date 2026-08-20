//! Skill 领域类型
//!
//! Skill 全局唯一标识，封装 `owner_agent_name + skill_name`。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SkillId {
    pub owner_agent_name: String,
    pub skill_name: String,
}

impl SkillId {
    pub fn new(owner_agent_name: impl Into<String>, skill_name: impl Into<String>) -> Self {
        Self {
            owner_agent_name: owner_agent_name.into(),
            skill_name: skill_name.into(),
        }
    }

    pub fn as_string(&self) -> String {
        format!("{}/{}", self.owner_agent_name, self.skill_name)
    }

    pub fn parse(s: &str) -> Option<Self> {
        let mut parts = s.splitn(2, '/');
        let owner_agent_name = parts.next()?.to_string();
        let skill_name = parts.next()?.to_string();
        if owner_agent_name.is_empty() || skill_name.is_empty() {
            return None;
        }
        Some(Self {
            owner_agent_name,
            skill_name,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_id_round_trip() {
        let id = SkillId::new("default-llm-agent", "coding");
        let s = id.as_string();
        assert_eq!(s, "default-llm-agent/coding");
        let parsed = SkillId::parse(&s).unwrap();
        assert_eq!(parsed, id);
    }

    #[test]
    fn skill_id_parse_rejects_invalid() {
        assert!(SkillId::parse("no-slash").is_none());
        assert!(SkillId::parse("/missing-owner").is_none());
        assert!(SkillId::parse("missing-name/").is_none());
        assert!(SkillId::parse("").is_none());
    }
}
