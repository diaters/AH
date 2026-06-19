use std::{fs, path::PathBuf};

use anyhow::{Context, Result};
use bevy::prelude::Resource;
use uuid::Uuid;

use crate::domain::TaskId;

/// 经验资产草稿：尚未持久化的文本资产。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExperienceAssetDraft {
    pub name: String,
    pub content: String,
}

/// Skill Package 草稿。
#[derive(Debug, Clone)]
pub struct SkillPackageDraft {
    pub skill_id: String,
    pub title: String,
    pub name: String,
    pub description: String,
    pub instructions: String,
    pub file_refs: Vec<crate::domain::SkillFileRef>,
    pub source_task_id: Option<TaskId>,
    pub source_candidate_id: Option<uuid::Uuid>,
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

    /// 将 Skill Package 草稿落盘为文件目录，返回相对路径（如 `<agent_name>/skills/<skill_name>`）。
    pub fn persist_skill_package(
        &self,
        agent_name: &str,
        draft: &SkillPackageDraft,
    ) -> Result<String> {
        let skill_name = draft
            .name
            .to_lowercase()
            .replace(' ', "-")
            .replace(|c: char| !c.is_alphanumeric() && c != '-', "");

        let skill_dir = self
            .base_dir
            .join(agent_name)
            .join("skills")
            .join(&skill_name);
        fs::create_dir_all(&skill_dir)
            .with_context(|| format!("failed to create skill dir {}", skill_dir.display()))?;

        // Generate SKILL.md
        let skill_md = format!(
            "---\nname: {}\ndescription: {}\n---\n\n{}\n",
            skill_name, draft.description, draft.instructions,
        );
        let skill_md_path = skill_dir.join("SKILL.md");
        fs::write(&skill_md_path, &skill_md)
            .with_context(|| format!("failed to write {}", skill_md_path.display()))?;

        // Copy file_refs to corresponding subdirectories
        for file_ref in &draft.file_refs {
            let sub_dir = match file_ref.role {
                crate::domain::SkillFileRole::Script => "scripts",
                crate::domain::SkillFileRole::Reference => "references",
                crate::domain::SkillFileRole::Asset => "assets",
            };
            let dest_dir = skill_dir.join(sub_dir);
            fs::create_dir_all(&dest_dir)
                .with_context(|| format!("failed to create {}", dest_dir.display()))?;

            let src_path = std::path::Path::new(&file_ref.path);
            let file_name = src_path
                .file_name()
                .ok_or_else(|| anyhow::anyhow!("invalid file path: {}", file_ref.path))?;
            let dest_path = dest_dir.join(file_name);

            if src_path.exists() {
                fs::copy(src_path, &dest_path).with_context(|| {
                    format!("failed to copy {} to {}", file_ref.path, dest_path.display())
                })?;
            }
        }

        Ok(format!("{}/skills/{}", agent_name, skill_name))
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

    #[test]
    fn persist_skill_package_creates_directory_and_skill_md() {
        let dir = tempfile::TempDir::new().unwrap();
        let service = AgentAssetService::new(dir.path().join("agents"));
        let draft = SkillPackageDraft {
            skill_id: "shell-smoke".to_string(),
            title: "Shell Smoke Test".to_string(),
            name: "shell-smoke-test".to_string(),
            description: "验证 shell 工具链是否正常工作".to_string(),
            instructions: "1. 运行脚本\n2. 检查输出".to_string(),
            file_refs: vec![],
            source_task_id: Some(uuid::Uuid::new_v4()),
            source_candidate_id: Some(uuid::Uuid::new_v4()),
        };

        let relative = service.persist_skill_package("test-agent", &draft).unwrap();
        let base = dir.path().join("agents").join(&relative);

        assert!(base.join("SKILL.md").exists());
        assert!(base.join("scripts").is_dir() || !base.join("scripts").exists());

        let skill_md = std::fs::read_to_string(base.join("SKILL.md")).unwrap();
        assert!(skill_md.contains(&draft.name));
        assert!(skill_md.contains("description"));
    }
}
