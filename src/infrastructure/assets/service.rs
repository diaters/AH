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
    pub problem: String,
    pub when_to_use: String,
    pub steps: String,
    pub asset_refs: Vec<String>,
    pub dependency_refs: Vec<String>,
    pub risks: String,
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

    /// 将 Skill Package 草稿落盘为文件目录，返回相对路径（如 `<agent_name>/skills/<skill_id>`）。
    pub fn persist_skill_package(
        &self,
        agent_name: &str,
        draft: &SkillPackageDraft,
    ) -> Result<String> {
        let relative = format!("{}/skills/{}", agent_name, draft.skill_id);
        let skill_dir = self.base_dir.join(&relative);
        fs::create_dir_all(skill_dir.join("scripts"))
            .with_context(|| format!("failed to create scripts dir for {}", skill_dir.display()))?;
        fs::create_dir_all(skill_dir.join("resources")).with_context(|| {
            format!("failed to create resources dir for {}", skill_dir.display())
        })?;

        let skill_md = format!(
            "# {}\n\n## 解决的问题\n{}\n\n## 什么时候使用\n{}\n\n## 使用步骤\n{}\n\n## 依赖脚本或资源说明\n- asset_refs: {:?}\n- dependency_refs: {:?}\n\n## 风险与限制\n{}\n\n## 来源追溯\n- task_id: {:?}\n- candidate_id: {:?}\n",
            draft.title,
            draft.problem,
            draft.when_to_use,
            draft.steps,
            draft.asset_refs,
            draft.dependency_refs,
            draft.risks,
            draft.source_task_id,
            draft.source_candidate_id,
        );

        fs::write(skill_dir.join("skill.md"), skill_md)
            .with_context(|| format!("failed to write skill.md for {}", skill_dir.display()))?;

        Ok(relative)
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
            problem: "验证 shell 工具链是否正常工作".to_string(),
            when_to_use: "修改 shell 相关代码后".to_string(),
            steps: "1. 运行脚本\n2. 检查输出".to_string(),
            asset_refs: vec!["script.sh".to_string()],
            dependency_refs: vec![],
            risks: "可能受环境差异影响".to_string(),
            source_task_id: Some(uuid::Uuid::new_v4()),
            source_candidate_id: Some(uuid::Uuid::new_v4()),
        };

        let relative = service.persist_skill_package("test-agent", &draft).unwrap();
        let base = dir.path().join("agents").join(&relative);

        assert!(base.join("skill.md").exists());
        assert!(base.join("scripts").is_dir());
        assert!(base.join("resources").is_dir());

        let skill_md = std::fs::read_to_string(base.join("skill.md")).unwrap();
        assert!(skill_md.contains(&draft.title));
        assert!(skill_md.contains("解决的问题"));
    }
}
