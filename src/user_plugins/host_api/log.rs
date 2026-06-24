use rhai::Engine;
use tracing::{error, info, warn};

pub fn register(engine: &mut Engine) {
    engine.register_fn("log_info", |msg: &str| {
        info!(event = "PluginLog", level = "info", "{}", msg);
    });
    engine.register_fn("log_warn", |msg: &str| {
        warn!(event = "PluginLog", level = "warn", "{}", msg);
    });
    engine.register_fn("log_error", |msg: &str| {
        error!(event = "PluginLog", level = "error", "{}", msg);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use rhai::Engine;

    #[test]
    fn log_info_does_not_panic() {
        let mut e = Engine::new();
        register(&mut e);
        let r = e.eval::<()>("log_info(\"test\");");
        assert!(r.is_ok());
    }
}
