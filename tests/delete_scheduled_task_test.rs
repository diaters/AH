//! Task 10: delete_scheduled_task 工具行为测试。
//!
//! 验证 worker 只做一件事——把入参 `kind` 包成
//! `ToolWorkerOutput::Effect(DeleteScheduledTask)`；不在 worker 里查存在性
//! （`existed` 真相在 apply 时刻，由 commit 系统产生）。
//!
//! 注：本测试用独立 `tokio::runtime::Runtime::block_on` 跑 `run_async`——
//! 这是工具本体的单元测试，不进 ECS、不依赖 `AsyncRuntime` 资源；
//! `#[test]` 而非 `#[tokio::test]` 仍遵守（runtime 嵌套 panic 规避）。

use harness::domain::{BuiltinTool, OwnedToolContext, ToolError, ToolWorkerOutput};
use harness::systems::tools::builtin::scheduled::delete::DeleteScheduledTaskTool;

#[test]
fn delete_wraps_kind_into_effect() {
    let tool = DeleteScheduledTaskTool;
    let rt = tokio::runtime::Runtime::new().unwrap();
    let output = rt
        .block_on(tool.run_async(
            serde_json::json!({"kind": "nightly"}),
            OwnedToolContext::empty_for_test(300),
        ))
        .unwrap();
    match output {
        ToolWorkerOutput::Effect(harness::domain::ToolEffect::DeleteScheduledTask { kind }) => {
            assert_eq!(kind, "nightly")
        }
        other => panic!("expected DeleteScheduledTask effect, got {:?}", other),
    }
}

#[test]
fn delete_rejects_missing_kind() {
    let tool = DeleteScheduledTaskTool;
    let rt = tokio::runtime::Runtime::new().unwrap();
    let result =
        rt.block_on(tool.run_async(serde_json::json!({}), OwnedToolContext::empty_for_test(300)));
    assert!(matches!(result, Err(ToolError::InvalidInput(_))));
}
