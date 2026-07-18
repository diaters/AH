use crate::infrastructure::skills::registry::{SkillEntry, SkillId, SkillRegistry};
use crate::prelude::Resource;
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
    pub version: u32,
    pub self_updatable: bool,
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

    /// 用指定 base_dir 构造 SkillLoader。
    ///
    /// `base_dir` 语义与 `default_path()` 一致：直接指向 `agents/` 目录本身，
    /// 而非其父目录。主要供测试注入临时目录使用。
    pub fn new(base_dir: PathBuf) -> Self {
        Self { base_dir }
    }

    /// 返回指定 skill 的 SKILL.md 文件路径。
    ///
    /// 路径约定：`<base_dir>/<owner_agent_name>/skills/<skill_name>/SKILL.md`
    ///（`base_dir` 本身就是 `agents/` 目录，与 `load_skills` / `build_registry` 语义一致）。
    pub fn skill_md_path(&self, skill_id: &SkillId) -> PathBuf {
        self.base_dir
            .join(&skill_id.owner_agent_name)
            .join("skills")
            .join(&skill_id.skill_name)
            .join("SKILL.md")
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
                    let content = std::fs::read_to_string(&skill_md).ok()?;
                    parse_skill_md(&content)
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
                let content = std::fs::read_to_string(&c.path).ok()?;
                parse_skill_md(&content).map(|mut s| {
                    s.name = format!("{}:{}", c.plugin_id, s.name);
                    s
                })
            })
            .collect()
    }

    /// 扫描所有 agent 的 skills 目录，构造 SkillRegistry。
    ///
    /// 遍历 `base_dir/<agent_name>/skills/<skill_name>/SKILL.md`，
    /// 解析每个 SKILL.md 并以 `SkillId(owner_agent_name, skill_name)` 为键
    /// 写入 `SkillRegistry`。
    ///
    /// `base_dir` 本身就是 `agents/` 目录，与 `default_path()` / `load_skills`
    /// 的语义保持一致。
    pub fn build_registry(&self) -> SkillRegistry {
        let mut registry = SkillRegistry::default();
        // base_dir 本身就是 agents/ 目录，与 load_skills / load_plugin_skills 一致
        let agents_dir = self.base_dir.clone();
        if let Ok(agent_entries) = std::fs::read_dir(&agents_dir) {
            for agent_entry in agent_entries.flatten() {
                let agent_name = agent_entry.file_name().to_string_lossy().to_string();
                let skills_dir = agent_entry.path().join("skills");
                if let Ok(skill_entries) = std::fs::read_dir(&skills_dir) {
                    for skill_entry in skill_entries.flatten() {
                        let skill_path = skill_entry.path().join("SKILL.md");
                        if let Ok(content) = std::fs::read_to_string(&skill_path)
                            && let Some(loaded) = parse_skill_md(&content)
                        {
                            let skill_id = SkillId::new(agent_name.clone(), loaded.name.clone());
                            let entry = SkillEntry {
                                skill_id,
                                name: loaded.name,
                                description: loaded.description,
                                instructions: loaded.instructions,
                                version: loaded.version,
                                owner_agent_name: agent_name.clone(),
                                self_updatable: loaded.self_updatable,
                            };
                            registry.upsert(entry);
                        }
                    }
                }
            }
        }
        registry
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

pub fn parse_skill_md(content: &str) -> Option<LoadedSkill> {
    let mut lines = content.lines();
    let first = lines.next()?;
    if first.trim() != "---" {
        return None;
    }

    let mut name = String::new();
    let mut description = String::new();
    let mut version: u32 = 1;
    let mut self_updatable: bool = true;
    let mut instructions_lines: Vec<String> = Vec::new();
    let mut in_frontmatter = true;
    let mut frontmatter_closed = false;

    for line in lines {
        if in_frontmatter {
            if line.trim() == "---" {
                in_frontmatter = false;
                frontmatter_closed = true;
                continue;
            }
            if let Some(rest) = line.strip_prefix("name:") {
                name = rest.trim().to_string();
            } else if let Some(rest) = line.strip_prefix("description:") {
                description = rest.trim().to_string();
            } else if let Some(rest) = line.strip_prefix("version:") {
                if let Ok(v) = rest.trim().parse::<u32>() {
                    version = v;
                }
            } else if let Some(rest) = line.strip_prefix("self_updatable:") {
                match rest.trim() {
                    "true" => self_updatable = true,
                    "false" => self_updatable = false,
                    _ => {}
                }
            }
        } else {
            instructions_lines.push(line.to_string());
        }
    }

    if !frontmatter_closed {
        return None;
    }

    if name.is_empty() {
        return None;
    }

    let instructions = instructions_lines.join("\n").trim().to_string();
    Some(LoadedSkill {
        name,
        description,
        instructions,
        version,
        self_updatable,
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
            version: 1,
            self_updatable: true,
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

#[cfg(test)]
mod version_field_tests {
    use super::*;

    #[test]
    fn parse_skill_md_with_version_and_self_updatable() {
        let content = "---\nname: my-skill\ndescription: A skill\nversion: 3\nself_updatable: false\n---\n\n## Usage\n\nDo the thing.\n";
        let parsed = parse_skill_md(content).unwrap();
        assert_eq!(parsed.version, 3);
        assert!(!parsed.self_updatable);
    }

    #[test]
    fn parse_skill_md_defaults_when_fields_missing() {
        let content =
            "---\nname: my-skill\ndescription: A skill\n---\n\n## Usage\n\nDo the thing.\n";
        let parsed = parse_skill_md(content).unwrap();
        assert_eq!(parsed.version, 1);
        assert!(parsed.self_updatable);
    }

    #[test]
    fn parse_skill_md_self_updatable_true_explicit() {
        let content = "---\nname: my-skill\ndescription: A skill\nself_updatable: true\n---\n\n## Usage\n\nDo the thing.\n";
        let parsed = parse_skill_md(content).unwrap();
        assert!(parsed.self_updatable);
    }

    #[test]
    fn parse_skill_md_rejects_unclosed_frontmatter() {
        // 缺少闭合 ---
        let content = "---\nname: my-skill\ndescription: A skill\n\n## Usage\n\nDo it.\n";
        assert!(parse_skill_md(content).is_none());
    }

    #[test]
    fn parse_skill_md_rejects_missing_name() {
        // name 字段缺失
        let content = "---\ndescription: A skill\n---\n\n## Usage\n\nDo it.\n";
        assert!(parse_skill_md(content).is_none());
    }

    #[test]
    fn parse_skill_md_invalid_version_falls_back_to_default() {
        // 无效 version 值，静默回退到默认 1
        let content = "---\nname: my-skill\nversion: abc\n---\n\n## Usage\n\nDo it.\n";
        let parsed = parse_skill_md(content).unwrap();
        assert_eq!(parsed.version, 1);
    }

    #[test]
    fn parse_skill_md_invalid_self_updatable_falls_back_to_default() {
        // 无效 self_updatable 值，静默回退到默认 true
        let content = "---\nname: my-skill\nself_updatable: maybe\n---\n\n## Usage\n\nDo it.\n";
        let parsed = parse_skill_md(content).unwrap();
        assert!(parsed.self_updatable);
    }
}

#[cfg(test)]
mod registry_build_tests {
    use super::*;
    use crate::infrastructure::skills::registry::{SkillId, SkillRegistry};
    use std::fs;
    use tempfile::TempDir;

    /// 写入一个 SKILL.md 到 `<agents_dir>/<agent>/skills/<skill_name>/SKILL.md`。
    /// `agents_dir` 应直接指向 `agents/` 目录本身，与 `default_path()` 语义一致。
    fn write_skill(agents_dir: &std::path::Path, agent: &str, skill_name: &str, content: &str) {
        let dir = agents_dir.join(agent).join("skills").join(skill_name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("SKILL.md"), content).unwrap();
    }

    #[test]
    fn build_registry_scans_all_agents() {
        let tmp = TempDir::new().unwrap();
        // base_dir 是 agents/ 目录本身（与 default_path() 语义一致）
        let agents_dir = tmp.path().join(".harness").join("assets").join("agents");
        write_skill(
            &agents_dir,
            "agent-a",
            "coding",
            "---\nname: coding\ndescription: coding skill\nversion: 2\n---\n\n## Usage\n\nDo it.\n",
        );
        write_skill(
            &agents_dir,
            "agent-a",
            "review",
            "---\nname: review\ndescription: review skill\n---\n\n## Usage\n\nReview.\n",
        );
        write_skill(
            &agents_dir,
            "agent-b",
            "writing",
            "---\nname: writing\ndescription: writing skill\nself_updatable: false\n---\n\n## Usage\n\nWrite.\n",
        );

        let loader = SkillLoader {
            base_dir: agents_dir.clone(),
        };
        let registry: SkillRegistry = loader.build_registry();

        assert_eq!(registry.skills.len(), 3);
        let coding = registry
            .get(&SkillId::new("agent-a", "coding"))
            .expect("coding skill should exist");
        assert_eq!(coding.version, 2);
        assert_eq!(coding.owner_agent_name, "agent-a");
        assert!(coding.self_updatable);
        let writing = registry
            .get(&SkillId::new("agent-b", "writing"))
            .expect("writing skill should exist");
        assert!(!writing.self_updatable);
    }
}
