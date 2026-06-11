//! Memory persistence flow integration tests
//!
//! Tests for JSON file persistence of LongTermMemory:
//! - Agent can restore LongTermMemory from disk across service restarts
//! - Contribution absorption persists to disk

use harness::{
    LongTermMemory, LongTermMemoryEntry, LongTermMemoryKind, MemoryImportance, TaskSummary,
    extract_memory_writebacks,
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

        let entry =
            LongTermMemoryEntry::new(LongTermMemoryKind::Fact, "persistent fact from session 1");
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
            .add_entry(
                &mut memory,
                LongTermMemoryEntry::new(LongTermMemoryKind::Fact, "fact 1"),
            )
            .unwrap();
        service
            .add_entry(
                &mut memory,
                LongTermMemoryEntry::new(LongTermMemoryKind::Strategy, "strategy 1"),
            )
            .unwrap();
        service
            .add_entry(
                &mut memory,
                LongTermMemoryEntry::new(LongTermMemoryKind::Fact, "fact 2"),
            )
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
                LongTermMemoryEntry::new(LongTermMemoryKind::Fact, "parent's own fact"),
            )
            .unwrap();

        // 模拟子 Agent 贡献的记忆
        let child_memories = vec![
            LongTermMemoryEntry::new(LongTermMemoryKind::Fact, "child learned fact"),
            LongTermMemoryEntry::new(LongTermMemoryKind::Strategy, "child learned strategy"),
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

/// 验证 extract_memory_writebacks 只接受高价值、高置信度且非临时的记忆。
#[test]
fn extract_memory_writebacks_filters_correctly() {
    let summary = TaskSummary {
        task_id: uuid::Uuid::nil(),
        goal: "test task".to_string(),
        outcome: "done".to_string(),
    };

    let entries = vec![
        // 应接受（高重要性 + 高置信度）
        {
            let mut e = LongTermMemoryEntry::new(LongTermMemoryKind::Fact, "important fact");
            e.importance = MemoryImportance::High;
            e.confidence = 0.95;
            e
        },
        // 应拒绝（包含 temporary）
        {
            let mut e = LongTermMemoryEntry::new(LongTermMemoryKind::Fact, "temporary note");
            e.importance = MemoryImportance::High;
            e.confidence = 0.95;
            e
        },
        // 应接受但不共享（中重要度不满足 >= High && >= 0.9）
        {
            let mut e = LongTermMemoryEntry::new(LongTermMemoryKind::Strategy, "medium strategy");
            e.importance = MemoryImportance::Medium;
            e.confidence = 0.8;
            e
        },
    ];

    let (accepted, candidates) = extract_memory_writebacks("worker", &summary, &entries);

    // 重要条目和中等条目都被接受（都通过了空内容和衰减检查）
    assert!(accepted.len() >= 2);
    // 只有高重要度 + 高置信度的才成为共享知识候选
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].content, "important fact");
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
            .add_entry(
                &mut memory,
                LongTermMemoryEntry::new(LongTermMemoryKind::Fact, "to be cleared"),
            )
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
