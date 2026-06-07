//! Session backend 契约

use crate::domain::{
    SessionCommand, SessionHandle, SessionHandleId, SessionOutputRequest, SessionOutputResponse,
    SessionStartRequest, SessionStopRequest, SessionWaitRequest,
};

/// SessionBackend 保持同步接口。
///
/// Phase 1 的 NativeProcessBackend 通过内部线程和互斥状态管理子进程，
/// 避免在 Bevy system 中使用嵌套 runtime block_on。
pub trait SessionBackend: Send + Sync + 'static {
    fn exec_blocking(&self, request: SessionStartRequest) -> Result<SessionHandle, String>;
    fn start_session(&self, request: SessionStartRequest) -> Result<SessionHandle, String>;
    fn get_status(&self, handle_id: SessionHandleId) -> Result<SessionHandle, String>;
    fn read_output(&self, request: SessionOutputRequest) -> Result<SessionOutputResponse, String>;
    fn send_input(&self, command: SessionCommand) -> Result<SessionHandle, String>;
    fn send_signal(&self, command: SessionCommand) -> Result<SessionHandle, String>;
    fn wait_session(&self, request: SessionWaitRequest) -> Result<Option<SessionHandle>, String>;
    fn stop_session(&self, request: SessionStopRequest) -> Result<SessionHandle, String>;
}
