use std::{fs, path::PathBuf};

use anyhow::{Context, Result};
use bevy::prelude::Resource;
use uuid::Uuid;

/// 经验资产草稿：尚未持久化的文本资产。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExperienceAssetDraft {
    pub name: String,
    pub content: String,
}

/// Agent 资产仓服务：负责将文本资产写入 `.harness/assets/agents/<agent_name>/` 并返回稳定引用。
#[derive(Resource, Debug, Clone)]
pub struct AgentAssetService {
    base_dir: PathBuf,
}

impl AgentAssetService {
    /// 使用指定根目录创建服务。
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self {
            base_dir: base_dir.into(),
        }
    }

    /// 使用默认路径 `.harness/assets/agents` 创建服务。
    pub fn default_path() -> Self {
        Self::new(".harness/assets/agents")
    }

    /// 将文本资产批量写入仓中，返回相对路径引用列表。
    ///
    /// 每个资产存储为 `<agent_name>/<uuid>-<draft.name>`，
    /// 返回值可直接作为 `ExecutableMemoryEntry::asset_refs` 使用。
    pub fn persist_text_assets(
        &self,
        agent_name: &str,
        drafts: &[ExperienceAssetDraft],
    ) -> Result<Vec<String>> {
        let agent_dir = self.base_dir.join(agent_name);
        fs::create_dir_all(&agent_dir)
            .with_context(|| format!("failed to create asset dir {}", agent_dir.display()))?;

        drafts
            .iter()
            .map(|draft| {
                let file_name = format!("{}-{}", Uuid::new_v4(), draft.name);
                let relative = format!("{}/{}", agent_name, file_name);
                let path = self.base_dir.join(&relative);
                fs::write(&path, &draft.content)
                    .with_context(|| format!("failed to write asset {}", path.display()))?;
                Ok(relative)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn persist_text_assets_returns_readable_refs() {
        let dir = TempDir::new().unwrap();
        let service = AgentAssetService::new(dir.path().join("agents"));
        let refs = service
            .persist_text_assets(
                "default-agent",
                &[ExperienceAssetDraft {
                    name: "shell-smoke.sh".to_string(),
                    content: "echo ok\n".to_string(),
                }],
            )
            .unwrap();

        assert_eq!(refs.len(), 1);
        assert!(refs[0].contains("default-agent"));
        assert!(std::fs::read_to_string(dir.path().join("agents").join(&refs[0])).is_ok());
    }
}