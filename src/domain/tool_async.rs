//! 异步工具桥的领域类型（最小骨架）。
//!
//! 本模块当前只包含 Phase 0 测试 harness 所需的通道类型：
//! - `ToolWorkerPayload`：worker 回传给 ECS 的载荷枚举
//! - `ToolAsyncResult`：worker 回传的完整结果
//! - `ToolResultSender` / `ToolResultReceiver`：作为 Resource 注入 World 的通道端
//!
//! Phase 1（Task 2）会在此骨架上扩展：
//! - 给 `ToolWorkerPayload` 增加 `Effect(ToolEffect)` 变体
//! - 补充 `ToolEffect`、`ToolEffectPending`、`ToolRequestPending`、
//!   `InFlightToolCall`、`OwnedToolContext` 与快照类型
//!
//! 在此之前，本模块仅承担“让 harness 能编译”的职责，不引入
//! 任何业务逻辑。

use bevy_ecs::prelude::Resource;
use tokio::sync::mpsc;

use crate::domain::ToolError;

/// worker 回传给 ECS 的载荷。
///
/// 当前只支持 `Completed`（工具执行完毕，附带成功值或错误）。
/// Task 2 会补 `Effect(ToolEffect)` 变体用于副作用回传。
#[derive(Debug, Clone)]
pub enum ToolWorkerPayload {
    Completed(Result<serde_json::Value, ToolError>),
    // Task 2 会补 Effect(ToolEffect) 变体
}

/// worker 回传给 ECS 的异步结果。
///
/// 一条 `ToolAsyncResult` 对应一次工具调用的终态（或终态前的一次副作用）。
#[derive(Debug, Clone)]
pub struct ToolAsyncResult {
    pub tool_call_id: String,
    pub payload: ToolWorkerPayload,
}

impl ToolAsyncResult {
    /// 构造一条 `Completed` 结果。
    pub fn completed(
        tool_call_id: impl Into<String>,
        result: Result<serde_json::Value, ToolError>,
    ) -> Self {
        Self {
            tool_call_id: tool_call_id.into(),
            payload: ToolWorkerPayload::Completed(result),
        }
    }
}

/// worker → ECS 通道的发送端，作为 Resource 注入 World。
#[derive(Resource)]
pub struct ToolResultSender(pub mpsc::UnboundedSender<ToolAsyncResult>);

/// worker → ECS 通道的接收端，作为 Resource 注入 World。
///
/// 持有 `UnboundedReceiver`：唯一持有，因此 ingest 系统对它有排他访问权。
#[derive(Resource)]
pub struct ToolResultReceiver(pub mpsc::UnboundedReceiver<ToolAsyncResult>);
