//! Task 1：BuiltinTool trait 异步三件套（kind / max_duration / run_async）行为测试。
//!
//! 验证：
//! - `kind()` 缺省 `Sync`，异步工具可 override 为 `Async`
//! - `max_duration()` 缺省返回全局配置（直收秒数，不挂 ctx）
//! - `run_async()` 缺省实现返回 `InternalState` 错误，避免 sync 工具误入 worker 路径
//! - `run_async()` 真实实现返回 `ToolWorkerOutput::Value`
//!
//! 本测试不依赖 ECS World / 通道，纯 trait 行为验证。

use harness::domain::{
    BuiltinTool, OwnedToolContext, ToolAction, ToolActionKind, ToolContext, ToolError,
    ToolWorkerOutput,
};

struct PlainSyncTool;
impl BuiltinTool for PlainSyncTool {
    fn name(&self) -> &str {
        "plain_sync"
    }
    fn execute(&self, _: &serde_json::Value, _: &ToolContext) -> Result<ToolAction, ToolError> {
        Ok(ToolAction::Direct(serde_json::json!({})))
    }
}

struct MigratedAsyncTool;
impl BuiltinTool for MigratedAsyncTool {
    fn name(&self) -> &str {
        "migrated_async"
    }
    fn kind(&self) -> ToolActionKind {
        ToolActionKind::Async
    }
    fn execute(&self, _: &serde_json::Value, _: &ToolContext) -> Result<ToolAction, ToolError> {
        unreachable!("async tool must never be executed via sync path")
    }
    fn run_async(
        &self,
        _input: serde_json::Value,
        _ctx: OwnedToolContext,
    ) -> harness::domain::ToolFuture {
        Box::pin(async { Ok(ToolWorkerOutput::Value(serde_json::json!({"done": true}))) })
    }
}

#[test]
fn kind_defaults_to_sync() {
    assert_eq!(PlainSyncTool.kind(), ToolActionKind::Sync);
}

#[test]
fn kind_can_override_to_async() {
    assert_eq!(MigratedAsyncTool.kind(), ToolActionKind::Async);
}

#[test]
fn max_duration_defaults_to_global_config() {
    let tool = PlainSyncTool;
    assert_eq!(
        tool.max_duration(&serde_json::json!({}), 300),
        std::time::Duration::from_secs(300)
    );
}

#[test]
fn run_async_default_impl_rejects_unmigrated_tool() {
    let tool = PlainSyncTool;
    let rt = tokio::runtime::Runtime::new().unwrap();
    let result =
        rt.block_on(tool.run_async(serde_json::json!({}), OwnedToolContext::empty_for_test(300)));
    assert!(matches!(result, Err(ToolError::InternalState(_))));
}

#[test]
fn run_async_returns_worker_output() {
    let tool = MigratedAsyncTool;
    let rt = tokio::runtime::Runtime::new().unwrap();
    let output = rt
        .block_on(tool.run_async(serde_json::json!({}), OwnedToolContext::empty_for_test(300)))
        .unwrap();
    match output {
        ToolWorkerOutput::Value(v) => assert_eq!(v["done"], true),
        other => panic!("expected Value, got {:?}", other),
    }
}
