//! Task 12：Rhai 插件 spawn_blocking 包裹器行为测试。
//!
//! 验证：
//! - 同步插件脚本经包裹后返回值原样到达
//! - 脚本错误（throw）→ ExecutionFailed
//! - 缺省与覆盖两档 max_duration（D14）
//!
//! 一律 `#[test]`，禁止 `#[tokio::test]`；用 `Runtime::new().block_on` 跑 future。

use std::time::Duration;

use harness::domain::{BuiltinTool, OwnedToolContext, ToolError, ToolWorkerOutput};
use harness::user_plugins::loader::new_sandboxed_engine;
use harness::user_plugins::tool_executor::{RhaiPluginAsyncWrapper, RhaiToolExecutor};

fn compile_ast(script: &str) -> rhai::AST {
    new_sandboxed_engine()
        .compile(script)
        .expect("AST compile must succeed in test")
}

// ============ 测试 1：同步插件返回值经包裹后原样到达 ============

#[test]
fn wrapper_returns_script_value() {
    let ast = compile_ast(r#"let name = args.name; "hello, " + name"#);
    let executor = RhaiToolExecutor::new("alpha", "hello", ast, None);
    let wrapper = RhaiPluginAsyncWrapper::new(executor);

    let rt = tokio::runtime::Runtime::new().unwrap();
    let result = rt.block_on(wrapper.run_async(
        serde_json::json!({"name": "world"}),
        OwnedToolContext::empty_for_test(300),
    ));

    match result {
        Ok(ToolWorkerOutput::Value(v)) => {
            assert_eq!(v, serde_json::json!("hello, world"));
        }
        other => panic!("expected Ok(ToolWorkerOutput::Value), got {:?}", other),
    }
}

// ============ 测试 2：脚本错误 → ExecutionFailed ============

#[test]
fn wrapper_maps_script_error_to_execution_failed() {
    let ast = compile_ast(r#"throw "boom""#);
    let executor = RhaiToolExecutor::new("alpha", "boom", ast, None);
    let wrapper = RhaiPluginAsyncWrapper::new(executor);

    let rt = tokio::runtime::Runtime::new().unwrap();
    let result = rt
        .block_on(wrapper.run_async(serde_json::json!({}), OwnedToolContext::empty_for_test(300)));

    assert!(
        matches!(result, Err(ToolError::ExecutionFailed(_))),
        "expected Err(ToolError::ExecutionFailed(_)), got {:?}",
        result
    );
}

// ============ 测试 3：缺省与覆盖两档 max_duration（D14） ============

#[test]
fn max_duration_defaults_to_global_when_unset_and_overrides_when_set() {
    // 缺省档：不设 timeout_secs → 走全局值（300s）
    let ast_default = compile_ast("42");
    let executor_default = RhaiToolExecutor::new("alpha", "noop", ast_default, None);
    let wrapper_default = RhaiPluginAsyncWrapper::new(executor_default);
    assert_eq!(
        wrapper_default.max_duration(&serde_json::json!({}), 300),
        Duration::from_secs(300),
        "缺省 timeout_secs 时应走全局值"
    );

    // 覆盖档：设 timeout_secs=60 → 60s
    let ast_override = compile_ast("42");
    let executor_override = RhaiToolExecutor::new("alpha", "noop", ast_override, Some(60));
    let wrapper_override = RhaiPluginAsyncWrapper::new(executor_override);
    assert_eq!(
        wrapper_override.max_duration(&serde_json::json!({}), 300),
        Duration::from_secs(60),
        "manifest 设 timeout_secs=60 时应覆盖全局值"
    );
}
