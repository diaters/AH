//! shell_exec 工具——上桥到异步 worker。
//!
//! 历史路径：sync `execute` 返回 `ToolAction::ExecSession`，由 orchestrator
//! 调 `backend.exec_blocking` 阻塞主线程 3s+ 拉长帧。
//!
//! 现路径：`kind() == Async`，dispatch 把请求改造为挂起实体 + spawn worker；
//! worker 在 `run_async` 内 `spawn_blocking` 包裹 `backend.exec_with_cancel`，
//! backend 监听 `ctx.cancel`，清理进程和输出 reader 后返回
//! `Err(ToolError::ExecutionFailed("cancelled"))`。
//!
//! 业务超时三层链不动：入参 `timeout_secs` → `shell_default_exec_timeout_secs`
//! → stop 超时。worker 侧 `exec_with_cancel` 内部 `deadline` 校验保留。
//! `max_duration = exec_timeout_secs + 30s margin`——「sweeper > 业务」由
//! 构造保证（D5）。

use std::sync::Arc;
use std::time::Duration;

use crate::domain::SessionBackend;
use crate::domain::{
    BuiltinTool, OwnedToolContext, SessionStartRequest, ShellExecResult, ToolAction,
    ToolActionKind, ToolContext, ToolError, ToolFuture, ToolWorkerOutput,
};

/// 缺省 tail_lines（与原 sync 路径 ToolContext 字段保持一致）。
const DEFAULT_TAIL_LINES: usize = 200;
/// max_duration margin（sweeper 必须晚于业务超时）。
const MAX_DURATION_MARGIN_SECS: u64 = 30;

pub struct ShellExecTool;

impl BuiltinTool for ShellExecTool {
    fn name(&self) -> &str {
        "shell_exec"
    }

    /// 上桥：dispatch 在 `kind() == Async` 时把请求改造为挂起实体并 spawn worker，
    /// 阻塞轮询搬到 worker 线程，主循环帧不被拉长。
    fn kind(&self) -> ToolActionKind {
        ToolActionKind::Async
    }

    /// `exec_timeout_secs + 30s margin`——sweeper 必须晚于业务超时，
    /// 否则 worker 还在跑业务超时就被 sweeper 兜底摘掉。
    fn max_duration(&self, input: &serde_json::Value, tool_inflight_timeout_secs: u64) -> Duration {
        let exec_timeout_secs = input
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(tool_inflight_timeout_secs);
        Duration::from_secs(exec_timeout_secs + MAX_DURATION_MARGIN_SECS)
    }

    fn execute(&self, _: &serde_json::Value, _: &ToolContext) -> Result<ToolAction, ToolError> {
        // Async 工具不会走到这里（dispatch 按 kind 分流）；快速失败防误调
        Err(ToolError::InternalState(
            "shell_exec is async-only".to_string(),
        ))
    }

    fn run_async(&self, input: serde_json::Value, ctx: OwnedToolContext) -> ToolFuture {
        Box::pin(async move {
            // 1. 解析入参（与原 sync execute 的解析逻辑对齐）
            let command = input
                .get("command")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ToolError::InvalidInput("missing 'command'".to_string()))?
                .to_string();

            let tail_lines = input
                .get("tail_lines")
                .and_then(|v| v.as_u64())
                .map(|v| v as usize)
                .unwrap_or(DEFAULT_TAIL_LINES)
                .min(DEFAULT_TAIL_LINES * 2 + 100); // 与 ctx.shell_max_tail_lines=500 一致

            let cwd = input
                .get("cwd")
                .and_then(|v| v.as_str())
                .map(ToString::to_string);

            let env = super::parse_env_map(&input)?;

            let timeout_secs = input
                .get("timeout_secs")
                .and_then(|v| v.as_u64())
                .or(Some(ctx.shell_default_exec_timeout_secs));

            // 2. 从 ctx 拿 backend 句柄（dispatch 已 clone 注入）
            let backend: Arc<dyn SessionBackend> = ctx.backend.clone().ok_or_else(|| {
                ToolError::InternalState(
                    "NativeProcessBackend not available in OwnedToolContext".to_string(),
                )
            })?;

            // 3. 保留 dispatch 注入的真实归属，运行中的 exec 也参与 Task 清理。
            let request = SessionStartRequest {
                command,
                session_name: None,
                cwd,
                env,
                timeout_secs,
                tail_lines,
                owner_task_id: ctx.current_task_id.ok_or_else(|| {
                    ToolError::InternalState(
                        "shell_exec requires current Task ownership".to_string(),
                    )
                })?,
                owner_agent_id: ctx.current_agent_id.ok_or_else(|| {
                    ToolError::InternalState(
                        "shell_exec requires current Agent ownership".to_string(),
                    )
                })?,
                notify_on_exit: false,
            };

            // 4. spawn_blocking 包裹同步 exec_with_cancel（内部 try_wait + sleep 循环）
            let cancel = ctx.cancel.clone();
            let join_handle =
                tokio::task::spawn_blocking(move || backend.exec_with_cancel(request, cancel));

            // 5. 等待 backend 完成取消清理，避免结果已回传但 OS 子进程仍在运行。
            // spawn_blocking 不能强制 abort；token 由内部轮询消费并执行 kill + reap。
            let result = join_handle
                .await
                .map_err(|join_err| {
                    ToolError::ExecutionFailed(format!("spawn_blocking join failed: {join_err}"))
                })?
                .map_err(ToolError::ExecutionFailed)?;

            // 6. 正常完成：把 handle 转 ShellExecResult
            Ok(ToolWorkerOutput::Value(
                serde_json::to_value(ShellExecResult::from_handle(&result))
                    .map_err(|e| ToolError::ExecutionFailed(format!("serialize failed: {e}")))?,
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{AgentId, TaskId};

    #[test]
    fn missing_owner_context_rejects_exec_before_spawn() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        for missing_task in [true, false] {
            let backend = crate::systems::tools::NativeProcessBackend::default();
            let mut ctx = OwnedToolContext::empty_for_test(30);
            ctx.backend = Some(Arc::new(backend.clone()));
            ctx.current_task_id = (!missing_task).then(TaskId::new);
            ctx.current_agent_id = missing_task.then(AgentId::new);
            let result = runtime
                .block_on(ShellExecTool.run_async(serde_json::json!({"command": "true"}), ctx));
            assert!(
                matches!(result, Err(ToolError::InternalState(_))),
                "missing owner must reject exec: {result:?}"
            );
            assert!(backend.sessions.lock().unwrap().is_empty());
        }
    }

    #[test]
    fn shell_exec_kind_is_async() {
        let tool = ShellExecTool;
        assert_eq!(tool.kind(), ToolActionKind::Async);
    }

    #[test]
    fn shell_exec_max_duration_uses_input_timeout_plus_margin() {
        let tool = ShellExecTool;
        // input 显式指定 timeout_secs=10 → max_duration = 10 + 30 = 40s
        let input = serde_json::json!({ "command": "echo ok", "timeout_secs": 10 });
        let duration = tool.max_duration(&input, 300);
        assert_eq!(duration, Duration::from_secs(40));
    }

    #[test]
    fn shell_exec_max_duration_falls_back_to_global_default() {
        let tool = ShellExecTool;
        // input 不指定 timeout_secs → 用全局 tool_inflight_timeout_secs=300
        // → max_duration = 300 + 30 = 330s
        let input = serde_json::json!({ "command": "echo ok" });
        let duration = tool.max_duration(&input, 300);
        assert_eq!(duration, Duration::from_secs(330));
    }
}
