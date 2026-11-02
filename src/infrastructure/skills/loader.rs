use crate::prelude::Resource;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
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
/// 派发时与全局目录中的 skill 合并。
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
    /// 依赖的 skill 名列表（缺省为空 Vec），在技能清单中提示。
    pub dependencies: Vec<String>,
    /// 可使用该 skill 的 Agent 名；为空表示仅主 Agent。
    pub agents: Vec<String>,
    /// Skill 目录路径（SKILL.md 所在目录），用于解析相对路径资源。
    pub skill_dir: PathBuf,
}

impl LoadedSkill {
    /// 该 skill 是否对指定 Agent 可见：未声明 agents 时仅主 Agent 可用。
    pub fn visible_to(&self, agent_name: &str, main_agent: &str) -> bool {
        if self.agents.is_empty() {
            agent_name == main_agent
        } else {
            self.agents.iter().any(|a| a == agent_name)
        }
    }
}

/// 最近一次派发时加载的全部 skill，供 `load_skill` 工具按名称读取完整说明。
///
/// 派发系统每次构建技能清单时刷新；工具 worker 只读。
#[derive(Resource, Debug, Clone, Default)]
pub struct SkillCatalog(Arc<RwLock<Vec<LoadedSkill>>>);

impl SkillCatalog {
    pub fn replace(&self, skills: Vec<LoadedSkill>) {
        *self.0.write().expect("skill catalog lock poisoned") = skills;
    }

    pub fn find(&self, name: &str) -> Option<LoadedSkill> {
        self.0
            .read()
            .expect("skill catalog lock poisoned")
            .iter()
            .find(|s| s.name == name)
            .cloned()
    }
}

/// Skill 加载器：扫描全局 skills 目录与插件贡献，解析 SKILL.md。
#[derive(Resource, Debug, Clone)]
pub struct SkillLoader {
    skills_dir: PathBuf,
    /// 旧版按 Agent 分目录的位置（`<agents>/<agent>/skills/`），仅用于启动期迁移提示。
    legacy_agents_dir: PathBuf,
}

impl SkillLoader {
    pub fn default_path() -> Self {
        Self {
            skills_dir: PathBuf::from(".harness/assets/skills"),
            legacy_agents_dir: PathBuf::from(".harness/assets/agents"),
        }
    }

    /// 用指定的全局 skills 目录构造，主要供测试注入临时目录。
    pub fn new(skills_dir: PathBuf) -> Self {
        let legacy_agents_dir = skills_dir.with_file_name("agents");
        Self {
            skills_dir,
            legacy_agents_dir,
        }
    }

    pub fn skills_dir(&self) -> &Path {
        &self.skills_dir
    }

    /// 加载全局目录与插件贡献的全部 skill（不做 Agent 过滤）。
    pub fn load_all(&self, plugins: &PluginSkillContributions) -> Vec<LoadedSkill> {
        let mut skills = scan_skill_dirs(&self.skills_dir);
        skills.extend(load_plugin_skills(plugins));
        skills
    }

    /// 旧版目录中仍存在的 SKILL.md 所在目录（不再加载）。
    pub fn legacy_skill_dirs(&self) -> Vec<PathBuf> {
        let Ok(agents) = std::fs::read_dir(&self.legacy_agents_dir) else {
            return Vec::new();
        };
        let mut dirs: Vec<PathBuf> = agents
            .flatten()
            .flat_map(|agent| {
                std::fs::read_dir(agent.path().join("skills"))
                    .into_iter()
                    .flatten()
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| p.join("SKILL.md").is_file())
                    .collect::<Vec<_>>()
            })
            .collect();
        dirs.sort();
        dirs
    }

    /// 旧版目录仍有 skill 时提示迁移：这些 skill 已不会被加载。
    pub fn warn_legacy_layout(&self) {
        for dir in self.legacy_skill_dirs() {
            warn!(
                event = "LegacySkillDirIgnored",
                path = %dir.display(),
                target_dir = %self.skills_dir.display(),
                "skill under per-agent directory is no longer loaded; move it to the global skills directory"
            );
        }
    }

    /// 将可用 skill 格式化为系统提示中的技能清单（仅名称、描述与依赖）。
    pub fn format_skills_catalog(skills: &[LoadedSkill]) -> String {
        if skills.is_empty() {
            return String::new();
        }
        let mut prompt = String::from(
            "## 可用技能\n\n任务与下列某个技能相关时，先调用 load_skill 读取它的完整说明，再按说明执行；不要凭描述猜测步骤。\n\n",
        );
        for skill in skills {
            prompt.push_str(&format!("- `{}`：{}", skill.name, skill.description));
            if !skill.dependencies.is_empty() {
                prompt.push_str(&format!("（依赖：{}）", skill.dependencies.join(", ")));
            }
            prompt.push('\n');
        }
        prompt
    }
}

/// 将相对路径转换为相对于工作区根目录的路径（如果可能）。
pub fn relativize_to_workspace(path: &Path) -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    path.strip_prefix(&cwd).unwrap_or(path).to_path_buf()
}

/// 扫描 `<dir>/<skill>/SKILL.md`，跳过隐藏目录（如 .sandbox）与无效文件。
fn scan_skill_dirs(dir: &Path) -> Vec<LoadedSkill> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut skills: Vec<LoadedSkill> = entries
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            if path
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with('.'))
            {
                return None;
            }
            let content = std::fs::read_to_string(path.join("SKILL.md")).ok()?;
            parse_skill_md(&content, path)
        })
        .collect();
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills
}

/// 加载插件贡献的 Skill，名称命名空间化为 `plugin_id:skill_name` 以避免冲突。
fn load_plugin_skills(contributions: &PluginSkillContributions) -> Vec<LoadedSkill> {
    contributions
        .entries
        .iter()
        .filter_map(|c| {
            let content = std::fs::read_to_string(&c.path).ok()?;
            let skill_dir = c.path.parent()?.to_path_buf();
            parse_skill_md(&content, skill_dir).map(|mut s| {
                s.name = format!("{}:{}", c.plugin_id, s.name);
                s
            })
        })
        .collect()
}

/// 解析 `[a, b]` 形式的列表；格式不符时返回 None。
fn parse_list(raw: &str) -> Option<Vec<String>> {
    let raw = raw.trim();
    let inner = raw.strip_prefix('[')?.strip_suffix(']')?;
    Some(
        inner
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
    )
}

pub fn parse_skill_md(content: &str, skill_dir: PathBuf) -> Option<LoadedSkill> {
    let mut lines = content.lines();
    if lines.next()?.trim() != "---" {
        return None;
    }

    let mut name = String::new();
    let mut description = String::new();
    let mut dependencies: Vec<String> = Vec::new();
    let mut agents: Vec<String> = Vec::new();
    let mut instructions_lines: Vec<&str> = Vec::new();
    let mut in_frontmatter = true;
    let mut frontmatter_closed = false;

    for line in lines {
        if !in_frontmatter {
            instructions_lines.push(line);
            continue;
        }
        if line.trim() == "---" {
            in_frontmatter = false;
            frontmatter_closed = true;
        } else if let Some(rest) = line.strip_prefix("name:") {
            name = rest.trim().to_string();
        } else if let Some(rest) = line.strip_prefix("description:") {
            description = rest.trim().to_string();
        } else if let Some((key, rest)) = line
            .strip_prefix("dependencies:")
            .map(|r| ("dependencies", r))
            .or_else(|| line.strip_prefix("agents:").map(|r| ("agents", r)))
        {
            match parse_list(rest) {
                Some(list) if key == "dependencies" => dependencies = list,
                Some(list) => agents = list,
                None => warn!(
                    event = "SkillListFieldMalformed",
                    skill = %name,
                    field = key,
                    raw = %rest.trim(),
                    "malformed list field, falling back to empty list"
                ),
            }
        }
    }

    if !frontmatter_closed || name.is_empty() {
        return None;
    }

    Some(LoadedSkill {
        name,
        description,
        instructions: instructions_lines.join("\n").trim().to_string(),
        dependencies,
        agents,
        skill_dir,
    })
}

#[cfg(test)]
#[path = "loader_tests.rs"]
mod tests;
