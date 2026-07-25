use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::user_plugins::API_VERSION;

/// 用户插件 Manifest
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    pub id: String,
    pub name: Option<String>,
    pub version: Option<String>,
    pub api_version: u32,
    pub author: Option<String>,
    pub description: Option<String>,

    #[serde(default)]
    pub hooks: Vec<HookSubscription>,
    #[serde(default)]
    pub tools: Vec<ToolContribution>,
    #[serde(default)]
    pub skills: Vec<SkillContribution>,
    #[serde(default)]
    pub agents: Vec<AgentContribution>,
    #[serde(default)]
    pub commands: Vec<CommandContribution>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookSubscription {
    pub event: String,
    pub script: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolContribution {
    pub id: String,
    pub schema: PathBuf,
    pub handler: PathBuf,
    pub description: String,
    pub default_permission: Option<crate::domain::ToolPermission>,
    /// 插件可选：覆盖全局 inflight 超时（秒）。
    ///
    /// 缺省 `None` 时由 `RhaiPluginAsyncWrapper` 走全局 `tool_inflight_timeout_secs`（D14）。
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillContribution {
    pub id: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentContribution {
    pub id: String,
    pub profile: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandContribution {
    pub id: String,
    pub display: String,
    pub script: PathBuf,
    pub description: Option<String>,
}

/// Manifest 校验结果
#[derive(Debug)]
pub enum ManifestError {
    Parse(toml::de::Error),
    Invalid(String),
}

impl PluginManifest {
    /// 从 TOML 字符串解析。
    pub fn from_toml(content: &str) -> Result<Self, ManifestError> {
        let manifest: PluginManifest = toml::from_str(content).map_err(ManifestError::Parse)?;
        manifest.validate().map_err(ManifestError::Invalid)?;
        Ok(manifest)
    }

    /// 静态校验：api_version、id 非空、display 非空、script 路径是相对路径。
    pub fn validate(&self) -> Result<(), String> {
        if self.id.trim().is_empty() {
            return Err("manifest.id must not be empty".into());
        }
        if self.id.contains(':') {
            return Err(format!("manifest.id must not contain ':': {}", self.id));
        }
        if self.api_version != API_VERSION {
            return Err(format!(
                "api_version mismatch: manifest={}, core={}",
                self.api_version, API_VERSION
            ));
        }
        for hook in &self.hooks {
            if hook.event.trim().is_empty() {
                return Err("hook.event must not be empty".into());
            }
            if hook.script.is_absolute() {
                return Err(format!(
                    "hook.script must be relative to plugin root: {}",
                    hook.script.display()
                ));
            }
        }
        for tool in &self.tools {
            if tool.id.trim().is_empty() {
                return Err("tool.id must not be empty".into());
            }
            if tool.id.contains(':') {
                return Err(format!("tool.id must not contain ':': {}", tool.id));
            }
            if tool.description.trim().is_empty() {
                return Err(format!("tool.description must not be empty: {}", tool.id));
            }
            if tool.schema.is_absolute() || tool.handler.is_absolute() {
                return Err(format!(
                    "tool paths must be relative: {}/{}",
                    tool.schema.display(),
                    tool.handler.display()
                ));
            }
            if let Some(secs) = tool.timeout_secs
                && secs == 0
            {
                return Err(format!(
                    "tool.timeout_secs must be > 0 when set: {}",
                    tool.id
                ));
            }
        }
        for skill in &self.skills {
            if skill.id.contains(':') {
                return Err(format!("skill.id must not contain ':': {}", skill.id));
            }
            if skill.path.is_absolute() {
                return Err(format!(
                    "skill.path must be relative: {}",
                    skill.path.display()
                ));
            }
        }
        for agent in &self.agents {
            if agent.id.contains(':') {
                return Err(format!("agent.id must not contain ':': {}", agent.id));
            }
            if agent.profile.is_absolute() {
                return Err(format!(
                    "agent.profile must be relative: {}",
                    agent.profile.display()
                ));
            }
        }
        for cmd in &self.commands {
            if cmd.id.contains(':') {
                return Err(format!("command.id must not contain ':': {}", cmd.id));
            }
            if !cmd.display.starts_with('/') {
                return Err(format!(
                    "command.display must start with '/': {}",
                    cmd.display
                ));
            }
            if cmd.script.is_absolute() {
                return Err(format!(
                    "command.script must be relative: {}",
                    cmd.script.display()
                ));
            }
        }
        // 单插件内 display 不允许重复（跨插件 display 冲突在 loader 阶段处理，见 Task 5）
        let mut seen_displays = std::collections::HashSet::new();
        for cmd in &self.commands {
            if !seen_displays.insert(cmd.display.as_str()) {
                return Err(format!(
                    "duplicate command.display within plugin: {}",
                    cmd.display
                ));
            }
        }
        Ok(())
    }

    /// 该 manifest 是否声明了某个 hook 事件
    pub fn subscribes_to(&self, event: &str) -> bool {
        self.hooks.iter().any(|h| h.event == event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_header() -> &'static str {
        "id = \"my-plugin\"\napi_version = 1\n"
    }

    #[test]
    fn parses_minimal_valid_manifest() {
        let toml_src = valid_header();
        let m = PluginManifest::from_toml(toml_src).unwrap();
        assert_eq!(m.id, "my-plugin");
        assert_eq!(m.api_version, 1);
        assert!(m.hooks.is_empty());
    }

    #[test]
    fn rejects_id_with_colon() {
        let toml_src = "id = \"bad:id\"\napi_version = 1\n";
        let err = PluginManifest::from_toml(toml_src).unwrap_err();
        assert!(matches!(err, ManifestError::Invalid(_)));
    }

    #[test]
    fn rejects_wrong_api_version() {
        let toml_src = "id = \"x\"\napi_version = 999\n";
        let err = PluginManifest::from_toml(toml_src).unwrap_err();
        assert!(matches!(err, ManifestError::Invalid(_)));
    }

    #[test]
    fn rejects_absolute_hook_script() {
        let toml_src = r#"
id = "x"
api_version = 1
[[hooks]]
event = "on_task_created"
script = "/abs/path.rhai"
"#;
        let err = PluginManifest::from_toml(toml_src).unwrap_err();
        assert!(matches!(err, ManifestError::Invalid(_)));
    }

    #[test]
    fn rejects_command_display_without_slash() {
        let toml_src = r#"
id = "x"
api_version = 1
[[commands]]
id = "summarize"
display = "summarize"
script = "commands/summarize.rhai"
"#;
        let err = PluginManifest::from_toml(toml_src).unwrap_err();
        assert!(matches!(err, ManifestError::Invalid(_)));
    }

    #[test]
    fn subscribes_to_detects_event() {
        let toml_src = r#"
id = "x"
api_version = 1
[[hooks]]
event = "on_task_created"
script = "hooks/on_task_created.rhai"
"#;
        let m = PluginManifest::from_toml(toml_src).unwrap();
        assert!(m.subscribes_to("on_task_created"));
        assert!(!m.subscribes_to("on_tool_called"));
    }

    #[test]
    fn rejects_duplicate_display_within_single_plugin() {
        let toml_src = r#"
id = "x"
api_version = 1
[[commands]]
id = "a"
display = "/hi"
script = "commands/a.rhai"
[[commands]]
id = "b"
display = "/hi"
script = "commands/b.rhai"
"#;
        let err = PluginManifest::from_toml(toml_src).unwrap_err();
        assert!(
            matches!(err, ManifestError::Invalid(s) if s.contains("duplicate command.display"))
        );
    }

    #[test]
    fn rejects_tool_with_empty_description() {
        let toml_src = r#"
id = "x"
api_version = 1
[[tools]]
id = "t"
description = ""
schema = "tools/t.schema.json"
handler = "tools/t.rhai"
"#;
        let err = PluginManifest::from_toml(toml_src).unwrap_err();
        assert!(
            matches!(err, ManifestError::Invalid(s) if s.contains("tool.description must not be empty"))
        );
    }
}
