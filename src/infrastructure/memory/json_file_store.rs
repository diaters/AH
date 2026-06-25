//! JSON 文件持久化实现
//!
//! 每个 Agent 一个 JSON 文件，使用安全化 agent_name 作为文件名，
//! 通过临时文件 + rename 实现原子写入。

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::Utc;
use tracing::{debug, warn};

use crate::contracts::MemoryStore;
use crate::domain::{LongTermMemoryEntry, MemorySnapshot};

/// JSON 文件记忆存储。
///
/// 存储目录为 `.harness/memory/agents/`，
/// 每个 Agent 对应 `<safe_name>.json` 文件。
pub struct JsonFileMemoryStore {
    base_dir: PathBuf,
}

impl JsonFileMemoryStore {
    /// 创建指向指定根目录的存储实例。
    ///
    /// 不会立即创建目录，目录在首次写入时按需创建。
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self {
            base_dir: base_dir.into(),
        }
    }

    /// 使用默认路径 `.harness/memory/agents/` 创建存储实例。
    pub fn default_path() -> Self {
        Self::new(".harness/memory/agents")
    }

    /// 将 agent_name 安全化为文件名。
    ///
    /// - 统一转小写
    /// - 空格替换为下划线
    /// - 移除路径分隔符和其他危险字符
    pub fn sanitize_agent_name(agent_name: &str) -> String {
        agent_name
            .to_lowercase()
            .replace(' ', "_")
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
            .collect()
    }

    /// 获取指定 Agent 的快照文件路径。
    fn snapshot_path(&self, agent_name: &str) -> PathBuf {
        let safe_name = Self::sanitize_agent_name(agent_name);
        self.base_dir.join(format!("{}.json", safe_name))
    }

    /// 确保存储目录存在。
    fn ensure_dir(&self) -> Result<()> {
        if !self.base_dir.exists() {
            fs::create_dir_all(&self.base_dir).with_context(|| {
                format!("failed to create memory dir: {}", self.base_dir.display())
            })?;
        }
        Ok(())
    }
}

impl MemoryStore for JsonFileMemoryStore {
    fn get_entries(&self, agent_name: &str) -> Vec<LongTermMemoryEntry> {
        self.get_snapshot(agent_name)
            .map(|s| s.entries)
            .unwrap_or_default()
    }

    fn get_snapshot(&self, agent_name: &str) -> Option<MemorySnapshot> {
        let path = self.snapshot_path(agent_name);
        if !path.exists() {
            debug!(
                event = "LongTermMemoryLoaded",
                agent_name = agent_name,
                entries_count = 0,
                file_path = %path.display(),
                "no persisted memory file found, using empty memory"
            );
            return None;
        }

        let content = fs::read_to_string(&path).ok()?;
        match serde_json::from_str::<MemorySnapshot>(&content) {
            Ok(snapshot) => {
                debug!(
                    event = "LongTermMemoryLoaded",
                    agent_name = agent_name,
                    entries_count = snapshot.entries.len(),
                    file_path = %path.display(),
                    schema_version = snapshot.schema_version,
                    "loaded persisted memory"
                );
                Some(snapshot)
            }
            Err(e) => {
                warn!(
                    event = "LongTermMemoryLoadFailed",
                    agent_name = agent_name,
                    file_path = %path.display(),
                    error = %e,
                    "corrupted memory file, falling back to empty memory"
                );
                None
            }
        }
    }

    fn save_snapshot(&mut self, snapshot: &MemorySnapshot) -> Result<()> {
        self.ensure_dir()?;

        let path = self.snapshot_path(&snapshot.agent_name);
        let tmp_path = path.with_extension(format!("tmp.{}", uuid::Uuid::new_v4()));

        let mut updated = snapshot.clone();
        updated.updated_at = Utc::now();

        let json = serde_json::to_string_pretty(&updated).with_context(|| {
            format!(
                "failed to serialize snapshot for agent {}",
                snapshot.agent_name
            )
        })?;

        fs::write(&tmp_path, &json)
            .with_context(|| format!("failed to write tmp file {}", tmp_path.display()))?;

        // 原子替换：tmp -> 正式文件
        fs::rename(&tmp_path, &path).with_context(|| {
            format!(
                "failed to rename {} to {}",
                tmp_path.display(),
                path.display()
            )
        })?;

        debug!(
            event = "LongTermMemoryPersisted",
            agent_name = %snapshot.agent_name,
            entries_count = snapshot.entries.len(),
            file_path = %path.display(),
            schema_version = snapshot.schema_version,
            "persisted memory snapshot"
        );

        Ok(())
    }

    fn clear(&mut self, agent_name: &str) -> Result<()> {
        let path = self.snapshot_path(agent_name);
        if path.exists() {
            fs::remove_file(&path).with_context(|| {
                format!("failed to remove memory file for agent {}", agent_name)
            })?;
        }
        debug!(
            event = "LongTermMemoryCleared",
            agent_name = agent_name,
            file_path = %path.display(),
            "cleared persisted memory"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::MemoryImportance;
    use tempfile::TempDir;

    #[test]
    fn sanitize_agent_name_handles_special_characters() {
        assert_eq!(
            JsonFileMemoryStore::sanitize_agent_name("My Agent"),
            "my_agent"
        );
        assert_eq!(
            JsonFileMemoryStore::sanitize_agent_name("test/../../../etc"),
            "testetc"
        );
        assert_eq!(
            JsonFileMemoryStore::sanitize_agent_name("UPPER_CASE"),
            "upper_case"
        );
        assert_eq!(
            JsonFileMemoryStore::sanitize_agent_name("a-b_c.1"),
            "a-b_c1"
        );
    }

    #[test]
    fn get_entries_returns_empty_when_file_not_found() {
        let dir = TempDir::new().unwrap();
        let store = JsonFileMemoryStore::new(dir.path().join("agents"));

        let entries = store.get_entries("nonexistent");
        assert!(entries.is_empty());
    }

    #[test]
    fn get_entries_returns_empty_when_json_corrupted() {
        let dir = TempDir::new().unwrap();
        let base = dir.path().join("agents");
        fs::create_dir_all(&base).unwrap();
        fs::write(base.join("corrupted.json"), "not valid json").unwrap();

        let store = JsonFileMemoryStore::new(&base);
        let entries = store.get_entries("corrupted");
        assert!(entries.is_empty());
    }

    #[test]
    fn save_and_load_round_trip() {
        let dir = TempDir::new().unwrap();
        let mut store = JsonFileMemoryStore::new(dir.path().join("agents"));

        let mut entry = LongTermMemoryEntry::new("Always keep summaries concise");
        entry.importance = MemoryImportance::High;
        entry.confidence = 0.95;
        entry.scope_tags = vec!["summarization".to_string(), "memory".to_string()];

        let snapshot = MemorySnapshot::new("summarizer", vec![entry]);
        store.save_snapshot(&snapshot).unwrap();

        let loaded = store.get_snapshot("summarizer").unwrap();
        assert_eq!(loaded.agent_name, "summarizer");
        assert_eq!(loaded.entries.len(), 1);
        assert_eq!(loaded.entries[0].content, "Always keep summaries concise");
        assert_eq!(loaded.entries[0].importance, MemoryImportance::High);
    }

    #[test]
    fn save_creates_directory_if_missing() {
        let dir = TempDir::new().unwrap();
        let base = dir.path().join("new_sub_dir").join("agents");
        assert!(!base.exists());

        let mut store = JsonFileMemoryStore::new(&base);
        let snapshot = MemorySnapshot::new("test-agent", vec![]);
        store.save_snapshot(&snapshot).unwrap();

        assert!(base.exists());
        assert!(base.join("test-agent.json").exists());
    }

    #[test]
    fn atomic_write_replaces_old_content() {
        let dir = TempDir::new().unwrap();
        let mut store = JsonFileMemoryStore::new(dir.path().join("agents"));

        let entry1 = LongTermMemoryEntry::new("first fact");
        let snapshot1 = MemorySnapshot::new("updater", vec![entry1.clone()]);
        store.save_snapshot(&snapshot1).unwrap();

        let entry2 = LongTermMemoryEntry::new("second fact");
        let snapshot2 = MemorySnapshot::new("updater", vec![entry1, entry2]);
        store.save_snapshot(&snapshot2).unwrap();

        let loaded = store.get_snapshot("updater").unwrap();
        assert_eq!(loaded.entries.len(), 2);
    }

    #[test]
    fn tmp_file_cleaned_up_on_success() {
        let dir = TempDir::new().unwrap();
        let base = dir.path().join("agents");
        let mut store = JsonFileMemoryStore::new(&base);

        let snapshot = MemorySnapshot::new("cleanup", vec![]);
        store.save_snapshot(&snapshot).unwrap();

        assert!(!base.join("cleanup.json.tmp").exists());
        assert!(base.join("cleanup.json").exists());
    }

    #[test]
    fn clear_removes_file() {
        let dir = TempDir::new().unwrap();
        let mut store = JsonFileMemoryStore::new(dir.path().join("agents"));

        let snapshot = MemorySnapshot::new("deleteme", vec![]);
        store.save_snapshot(&snapshot).unwrap();
        assert!(store.get_snapshot("deleteme").is_some());

        store.clear("deleteme").unwrap();
        assert!(store.get_snapshot("deleteme").is_none());
    }

    #[test]
    fn clear_is_noop_when_file_missing() {
        let dir = TempDir::new().unwrap();
        let mut store = JsonFileMemoryStore::new(dir.path().join("agents"));
        // 不应报错
        store.clear("ghost").unwrap();
    }
}
