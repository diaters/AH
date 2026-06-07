//! Session backend 契约

use crate::domain::{
    SessionCommand, SessionHandle, SessionHandleId, SessionOutputRequest, SessionOutputResponse,
    SessionStartRequest, SessionStopRequest, SessionWaitRequest,
};

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
