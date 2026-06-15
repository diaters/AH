use std::fs;
use std::path::PathBuf;

use anyhow::Result;
use bevy::prelude::Resource;
use serde::{Deserialize, Serialize};

use crate::domain::AgentProfile;

/// 孵化出的持久型 Agent 记录。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncubatedAgentRecord {
    pub profile: AgentProfile,
    pub tags: Vec<String>,
    pub description: String,
    pub tools: Vec<String>,
}

/// 孵化 Agent 注册持久化服务。
#[derive(Resource, Debug, Clone)]
pub struct IncubatedAgentRegistry {
    path: PathBuf,
}

impl IncubatedAgentRegistry {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn default_path() -> Self {
        Self::new(PathBuf::from(".harness/incubation/agents.toml"))
    }

    /// 追加一条孵化 Agent 记录。
    pub fn append(&self, record: &IncubatedAgentRecord) -> Result<()> {
        let mut records = self.load()?;
        records.push(record.clone());
        self.save(&records)
    }

    /// 加载所有孵化 Agent 记录。
    pub fn load(&self) -> Result<Vec<IncubatedAgentRecord>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let content = fs::read_to_string(&self.path)?;
        let records: Vec<IncubatedAgentRecord> = serde_json::from_str(&content)?;
        Ok(records)
    }

    fn save(&self, records: &[IncubatedAgentRecord]) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(records)?;
        fs::write(&self.path, json)?;
        Ok(())
    }
}
