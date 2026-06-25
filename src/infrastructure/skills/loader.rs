use bevy::prelude::Resource;
use std::path::PathBuf;

/// 插件贡献的 Skill 条目。
#[derive(Debug, Clone)]
pub struct PluginSkillEntry {
    pub plugin_id: String,
    pub skill_id: String,
    pub path: PathBuf,
}

/// 插件贡献的 Skill 汇总资源。
///
/// 在插件加载 startup 阶段从 PluginRegistry 中提取所有已声明 skill 的路径，
/// 供 SkillLoader.load_plugin_skills 在任务派发时合并到 agent prompt 中。
#[derive(Resource, Debug, Clone, Default)]
pub struct PluginSkillContributions {
    pub entries: Vec<PluginSkillEntry>,
}

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

    /// 加载插件贡献的 Skill。
    ///
    /// 遍历 `PluginSkillContributions` 中的条目，解析每个 SKILL.md，
    /// 并将名称命名空间化为 `plugin_id:skill_name` 以避免冲突。
    pub fn load_plugin_skills(
        &self,
        contributions: &PluginSkillContributions,
        _agent_name: &str,
    ) -> Vec<LoadedSkill> {
        contributions
            .entries
            .iter()
            .filter_map(|c| {
                parse_skill_md(&c.path).map(|mut s| {
                    s.name = format!("{}:{}", c.plugin_id, s.name);
                    s
                })
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

    #[test]
    fn load_plugin_skills_namespaces_skill_name() {
        let dir = tempfile::TempDir::new().unwrap();
        let skill_md = dir.path().join("SKILL.md");
        std::fs::write(
            &skill_md,
            "---\nname: negotiation\ndescription: 谈判技巧\n---\n1. 倾听\n",
        )
        .unwrap();

        let loader = SkillLoader::default_path();
        let contributions = PluginSkillContributions {
            entries: vec![PluginSkillEntry {
                plugin_id: "my-plugin".to_string(),
                skill_id: "negotiation".to_string(),
                path: skill_md,
            }],
        };
        let skills = loader.load_plugin_skills(&contributions, "any-agent");
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "my-plugin:negotiation");
        assert_eq!(skills[0].description, "谈判技巧");
        assert_eq!(skills[0].instructions, "1. 倾听");
    }

    #[test]
    fn load_plugin_skills_skips_invalid_skill_md() {
        let dir = tempfile::TempDir::new().unwrap();
        let skill_md = dir.path().join("SKILL.md");
        // 缺少 frontmatter 的无效 SKILL.md
        std::fs::write(&skill_md, "just some text without frontmatter\n").unwrap();

        let loader = SkillLoader::default_path();
        let contributions = PluginSkillContributions {
            entries: vec![PluginSkillEntry {
                plugin_id: "bad-plugin".to_string(),
                skill_id: "broken".to_string(),
                path: skill_md,
            }],
        };
        let skills = loader.load_plugin_skills(&contributions, "any-agent");
        assert!(skills.is_empty());
    }
}
