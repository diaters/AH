//! 基础设施层
//!
//! 负责持久化、序列化、文件 I/O 等底层实现，
//! 系统层不直接耦合文件格式和存储细节。

pub mod assets;
pub mod incubation;
pub mod memory;
pub mod skills;
