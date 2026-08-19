use rhai::Engine;
use tracing::warn;

use crate::user_plugins::dispatcher::SharedHookOutcome;

/// 当前工具调用上下文（仅 `on_tool_called` 前置 hook 派发时填充，其余 hook 为默认空值）。
///
/// 条件拒绝是该 hook 的核心用途：插件依据 `tool_call_name()` / `tool_call_input_json()`
/// 判断是否调用 `tool_deny`。
#[derive(Clone, Default)]
pub struct ToolCallContext {
    /// 工具名（空串表示当前 hook 派发不携带工具调用）
    pub name: String,
    /// 工具入参 JSON（无工具调用上下文时为 `null`）
    pub input: serde_json::Value,
}

impl ToolCallContext {
    /// 以请求侧字段构造。
    pub fn new(name: &str, input: serde_json::Value) -> Self {
        Self {
            name: name.to_string(),
            input,
        }
    }
}

pub fn register(engine: &mut Engine, outcome: SharedHookOutcome, tool: ToolCallContext) {
    let o = outcome.clone();
    engine.register_fn("tool_deny", move |reason: &str| {
        let mut g = o.lock().unwrap();
        warn!(
            event = "PluginToolDenied",
            reason = reason,
            "plugin denied tool call"
        );
        g.deny_reason = Some(reason.to_string());
    });

    let o = outcome.clone();
    engine.register_fn("tool_set_result", move |value: rhai::Dynamic| {
        let json = rhai_to_json(value);
        let mut g = o.lock().unwrap();
        warn!(event = "PluginToolResultSet", "plugin replaced tool result");
        g.replaced_result = Some(json);
    });

    let t = tool.clone();
    engine.register_fn("tool_call_name", move || -> String { t.name.clone() });

    let t = tool.clone();
    engine.register_fn("tool_call_input_json", move || -> String {
        t.input.to_string()
    });
}

fn rhai_to_json(v: rhai::Dynamic) -> serde_json::Value {
    serde_json::Value::String(v.to_string())
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::user_plugins::dispatcher::HookOutcome;

    #[test]
    fn tool_deny_sets_reason() {
        let outcome = Arc::new(Mutex::new(HookOutcome::default()));
        let mut e = Engine::new();
        register(&mut e, outcome.clone(), ToolCallContext::default());
        let _: () = e.eval(r#"tool_deny("blocked")"#).unwrap();
        assert_eq!(
            outcome.lock().unwrap().deny_reason.as_deref(),
            Some("blocked")
        );
    }

    #[test]
    fn tool_set_result_sets_value() {
        let outcome = Arc::new(Mutex::new(HookOutcome::default()));
        let mut e = Engine::new();
        register(&mut e, outcome.clone(), ToolCallContext::default());
        let _: () = e.eval(r#"tool_set_result("hello")"#).unwrap();
        assert!(outcome.lock().unwrap().replaced_result.is_some());
    }

    #[test]
    fn tool_call_name_and_input_exposed() {
        let mut e = Engine::new();
        register(
            &mut e,
            Arc::new(Mutex::new(HookOutcome::default())),
            ToolCallContext::new("shell_exec", serde_json::json!({"command": "ls"})),
        );
        let name: String = e.eval(r#"tool_call_name()"#).unwrap();
        assert_eq!(name, "shell_exec");
        let input: String = e.eval(r#"tool_call_input_json()"#).unwrap();
        assert_eq!(input, r#"{"command":"ls"}"#);
    }

    #[test]
    fn tool_call_name_defaults_to_empty_outside_tool_hook() {
        let mut e = Engine::new();
        register(
            &mut e,
            Arc::new(Mutex::new(HookOutcome::default())),
            ToolCallContext::default(),
        );
        let name: String = e.eval(r#"tool_call_name()"#).unwrap();
        assert_eq!(name, "");
    }
}
