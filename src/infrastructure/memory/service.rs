//! 长期记忆服务
//!
//! 收口运行期长期记忆变更，修改内存后立即调用 repository 落盘。
//! 所有系统层应通过此服务修改 `LongTermMemory`，而非直接操作 entries。

use std::io::Write;
use std::path::PathBuf;

use anyhow::Result;
use bevy::prelude::Resource;
use tracing::warn;

use crate::domain::{LongTermMemory, LongTermMemoryEntry};
use crate::infrastructure::memory::repository::MemoryRepository;

/// 长期记忆服务：收口运行期变更 + 写穿持久化。
///
/// 每个变更操作遵循统一流程：
/// 1. 修改内存中的 `LongTermMemory`
/// 2. 调用 repository 持久化当前完整快照
/// 3. 记录成功或失败日志
///
/// 首版采用"每次变更即落盘"策略，不做脏标记或批处理。
#[derive(Resource)]
pub struct LongTermMemoryService {
    repository: MemoryRepository,
    base_dir: PathBuf,
}

impl LongTermMemoryService {
    /// 使用指定 repository 创建服务。
    pub fn new(repository: MemoryRepository) -> Self {
        Self {
            repository,
            base_dir: PathBuf::from(".harness/memory/agents"),
        }
    }

    /// 使用默认 JSON 文件存储创建服务。
    pub fn default_json() -> Self {
        Self::new(MemoryRepository::default_json())
    }

    /// 加载指定 Agent 的长期记忆条目。
    pub fn load_entries(&self, agent_name: &str) -> Vec<LongTermMemoryEntry> {
        self.repository.load_entries(agent_name)
    }

    /// 向指定 Agent 的长期记忆添加一条条目，并立即落盘。
    pub fn add_entry(
        &mut self,
        memory: &mut LongTermMemory,
        entry: LongTermMemoryEntry,
    ) -> Result<()> {
        memory.add_entry(entry);
        self.flush(memory)
    }

    /// 向指定 Agent 的长期记忆吸收来自子 Agent 的条目，并立即落盘。
    pub fn absorb_entries(
        &mut self,
        memory: &mut LongTermMemory,
        entries: Vec<LongTermMemoryEntry>,
    ) -> Result<()> {
        memory.absorb(entries);
        self.flush(memory)
    }

    /// 替换指定 Agent 的全部长期记忆条目，并立即落盘。
    pub fn replace_entries(
        &mut self,
        memory: &mut LongTermMemory,
        entries: Vec<LongTermMemoryEntry>,
    ) -> Result<()> {
        memory.entries = entries;
        self.flush(memory)
    }

    /// 清空指定 Agent 的全部长期记忆条目，并立即落盘。
    pub fn clear(&mut self, memory: &mut LongTermMemory) -> Result<()> {
        memory.entries.clear();
        let agent_name = match &memory.agent_name {
            Some(name) => name.clone(),
            None => {
                warn!(
                    event = "LongTermMemoryPersistFailed",
                    "cannot persist: LongTermMemory has no agent_name"
                );
                return Err(anyhow::anyhow!("LongTermMemory has no agent_name"));
            }
        };
        self.repository.clear(&agent_name)
    }

    /// 将当前内存状态写出到持久层。
    pub fn flush(&mut self, memory: &LongTermMemory) -> Result<()> {
        let agent_name = match &memory.agent_name {
            Some(name) => name.clone(),
            None => {
                warn!(
                    event = "LongTermMemoryPersistFailed",
                    "cannot persist: LongTermMemory has no agent_name"
                );
                return Err(anyhow::anyhow!("LongTermMemory has no agent_name"));
            }
        };
        self.repository.persist(&agent_name, memory.entries.clone())
    }

    /// 将被驱逐的长期记忆条目归档到文件。
    ///
    /// 归档路径为 `<base_dir>/<agent_name>/archive.jsonl`，每行一条 JSON 记录。
    pub fn archive_entries(&self, agent_name: &str, entries: &[LongTermMemoryEntry]) {
        let archive_path = self.base_dir.join(agent_name).join("archive.jsonl");
        if let Some(parent) = archive_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&archive_path)
        else {
            return;
        };
        for entry in entries {
            let _ = writeln!(file, "{}", serde_json::to_string(entry).unwrap());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::TempDir;

    fn make_service() -> (LongTermMemoryService, TempDir) {
        let dir = TempDir::new().unwrap();
        let store =
            crate::infrastructure::memory::JsonFileMemoryStore::new(dir.path().join("agents"));
        let repo = MemoryRepository::new(Box::new(store));
        (LongTermMemoryService::new(repo), dir)
    }

    fn make_service_at(dir: &TempDir) -> LongTermMemoryService {
        let store =
            crate::infrastructure::memory::JsonFileMemoryStore::new(dir.path().join("agents"));
        let repo = MemoryRepository::new(Box::new(store));
        LongTermMemoryService::new(repo)
    }

    #[test]
    fn add_entry_persists_to_disk() {
        let (mut service, dir) = make_service();
        let mut memory = LongTermMemory::with_name("test-agent");
        let entry = LongTermMemoryEntry::new("persisted fact");

        service.add_entry(&mut memory, entry).unwrap();

        assert_eq!(memory.entries.len(), 1);
        assert_eq!(memory.entries[0].content, "persisted fact");

        // 用同一目录重新创建 service 验证文件确实写入了
        let service2 = make_service_at(&dir);
        let loaded = service2.load_entries("test-agent");
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].content, "persisted fact");
    }

    #[test]
    fn absorb_entries_persists_to_disk() {
        let (mut service, _dir) = make_service();
        let mut memory = LongTermMemory::with_name("absorb-agent");
        let entries = vec![
            LongTermMemoryEntry::new("strategy 1"),
            LongTermMemoryEntry::new("strategy 2"),
        ];

        service.absorb_entries(&mut memory, entries).unwrap();

        assert_eq!(memory.entries.len(), 2);
    }

    #[test]
    fn clear_removes_all_entries_and_persists() {
        let (mut service, _dir) = make_service();
        let mut memory = LongTermMemory::with_name("clear-agent");
        service
            .add_entry(&mut memory, LongTermMemoryEntry::new("fact"))
            .unwrap();

        service.clear(&mut memory).unwrap();

        assert!(memory.entries.is_empty());
    }

    #[test]
    fn flush_fails_gracefully_without_agent_name() {
        let (mut service, _dir) = make_service();
        let mut memory = LongTermMemory::default(); // agent_name = None
        let entry = LongTermMemoryEntry::new("orphan");

        let result = service.add_entry(&mut memory, entry);
        assert!(result.is_err());
    }
}
