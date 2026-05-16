use bevy::prelude::Component;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_entry_new_creates_user_entry() {
        let entry = MemoryEntry::new(1, EntryRole::User, "hello");
        assert_eq!(entry.turn, 1);
        assert_eq!(entry.role, EntryRole::User);
        assert_eq!(entry.content, "hello");
    }

    #[test]
    fn short_term_memory_default_is_empty() {
        let memory = ShortTermMemory::default();
        assert!(memory.entries.is_empty());
        assert_eq!(memory.turn_count, 0);
        assert!(memory.summary_prefix.is_none());
    }

    #[test]
    fn short_term_memory_add_entry_increments_turn() {
        let mut memory = ShortTermMemory::default();
        memory.add_entry(EntryRole::User, "hello", EntryMetadata::default());
        assert_eq!(memory.turn_count, 1);
        assert_eq!(memory.entries.len(), 1);
    }

    #[test]
    fn long_term_memory_default_is_empty() {
        let memory = LongTermMemory::default();
        assert!(memory.entries.is_empty());
    }
}

/// 记忆条目
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub turn: u32,
    pub role: EntryRole,
    pub content: String,
    pub metadata: EntryMetadata,
}

impl MemoryEntry {
    pub fn new(turn: u32, role: EntryRole, content: impl Into<String>) -> Self {
        Self {
            turn,
            role,
            content: content.into(),
            metadata: EntryMetadata::default(),
        }
    }

    pub fn with_metadata(mut self, metadata: EntryMetadata) -> Self {
        self.metadata = metadata;
        self
    }
}

/// 记忆条目角色
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum EntryRole {
    User,
    Assistant,
    Summary,
    Archive,
}

/// 记忆条目元数据
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EntryMetadata {
    pub tool_calls: Vec<ToolCall>,
    pub resources: Vec<String>,
    pub reasoning: Option<String>,
    pub keywords: Vec<String>,
}

/// 工具调用记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub tool_name: String,
    pub input: String,
    pub output: String,
    pub timestamp: DateTime<Utc>,
}

/// 短期记忆（绑定 Task）
#[derive(Component, Default)]
pub struct ShortTermMemory {
    pub entries: Vec<MemoryEntry>,
    pub turn_count: u32,
    pub summary_prefix: Option<String>,
    pub summary_range: Option<(u32, u32)>,
    pub last_cached_tokens: Option<u32>,
}

impl ShortTermMemory {
    /// 添加新条目
    pub fn add_entry(
        &mut self,
        role: EntryRole,
        content: impl Into<String>,
        metadata: EntryMetadata,
    ) {
        self.turn_count += 1;
        let entry = MemoryEntry::new(self.turn_count, role, content).with_metadata(metadata);
        self.entries.push(entry);
    }

    /// 获取需要发送给 LLM 的条目（排除已摘要的部分）
    pub fn active_entries(&self) -> impl Iterator<Item = &MemoryEntry> {
        let start_turn = self.summary_range.map(|(_, end)| end).unwrap_or(0);
        self.entries.iter().filter(move |e| e.turn >= start_turn)
    }
}

/// 长期记忆（绑定 Agent）
#[derive(Component, Default)]
pub struct LongTermMemory {
    pub entries: Vec<MemoryEntry>,
}

impl LongTermMemory {
    /// 添加归档条目
    pub fn add_archive(&mut self, content: impl Into<String>) {
        let entry = MemoryEntry::new(0, EntryRole::Archive, content);
        self.entries.push(entry);
    }

    /// 吸收来自子 Agent 的记忆
    pub fn absorb(&mut self, entries: Vec<MemoryEntry>) {
        self.entries.extend(entries);
    }
}
