//! 经验库契约
//!
//! 客户端通道（管理与确认）与 ECS（检索与起草）共用同一份经验库，
//! 因此方法只取 `&self`，由实现自行保证并发写入安全。

use std::sync::Arc;

use crate::domain::ExperienceEntry;

pub trait ExperienceLibrary: Send + Sync + 'static {
    /// 列出全部可解析的经验；损坏的文件被跳过并记录日志。
    fn list(&self) -> anyhow::Result<Vec<ExperienceEntry>>;

    fn get(&self, id: &str) -> anyhow::Result<Option<ExperienceEntry>>;

    /// 新建或覆盖一条经验（原子写入）。
    fn save(&self, entry: &ExperienceEntry) -> anyhow::Result<()>;

    /// 删除一条经验；不存在时返回 `false`。
    fn delete(&self, id: &str) -> anyhow::Result<bool>;
}

pub type SharedExperienceLibrary = Arc<dyn ExperienceLibrary>;
