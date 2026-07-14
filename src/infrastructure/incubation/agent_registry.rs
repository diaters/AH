use std::fs;
use std::path::Path;

use crate::prelude::Resource;
use anyhow::Result;
use tracing::{debug, warn};

use crate::domain::{AgentConfig, AgentEntry, AgentToolsConfig, ModelChainEntry};

/// 孵化出的持久型 Agent 记录（内部转换结构）。
#[derive(Debug, Clone)]
pub struct IncubatedAgentRecord {
    pub name: String,
    pub model: String,
    /// 有序模型链，第一个为最高优先级。
    pub models: Vec<ModelChainEntry>,
    pub tags: Vec<String>,
    pub description: String,
    pub tools: Option<AgentToolsConfig>,
    pub skills: Option<Vec<String>>,
}

/// 孵化 Agent 注册持久化服务。
///
/// 不持有内部状态，仅作为 Bevy Resource 类型标记。
/// 写入目标为 `agents.toml`，路径通过 `append()` 参数传入，
/// 与 `load_agents_system` 使用同一文件。
#[derive(Resource, Debug, Clone, Default)]
pub struct IncubatedAgentRegistry;

impl IncubatedAgentRegistry {
    /// 向 `agents.toml` 追加一条孵化 Agent 条目。
    ///
    /// 读取现有配置、按 `name` 去重、追加新条目、原子写回。
    pub fn append(&self, config_path: &str, record: &IncubatedAgentRecord) -> Result<()> {
        let config_path = Path::new(config_path);

        // 读取现有配置
        let mut config = if config_path.exists() {
            let content = fs::read_to_string(config_path)?;
            match toml::from_str::<AgentConfig>(&content) {
                Ok(c) => c,
                Err(e) => {
                    warn!(
                        event = "IncubatedAgentConfigParseError",
                        path = %config_path.display(),
                        error = %e,
                        "failed to parse agents.toml, starting fresh"
                    );
                    AgentConfig { agent: vec![] }
                }
            }
        } else {
            AgentConfig { agent: vec![] }
        };

        // 按 name 去重
        if config.agent.iter().any(|a| a.name == record.name) {
            debug!(
                event = "IncubatedAgentDuplicateSkipped",
                name = %record.name,
                "agent with same name already exists, skipping"
            );
            return Ok(());
        }

        // 追加新条目
        config.agent.push(AgentEntry {
            name: record.name.clone(),
            model: Some(record.model.clone()),
            models: record.models.clone(),
            tags: record.tags.clone(),
            description: record.description.clone(),
            tools: record.tools.clone(),
            skills: record.skills.clone(),
        });

        // 序列化
        let toml_str = toml::to_string(&config)?;

        // 原子写回：先写临时文件，再 rename 覆盖
        if let Some(parent) = config_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp_path = config_path.with_extension("toml.tmp");
        fs::write(&tmp_path, &toml_str)?;
        fs::rename(&tmp_path, config_path)?;

        debug!(
            event = "IncubatedAgentAppended",
            name = %record.name,
            path = %config_path.display(),
            "incubated agent appended to agents.toml"
        );

        Ok(())
    }

    /// 向 `agents.toml` 追加一条孵化 Agent 条目，若 name 已存在则自动追加后缀。
    ///
    /// 读取现有配置，检查 name 是否已存在。若重名，追加 `-2`、`-3` 后缀直到唯一，
    /// 修改 `record.name` 为最终名称，然后调用 `append` 写入。
    pub fn append_or_rename(
        &self,
        config_path: &str,
        record: &mut IncubatedAgentRecord,
    ) -> Result<()> {
        let config_path = Path::new(config_path);

        // 读取现有配置以收集已存在的 name
        let existing_names: std::collections::HashSet<String> = if config_path.exists() {
            let content = fs::read_to_string(config_path)?;
            match toml::from_str::<AgentConfig>(&content) {
                Ok(c) => c.agent.iter().map(|a| a.name.clone()).collect(),
                Err(_) => std::collections::HashSet::new(),
            }
        } else {
            std::collections::HashSet::new()
        };

        // 若 name 已存在，追加后缀直到唯一
        if existing_names.contains(&record.name) {
            let mut suffix = 2u32;
            loop {
                let candidate = format!("{}-{}", record.name, suffix);
                if !existing_names.contains(&candidate) {
                    record.name = candidate;
                    break;
                }
                suffix += 1;
            }
        }

        self.append(config_path.to_str().unwrap(), record)
    }

    /// 更新 `agents.toml` 中指定 Agent 的 tags 和 description。
    ///
    /// 读取现有配置，按 name 查找条目，替换 tags 和 description，原子写回。
    /// model 和 models 链不变。
    pub fn update(
        &self,
        config_path: &str,
        agent_name: &str,
        new_tags: &[String],
        new_description: &str,
    ) -> Result<()> {
        let config_path = Path::new(config_path);

        let content = fs::read_to_string(config_path)?;
        let mut config = toml::from_str::<AgentConfig>(&content)?;

        let entry = config
            .agent
            .iter_mut()
            .find(|a| a.name == agent_name)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "agent '{}' not found in {}",
                    agent_name,
                    config_path.display()
                )
            })?;

        entry.tags = new_tags.to_vec();
        entry.description = new_description.to_string();

        let toml_str = toml::to_string(&config)?;

        // 原子写回
        let tmp_path = config_path.with_extension("toml.tmp");
        fs::write(&tmp_path, &toml_str)?;
        fs::rename(&tmp_path, config_path)?;

        debug!(
            event = "IncubatedAgentUpdated",
            name = %agent_name,
            path = %config_path.display(),
            "incubated agent updated in agents.toml"
        );

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn append_creates_agents_toml_from_scratch() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("agents.toml");
        let registry = IncubatedAgentRegistry;

        registry
            .append(
                path.to_str().unwrap(),
                &IncubatedAgentRecord {
                    name: "physics-specialist".to_string(),
                    model: "gpt-4.1-mini".to_string(),
                    models: vec![],
                    tags: vec!["incubated".to_string()],
                    description: "physics specialist".to_string(),
                    tools: None,
                    skills: None,
                },
            )
            .unwrap();

        let content = fs::read_to_string(&path).unwrap();
        let config: AgentConfig = toml::from_str(&content).unwrap();
        assert_eq!(config.agent.len(), 1);
        assert_eq!(config.agent[0].name, "physics-specialist");
        assert_eq!(config.agent[0].model, Some("gpt-4.1-mini".to_string()));
    }

    #[test]
    fn append_preserves_existing_entries() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("agents.toml");

        // 写入初始配置
        let initial = AgentConfig {
            agent: vec![AgentEntry {
                name: "default".to_string(),
                model: Some("gpt-4".to_string()),
                models: vec![],
                tags: vec!["default".to_string()],
                description: "default agent".to_string(),
                tools: None,
                skills: None,
            }],
        };
        fs::write(&path, toml::to_string(&initial).unwrap()).unwrap();

        let registry = IncubatedAgentRegistry;
        registry
            .append(
                path.to_str().unwrap(),
                &IncubatedAgentRecord {
                    name: "incubated-agent".to_string(),
                    model: "gpt-4.1-mini".to_string(),
                    models: vec![],
                    tags: vec!["incubated".to_string()],
                    description: "incubated".to_string(),
                    tools: None,
                    skills: None,
                },
            )
            .unwrap();

        let content = fs::read_to_string(&path).unwrap();
        let config: AgentConfig = toml::from_str(&content).unwrap();
        assert_eq!(config.agent.len(), 2);
        assert_eq!(config.agent[0].name, "default");
        assert_eq!(config.agent[1].name, "incubated-agent");
    }

    #[test]
    fn duplicate_name_skips_append() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("agents.toml");

        let initial = AgentConfig {
            agent: vec![AgentEntry {
                name: "existing".to_string(),
                model: Some("gpt-4".to_string()),
                models: vec![],
                tags: vec![],
                description: "existing".to_string(),
                tools: None,
                skills: None,
            }],
        };
        fs::write(&path, toml::to_string(&initial).unwrap()).unwrap();

        let registry = IncubatedAgentRegistry;
        registry
            .append(
                path.to_str().unwrap(),
                &IncubatedAgentRecord {
                    name: "existing".to_string(),
                    model: "other".to_string(),
                    models: vec![],
                    tags: vec![],
                    description: "duplicate".to_string(),
                    tools: None,
                    skills: None,
                },
            )
            .unwrap();

        let config: AgentConfig = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(config.agent.len(), 1);
        assert_eq!(config.agent[0].model, Some("gpt-4".to_string()));
    }

    #[test]
    fn agent_config_toml_roundtrip_with_tools() {
        // 回归保护：验证 AgentToolsConfig 的 #[serde(flatten)] + HashMap
        // 经 toml::to_string → toml::from_str roundtrip 正确
        let config = AgentConfig {
            agent: vec![AgentEntry {
                name: "test-agent".to_string(),
                model: Some("gpt-4".to_string()),
                models: vec![],
                tags: vec!["incubated".to_string()],
                description: "test".to_string(),
                tools: Some(crate::domain::AgentToolsConfig {
                    default_permission: Some(crate::domain::ToolPermission::Allow),
                    overrides: {
                        let mut m = HashMap::new();
                        m.insert(
                            "shell_exec".to_string(),
                            crate::domain::ToolPermission::Allow,
                        );
                        m
                    },
                }),
                skills: None,
            }],
        };

        let toml_str = toml::to_string(&config).unwrap();
        let parsed: AgentConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.agent.len(), 1);
        assert!(parsed.agent[0].tools.is_some());
        let tools = parsed.agent[0].tools.as_ref().unwrap();
        assert_eq!(
            tools.default_permission,
            Some(crate::domain::ToolPermission::Allow)
        );
        assert_eq!(
            tools.overrides.get("shell_exec"),
            Some(&crate::domain::ToolPermission::Allow)
        );
    }

    #[test]
    fn append_or_rename_adds_suffix_on_duplicate() {
        let dir = tempfile::TempDir::new().unwrap();
        let config_path = dir.path().join("agents.toml");
        // 预写入一个 agent
        let initial = r#"
[[agent]]
name = "physics-specialist"
model = "deepseek-chat"
tags = ["physics"]
description = "test"
"#;
        fs::write(&config_path, initial).unwrap();

        let registry = IncubatedAgentRegistry;
        let mut record = IncubatedAgentRecord {
            name: "physics-specialist".to_string(),
            model: "deepseek-chat".to_string(),
            models: vec![],
            tags: vec!["physics".to_string()],
            description: "test".to_string(),
            tools: None,
            skills: None,
        };

        registry
            .append_or_rename(config_path.to_str().unwrap(), &mut record)
            .unwrap();
        assert_eq!(record.name, "physics-specialist-2");

        // 验证文件中有两个条目
        let content = fs::read_to_string(&config_path).unwrap();
        let config: AgentConfig = toml::from_str(&content).unwrap();
        assert_eq!(config.agent.len(), 2);
        assert_eq!(config.agent[1].name, "physics-specialist-2");
    }

    #[test]
    fn append_or_rename_keeps_name_when_unique() {
        let dir = tempfile::TempDir::new().unwrap();
        let config_path = dir.path().join("agents.toml");
        let initial = r#"
[[agent]]
name = "other-agent"
model = "gpt-4"
tags = []
description = ""
"#;
        fs::write(&config_path, initial).unwrap();

        let registry = IncubatedAgentRegistry;
        let mut record = IncubatedAgentRecord {
            name: "physics-specialist".to_string(),
            model: "deepseek-chat".to_string(),
            models: vec![],
            tags: vec!["physics".to_string()],
            description: "test".to_string(),
            tools: None,
            skills: None,
        };

        registry
            .append_or_rename(config_path.to_str().unwrap(), &mut record)
            .unwrap();
        assert_eq!(record.name, "physics-specialist");
    }

    #[test]
    fn update_modifies_tags_and_description() {
        let dir = tempfile::TempDir::new().unwrap();
        let config_path = dir.path().join("agents.toml");
        let initial = r#"
[[agent]]
name = "physics-specialist"
model = "deepseek-chat"
tags = ["physics"]
description = "old"
"#;
        fs::write(&config_path, initial).unwrap();

        let registry = IncubatedAgentRegistry;
        registry
            .update(
                config_path.to_str().unwrap(),
                "physics-specialist",
                &["physics".to_string(), "quantum".to_string()],
                "new description",
            )
            .unwrap();

        let content = fs::read_to_string(&config_path).unwrap();
        let config: AgentConfig = toml::from_str(&content).unwrap();
        assert_eq!(config.agent.len(), 1);
        assert_eq!(config.agent[0].tags, vec!["physics", "quantum"]);
        assert_eq!(config.agent[0].description, "new description");
        // model 不变
        assert_eq!(config.agent[0].model, Some("deepseek-chat".to_string()));
    }

    #[test]
    fn update_returns_error_when_agent_not_found() {
        let dir = tempfile::TempDir::new().unwrap();
        let config_path = dir.path().join("agents.toml");
        let initial = r#"
[[agent]]
name = "physics-specialist"
model = "deepseek-chat"
tags = ["physics"]
description = "old"
"#;
        fs::write(&config_path, initial).unwrap();

        let registry = IncubatedAgentRegistry;
        let result = registry.update(
            config_path.to_str().unwrap(),
            "nonexistent",
            &["new".to_string()],
            "new",
        );
        assert!(result.is_err());
    }
}
