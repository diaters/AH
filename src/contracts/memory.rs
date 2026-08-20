//! Memory 契约
//!
//! 定义记忆存储的 trait 接口。

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
