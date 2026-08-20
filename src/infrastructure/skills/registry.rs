use std::collections::HashMap;

use crate::domain::SkillId;
use crate::prelude::Resource;

/// Skill 元数据：名称、描述、指令、版本与归属 Agent
#[derive(Clone, Debug)]
pub struct SkillEntry {
    pub skill_id: SkillId,
    pub name: String,
    pub description: String,
    pub instructions: String,
    pub version: u32,
    pub owner_agent_name: String,
    pub self_updatable: bool,
    /// 同 agent 名下依赖的 skill 名列表（缺省为空 Vec）。
    pub dependencies: Vec<String>,
}

#[derive(Resource, Default, Debug)]
pub struct SkillRegistry {
    pub skills: HashMap<SkillId, SkillEntry>,
}

impl SkillRegistry {
    pub fn get(&self, skill_id: &SkillId) -> Option<&SkillEntry> {
        self.skills.get(skill_id)
    }

    pub fn list_by_owner(&self, owner_agent_name: &str) -> Vec<&SkillEntry> {
        self.skills
            .values()
            .filter(|e| e.owner_agent_name == owner_agent_name)
            .collect()
    }

    pub fn upsert(&mut self, entry: SkillEntry) {
        debug_assert_eq!(
            entry.skill_id.owner_agent_name, entry.owner_agent_name,
            "SkillEntry.owner_agent_name must match SkillEntry.skill_id.owner_agent_name"
        );
        self.skills.insert(entry.skill_id.clone(), entry);
    }

    /// 刷新单个 skill entry（skill-updater 写入后调用）
    pub fn refresh(&mut self, entry: SkillEntry) {
        debug_assert_eq!(
            entry.skill_id.owner_agent_name, entry.owner_agent_name,
            "SkillEntry.owner_agent_name must match SkillEntry.skill_id.owner_agent_name"
        );
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
            dependencies: Vec::new(),
        }
    }

    #[test]
    fn registry_upsert_replaces() {
        let mut reg = SkillRegistry::default();
        let mut entry = sample_entry("coding", "agent-a");
        reg.upsert(entry.clone());
        entry.version = 2;
        reg.upsert(entry);
        assert_eq!(
            reg.get(&SkillId::new("agent-a", "coding"))
                .expect("skill should exist")
                .version,
            2
        );
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

#[cfg(test)]
mod refresh_tests {
    use super::*;

    #[test]
    fn refresh_replaces_entry() {
        let mut reg = SkillRegistry::default();
        let mut entry = SkillEntry {
            skill_id: SkillId::new("agent", "skill"),
            name: "skill".to_string(),
            description: "old".to_string(),
            instructions: "old".to_string(),
            version: 1,
            owner_agent_name: "agent".to_string(),
            self_updatable: true,
            dependencies: Vec::new(),
        };
        reg.upsert(entry.clone());
        entry.version = 2;
        entry.instructions = "new".to_string();
        reg.refresh(entry);
        let got = reg
            .get(&SkillId::new("agent", "skill"))
            .expect("skill should exist");
        assert_eq!(got.version, 2);
        assert_eq!(got.instructions, "new");
    }

    #[test]
    fn refresh_inserts_if_missing() {
        let mut reg = SkillRegistry::default();
        let entry = SkillEntry {
            skill_id: SkillId::new("agent", "new-skill"),
            name: "new-skill".to_string(),
            description: "d".to_string(),
            instructions: "i".to_string(),
            version: 1,
            owner_agent_name: "agent".to_string(),
            self_updatable: true,
            dependencies: Vec::new(),
        };
        reg.refresh(entry);
        assert_eq!(reg.skills.len(), 1);
    }
}
