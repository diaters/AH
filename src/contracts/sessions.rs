//! Session backend 契约

use crate::domain::{
    SessionHandle, SessionHandleId, SessionInputRequest, SessionReadRequest, SessionStartRequest,
    SessionSummary, TaskId,
};

/// SessionBackend 保持同步接口。
///
/// Phase 1 的 NativeProcessBackend 通过内部线程和互斥状态管理子进程，
/// 避免在 Bevy system 中使用嵌套 runtime block_on。
pub trait SessionBackend: Send + Sync + 'static {
    fn exec_blocking(&self, request: SessionStartRequest) -> Result<SessionHandle, String>;
    fn start_session(&self, request: SessionStartRequest) -> Result<SessionHandle, String>;
    fn read_session(&self, request: SessionReadRequest) -> Result<SessionSummary, String>;
    fn list_active_sessions(&self) -> Result<Vec<SessionSummary>, String>;
    fn input_session(&self, request: SessionInputRequest) -> Result<SessionHandle, String>;
    fn stop_session(&self, handle_id: SessionHandleId) -> Result<SessionHandle, String>;

    /// 列出指定 Task 拥有的活动 session
    fn list_task_sessions(&self, task_id: TaskId) -> Result<Vec<SessionSummary>, String>;

    /// 校验指定 session 是否属于指定 Task，不属于则返回错误
    fn assert_task_owns_session(
        &self,
        task_id: TaskId,
        handle_id: SessionHandleId,
    ) -> Result<(), String>;

    /// 批量停止指定 Task 的所有活动 session，返回已停止的 session id 列表
    fn stop_task_sessions(&self, task_id: TaskId) -> Result<Vec<SessionHandleId>, String>;
}
