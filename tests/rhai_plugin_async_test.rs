//! Task 12：Rhai 插件 spawn_blocking 包裹器行为测试。
//!
//! 验证：
//! - 同步插件脚本经包裹后返回值原样到达
//! - 脚本错误（throw）→ ExecutionFailed
//! - 缺省与覆盖两档 max_duration（D14）
//! - Rhai 加固：取消令牌 / max_operations 兜底 / on_progress 协作式取消
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

// ============ 测试 4：取消令牌已触发 → 立即返回 cancelled ============

#[test]
fn wrapper_returns_cancelled_when_token_already_fired() {
    // 死循环脚本：只能被 cancel 或 max_ops 终止
    let ast = compile_ast("loop { }");
    let executor = RhaiToolExecutor::new("alpha", "loop", ast, None);
    let wrapper = RhaiPluginAsyncWrapper::new(executor);

    let rt = tokio::runtime::Runtime::new().unwrap();
    let ctx = OwnedToolContext::empty_for_test(300);
    // 立即取消：select! 的 cancelled() 分支立即就绪，应 race 胜过 spawn_blocking
    ctx.cancel.cancel();

    let result = rt.block_on(wrapper.run_async(serde_json::json!({}), ctx));
    match result {
        Err(ToolError::ExecutionFailed(msg)) if msg == "cancelled" => {
            // expected
        }
        other => panic!(
            "expected Err(ToolError::ExecutionFailed(\"cancelled\")), got {:?}",
            other
        ),
    }
}

// 注：「运行中触发取消」场景不另设集成测试——`loop { }` 在 50ms 内即跑完
// 1M max_ops，cancel 来不及触发，测试会退化为 max_ops 终止。该场景已由
// 单元测试 `run_rhai_tool_script_terminates_via_on_progress_on_cancel`
// 覆盖（直接调 `run_rhai_tool_script` + 已取消 token，证明 on_progress
// 在脚本运行中终止）。集成层 select! 分支由本测试 4（cancel 先于 run）
// 覆盖。

// ============ 测试 5：max_operations 兜底终止死循环（无取消）============

#[test]
fn wrapper_terminates_on_max_operations_for_infinite_loop() {
    // 死循环脚本：不取消 token，靠 max_operations=1_000_000 兜底终止
    let ast = compile_ast("loop { }");
    let executor = RhaiToolExecutor::new("alpha", "loop", ast, None);
    let wrapper = RhaiPluginAsyncWrapper::new(executor);

    let rt = tokio::runtime::Runtime::new().unwrap();
    let result = rt
        .block_on(wrapper.run_async(serde_json::json!({}), OwnedToolContext::empty_for_test(300)));

    match result {
        Err(ToolError::ExecutionFailed(msg)) => {
            // 应是 max_operations 终止，而非 "cancelled"（token 从未取消）
            assert_ne!(
                msg, "cancelled",
                "应是 max_operations 兜底终止，而非 cancelled"
            );
        }
        other => panic!(
            "expected Err(ToolError::ExecutionFailed(_)) from max_ops，got {:?}",
            other
        ),
    }
}
