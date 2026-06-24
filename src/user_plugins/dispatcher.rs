//! 钩子派发器骨架。
//!
//! 完整派发实现在 Task 15 补齐。这里只定义 HookOutcome 状态对象，
//! 供 tool_control 等 host API 读写。

use std::sync::{Arc, Mutex};

/// 单次 hook 派发的累积结果。同一 hook 点多个订阅者顺序派发，
/// 前一个的 outcome 会作为后一个的输入。
#[derive(Debug, Default, Clone)]
pub struct HookOutcome {
    pub deny_reason: Option<String>,
    pub replaced_result: Option<serde_json::Value>,
}

pub type SharedHookOutcome = Arc<Mutex<HookOutcome>>;
