//! Task 10: delete_scheduled_task 工具行为测试。
//!
//! 验证 worker 只做一件事——把入参 `kind` 或 `name` 包成
//! `ToolWorkerOutput::Effect(DeleteScheduledTask)`；不在 worker 里查存在性
//! （`existed` 真相在 apply 时刻，由 commit 系统产生）。
//!
//! 注：本测试用独立 `tokio::runtime::Runtime::block_on` 跑 `run_async`——
//! 这是工具本体的单元测试，不进 ECS、不依赖 `AsyncRuntime` 资源；
//! `#[test]` 而非 `#[tokio::test]` 仍遵守（runtime 嵌套 panic 规避）。

use harness::domain::{
    BuiltinTool, OwnedToolContext, ScheduleSelector, ToolEffect, ToolError, ToolWorkerOutput,
};
use harness::systems::tools::builtin::scheduled::delete::DeleteScheduledTaskTool;

fn run_delete(input: serde_json::Value) -> Result<ToolWorkerOutput, ToolError> {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(DeleteScheduledTaskTool.run_async(input, OwnedToolContext::empty_for_test(300)))
}

fn selector_of(output: ToolWorkerOutput) -> ScheduleSelector {
    match output {
        ToolWorkerOutput::Effect(ToolEffect::DeleteScheduledTask { selector }) => selector,
        other => panic!("expected DeleteScheduledTask effect, got {:?}", other),
    }
}

#[test]
fn delete_wraps_kind_into_effect() {
    let output = run_delete(serde_json::json!({"kind": "scheduled:nightly"})).unwrap();
    assert_eq!(
        selector_of(output),
        ScheduleSelector::Kind("scheduled:nightly".into())
    );
}

#[test]
fn delete_wraps_name_into_effect() {
    let output = run_delete(serde_json::json!({"name": "nightly"})).unwrap();
    assert_eq!(
        selector_of(output),
        ScheduleSelector::Name("nightly".into())
    );
}

#[test]
fn delete_rejects_missing_kind_and_name() {
    let result = run_delete(serde_json::json!({}));
    assert!(matches!(result, Err(ToolError::InvalidInput(_))));
}
