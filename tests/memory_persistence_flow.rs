//! Memory persistence flow integration tests
//!
//! Tests for JSON file persistence of LongTermMemory:
//! - Agent can restore LongTermMemory from disk across service restarts
//! - Contribution absorption persists to disk

use harness::{
    domain::{LongTermMemory, LongTermMemoryEntry},
    infrastructure::memory::{JsonFileMemoryStore, LongTermMemoryService, MemoryRepository},
};

/// 验证 LongTermMemoryService 可以将记忆持久化到 JSON 文件，
/// 并在重新创建 service 后恢复这些记忆。
#[test]
fn agent_restores_long_term_memory_from_disk_across_restarts() {
    let dir = tempfile::TempDir::new().unwrap();
    let agent_name = "test-persist-agent";

    // 第一次：创建 service，添加记忆，写入磁盘
    {
        let store = JsonFileMemoryStore::new(dir.path().join("agents"));
        let repo = MemoryRepository::new(Box::new(store));
        let mut service = LongTermMemoryService::new(repo);
        let mut memory = LongTermMemory::with_name(agent_name);

        let entry = LongTermMemoryEntry::new("persistent fact from session 1");
        service.add_entry(&mut memory, entry).unwrap();

        assert_eq!(memory.entries.len(), 1);
    }

    // 第二次：用同一目录重建 service，验证记忆被恢复
    {
        let store = JsonFileMemoryStore::new(dir.path().join("agents"));
        let repo = MemoryRepository::new(Box::new(store));
        let service = LongTermMemoryService::new(repo);

        let entries = service.load_entries(agent_name);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].content, "persistent fact from session 1");
    }
}

/// 验证多次写入的记忆全部被持久化，重启后全部可读。
#[test]
fn multiple_entries_persist_and_restore_correctly() {
    let dir = tempfile::TempDir::new().unwrap();
    let agent_name = "multi-entry-agent";

    {
        let store = JsonFileMemoryStore::new(dir.path().join("agents"));
        let repo = MemoryRepository::new(Box::new(store));
        let mut service = LongTermMemoryService::new(repo);
        let mut memory = LongTermMemory::with_name(agent_name);

        service
            .add_entry(&mut memory, LongTermMemoryEntry::new("fact 1"))
            .unwrap();
        service
            .add_entry(&mut memory, LongTermMemoryEntry::new("strategy 1"))
            .unwrap();
        service
            .add_entry(&mut memory, LongTermMemoryEntry::new("fact 2"))
            .unwrap();

        assert_eq!(memory.entries.len(), 3);
    }

    {
        let store = JsonFileMemoryStore::new(dir.path().join("agents"));
        let repo = MemoryRepository::new(Box::new(store));
        let service = LongTermMemoryService::new(repo);
        let entries = service.load_entries(agent_name);

        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].content, "fact 1");
        assert_eq!(entries[1].content, "strategy 1");
        assert_eq!(entries[2].content, "fact 2");
    }
}

/// 验证贡献吸收后的记忆被持久化到磁盘。
#[test]
fn contribution_absorption_persists_to_disk() {
    let dir = tempfile::TempDir::new().unwrap();
    let parent_agent_name = "parent-agent";

    {
        let store = JsonFileMemoryStore::new(dir.path().join("agents"));
        let repo = MemoryRepository::new(Box::new(store));
        let mut service = LongTermMemoryService::new(repo);

        let mut parent_memory = LongTermMemory::with_name(parent_agent_name);
        service
            .add_entry(
                &mut parent_memory,
                LongTermMemoryEntry::new("parent's own fact"),
            )
            .unwrap();

        // 模拟子 Agent 贡献的记忆
        let child_memories = vec![
            LongTermMemoryEntry::new("child learned fact"),
            LongTermMemoryEntry::new("child learned strategy"),
        ];

        service
            .absorb_entries(&mut parent_memory, child_memories)
            .unwrap();

        assert_eq!(parent_memory.entries.len(), 3);
    }

    // 重启后验证
    {
        let store = JsonFileMemoryStore::new(dir.path().join("agents"));
        let repo = MemoryRepository::new(Box::new(store));
        let service = LongTermMemoryService::new(repo);
        let entries = service.load_entries(parent_agent_name);

        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].content, "parent's own fact");
        assert_eq!(entries[1].content, "child learned fact");
        assert_eq!(entries[2].content, "child learned strategy");
    }
}

/// 验证不存在持久化文件时 service 返回空记忆。
#[test]
fn load_from_nonexistent_dir_returns_empty() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = JsonFileMemoryStore::new(dir.path().join("nonexistent"));
    let repo = MemoryRepository::new(Box::new(store));
    let service = LongTermMemoryService::new(repo);

    let entries = service.load_entries("ghost-agent");
    assert!(entries.is_empty());
}

/// 验证 clear 操作清除所有记忆并持久化。
#[test]
fn clear_removes_all_entries_and_persists() {
    let dir = tempfile::TempDir::new().unwrap();
    let agent_name = "clearable-agent";

    {
        let store = JsonFileMemoryStore::new(dir.path().join("agents"));
        let repo = MemoryRepository::new(Box::new(store));
        let mut service = LongTermMemoryService::new(repo);
        let mut memory = LongTermMemory::with_name(agent_name);

        service
            .add_entry(&mut memory, LongTermMemoryEntry::new("to be cleared"))
            .unwrap();
        service.clear(&mut memory).unwrap();
        assert!(memory.entries.is_empty());
    }

    {
        let store = JsonFileMemoryStore::new(dir.path().join("agents"));
        let repo = MemoryRepository::new(Box::new(store));
        let service = LongTermMemoryService::new(repo);
        let entries = service.load_entries(agent_name);
        assert!(entries.is_empty());
    }
}
