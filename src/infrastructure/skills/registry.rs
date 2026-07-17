use std::collections::HashMap;

use crate::prelude::Resource;

/// Skill 全局唯一标识，封装 `owner_agent_name + skill_name`
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

#[derive(Clone, Debug)]
pub struct SkillEntry {
    pub skill_id: SkillId,
    pub name: String,
    pub description: String,
    pub instructions: String,
    pub version: u32,
    pub owner_agent_name: String,
    pub self_updatable: bool,
}

#[derive(Resource, Default, Debug)]
pub struct SkillRegistry {
    pub skills: HashMap<SkillId, SkillEntry>,
}

impl SkillRegistry {
    pub fn get(&self, skill_id: &SkillId) -> &SkillEntry {
        &self.skills[skill_id]
    }

    pub fn list_by_owner(&self, owner_agent_name: &str) -> Vec<&SkillEntry> {
        self.skills
            .values()
            .filter(|e| e.owner_agent_name == owner_agent_name)
            .collect()
    }

    pub fn upsert(&mut self, entry: SkillEntry) {
        self.skills.insert(entry.skill_id.clone(), entry);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_entry(name: &str, owner: &str) -> SkillEntry {
        SkillEntry {
            skill_id: SkillId::new(owner, name),
            name: name.to_string(),
            description: format!("desc for {}", name),
            instructions: "instructions".to_string(),
            version: 1,
            owner_agent_name: owner.to_string(),
            self_updatable: true,
        }
    }

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

    #[test]
    fn registry_upsert_replaces() {
        let mut reg = SkillRegistry::default();
        let mut entry = sample_entry("coding", "agent-a");
        entry.version = 1;
        reg.upsert(entry.clone());
        entry.version = 2;
        reg.upsert(entry);
        assert_eq!(reg.get(&SkillId::new("agent-a", "coding")).version, 2);
    }

    #[test]
    fn registry_list_by_owner() {
        let mut reg = SkillRegistry::default();
        reg.upsert(sample_entry("a", "agent-a"));
        reg.upsert(sample_entry("b", "agent-a"));
        reg.upsert(sample_entry("c", "agent-b"));
        let owned = reg.list_by_owner("agent-a");
        assert_eq!(owned.len(), 2);
    }
}
