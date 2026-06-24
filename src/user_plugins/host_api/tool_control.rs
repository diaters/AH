use rhai::Engine;
use tracing::warn;

use crate::user_plugins::dispatcher::SharedHookOutcome;

pub fn register(engine: &mut Engine, outcome: SharedHookOutcome) {
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
}

fn rhai_to_json(v: rhai::Dynamic) -> serde_json::Value {
    serde_json::Value::String(v.to_string())
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    #[test]
    fn tool_deny_sets_reason() {
        let outcome = Arc::new(Mutex::new(
            crate::user_plugins::dispatcher::HookOutcome::default(),
        ));
        let mut e = Engine::new();
        register(&mut e, outcome.clone());
        let _: () = e.eval(r#"tool_deny("blocked")"#).unwrap();
        assert_eq!(
            outcome.lock().unwrap().deny_reason.as_deref(),
            Some("blocked")
        );
    }

    #[test]
    fn tool_set_result_sets_value() {
        let outcome = Arc::new(Mutex::new(
            crate::user_plugins::dispatcher::HookOutcome::default(),
        ));
        let mut e = Engine::new();
        register(&mut e, outcome.clone());
        let _: () = e.eval(r#"tool_set_result("hello")"#).unwrap();
        assert!(outcome.lock().unwrap().replaced_result.is_some());
    }
}
