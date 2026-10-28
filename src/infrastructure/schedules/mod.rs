//! 动态定时任务持久化
//!
//! `ScheduleStore` 把 `ScheduleDefinition` 列表保存到 `.harness/schedules/schedules.json`，
//! 让 `schedule_task` 创建的定时任务跨进程重启保留。

mod store;

pub use store::{ScheduleStore, ScheduleStoreError};
