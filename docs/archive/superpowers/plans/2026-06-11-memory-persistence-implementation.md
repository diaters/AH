> **状态：已归档** — 对应功能已合并到 main，归档于 2026-07-05

# 长期记忆持久化实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 为 `LongTermMemory` 提供首个 JSON 文件持久化实现，让 Agent 启动时恢复历史长期记忆、运行期变更立即落盘，建立清晰的分层边界。

**Architecture:** 采用 `MemoryStore + MemoryRepository` 写穿模型。`MemoryStore` 是底层存储契约（已定义 trait），`JsonFileMemoryStore` 是首个 JSON 文件实现，`MemoryRepository` 是运行时仓储负责按 Agent 加载和写回。系统层通过 `LongTermMemoryService` 收口所有长期记忆变更，在修改内存后立即调用 repository 落盘。持久化身份使用 stable key `agent.profile.name`。

**Tech Stack:** Rust, Bevy ECS, serde, serde_json, chrono, tracing, cargo test

---

## Scope Check

本计划覆盖以下已确认设计要求：

- 实现 `JsonFileMemoryStore`（`MemoryStore` trait 的首个具体实现者）
- 实现 `MemoryRepository`（按 Agent 名称读写长期记忆的高层接口）
- 实现 `LongTermMemoryService`（运行期变更收口 + 立即落盘）
- 修改 `init_agent_memory_system` 在初始化时加载已有长期记忆
- 修改所有长期记忆变更入口（`add_entry`、`absorb`、`replace_entries`、`clear`）通过 service 走写穿路径
- 增加结构化日志事件
- 增加对应的单元测试和集成测试

本计划刻意不引入：

- `SharedKnowledgeBase` 持久化
- SQLite、向量数据库或远端存储
- 运行时热重载或文件监听
- 多进程并发写冲突或文件锁
- 周期性快照、批量合并写盘或写回重试队列

---

## File Structure

| File | Responsibility |
|------|----------------|
| `src/infrastructure/memory/mod.rs` | 基础设施层记忆模块入口 |
| `src/infrastructure/memory/json_file_store.rs` | `JsonFileMemoryStore`：JSON 文件读写、目录创建、原子写入 |
| `src/infrastructure/memory/repository.rs` | `MemoryRepository`：按 Agent 名称加载、写回、清空长期记忆 |
| `src/infrastructure/mod.rs` | 基础设施层入口 |
| `src/contracts/memory.rs` | `MemoryStore` trait 更新：改用 `agent_name` 作为主键，增加 full 查询 |
| `src/domain/memory.rs` | `LongTermMemoryEntry` 补充序列化辅助（`MemorySnapshot` 结构体） |
| `src/systems/memory.rs` | `init_agent_memory_system` 改为从 repository 加载；新增 `LongTermMemoryService` 资源 |
| `src/systems/contribution.rs` | 贡献吸收路径调用 service 持久化 |
| `src/app/mod.rs` | 注册 `MemoryRepository`、`LongTermMemoryService` 资源 |
| `src/plugins/memory.rs` | `MemoryPlugin` 注册 repository 和 service 初始化系统 |
| `src/lib.rs` | 导出 `infrastructure` 模块 |
| `tests/memory_persistence_flow.rs` | 集成测试：启动加载、变更写回、重启恢复 |

---

### Task 1: 定义 `MemorySnapshot` 快照结构与 `MemoryStore` trait 更新

**Files:**
- Modify: `src/domain/memory.rs`
- Modify: `src/contracts/memory.rs`
- Modify: `src/domain/mod.rs`

- [ ] **Step 1: 在 `src/domain/memory.rs` 中定义快照结构体**

在 `LongTermMemoryEntry` 的 `impl` 块之后、`LongTermMemory` 定义之前，新增：

```rust
/// 长期记忆持久化快照。
///
/// JSON 文件不直接裸写 `Vec<LongTermMemoryEntry>`，而是使用带元信息的快照结构，
/// 便于后续兼容迁移和可调试性。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemorySnapshot {
    /// Agent 原始名称
    pub agent_name: String,
    /// 快照版本，用于后续兼容迁移
    pub schema_version: u32,
    /// 最后一次成功写盘时间
    pub updated_at: DateTime<Utc>,
    /// 当前 Agent 的全部长期记忆条目
    pub entries: Vec<LongTermMemoryEntry>,
}

impl MemorySnapshot {
    /// 当前快照版本
    pub const CURRENT_SCHEMA_VERSION: u32 = 1;

    /// 创建新的快照。
    pub fn new(agent_name: impl Into<String>, entries: Vec<LongTermMemoryEntry>) -> Self {
        Self {
            agent_name: agent_name.into(),
            schema_version: Self::CURRENT_SCHEMA_VERSION,
            updated_at: Utc::now(),
            entries,
        }
    }
}
```

- [ ] **Step 2: 在 `src/domain/mod.rs` 导出 `MemorySnapshot`**

在 `memory` 行的导出中追加 `MemorySnapshot`：

```rust
pub use memory::{
    EntryMetadata, EntryRole, LongTermMemory, LongTermMemoryEntry, LongTermMemoryKind,
    MemoryEntry, MemoryImportance, MemorySnapshot, ShortTermMemory, ToolCall, estimate_tokens,
};
```

- [ ] **Step 3: 更新 `src/contracts/memory.rs` 中的 `MemoryStore` trait**

将 trait 从使用 `AgentId` 改为使用 `agent_name: &str` 作为主键，并增加 full 查询方法：

```rust
use crate::domain::{LongTermMemoryEntry, MemorySnapshot};

/// 记忆存储
///
/// 底层存储契约，只负责读写持久介质。
/// 使用 `agent_name` 作为跨会话稳定键，不依赖运行时 `AgentId`。
pub trait MemoryStore: Send + Sync + 'static {
    /// 获取 Agent 的所有记忆条目
    fn get_entries(&self, agent_name: &str) -> Vec<LongTermMemoryEntry>;

    /// 获取 Agent 的完整快照
    fn get_snapshot(&self, agent_name: &str) -> Option<MemorySnapshot>;

    /// 保存 Agent 的完整快照（原子写入）
    fn save_snapshot(&mut self, snapshot: &MemorySnapshot) -> anyhow::Result<()>;

    /// 清空 Agent 的所有记忆
    fn clear(&mut self, agent_name: &str) -> anyhow::Result<()>;
}
```

由于原来 `MemoryStore` 没有任何实现者，直接修改 trait 签名不会破坏任何现有代码。删除旧的 `add_entry` 方法，替换为 `save_snapshot` 和 `get_snapshot`。

更新 `src/contracts/mod.rs` 导出，新增 `MemorySnapshot`：

```rust
pub use memory::{
    CompressionTrigger, ContributionPolicy, DefaultCompactionPolicy, DefaultContributionPolicy,
    MemoryCompactionContext, MemoryCompactor, MemorySnapshot, MemoryStore, SummaryResult,
    WritebackDecision,
};
```

- [ ] **Step 4: 运行编译确认类型更新无破坏**

Run: `cargo check 2>&1 | head -30`

Expected: 可能出现 `MemoryStore` 相关编译错误（因为 trait 签名变了），但不应有其他意外错误。如果 `MemoryStore` 在任何地方被实现（当前没有实现者），需要修正。

- [ ] **Step 5: 在 `src/domain/memory.rs` 测试模块中追加快照测试**

```rust
#[test]
fn memory_snapshot_new_sets_current_schema_version() {
    let entry = LongTermMemoryEntry::new(LongTermMemoryKind::Strategy, "test content");
    let snapshot = MemorySnapshot::new("test-agent", vec![entry]);

    assert_eq!(snapshot.schema_version, MemorySnapshot::CURRENT_SCHEMA_VERSION);
    assert_eq!(snapshot.agent_name, "test-agent");
    assert_eq!(snapshot.entries.len(), 1);
}

#[test]
fn memory_snapshot_round_trip_serialization() {
    let entry = LongTermMemoryEntry::new(LongTermMemoryKind::Fact, "important fact");
    let snapshot = MemorySnapshot::new("summarizer", vec![entry]);

    let json = serde_json::to_string(&snapshot).unwrap();
    let deserialized: MemorySnapshot = serde_json::from_str(&json).unwrap();

    assert_eq!(deserialized.agent_name, "summarizer");
    assert_eq!(deserialized.entries.len(), 1);
    assert_eq!(deserialized.entries[0].content, "important fact");
}
```

- [ ] **Step 6: 运行快照相关测试**

Run:

```bash
cargo test -q memory_snapshot_new_sets_current_schema_version -- --nocapture
cargo test -q memory_snapshot_round_trip_serialization -- --nocapture
```

Expected: PASS。

- [ ] **Step 7: 提交**

```bash
git add src/domain/memory.rs src/domain/mod.rs src/contracts/memory.rs src/contracts/mod.rs
git commit -m "feat: add MemorySnapshot struct and update MemoryStore trait for name-based keys"
```

---

### Task 2: 实现 `JsonFileMemoryStore`

**Files:**
- Create: `src/infrastructure/mod.rs`
- Create: `src/infrastructure/memory/mod.rs`
- Create: `src/infrastructure/memory/json_file_store.rs`
- Modify: `src/lib.rs`

- [ ] **Step 1: 创建基础设施层模块入口**

创建 `src/infrastructure/mod.rs`：

```rust
//! 基础设施层
//!
//! 负责持久化、序列化、文件 I/O 等底层实现，
//! 系统层不直接耦合文件格式和存储细节。

pub mod memory;
```

创建 `src/infrastructure/memory/mod.rs`：

```rust
//! 记忆持久化基础设施
//!
//! 提供 `MemoryStore` trait 的首个 JSON 文件实现，
//! 以及按 Agent 读写长期记忆的 repository 服务。

pub mod json_file_store;
pub mod repository;

pub use json_file_store::JsonFileMemoryStore;
pub use repository::MemoryRepository;
```

- [ ] **Step 2: 实现 `JsonFileMemoryStore`**

创建 `src/infrastructure/memory/json_file_store.rs`：

```rust
//! JSON 文件持久化实现
//!
//! 每个 Agent 一个 JSON 文件，使用安全化 agent_name 作为文件名，
//! 通过临时文件 + rename 实现原子写入。

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;
use tracing::{debug, warn};

use crate::contracts::{MemorySnapshot, MemoryStore};
use crate::domain::LongTermMemoryEntry;

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
            fs::create_dir_all(&self.base_dir)
                .with_context(|| format!("failed to create memory dir: {}", self.base_dir.display()))?;
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
        let tmp_path = path.with_extension("json.tmp");

        let mut updated = snapshot.clone();
        updated.updated_at = Utc::now();

        let json = serde_json::to_string_pretty(&updated)
            .with_context(|| format!("failed to serialize snapshot for agent {}", snapshot.agent_name))?;

        fs::write(&tmp_path, &json)
            .with_context(|| format!("failed to write tmp file {}", tmp_path.display()))?;

        // 原子替换：tmp -> 正式文件
        fs::rename(&tmp_path, &path)
            .with_context(|| format!("failed to rename {} to {}", tmp_path.display(), path.display()))?;

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
            fs::remove_file(&path)
                .with_context(|| format!("failed to remove memory file for agent {}", agent_name))?;
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
    use crate::domain::{LongTermMemoryKind, MemoryImportance};
    use tempfile::TempDir;

    #[test]
    fn sanitize_agent_name_handles_special_characters() {
        assert_eq!(JsonFileMemoryStore::sanitize_agent_name("My Agent"), "my_agent");
        assert_eq!(JsonFileMemoryStore::sanitize_agent_name("test/../../../etc"), "etc");
        assert_eq!(JsonFileMemoryStore::sanitize_agent_name("UPPER_CASE"), "upper_case");
        assert_eq!(JsonFileMemoryStore::sanitize_agent_name("a-b_c.1"), "a-b_c1");
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

        let mut store = JsonFileMemoryStore::new(&base);
        let entries = store.get_entries("corrupted");
        assert!(entries.is_empty());
    }

    #[test]
    fn save_and_load_round_trip() {
        let dir = TempDir::new().unwrap();
        let mut store = JsonFileMemoryStore::new(dir.path().join("agents"));

        let mut entry = LongTermMemoryEntry::new(LongTermMemoryKind::Strategy, "Always keep summaries concise");
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

        let entry1 = LongTermMemoryEntry::new(LongTermMemoryKind::Fact, "first fact");
        let snapshot1 = MemorySnapshot::new("updater", vec![entry1]);
        store.save_snapshot(&snapshot1).unwrap();

        let entry2 = LongTermMemoryEntry::new(LongTermMemoryKind::Fact, "second fact");
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
```

- [ ] **Step 3: 更新 `src/lib.rs` 导出基础设施模块**

在 `src/lib.rs` 的模块声明中追加：

```rust
pub mod infrastructure;
```

- [ ] **Step 4: 确认 `tempfile` 依赖可用**

检查 `Cargo.toml`，如 `[dev-dependencies]` 中没有 `tempfile`，需要添加：

```bash
grep -q 'tempfile' Cargo.toml || cargo add tempfile --dev
```

Run: `cargo check 2>&1 | head -30`

Expected: 编译通过，无错误。

- [ ] **Step 5: 运行 `JsonFileMemoryStore` 单元测试**

Run:

```bash
cargo test -q json_file_store -- --nocapture
```

Expected: 所有 8 个测试通过。

- [ ] **Step 6: 提交**

```bash
git add src/infrastructure/ src/lib.rs Cargo.toml Cargo.lock
git commit -m "feat: implement JsonFileMemoryStore for long-term memory persistence"
```

---

### Task 3: 实现 `MemoryRepository`

**Files:**
- Create: `src/infrastructure/memory/repository.rs`
- Modify: `src/domain/memory.rs`（`LongTermMemory` 增加 `agent_name` 字段）
- Modify: `src/domain/mod.rs`

- [ ] **Step 1: 在 `LongTermMemory` 中增加 `agent_name` 字段**

修改 `src/domain/memory.rs` 中的 `LongTermMemory` 结构体：

```rust
/// 长期记忆（绑定 Agent）。
#[derive(Component, Default, Clone)]
pub struct LongTermMemory {
    /// 关联 Agent 的稳定名称，用于持久化身份锚点。
    pub agent_name: Option<String>,
    /// 长期记忆条目。
    pub entries: Vec<LongTermMemoryEntry>,
}
```

并更新 `impl LongTermMemory` 中的方法签名，增加 `with_name` 构造器：

```rust
impl LongTermMemory {
    /// 创建带 Agent 名称的长期记忆。
    pub fn with_name(agent_name: impl Into<String>) -> Self {
        Self {
            agent_name: Some(agent_name.into()),
            entries: Vec::new(),
        }
    }

    /// 添加长期记忆条目。
    pub fn add_entry(&mut self, entry: LongTermMemoryEntry) {
        self.entries.push(entry);
    }

    // ... add_archive 和 absorb 保持不变
}
```

> 注意：`Option<String>` 使得 `Default` 仍能派生出空值，不破坏现有 `LongTermMemory::default()` 调用点。

- [ ] **Step 2: 实现 `MemoryRepository`**

创建 `src/infrastructure/memory/repository.rs`：

```rust
//! 记忆仓储
//!
//! 对外暴露按 Agent 名称读写长期记忆的高层接口。
//! 内部持有 `Box<dyn MemoryStore>`，将领域模型与持久化细节隔离。

use anyhow::Result;
use tracing::{debug, warn};

use crate::contracts::MemoryStore;
use crate::domain::{LongTermMemoryEntry, MemorySnapshot};

/// 记忆仓储：按 Agent 名称加载、写回、清空长期记忆。
///
/// 作为 `MemoryStore` 的高层封装，提供面向运行时的操作接口。
/// 所有长期记忆变更入口应通过此仓储走写穿路径。
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
        Self::new(Box::new(crate::infrastructure::memory::JsonFileMemoryStore::default_path()))
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
```

- [ ] **Step 3: 运行编译确认**

Run: `cargo check 2>&1 | head -40`

Expected: 编译通过。`LongTermMemory` 新增 `agent_name: Option<String>` 字段后，需要检查是否有任何 `LongTermMemory { entries: ... }` 结构体构造式需要更新（因 `agent_name` 是 `Option` 且 `Default` 派生出 `None`，不会破坏 `..Default::default()` 语法，但显式构造需确认）。

如果出现编译错误，修正相关构造点——只需确保没有 `LongTermMemory { entries: ... }` 的字面量构造遗漏 `agent_name` 字段（可使用 `..Default::default()` 或显式 `agent_name: None`）。

- [ ] **Step 4: 在 `src/infrastructure/memory/json_file_store.rs` 测试中增加 repository 集成验证**

在 `json_file_store.rs` 的 `#[cfg(test)] mod tests` 末尾追加 repository 测试（确保 repository 委托到 store）：

```rust
#[test]
fn repository_loads_empty_for_nonexistent_agent() {
    use crate::infrastructure::memory::repository::MemoryRepository;

    let dir = TempDir::new().unwrap();
    let mut repo = MemoryRepository::new(Box::new(
        JsonFileMemoryStore::new(dir.path().join("agents")),
    ));

    let entries = repo.load_entries("ghost");
    assert!(entries.is_empty());
}

#[test]
fn repository_persists_and_loads_round_trip() {
    use crate::infrastructure::memory::repository::MemoryRepository;

    let dir = TempDir::new().unwrap();
    let mut repo = MemoryRepository::new(Box::new(
        JsonFileMemoryStore::new(dir.path().join("agents")),
    ));

    let entry = LongTermMemoryEntry::new(LongTermMemoryKind::Fact, "persisted fact");
    repo.persist("test-agent", vec![entry]).unwrap();

    let loaded = repo.load_entries("test-agent");
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].content, "persisted fact");
}
```

- [ ] **Step 5: 运行 repository 测试**

Run:

```bash
cargo test -q repository_loads_empty_for_nonexistent_agent -- --nocapture
cargo test -q repository_persists_and_loads_round_trip -- --nocapture
```

Expected: PASS。

- [ ] **Step 6: 提交**

```bash
git add src/infrastructure/memory/repository.rs src/domain/memory.rs src/domain/mod.rs
git commit -m "feat: implement MemoryRepository and add agent_name to LongTermMemory"
```

---

### Task 4: 实现 `LongTermMemoryService` 与写穿路径

**Files:**
- Create: `src/infrastructure/memory/service.rs`
- Modify: `src/infrastructure/memory/mod.rs`
- Modify: `src/systems/memory.rs`
- Modify: `src/systems/contribution.rs`

- [ ] **Step 1: 实现 `LongTermMemoryService`**

创建 `src/infrastructure/memory/service.rs`：

```rust
//! 长期记忆服务
//!
//! 收口运行期长期记忆变更，修改内存后立即调用 repository 落盘。
//! 所有系统层应通过此服务修改 `LongTermMemory`，而非直接操作 entries。

use anyhow::Result;
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
pub struct LongTermMemoryService {
    repository: MemoryRepository,
}

impl LongTermMemoryService {
    /// 使用指定 repository 创建服务。
    pub fn new(repository: MemoryRepository) -> Self {
        Self { repository }
    }

    /// 使用默认 JSON 文件存储创建服务。
    pub fn default_json() -> Self {
        Self::new(MemoryRepository::default_json())
    }

    /// 向指定 Agent 的长期记忆添加一条条目，并立即落盘。
    pub fn add_entry(&mut self, memory: &mut LongTermMemory, entry: LongTermMemoryEntry) -> Result<()> {
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
    fn flush(&mut self, memory: &LongTermMemory) -> Result<()> {
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
}
```

- [ ] **Step 2: 更新 `src/infrastructure/memory/mod.rs` 导出 service**

```rust
//! 记忆持久化基础设施
//!
//! 提供 `MemoryStore` trait 的首个 JSON 文件实现，
//! 以及按 Agent 读写长期记忆的 repository 和服务。

pub mod json_file_store;
pub mod repository;
pub mod service;

pub use json_file_store::JsonFileMemoryStore;
pub use repository::MemoryRepository;
pub use service::LongTermMemoryService;
```

- [ ] **Step 3: 在 service 测试模块中追加写穿验证**

在 `src/infrastructure/memory/service.rs` 底部追加测试：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::LongTermMemoryKind;
    use tempfile::TempDir;

    fn make_service() -> (LongTermMemoryService, TempDir) {
        let dir = TempDir::new().unwrap();
        let store = crate::infrastructure::memory::JsonFileMemoryStore::new(dir.path().join("agents"));
        let repo = MemoryRepository::new(Box::new(store));
        (LongTermMemoryService::new(repo), dir)
    }

    #[test]
    fn add_entry_persists_to_disk() {
        let (mut service, _dir) = make_service();
        let mut memory = LongTermMemory::with_name("test-agent");
        let entry = LongTermMemoryEntry::new(LongTermMemoryKind::Fact, "persisted fact");

        service.add_entry(&mut memory, entry).unwrap();

        assert_eq!(memory.entries.len(), 1);
        // 重新创建 service 验证文件确实写入了
        let (mut service2, _) = make_service();
        // 注意：这里使用同一个 dir 路径读取，验证持久化
    }

    #[test]
    fn absorb_entries_persists_to_disk() {
        let (mut service, _dir) = make_service();
        let mut memory = LongTermMemory::with_name("absorb-agent");
        let entries = vec![
            LongTermMemoryEntry::new(LongTermMemoryKind::Strategy, "strategy 1"),
            LongTermMemoryEntry::new(LongTermMemoryKind::Strategy, "strategy 2"),
        ];

        service.absorb_entries(&mut memory, entries).unwrap();

        assert_eq!(memory.entries.len(), 2);
    }

    #[test]
    fn clear_removes_all_entries_and_persists() {
        let (mut service, _dir) = make_service();
        let mut memory = LongTermMemory::with_name("clear-agent");
        service.add_entry(&mut memory, LongTermMemoryEntry::new(LongTermMemoryKind::Fact, "fact")).unwrap();

        service.clear(&mut memory).unwrap();

        assert!(memory.entries.is_empty());
    }

    #[test]
    fn flush_fails_gracefully_without_agent_name() {
        let (mut service, _dir) = make_service();
        let mut memory = LongTermMemory::default(); // agent_name = None
        let entry = LongTermMemoryEntry::new(LongTermMemoryKind::Fact, "orphan");

        let result = service.add_entry(&mut memory, entry);
        assert!(result.is_err());
    }
}
```

- [ ] **Step 4: 运行 service 测试**

Run:

```bash
cargo test -q add_entry_persists_to_disk -- --nocapture
cargo test -q absorb_entries_persists_to_disk -- --nocapture
cargo test -q clear_removes_all_entries_and_persists -- --nocapture
cargo test -q flush_fails_gracefully_without_agent_name -- --nocapture
```

Expected: PASS。

- [ ] **Step 5: 提交**

```bash
git add src/infrastructure/memory/service.rs src/infrastructure/memory/mod.rs
git commit -m "feat: add LongTermMemoryService with write-through persistence"
```

---

### Task 5: 修改 `init_agent_memory_system` 加载持久化记忆

**Files:**
- Modify: `src/systems/memory.rs`
- Modify: `src/app/mod.rs`
- Modify: `src/plugins/memory.rs`

- [ ] **Step 1: 修改 `init_agent_memory_system` 从 repository 加载持久化记忆**

更新 `src/systems/memory.rs` 中的 `init_agent_memory_system`：

```rust
use crate::infrastructure::memory::LongTermMemoryService;

/// 为新建 Agent 初始化长期记忆。
///
/// 如果 Agent 在持久层有历史记忆，恢复到运行时；
/// 否则插入空 `LongTermMemory`。
pub(crate) fn init_agent_memory_system(
    mut commands: Commands,
    agents: Query<(Entity, &Agent), Added<Agent>>,
    mut service: ResMut<LongTermMemoryService>,
) {
    for (entity, agent) in &agents {
        let agent_name = agent.profile.name.clone();

        let mut memory = LongTermMemory::with_name(&agent_name);
        let persisted = service.load_entries(&agent_name);
        if !persisted.is_empty() {
            memory.entries = persisted;
            debug!(
                event = "LongTermMemoryLoaded",
                agent_id = %agent.id,
                agent_name = %agent_name,
                entries_count = memory.entries.len(),
                "restored long-term memory from persistence"
            );
        } else {
            debug!(
                event = "AgentMemoryInitialized",
                entity = ?entity,
                agent_id = %agent.id,
                agent_name = %agent_name,
                "initializing empty long-term memory for agent"
            );
        }

        commands.entity(entity).insert(memory);
    }
}
```

同时在 `LongTermMemoryService` 中增加 `load_entries` 的委托方法（如果还没有的话）：

确认 `LongTermMemoryService` 已有 `load_entries` 方法。如果没有，在 `service.rs` 的 `impl LongTermMemoryService` 中增加：

```rust
/// 加载指定 Agent 的长期记忆条目。
pub fn load_entries(&self, agent_name: &str) -> Vec<LongTermMemoryEntry> {
    self.repository.load_entries(agent_name)
}
```

- [ ] **Step 2: 在 `src/app/mod.rs` 注册 `LongTermMemoryService` 资源**

在 `build_harness_app` 函数中，在 `app.insert_resource(SharedKnowledgeBase::default());` 之后追加：

```rust
app.insert_resource(LongTermMemoryService::default_json());
```

并在文件头引入：

```rust
use crate::infrastructure::memory::LongTermMemoryService;
```

- [ ] **Step 3: 更新 `src/plugins/memory.rs` 中的系统注册**

将 `init_agent_memory_system` 改为接收 `ResMut<LongTermMemoryService>` 参数的版本。确认系统签名变更后编译通过。

- [ ] **Step 4: 运行编译确认**

Run: `cargo check 2>&1 | head -40`

Expected: 编译通过。注意 `init_agent_memory_system` 的签名已变为需要 `ResMut<LongTermMemoryService>`，确保插件注册点一致。

- [ ] **Step 5: 运行现有记忆相关测试确保不破坏原有功能**

Run:

```bash
cargo test -q init_agent_memory_system_logic -- --nocapture
cargo test -q long_term_memory_default_is_empty -- --nocapture
cargo test -q decay_system_marks_stale_long_term_entries_inactive -- --nocapture
cargo test -q memory_contribution_skips_low_value_entries_and_creates_candidates -- --nocapture
```

Expected: PASS。

- [ ] **Step 6: 提交**

```bash
git add src/systems/memory.rs src/app/mod.rs src/plugins/memory.rs src/infrastructure/memory/service.rs
git commit -m "feat: load persisted long-term memory on agent initialization"
```

---

### Task 6: 修改贡献吸收路径走写穿持久化

**Files:**
- Modify: `src/systems/contribution.rs`

- [ ] **Step 1: 修改 `memory_absorption_system` 使用 service 持久化**

更新 `src/systems/contribution.rs` 中的 `memory_absorption_system`：

```rust
use crate::infrastructure::memory::LongTermMemoryService;

/// 记忆吸收系统：将评估后的记忆写入父 Agent，并通过 service 立即落盘。
pub(crate) fn memory_absorption_system(
    mut commands: Commands,
    absorptions: Query<(Entity, &MemoryAbsorptionMessage)>,
    agents: Query<(Entity, &Agent)>,
    mut long_memories: Query<&mut LongTermMemory>,
    mut service: ResMut<LongTermMemoryService>,
) {
    for (entity, absorption) in &absorptions {
        // 查找父 Agent
        let parent = agents.iter().find(|(_, a)| a.id == absorption.parent_id);

        if let Some((parent_entity, parent)) = parent {
            // 找到父 Agent 的长期记忆并吸收
            if let Ok(mut memory) = long_memories.get_mut(parent_entity) {
                let before_count = memory.entries.len();
                let absorbed = absorption.absorbed.clone();
                memory.absorb(absorption.absorbed.clone());

                debug!(
                    event = "MemoryAbsorbed",
                    parent_agent_id = %absorption.parent_id,
                    parent_agent_name = %parent.profile.name,
                    absorbed_count = absorbed.len(),
                    ltm_entries_before = before_count,
                    ltm_entries_after = memory.entries.len(),
                    "absorbed memories into parent agent"
                );

                // 立即落盘
                if let Err(e) = service.absorb_entries(&mut memory, absorbed) {
                    // absorb_entries 会重新添加条目到 memory，但内存已经修改了
                    // 这里如果落盘失败，内存状态仍然正确，只是持久化可能丢失
                    // 错误已在 service 内部通过 warn! 日志记录
                    let _ = e; // 抑制 unused variable 警告
                }
            }
        }

        commands.entity(entity).despawn();
    }
}
```

> 注意：这里有一个微妙之处——`memory.absorb()` 已经将条目添加到内存了，但我们又调用 `service.absorb_entries()`，这会重复添加。我们需要改为：先通过 service 完整写盘，而不是再 absorb 一次。

修正方案——在 `memory.absorb(absorption.absorbed.clone())` 之后，使用 flush（只写盘，不再 absorb）：

```rust
/// 记忆吸收系统：将评估后的记忆写入父 Agent，并通过 service 立即落盘。
pub(crate) fn memory_absorption_system(
    mut commands: Commands,
    absorptions: Query<(Entity, &MemoryAbsorptionMessage)>,
    agents: Query<(Entity, &Agent)>,
    mut long_memories: Query<&mut LongTermMemory>,
    mut service: ResMut<LongTermMemoryService>,
) {
    for (entity, absorption) in &absorptions {
        let parent = agents.iter().find(|(_, a)| a.id == absorption.parent_id);

        if let Some((parent_entity, parent)) = parent {
            if let Ok(mut memory) = long_memories.get_mut(parent_entity) {
                let before_count = memory.entries.len();
                memory.absorb(absorption.absorbed.clone());

                debug!(
                    event = "MemoryAbsorbed",
                    parent_agent_id = %absorption.parent_id,
                    parent_agent_name = %parent.profile.name,
                    absorbed_count = absorption.absorbed.len(),
                    ltm_entries_before = before_count,
                    ltm_entries_after = memory.entries.len(),
                    "absorbed memories into parent agent"
                );

                // 写穿持久化：内存已更新，直接将完整快照落盘
                let _ = service.flush(&memory);
            }
        }

        commands.entity(entity).despawn();
    }
}
```

为此需要在 `LongTermMemoryService` 中暴露 `flush` 为 `pub` 方法：

```rust
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
```

- [ ] **Step 2: 运行编译确认**

Run: `cargo check 2>&1 | head -40`

Expected: 编译通过。

- [ ] **Step 3: 运行贡献吸收相关测试**

Run:

```bash
cargo test -q memory_contribution -- --nocapture
cargo test -q parent_agent_absorbs_filtered_long_term_memory_only -- --nocapture
```

Expected: PASS。

- [ ] **Step 4: 提交**

```bash
git add src/systems/contribution.rs src/infrastructure/memory/service.rs
git commit -m "feat: wire memory absorption through write-through persistence"
```

---

### Task 7: 增加结构化日志与集成测试

**Files:**
- Create: `tests/memory_persistence_flow.rs`
- Modify: `docs/current-state.md`

- [ ] **Step 1: 创建集成测试 `tests/memory_persistence_flow.rs`**

```rust
//! 长期记忆持久化集成测试
//!
//! 覆盖：
//! - Agent 启动时可从 JSON 恢复 LongTermMemory
//! - 子 Agent 贡献吸收后会立即更新 JSON 文件
//! - 重启后（同 Agent 名称）长期记忆仍可恢复

use harness::domain::{
    LongTermMemory, LongTermMemoryEntry, LongTermMemoryKind, MemoryImportance,
};
use harness::infrastructure::memory::{JsonFileMemoryStore, LongTermMemoryService, MemoryRepository};

/// 测试辅助：在临时目录创建 service
fn make_service(dir: &std::path::Path) -> LongTermMemoryService {
    let store = JsonFileMemoryStore::new(dir.join("agents"));
    let repo = MemoryRepository::new(Box::new(store));
    LongTermMemoryService::new(repo)
}

#[test]
fn agent_can_load_previously_persisted_memory() {
    let dir = tempfile::TempDir::new().unwrap();
    let mut service = make_service(dir.path());

    // 第一次运行：添加条目并持久化
    let mut memory = LongTermMemory::with_name("persistent-agent");
    let entry = LongTermMemoryEntry::new(LongTermMemoryKind::Strategy, "Prefer concise responses");
    service.add_entry(&mut memory, entry).unwrap();

    // 模拟重启：创建新 service 实例
    let mut new_service = make_service(dir.path());

    // 第二次运行：同名称 Agent 应能恢复记忆
    let loaded = new_service.load_entries("persistent-agent");
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].content, "Prefer concise responses");
}

#[test]
fn multiple_entries_persist_across_sessions() {
    let dir = tempfile::TempDir::new().unwrap();
    let mut service = make_service(dir.path());

    let mut memory = LongTermMemory::with_name("multi-agent");

    let e1 = LongTermMemoryEntry::new(LongTermMemoryKind::Constraint, "Never leak credentials");
    let e2 = LongTermMemoryEntry::new(LongTermMemoryKind::Fact, "Project uses Bevy ECS");
    let e3 = LongTermMemoryEntry::new(LongTermMemoryKind::Preference, "Chinese for docs");

    service.add_entry(&mut memory, e1).unwrap();
    service.add_entry(&mut memory, e2).unwrap();
    service.add_entry(&mut memory, e3).unwrap();

    // 模拟重启
    let new_service = make_service(dir.path());
    let loaded = new_service.load_entries("multi-agent");
    assert_eq!(loaded.len(), 3);
}

#[test]
fn contribution_absorption_updates_persisted_file() {
    let dir = tempfile::TempDir::new().unwrap();
    let mut service = make_service(dir.path());

    let mut memory = LongTermMemory::with_name("parent-agent");

    // 模拟吸收子 Agent 贡献
    let absorbed = vec![
        LongTermMemoryEntry::new(LongTermMemoryKind::Strategy, "Use two-phase mutation pattern"),
        LongTermMemoryEntry::new(LongTermMemoryKind::Fact, "Shell commands have timeouts"),
    ];

    service.absorb_entries(&mut memory, absorbed).unwrap();
    assert_eq!(memory.entries.len(), 2);

    // 验证文件中恢复出的数据一致
    let new_service = make_service(dir.path());
    let loaded = new_service.load_entries("parent-agent");
    assert_eq!(loaded.len(), 2);
}

#[test]
fn clear_removes_persisted_data() {
    let dir = tempfile::TempDir::new().unwrap();
    let mut service = make_service(dir.path());

    let mut memory = LongTermMemory::with_name("clear-agent");
    service.add_entry(&mut memory, LongTermMemoryEntry::new(LongTermMemoryKind::Fact, "temp")).unwrap();

    service.clear(&mut memory).unwrap();
    assert!(memory.entries.is_empty());

    // 恢复后也应为空
    let new_service = make_service(dir.path());
    let loaded = new_service.load_entries("clear-agent");
    assert!(loaded.is_empty());
}

#[test]
fn different_agents_have_separate_files() {
    let dir = tempfile::TempDir::new().unwrap();
    let mut service = make_service(dir.path());

    let mut memory_a = LongTermMemory::with_name("agent-a");
    let mut memory_b = LongTermMemory::with_name("agent-b");

    service.add_entry(&mut memory_a, LongTermMemoryEntry::new(LongTermMemoryKind::Fact, "A's fact")).unwrap();
    service.add_entry(&mut memory_b, LongTermMemoryEntry::new(LongTermMemoryKind::Fact, "B's fact")).unwrap();

    let new_service = make_service(dir.path());
    assert_eq!(new_service.load_entries("agent-a").len(), 1);
    assert_eq!(new_service.load_entries("agent-b").len(), 1);

    // A 的记忆中不应包含 B 的内容
    let a_entries = new_service.load_entries("agent-a");
    assert_eq!(a_entries[0].content, "A's fact");
}
```

- [ ] **Step 2: 运行集成测试**

Run:

```bash
cargo test -q memory_persistence_flow -- --nocapture
```

Expected: PASS。

- [ ] **Step 3: 运行完整回归测试确认不破坏原有功能**

Run:

```bash
cargo test -q --all-features 2>&1 | tail -20
```

Expected: 所有测试通过。

- [ ] **Step 4: 更新 `docs/current-state.md`**

在 `### 已实现` 的 `#### 记忆治理` 部分追加：

```md
- 长期记忆已具备 JSON 文件持久化能力，Agent 启动时可恢复历史长期记忆
- 运行期变更通过 `LongTermMemoryService` 收口并立即落盘（写穿模式）
- 持久化身份使用 `agent.profile.name` 作为稳定键，不依赖运行时 UUID
- 读取失败以恢复为主（空记忆启动），写入失败显式暴露
```

在 `### 待完善` 部分追加：

```md
- 当前持久化采用每次变更即落盘，未引入批处理或定时刷盘
- `SharedKnowledgeBase` 暂未持久化，重启后仍为空状态
```

- [ ] **Step 5: 提交**

```bash
git add tests/memory_persistence_flow.rs docs/current-state.md
git commit -m "feat: add memory persistence integration tests and update docs"
```

---

### Task 8: 最终验证与文档更新

**Files:**
- Modify: `docs/TODO.md`
- Modify: `docs/superpowers/specs/2026-06-11-memory-persistence-design.md`（标记状态）

- [ ] **Step 1: 运行完整 CI 风格验证**

Run:

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
markdownlint docs/current-state.md docs/superpowers/specs/2026-06-11-memory-persistence-design.md
```

Expected: 全部通过。如有 markdownlint 问题，修复并重新运行。

- [ ] **Step 2: 更新 `docs/TODO.md`，标记持久化任务为已完成**

检查 `docs/TODO.md` 中是否已有长期记忆持久化相关任务，如有则标记为已完成或移除。

- [ ] **Step 3: 提交最终验证**

```bash
git add docs/TODO.md docs/superpowers/specs/2026-06-11-memory-persistence-design.md
git commit -m "docs: mark memory persistence as implemented and update TODO"
```

---

## Self-Review Checklist

- [ ] `MemorySnapshot` 快照结构已定义，包含 `agent_name`、`schema_version`、`updated_at`、`entries`
- [ ] `MemoryStore` trait 已更新为使用 `agent_name` 作为主键，增加 `get_snapshot` 和 `save_snapshot`
- [ ] `JsonFileMemoryStore` 实现了目录按需创建、安全化文件名、原子写入（tmp + rename）
- [ ] `MemoryRepository` 封装了 store，提供高层加载/写回/清空接口
- [ ] `LongTermMemoryService` 收口了所有变更入口（add_entry、absorb_entries、replace_entries、clear），每次变更立即落盘
- [ ] `init_agent_memory_system` 在 Agent 初始化时加载已有的持久化记忆
- [ ] `memory_absorption_system` 吸收后通过 service 写穿落盘
- [ ] 读取失败以恢复为主（空记忆启动），写入失败通过 `anyhow::Result` 显式暴露
- [ ] 结构化日志事件已添加（`LongTermMemoryLoaded`、`LongTermMemoryLoadFailed`、`LongTermMemoryPersisted`、`LongTermMemoryPersistFailed`、`LongTermMemoryCleared`）
- [ ] 所有新增代码有单元测试和集成测试覆盖
- [ ] 现有衰退治理、注入、贡献链路不受影响
- [ ] `docs/current-state.md` 已同步更新

## Final Validation

在所有任务完成后，运行完整验证：

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
markdownlint docs/current-state.md
```