use std::path::PathBuf;
use bevy::prelude::Resource;

/// 已加载的 Skill。
#[derive(Debug, Clone)]
pub struct LoadedSkill {
    pub name: String,
    pub description: String,
    pub instructions: String,
}

/// Skill 加载器：扫描 Agent 的 skills 目录，解析 SKILL.md。
#[derive(Resource, Debug, Clone)]
pub struct SkillLoader {
    base_dir: PathBuf,
}

impl SkillLoader {
    pub fn default_path() -> Self {
        Self {
            base_dir: PathBuf::from(".harness/assets/agents"),
        }
    }

    /// 扫描指定 Agent 的 skills 目录，返回所有已加载的 Skill。
    pub fn load_skills(&self, agent_name: &str) -> Vec<LoadedSkill> {
        let skills_dir = self.base_dir.join(agent_name).join("skills");
        let Ok(entries) = std::fs::read_dir(&skills_dir) else {
            return Vec::new();
        };
        entries
            .filter_map(|entry| {
                let path = entry.ok()?.path();
                let skill_md = path.join("SKILL.md");
                if skill_md.exists() {
                    parse_skill_md(&skill_md)
                } else {
                    None
                }
            })
            .collect()
    }

    /// 将 Skill 列表格式化为系统提示注入文本。
    pub fn format_skills_prompt(skills: &[LoadedSkill]) -> String {
        if skills.is_empty() {
            return String::new();
        }
        let mut prompt = String::from("## 可用技能\n\n");
        for skill in skills {
            prompt.push_str(&format!("### {}\n", skill.name));
            prompt.push_str(&format!("{}\n\n", skill.description));
            prompt.push_str(&format!("{}\n\n", skill.instructions));
        }
        prompt
    }
}

fn parse_skill_md(path: &std::path::Path) -> Option<LoadedSkill> {
    let content = std::fs::read_to_string(path).ok()?;
    if !content.starts_with("---") {
        return None;
    }
    let rest = &content[3..];
    let end = rest.find("---")?;
    let frontmatter = &rest[..end];
    let instructions = rest[end + 3..].trim().to_string();

    let name = frontmatter
        .lines()
        .find(|l| l.starts_with("name:"))
        .map(|l| l.trim_start_matches("name:").trim().to_string())
        .unwrap_or_default();
    let description = frontmatter
        .lines()
        .find(|l| l.starts_with("description:"))
        .map(|l| l.trim_start_matches("description:").trim().to_string())
        .unwrap_or_default();

    Some(LoadedSkill {
        name,
        description,
        instructions,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_skills_prompt_produces_section() {
        let skills = vec![LoadedSkill {
            name: "smoke-test".to_string(),
            description: "验证工具链".to_string(),
            instructions: "1. 运行脚本".to_string(),
        }];
        let prompt = SkillLoader::format_skills_prompt(&skills);
        assert!(prompt.contains("## 可用技能"));
        assert!(prompt.contains("### smoke-test"));
        assert!(prompt.contains("验证工具链"));
        assert!(prompt.contains("1. 运行脚本"));
    }

    #[test]
    fn format_skills_prompt_empty_returns_empty() {
        let prompt = SkillLoader::format_skills_prompt(&[]);
        assert!(prompt.is_empty());
    }
}
