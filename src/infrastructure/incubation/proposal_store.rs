use std::fs;
use std::path::PathBuf;

use crate::prelude::Resource;
use anyhow::Result;

use crate::domain::IncubationProposal;

/// 任务级孵化提案文件持久化。
#[derive(Resource, Debug, Clone)]
pub struct IncubationProposalStore {
    base_dir: PathBuf,
}

impl IncubationProposalStore {
    pub fn new(base_dir: PathBuf) -> Self {
        Self { base_dir }
    }

    pub fn default_path() -> Self {
        Self::new(PathBuf::from(".harness/incubation/proposals"))
    }

    /// 持久化提案到 JSON 文件。
    pub fn persist(&self, proposal: &IncubationProposal) -> Result<()> {
        fs::create_dir_all(&self.base_dir)?;
        let path = self.base_dir.join(format!("{}.json", proposal.proposal_id));
        let json = serde_json::to_string_pretty(proposal)?;
        fs::write(path, json)?;
        Ok(())
    }

    /// 加载所有已持久化的提案。
    pub fn load_all(&self) -> Result<Vec<IncubationProposal>> {
        if !self.base_dir.exists() {
            return Ok(Vec::new());
        }
        let mut proposals = Vec::new();
        for entry in fs::read_dir(&self.base_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "json") {
                let content = fs::read_to_string(&path)?;
                if let Ok(proposal) = serde_json::from_str::<IncubationProposal>(&content) {
                    proposals.push(proposal);
                }
            }
        }
        Ok(proposals)
    }
}
