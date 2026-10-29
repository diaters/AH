//! Session backend 领域契约
//!
//! 同步接口：Phase 1 的 NativeProcessBackend 通过内部线程和互斥状态管理子进程，
//! 避免在 Bevy system 中使用嵌套 runtime block_on。

use tokio_util::sync::CancellationToken;

use crate::domain::{
    SessionHandle, SessionHandleId, SessionInputRequest, SessionOutputEvent, SessionReadRequest,
    SessionStartRequest, SessionSummary, TaskId,
};

/// `Debug` bound 让 `OwnedToolContext` 等持有 `Arc<dyn SessionBackend>` 的
/// 结构体可 derive `Debug`（worker panic catch_unwind 路径会打印 ctx）。
pub trait SessionBackend: std::fmt::Debug + Send + Sync + 'static {
    fn exec_blocking(&self, request: SessionStartRequest) -> Result<SessionHandle, String>;

    /// 取消感知的阻塞执行入口。
    ///
    /// 默认实现忽略 `cancel` 直接走 `exec_blocking`——已存在的同步 backend
    /// 零改动即可编译；真正长任务的 backend（如 `NativeProcessBackend`）
    /// override 本方法在循环中检查 `cancel.is_cancelled()`，触发时清理
    /// 子进程及同组后代、回收组长并限时 drain 输出，再返回 `Err("cancelled")`。
    ///
    /// 调用方（worker）用 `tokio::task::spawn_blocking` 包裹本方法并等待句柄，
    /// 由 backend 消费取消信号，确保回送结果前进程清理已经完成。
    /// 取消是协作式约定；默认实现不提供取消保证。
    fn exec_with_cancel(
        &self,
        request: SessionStartRequest,
        cancel: CancellationToken,
    ) -> Result<SessionHandle, String> {
        let _ = cancel; // 默认实现忽略 cancel，保持向后兼容
        self.exec_blocking(request)
    }

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

    /// 窥视尚未确认的 stdout 事件；调用本身不消费事件。
    fn poll_output_events(&self) -> Result<Vec<SessionOutputEvent>, String> {
        Ok(Vec::new())
    }

    /// 累计确认某会话的事件；确认后不再出现在后续 poll 中。
    fn ack_output_events(
        &self,
        _session_id: SessionHandleId,
        _through_sequence: u64,
    ) -> Result<(), String> {
        Ok(())
    }
}
