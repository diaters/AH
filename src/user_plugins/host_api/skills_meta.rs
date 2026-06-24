use std::sync::Arc;

use rhai::{Dynamic, Engine, Map};

/// Skill 元数据的快照。dispatcher 在派发前从 SkillLoader 拷贝。
///
/// v1 实现暂不填充：SkillLoader 当前只提供按 agent 加载 SKILL.md 的接口，
/// 没有 global iter_skills。真实填充逻辑在 Task 37（SkillLoader 合并插件贡献）
/// 中补上。
#[derive(Clone, Default)]
pub struct SkillsSnapshot {
    pub skills: Arc<Vec<SkillInfo>>,
}

#[derive(Debug, Clone)]
pub struct SkillInfo {
    pub id: String,
    pub title: String,
    pub description: String,
}

impl SkillsSnapshot {
    pub fn empty() -> Self {
        Self { skills: Arc::new(Vec::new()) }
    }
}

pub fn register(engine: &mut Engine, snapshot: SkillsSnapshot) {
    let snap = snapshot.clone();
    engine.register_fn("list_skills", move || -> Vec<Dynamic> {
        snap.skills
            .iter()
            .map(|s| {
                let mut m = Map::new();
                m.insert("id".into(), Dynamic::from(s.id.clone()));
                m.insert("title".into(), Dynamic::from(s.title.clone()));
                m.insert("description".into(), Dynamic::from(s.description.clone()));
                Dynamic::from(m)
            })
            .collect()
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_skills_returns_map_array() {
        let snap = SkillsSnapshot {
            skills: Arc::new(vec![SkillInfo {
                id: "core:negotiation".into(),
                title: "Negotiation".into(),
                description: "negotiation skill".into(),
            }]),
        };
        let mut e = Engine::new();
        register(&mut e, snap);
        let v: Vec<Dynamic> = e.eval("list_skills()").unwrap();
        assert_eq!(v.len(), 1);
    }

    #[test]
    fn empty_snapshot_returns_empty_vec() {
        let mut e = Engine::new();
        register(&mut e, SkillsSnapshot::empty());
        let v: Vec<Dynamic> = e.eval("list_skills()").unwrap();
        assert!(v.is_empty());
    }
}