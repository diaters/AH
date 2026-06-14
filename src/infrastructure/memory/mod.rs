//! 记忆持久化基础设施
//!
//! 提供 `MemoryStore` trait 的首个 JSON 文件实现，
//! 以及按 Agent 读写长期记忆的 repository 和服务。

pub mod json_file_store;
pub mod repository;
pub mod service;
pub mod upgrade_service;

pub use json_file_store::JsonFileMemoryStore;
pub use repository::MemoryRepository;
pub use service::LongTermMemoryService;
pub use upgrade_service::SharedKnowledgeUpgradeService;
