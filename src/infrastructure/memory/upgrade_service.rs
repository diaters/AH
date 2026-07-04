use std::{fs, path::PathBuf};

use anyhow::{Context, Result};
use crate::prelude::Resource;

use crate::domain::SharedKnowledgeUpgradeQueue;

#[derive(Resource, Debug, Clone)]
pub struct SharedKnowledgeUpgradeService {
    base_dir: PathBuf,
}

impl SharedKnowledgeUpgradeService {
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self {
            base_dir: base_dir.into(),
        }
    }

    pub fn default_path() -> Self {
        Self::new(".harness/memory/shared_knowledge")
    }

    pub fn persist(&self, queue: &SharedKnowledgeUpgradeQueue) -> Result<()> {
        fs::create_dir_all(&self.base_dir)
            .with_context(|| format!("failed to create upgrade dir {}", self.base_dir.display()))?;
        let path = self.base_dir.join("upgrades.json");
        let tmp_path = path.with_extension("json.tmp");
        let json = serde_json::to_string_pretty(queue)
            .context("failed to serialize shared knowledge upgrade queue")?;
        fs::write(&tmp_path, json)
            .with_context(|| format!("failed to write tmp file {}", tmp_path.display()))?;
        fs::rename(&tmp_path, &path).with_context(|| {
            format!(
                "failed to rename {} to {}",
                tmp_path.display(),
                path.display()
            )
        })?;
        Ok(())
    }

    pub fn load(&self) -> Result<SharedKnowledgeUpgradeQueue> {
        let path = self.base_dir.join("upgrades.json");
        if !path.exists() {
            return Ok(SharedKnowledgeUpgradeQueue::default());
        }
        let content = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        serde_json::from_str(&content)
            .with_context(|| format!("failed to parse {}", path.display()))
    }
}
