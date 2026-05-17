use bevy::prelude::Component;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tiktoken_rs::cl100k_base;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_entry_new_creates_user_entry() {
        let entry = MemoryEntry::new(EntryRole::User, "hello");
        assert_eq!(entry.role, EntryRole::User);
        assert_eq!(entry.content, "hello");
    }

    #[test]
    fn short_term_memory_default_is_empty() {
        let memory = ShortTermMemory::default();
        assert!(memory.entries.is_empty());
        assert_eq!(memory.estimated_tokens, 0);
        assert!(memory.summary_prefix.is_none());
    }

    #[test]
    fn short_term_memory_add_entry_updates_tokens() {
        let mut memory = ShortTermMemory::default();
        memory.add_entry(EntryRole::User, "hello world", EntryMetadata::default());
        assert_eq!(memory.entries.len(), 1);
        assert!(memory.estimated_tokens > 0);
    }

    #[test]
    fn estimate_tokens_returns_positive() {
        let tokens = estimate_tokens("Hello, world!");
        assert!(tokens > 0);
    }

    #[test]
    fn long_term_memory_default_is_empty() {
        let memory = LongTermMemory::default();
        assert!(memory.entries.is_empty());
    }
}

/// 估算文本的 token 数
pub fn estimate_tokens(text: &str) -> u32 {
    cl100k_base()
        .map(|enc| enc.encode_with_special_tokens(text).len() as u32)
        .unwrap_or_else(|_| (text.len() / 4) as u32) // fallback: 4 chars ≈ 1 token
}

/// 记忆条目
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryEntry {
    pub role: EntryRole,
    pub content: String,
    pub metadata: EntryMetadata,
}

impl MemoryEntry {
    pub fn new(role: EntryRole, content: impl Into<String>) -> Self {
        Self {
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
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct EntryMetadata {
    pub tool_calls: Vec<ToolCall>,
    pub resources: Vec<String>,
    pub reasoning: Option<String>,
    pub keywords: Vec<String>,
}

/// 工具调用记录
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolCall {
    pub tool_name: String,
    pub input: String,
    pub output: String,
    pub timestamp: DateTime<Utc>,
}

/// 短期记忆（绑定 Task）
#[derive(Component, Default, Clone)]
pub struct ShortTermMemory {
    /// 完整对话条目
    pub entries: Vec<MemoryEntry>,
    /// 摘要前缀（压缩后的旧内容）
    pub summary_prefix: Option<String>,
    /// 当前 token 估算
    pub estimated_tokens: u32,
    /// 最后一次缓存命中的 token 数
    pub last_cached_tokens: Option<u32>,
}

impl ShortTermMemory {
    /// 记录 Tool 调用
    ///
    /// 将 Tool 调用记录追加到最后一个 Assistant 条目的元数据中，
    /// 如果没有 Assistant 条目则创建一个新的。
    pub fn record_tool_call(
        &mut self,
        tool_name: String,
        input: String,
        output: String,
        timestamp: DateTime<Utc>,
    ) {
        let tool_call = ToolCall {
            tool_name,
            input,
            output,
            timestamp,
        };

        // 查找最后一个 Assistant 条目
        if let Some(last_entry) = self.entries.last_mut() {
            if last_entry.role == EntryRole::Assistant {
                last_entry.metadata.tool_calls.push(tool_call);
                return;
            }
        }

        // 如果没有 Assistant 条目，创建一个新的
        let mut metadata = EntryMetadata::default();
        metadata.tool_calls.push(tool_call);
        self.entries.push(MemoryEntry {
            role: EntryRole::Assistant,
            content: String::new(),
            metadata,
        });
    }

    /// 添加新条目
    pub fn add_entry(
        &mut self,
        role: EntryRole,
        content: impl Into<String>,
        metadata: EntryMetadata,
    ) {
        let content = content.into();
        // 更新 token 估算
        self.estimated_tokens += estimate_tokens(&content);
        let entry = MemoryEntry::new(role, content).with_metadata(metadata);
        self.entries.push(entry);
    }

    /// 重新计算 token 估算
    pub fn recalculate_tokens(&mut self) {
        let mut total = 0u32;
        if let Some(summary) = &self.summary_prefix {
            total += estimate_tokens(summary);
        }
        for entry in &self.entries {
            total += estimate_tokens(&entry.content);
        }
        self.estimated_tokens = total;
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
        let entry = MemoryEntry::new(EntryRole::Archive, content);
        self.entries.push(entry);
    }

    /// 吸收来自子 Agent 的记忆
    pub fn absorb(&mut self, entries: Vec<MemoryEntry>) {
        self.entries.extend(entries);
    }
}
