//! Session backend 契约

use crate::domain::{
    SessionHandle, SessionHandleId, SessionInputRequest, SessionReadRequest, SessionStartRequest,
    SessionSummary,
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
}
