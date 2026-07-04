//! 记忆仓储
//!
//! 对外暴露按 Agent 名称读写长期记忆的高层接口。
//! 内部持有 `Box<dyn MemoryStore>`，将领域模型与持久化细节隔离。

use crate::prelude::Resource;
use anyhow::Result;
use tracing::{debug, warn};

use crate::contracts::MemoryStore;
use crate::domain::{LongTermMemoryEntry, MemorySnapshot};

/// 记忆仓储：按 Agent 名称加载、写回、清空长期记忆。
///
/// 作为 `MemoryStore` 的高层封装，提供面向运行时的操作接口。
/// 所有长期记忆变更入口应通过此仓储走写穿路径。
#[derive(Resource)]
pub struct MemoryRepository {
    store: Box<dyn MemoryStore>,
}

impl MemoryRepository {
    /// 使用指定存储后端创建仓储。
    pub fn new(store: Box<dyn MemoryStore>) -> Self {
        Self { store }
    }

    /// 使用默认 JSON 文件存储创建仓储。
    pub fn default_json() -> Self {
        Self::new(Box::new(
            crate::infrastructure::memory::JsonFileMemoryStore::default_path(),
        ))
    }

    /// 加载指定 Agent 的长期记忆条目。
    ///
    /// 如果文件不存在，返回空 vec；如果文件损坏，记录警告后返回空 vec。
    pub fn load_entries(&self, agent_name: &str) -> Vec<LongTermMemoryEntry> {
        self.store.get_entries(agent_name)
    }

    /// 加载指定 Agent 的完整快照。
    pub fn load_snapshot(&self, agent_name: &str) -> Option<MemorySnapshot> {
        self.store.get_snapshot(agent_name)
    }

    /// 将指定 Agent 的长期记忆条目持久化。
    ///
    /// 每次调用都会覆盖该 Agent 的完整快照。
    pub fn persist(&mut self, agent_name: &str, entries: Vec<LongTermMemoryEntry>) -> Result<()> {
        let snapshot = MemorySnapshot::new(agent_name, entries);
        match self.store.save_snapshot(&snapshot) {
            Ok(()) => {
                debug!(
                    event = "LongTermMemoryPersisted",
                    agent_name = agent_name,
                    entries_count = snapshot.entries.len(),
                    "persisted long-term memory via repository"
                );
                Ok(())
            }
            Err(e) => {
                warn!(
                    event = "LongTermMemoryPersistFailed",
                    agent_name = agent_name,
                    error = %e,
                    "failed to persist long-term memory"
                );
                Err(e)
            }
        }
    }

    /// 清空指定 Agent 的持久化记忆。
    pub fn clear(&mut self, agent_name: &str) -> Result<()> {
        match self.store.clear(agent_name) {
            Ok(()) => {
                debug!(
                    event = "LongTermMemoryCleared",
                    agent_name = agent_name,
                    "cleared persisted long-term memory via repository"
                );
                Ok(())
            }
            Err(e) => {
                warn!(
                    event = "LongTermMemoryPersistFailed",
                    agent_name = agent_name,
                    error = %e,
                    "failed to clear long-term memory"
                );
                Err(e)
            }
        }
    }
}
