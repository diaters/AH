//! Agent loop 控制系统：引导式插入、回合收件箱唤醒、请求期上下文注入。
//!
//! 设计见 `docs/design/2026-10-28-loop-steering-and-background-notification-design.md`。

mod attachment;
mod background;
mod control;
mod inbox;
mod monitor;
mod stop;

pub(crate) use attachment::context_attachment_system;
pub(crate) use background::{
    background_job_cleanup_system, background_job_monitor_system, register_background_job,
    spawn_background_job,
};
pub(crate) use control::background_job_control_system;
pub(crate) use inbox::{inbox_ingest_system, inbox_status_sync_system, inbox_wake_system};
pub(crate) use stop::{clear_stop_requested_system, stop_task_system};

pub(crate) use monitor::monitor_output_system;
pub use monitor::{MonitorDeliveries, MonitorDeliveryRecord};
