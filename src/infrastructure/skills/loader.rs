use crate::infrastructure::skills::registry::{SkillEntry, SkillId, SkillRegistry};
use crate::prelude::Resource;
use std::path::PathBuf;
use tracing::warn;

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
    /// 同 agent 名下依赖的 skill 名列表（缺省为空 Vec）。
    pub dependencies: Vec<String>,
    /// Skill 目录路径（SKILL.md 所在目录），用于解析相对路径资源。
    pub skill_dir: PathBuf,
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

    /// 返回 base_dir 路径引用。
    pub fn base_dir(&self) -> &std::path::Path {
        &self.base_dir
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
                // 跳过隐藏目录（如 .sandbox）
                if path
                    .file_name()
                    .map(|n| n.to_string_lossy().starts_with('.'))
                    .unwrap_or(false)
                {
                    return None;
                }
                let skill_md = path.join("SKILL.md");
                if skill_md.exists() {
                    let content = std::fs::read_to_string(&skill_md).ok()?;
                    // path 即为 skill 目录
                    parse_skill_md(&content, path)
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
                // c.path 是 SKILL.md 文件路径，取其父目录作为 skill_dir
                let skill_dir = c.path.parent()?.to_path_buf();
                parse_skill_md(&content, skill_dir).map(|mut s| {
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
                        let skill_path_raw = skill_entry.path();
                        // 跳过隐藏目录（如 .sandbox）
                        if skill_path_raw
                            .file_name()
                            .map(|n| n.to_string_lossy().starts_with('.'))
                            .unwrap_or(false)
                        {
                            continue;
                        }
                        let skill_path = skill_path_raw.join("SKILL.md");
                        // skill_entry.path() 即为 skill 目录
                        let skill_dir = skill_entry.path();
                        if let Ok(content) = std::fs::read_to_string(&skill_path)
                            && let Some(loaded) = parse_skill_md(&content, skill_dir)
                        {
                            // 使用目录名作为 skill_name（而非 frontmatter name），
                            // 确保 skill_md_path() 能正确重建文件路径。
                            let dir_name = skill_entry.file_name().to_string_lossy().to_string();
                            let skill_id = SkillId::new(agent_name.clone(), dir_name);
                            let entry = SkillEntry {
                                skill_id,
                                name: loaded.name,
                                description: loaded.description,
                                instructions: loaded.instructions,
                                version: loaded.version,
                                owner_agent_name: agent_name.clone(),
                                self_updatable: loaded.self_updatable,
                                dependencies: loaded.dependencies,
                            };
                            registry.upsert(entry);
                        }
                    }
                }
            }
        }
        registry
    }

    /// 将相对路径转换为相对于工作区根目录的路径（如果可能）。
    ///
    /// 如果路径不在当前工作目录下，返回原路径。
    fn relativize_to_workspace(path: &std::path::Path) -> PathBuf {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        path.strip_prefix(&cwd).unwrap_or(path).to_path_buf()
    }

    /// 将 Skill 列表格式化为系统提示注入文本。
    ///
    /// 每个 skill 会注入：
    /// - 名称
    /// - 描述
    /// - **Skill 目录**：相对于工作区根目录的路径，便于 LLM 定位资源
    /// - 使用说明
    pub fn format_skills_prompt(skills: &[LoadedSkill]) -> String {
        if skills.is_empty() {
            return String::new();
        }
        let mut prompt = String::from("## 可用技能\n\n");
        for skill in skills {
            prompt.push_str(&format!("### {}\n", skill.name));
            prompt.push_str(&format!("{}\n\n", skill.description));
            // 注入 skill 目录路径
            let relative_dir = Self::relativize_to_workspace(&skill.skill_dir);
            prompt.push_str(&format!("**Skill 目录**: `{}`\n\n", relative_dir.display()));
            prompt.push_str(&format!("{}\n\n", skill.instructions));
        }
        prompt
    }
}

pub fn parse_skill_md(content: &str, skill_dir: PathBuf) -> Option<LoadedSkill> {
    let mut lines = content.lines();
    let first = lines.next()?;
    if first.trim() != "---" {
        return None;
    }

    let mut name = String::new();
    let mut description = String::new();
    let mut version: u32 = 1;
    let mut self_updatable: bool = true;
    let mut dependencies: Vec<String> = Vec::new();
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
            } else if let Some(rest) = line.strip_prefix("dependencies:") {
                let rest = rest.trim();
                if rest.starts_with('[') && rest.ends_with(']') {
                    let inner = &rest[1..rest.len() - 1];
                    dependencies = inner
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                } else {
                    // 非数组格式回退为空 Vec，并 warn（name 此时可能尚未解析到）
                    warn!(
                        event = "SkillDependenciesMalformed",
                        skill = %name,
                        raw = %rest,
                        "malformed dependencies field, falling back to empty list"
                    );
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
        dependencies,
        skill_dir,
    })
}

/// 解析 skill 的传递依赖闭包，按拓扑序返回（依赖在前，选中 skill 最后）。
/// - 依赖缺失：跳过并 warn，不失败
/// - 循环依赖：环上边截断并 warn
///
/// 数据源约定：入参为 `SkillLoader::load_skills(agent_name)` 的磁盘扫描结果
/// （`Vec<LoadedSkill>`），而非 SkillRegistry——与 dispatch 注入的既有数据源
/// 保持一致。
pub fn resolve_skill_closure<'a>(
    loaded: &'a [LoadedSkill],
    skill_name: &str,
) -> Vec<&'a LoadedSkill> {
    fn dfs<'a>(
        loaded: &'a [LoadedSkill],
        name: &str,
        stack: &mut Vec<String>,
        result: &mut Vec<&'a LoadedSkill>,
        resolved: &mut Vec<String>,
    ) {
        let Some(current) = loaded.iter().find(|s| s.name == name) else {
            warn!(
                event = "SkillDependencyMissing",
                skill = %name,
                "skill dependency not found, skipping"
            );
            return;
        };
        stack.push(name.to_string());
        for dep in &current.dependencies {
            if stack.iter().any(|n| n == dep) {
                warn!(
                    event = "SkillDependencyCycle",
                    skill = %name,
                    dependency = %dep,
                    "circular dependency detected, truncating edge"
                );
                continue;
            }
            if resolved.iter().any(|n| n == dep) {
                continue;
            }
            dfs(loaded, dep, stack, result, resolved);
        }
        stack.pop();
        result.push(current);
        resolved.push(name.to_string());
    }

    if !loaded.iter().any(|s| s.name == skill_name) {
        warn!(
            event = "SkillDependencyRootMissing",
            skill = %skill_name,
            "skill not found in loaded skills, returning empty closure"
        );
        return Vec::new();
    }

    let mut stack = Vec::new();
    let mut result = Vec::new();
    let mut resolved = Vec::new();
    dfs(loaded, skill_name, &mut stack, &mut result, &mut resolved);
    result
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
            dependencies: Vec::new(),
            skill_dir: PathBuf::from(".harness/assets/agents/main/skills/smoke-test"),
        }];
        let prompt = SkillLoader::format_skills_prompt(&skills);
        assert!(prompt.contains("## 可用技能"));
        assert!(prompt.contains("### smoke-test"));
        assert!(prompt.contains("验证工具链"));
        assert!(prompt.contains("1. 运行脚本"));
        assert!(prompt.contains("**Skill 目录**"));
    }

    #[test]
    fn format_skills_prompt_empty_returns_empty() {
        let prompt = SkillLoader::format_skills_prompt(&[]);
        assert!(prompt.is_empty());
    }

    #[test]
    fn format_skills_prompt_includes_skill_dir() {
        let skills = vec![LoadedSkill {
            name: "my-skill".to_string(),
            description: "测试技能".to_string(),
            instructions: "1. 运行 scripts/setup.sh".to_string(),
            version: 1,
            self_updatable: true,
            dependencies: Vec::new(),
            skill_dir: PathBuf::from(".harness/assets/agents/main/skills/my-skill"),
        }];
        let prompt = SkillLoader::format_skills_prompt(&skills);
        assert!(prompt.contains("**Skill 目录**"));
        assert!(prompt.contains("my-skill"));
        // 路径在 instructions 之前
        let dir_pos = prompt.find("**Skill 目录**").unwrap();
        let instr_pos = prompt.find("scripts/setup.sh").unwrap();
        assert!(dir_pos < instr_pos, "路径应在说明之前注入");
    }

    #[test]
    fn format_skills_prompt_relative_path_injection() {
        // 绝对路径应被转换为相对路径
        let cwd = std::env::current_dir().unwrap();
        let abs_dir = cwd.join(".harness/assets/agents/main/skills/test-skill");
        let skills = vec![LoadedSkill {
            name: "test-skill".to_string(),
            description: "测试".to_string(),
            instructions: "do stuff".to_string(),
            version: 1,
            self_updatable: true,
            dependencies: Vec::new(),
            skill_dir: abs_dir,
        }];
        let prompt = SkillLoader::format_skills_prompt(&skills);
        // 相对路径应出现在输出中
        assert!(
            prompt.contains(".harness/assets/agents/main/skills/test-skill"),
            "绝对路径应被转换为相对路径"
        );
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
        // skill_dir 应为 SKILL.md 的父目录
        assert_eq!(skills[0].skill_dir, dir.path());
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
        let parsed = parse_skill_md(content, PathBuf::from(".harness/skills/my-skill")).unwrap();
        assert_eq!(parsed.version, 3);
        assert!(!parsed.self_updatable);
        assert_eq!(parsed.skill_dir, PathBuf::from(".harness/skills/my-skill"));
    }

    #[test]
    fn parse_skill_md_defaults_when_fields_missing() {
        let content =
            "---\nname: my-skill\ndescription: A skill\n---\n\n## Usage\n\nDo the thing.\n";
        let parsed = parse_skill_md(content, PathBuf::from("skills/my-skill")).unwrap();
        assert_eq!(parsed.version, 1);
        assert!(parsed.self_updatable);
    }

    #[test]
    fn parse_skill_md_self_updatable_true_explicit() {
        let content = "---\nname: my-skill\ndescription: A skill\nself_updatable: true\n---\n\n## Usage\n\nDo the thing.\n";
        let parsed = parse_skill_md(content, PathBuf::from("skills/my-skill")).unwrap();
        assert!(parsed.self_updatable);
    }

    #[test]
    fn parse_skill_md_rejects_unclosed_frontmatter() {
        // 缺少闭合 ---
        let content = "---\nname: my-skill\ndescription: A skill\n\n## Usage\n\nDo it.\n";
        assert!(parse_skill_md(content, PathBuf::from("skills/my-skill")).is_none());
    }

    #[test]
    fn parse_skill_md_rejects_missing_name() {
        // name 字段缺失
        let content = "---\ndescription: A skill\n---\n\n## Usage\n\nDo it.\n";
        assert!(parse_skill_md(content, PathBuf::from("skills/my-skill")).is_none());
    }

    #[test]
    fn parse_skill_md_invalid_version_falls_back_to_default() {
        // 无效 version 值，静默回退到默认 1
        let content = "---\nname: my-skill\nversion: abc\n---\n\n## Usage\n\nDo it.\n";
        let parsed = parse_skill_md(content, PathBuf::from("skills/my-skill")).unwrap();
        assert_eq!(parsed.version, 1);
    }

    #[test]
    fn parse_skill_md_invalid_self_updatable_falls_back_to_default() {
        // 无效 self_updatable 值，静默回退到默认 true
        let content = "---\nname: my-skill\nself_updatable: maybe\n---\n\n## Usage\n\nDo it.\n";
        let parsed = parse_skill_md(content, PathBuf::from("skills/my-skill")).unwrap();
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

    #[test]
    fn build_registry_skips_hidden_directories() {
        let tmp = TempDir::new().unwrap();
        let agents_dir = tmp.path().join(".harness").join("assets").join("agents");
        write_skill(
            &agents_dir,
            "agent-a",
            "coding",
            "---\nname: coding\ndescription: coding skill\n---\n\n## Usage\n\nDo it.\n",
        );
        write_skill(
            &agents_dir,
            "agent-a",
            ".sandbox",
            "---\nname: draft\ndescription: draft\n---\n\n## Usage\n\nDraft.\n",
        );

        let loader = SkillLoader {
            base_dir: agents_dir.clone(),
        };
        let registry = loader.build_registry();
        assert_eq!(registry.skills.len(), 1, "should skip .sandbox directory");
        assert!(registry.get(&SkillId::new("agent-a", "coding")).is_some());
        assert!(registry.get(&SkillId::new("agent-a", ".sandbox")).is_none());
    }

    #[test]
    fn load_skills_skips_hidden_directories() {
        let tmp = TempDir::new().unwrap();
        let agents_dir = tmp.path().join(".harness").join("assets").join("agents");
        write_skill(
            &agents_dir,
            "agent-a",
            "coding",
            "---\nname: coding\ndescription: coding skill\n---\n\n## Usage\n\nDo it.\n",
        );
        write_skill(
            &agents_dir,
            "agent-a",
            ".sandbox",
            "---\nname: draft\ndescription: draft\n---\n\n## Usage\n\nDraft.\n",
        );

        let loader = SkillLoader {
            base_dir: agents_dir,
        };
        let skills = loader.load_skills("agent-a");
        assert_eq!(skills.len(), 1, "should skip .sandbox directory");
        assert_eq!(skills[0].name, "coding");
    }
}

#[cfg(test)]
mod dependencies_tests {
    use super::*;

    /// 构造测试用 LoadedSkill。
    fn skill(name: &str, dependencies: Vec<&str>) -> LoadedSkill {
        LoadedSkill {
            name: name.to_string(),
            description: format!("desc {}", name),
            instructions: format!("instr {}", name),
            version: 1,
            self_updatable: true,
            skill_dir: PathBuf::from(".harness/skills").join(name),
            dependencies: dependencies.into_iter().map(|s| s.to_string()).collect(),
        }
    }

    // ---------- parse_skill_md 的 dependencies 解析 ----------

    #[test]
    fn parse_skill_md_parses_dependencies() {
        let content = "---\nname: daily-news\ndescription: news\nversion: 1\nself_updatable: false\ndependencies: [browser-automation, another-skill]\n---\n\n## Usage\n\nDo it.\n";
        let parsed = parse_skill_md(content, PathBuf::from("skills/daily-news")).unwrap();
        assert_eq!(
            parsed.dependencies,
            vec![
                "browser-automation".to_string(),
                "another-skill".to_string()
            ]
        );
    }

    #[test]
    fn parse_skill_md_dependencies_default_empty() {
        // 未声明 dependencies 时缺省为空 Vec
        let content = "---\nname: my-skill\ndescription: A skill\n---\n\n## Usage\n\nDo it.\n";
        let parsed = parse_skill_md(content, PathBuf::from("skills/my-skill")).unwrap();
        assert!(parsed.dependencies.is_empty());
    }

    #[test]
    fn parse_skill_md_dependencies_invalid_format_falls_back_to_empty() {
        // 非数组格式（如裸字符串或空值）静默回退为空 Vec
        let content =
            "---\nname: my-skill\ndependencies: browser-automation\n---\n\n## Usage\n\nDo it.\n";
        let parsed = parse_skill_md(content, PathBuf::from("skills/my-skill")).unwrap();
        assert!(parsed.dependencies.is_empty());
    }

    #[test]
    fn parse_skill_md_dependencies_trims_whitespace_and_skips_empty_items() {
        // 逗号后带空白、存在空项时应 trim 并过滤
        let content = "---\nname: my-skill\ndependencies: [ a, , b ]\n---\n\n## Usage\n\nDo it.\n";
        let parsed = parse_skill_md(content, PathBuf::from("skills/my-skill")).unwrap();
        assert_eq!(parsed.dependencies, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn parse_skill_md_single_element_array() {
        let content =
            "---\nname: my-skill\ndependencies: [browser-automation]\n---\n\n## Usage\n\nDo it.\n";
        let parsed = parse_skill_md(content, PathBuf::from("skills/my-skill")).unwrap();
        assert_eq!(parsed.dependencies, vec!["browser-automation".to_string()]);
    }

    #[test]
    fn parse_skill_md_empty_array() {
        let content = "---\nname: my-skill\ndependencies: []\n---\n\n## Usage\n\nDo it.\n";
        let parsed = parse_skill_md(content, PathBuf::from("skills/my-skill")).unwrap();
        assert!(parsed.dependencies.is_empty());
    }

    // ---------- resolve_skill_closure ----------

    #[test]
    fn resolve_closure_single_level_dependency() {
        let loaded = vec![
            skill("browser-automation", vec![]),
            skill("daily-news", vec!["browser-automation"]),
        ];
        let closure = resolve_skill_closure(&loaded, "daily-news");
        let names: Vec<&str> = closure.iter().map(|s| s.name.as_str()).collect();
        // 依赖在前，选中 skill 最后
        assert_eq!(names, vec!["browser-automation", "daily-news"]);
    }

    #[test]
    fn resolve_closure_transitive_dependencies() {
        let loaded = vec![
            skill("base", vec![]),
            skill("mid", vec!["base"]),
            skill("top", vec!["mid"]),
        ];
        let closure = resolve_skill_closure(&loaded, "top");
        let names: Vec<&str> = closure.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["base", "mid", "top"]);
    }

    #[test]
    fn resolve_closure_preserves_sibling_load_order() {
        // 同层依赖保持 loaded 中的出现顺序
        let loaded = vec![
            skill("c", vec![]),
            skill("a", vec!["c"]),
            skill("b", vec!["c"]),
        ];
        let closure = resolve_skill_closure(&loaded, "a");
        let names: Vec<&str> = closure.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["c", "a"]);
    }

    #[test]
    fn resolve_closure_skips_missing_dependency() {
        // 缺失依赖应被跳过，不失败，其余依赖仍注入
        let loaded = vec![skill("good", vec![]), skill("top", vec!["good", "missing"])];
        let closure = resolve_skill_closure(&loaded, "top");
        let names: Vec<&str> = closure.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["good", "top"]);
    }

    #[test]
    fn resolve_closure_breaks_cycle() {
        // 环上边截断，不无限递归，其余节点仍注入
        let loaded = vec![
            skill("a", vec!["b"]),
            skill("b", vec!["a"]),
            skill("top", vec!["a"]),
        ];
        let closure = resolve_skill_closure(&loaded, "top");
        let names: Vec<&str> = closure.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["b", "a", "top"]);
    }

    #[test]
    fn resolve_closure_deduplicates_shared_dependency() {
        // 同一依赖被多个路径引用时只注入一次
        let loaded = vec![
            skill("shared", vec![]),
            skill("x", vec!["shared"]),
            skill("y", vec!["shared"]),
            skill("top", vec!["x", "y"]),
        ];
        let closure = resolve_skill_closure(&loaded, "top");
        let names: Vec<&str> = closure.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["shared", "x", "y", "top"]);
    }

    #[test]
    fn resolve_closure_unknown_skill_returns_empty() {
        // 找不到 skill_name 本身时返回空 Vec
        let loaded = vec![skill("a", vec![])];
        let closure = resolve_skill_closure(&loaded, "not-exist");
        assert!(closure.is_empty());
    }

    #[test]
    fn resolve_closure_no_dependencies_returns_self() {
        let loaded = vec![skill("a", vec![])];
        let closure = resolve_skill_closure(&loaded, "a");
        let names: Vec<&str> = closure.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["a"]);
    }
}
