//! `agents.toml` 读取与启动期校验。

use std::fs;
use std::path::Path;

use anyhow::Result;

use crate::domain::AgentConfig;

/// 读取并解析 `agents.toml`（agents 配置解析的唯一权威入口）。
///
/// 语义：
/// - 文件不存在 → `Ok(None)`：调用方按需容错（如 warn 后跳过）；
/// - 读取或解析失败 → `Err`：配置已损坏必须显式暴露。
pub fn load_agent_config(path: &Path) -> Result<Option<AgentConfig>> {
    if !path.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(path)?;
    let config = toml::from_str::<AgentConfig>(&content)
        .map_err(|e| anyhow::anyhow!("failed to parse {}: {e}", path.display()))?;
    Ok(Some(config))
}

/// 启动期校验：配置存在时必须可解析且无重名 agent。
///
/// 供 `main` 在装配前调用，配置错误以进程退出码暴露而非运行期 panic。
pub fn validate_agent_config(path: &str) -> Result<()> {
    let Some(config) = load_agent_config(Path::new(path))? else {
        return Ok(());
    };
    let mut seen = std::collections::HashSet::new();
    for entry in &config.agent {
        if !seen.insert(entry.name.as_str()) {
            anyhow::bail!("duplicate agent name '{}' in {}", entry.name, path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agents.toml");
        assert!(load_agent_config(&path).unwrap().is_none());
        assert!(validate_agent_config(path.to_str().unwrap()).is_ok());
    }

    #[test]
    fn tools_overrides_parse() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agents.toml");
        fs::write(
            &path,
            "[[agent]]\nname = \"a\"\ntags = []\ndescription = \"\"\n\n[agent.tools]\ndefault_permission = \"Allow\"\nshell_exec = \"Deny\"\n",
        )
        .unwrap();
        let config = load_agent_config(&path).unwrap().unwrap();
        let tools = config.agent[0].tools.as_ref().unwrap();
        assert_eq!(
            tools.default_permission,
            Some(crate::domain::ToolPermission::Allow)
        );
        assert_eq!(
            tools.overrides.get("shell_exec"),
            Some(&crate::domain::ToolPermission::Deny)
        );
    }

    #[test]
    fn duplicate_names_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agents.toml");
        fs::write(
            &path,
            "[[agent]]\nname = \"a\"\ntags = []\ndescription = \"\"\n\n[[agent]]\nname = \"a\"\ntags = []\ndescription = \"\"\n",
        )
        .unwrap();
        assert!(validate_agent_config(path.to_str().unwrap()).is_err());
    }

    #[test]
    fn malformed_file_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agents.toml");
        fs::write(&path, "[[agent]\nname=").unwrap();
        assert!(load_agent_config(&path).is_err());
    }
}
