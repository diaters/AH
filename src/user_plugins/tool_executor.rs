//! 插件 Tool 执行器
//!
//! 提供以 `plugin_id:tool_id` 为命名空间的插件 Tool 执行能力。
//! Phase 12 起，插件工具统一经 `RhaiPluginAsyncWrapper` 上桥：
//! - `RhaiToolExecutor` 持有预编译 AST + 可选超时
//! - `RhaiPluginAsyncWrapper` 包裹之，`kind()=Async`，
//!   `run_async` 内 `tokio::task::spawn_blocking` 执行 Rhai 脚本，
//!   脚本错误 / worker panic 映射为 `ToolError::ExecutionFailed`。

use rhai::AST;

use crate::domain::{
    BuiltinTool, OwnedToolContext, ToolAction, ToolActionKind, ToolContext, ToolError, ToolFuture,
    ToolWorkerOutput,
};
use crate::user_plugins::loader::new_sandboxed_engine;

/// 插件贡献的 Tool 执行器
///
/// 每个 `RhaiToolExecutor` 对应一个插件贡献的 tool，
/// 以 `plugin_id:tool_id` 形式注册到 `BuiltinToolExecutors`。
///
/// Phase 12 起 `execute` 仅做 async-only 快速失败——真实执行走
/// `RhaiPluginAsyncWrapper::run_async`（spawn_blocking 跑 Rhai AST）。
pub struct RhaiToolExecutor {
    /// 命名空间全名（`plugin_id:tool_id`），同时作为 BuiltinToolExecutors 的 key。
    namespaced: String,
    /// 预编译的 Rhai AST（handler 顶层）。
    ast: AST,
    /// 可选超时（秒），覆盖全局 `tool_inflight_timeout_secs`（D14）。
    timeout_secs: Option<u64>,
}

impl RhaiToolExecutor {
    pub fn new(plugin_id: &str, tool_id: &str, ast: AST, timeout_secs: Option<u64>) -> Self {
        Self {
            namespaced: format!("{}:{}", plugin_id, tool_id),
            ast,
            timeout_secs,
        }
    }

    /// 预编译的工具 handler AST。
    pub fn ast(&self) -> &AST {
        &self.ast
    }

    /// manifest 可选填写的超时（秒），`None` 走全局值。
    pub fn timeout_secs(&self) -> Option<u64> {
        self.timeout_secs
    }
}

impl BuiltinTool for RhaiToolExecutor {
    fn name(&self) -> &str {
        &self.namespaced
    }

    fn execute(
        &self,
        _input: &serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolAction, ToolError> {
        // async-only：真实路径走 run_async（由包裹器提供）
        Err(ToolError::InternalState(
            "plugin tool is async-only".to_string(),
        ))
    }
}

/// Rhai 插件工具的异步包裹器。
///
/// 注册阶段由 `register_plugin_tools` 自动包裹 `RhaiToolExecutor`：
/// - `kind()` = `Async`
/// - `max_duration()` 读 `inner.timeout_secs`，`None` 走全局值（D14 缺省链）
/// - `run_async()` 把 AST + input 丢到 `spawn_blocking` 跑沙箱 Rhai，
///   脚本错误 / worker panic 全部映射为 `ToolError::ExecutionFailed`
///
/// 插件作者零改动即可上桥——注册链负责包裹。
pub struct RhaiPluginAsyncWrapper {
    inner: RhaiToolExecutor,
}

impl RhaiPluginAsyncWrapper {
    pub fn new(inner: RhaiToolExecutor) -> Self {
        Self { inner }
    }
}

impl BuiltinTool for RhaiPluginAsyncWrapper {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn kind(&self) -> ToolActionKind {
        ToolActionKind::Async
    }

    fn max_duration(
        &self,
        _input: &serde_json::Value,
        tool_inflight_timeout_secs: u64,
    ) -> std::time::Duration {
        std::time::Duration::from_secs(
            self.inner
                .timeout_secs
                .unwrap_or(tool_inflight_timeout_secs),
        )
    }

    fn run_async(&self, input: serde_json::Value, _ctx: OwnedToolContext) -> ToolFuture {
        let ast = self.inner.ast.clone();
        Box::pin(async move {
            let join = tokio::task::spawn_blocking(move || run_rhai_tool_script(&ast, &input));
            match join.await {
                Ok(Ok(value)) => Ok(ToolWorkerOutput::Value(value)),
                Ok(Err(e)) => Err(ToolError::ExecutionFailed(e)),
                Err(join_err) => Err(ToolError::ExecutionFailed(format!(
                    "plugin worker panicked: {}",
                    join_err
                ))),
            }
        })
    }

    fn execute(
        &self,
        _input: &serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolAction, ToolError> {
        Err(ToolError::InternalState(
            "plugin tool is async-only".to_string(),
        ))
    }
}

/// 跑沙箱 Rhai AST 作为工具 handler。
///
/// 协议：通过 `Scope` 注入全局变量 `args`（`serde_json::Value` → `rhai::Dynamic`），
/// 脚本返回值转回 `serde_json::Value`。
///
/// 失败路径统一为 `Err(String)`，由调用方映射为 `ToolError::ExecutionFailed`。
fn run_rhai_tool_script(ast: &AST, input: &serde_json::Value) -> Result<serde_json::Value, String> {
    let engine = new_sandboxed_engine();
    let mut scope = rhai::Scope::new();
    let args_dynamic = rhai::serde::to_dynamic(input).map_err(|e| e.to_string())?;
    scope.push("args", args_dynamic);
    let result: rhai::Dynamic = engine
        .eval_ast_with_scope(&mut scope, ast)
        .map_err(|e| e.to_string())?;
    let value: serde_json::Value = rhai::serde::from_dynamic(&result).map_err(|e| e.to_string())?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn namespaced_name_format() {
        let engine = new_sandboxed_engine();
        let ast = engine.compile("42").unwrap();
        let executor = RhaiToolExecutor::new("alpha", "search", ast, None);
        assert_eq!(executor.name(), "alpha:search");
    }

    #[test]
    fn timeout_secs_accessor_roundtrips() {
        let engine = new_sandboxed_engine();
        let ast = engine.compile("42").unwrap();
        let executor = RhaiToolExecutor::new("alpha", "search", ast, Some(120));
        assert_eq!(executor.timeout_secs(), Some(120));
    }
}
