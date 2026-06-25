use crossbeam_channel::Sender;
use rhai::{Dynamic, Engine};

#[derive(Clone)]
pub struct MessageContext {
    pub plugin_id: String,
    pub tx: Sender<EmittedMessage>,
}

#[derive(Debug, Clone)]
pub struct EmittedMessage {
    pub plugin_id: String,
    pub channel: String,
    pub payload: serde_json::Value,
}

pub fn register(engine: &mut Engine, ctx: MessageContext) {
    let c = ctx.clone();
    engine.register_fn("emit_message", move |channel: &str, payload: Dynamic| {
        let plugin_id = c.plugin_id.clone();
        let payload_json =
            crate::user_plugins::host_api::entity_write::rhai_dynamic_to_json(payload);
        // v1 限制：插件消息尚未接入路由层，仅记录日志。
        tracing::debug!(
            event = "PluginMessageEmitted",
            plugin_id = %plugin_id,
            channel = %channel,
            "plugin emitted message (v1: not yet routed)"
        );
        let _ = c.tx.send(EmittedMessage {
            plugin_id,
            channel: channel.to_string(),
            payload: payload_json,
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::unbounded;

    #[test]
    fn emit_message_sends_typed_message() {
        let (tx, rx) = unbounded();
        let mut e = Engine::new();
        register(
            &mut e,
            MessageContext {
                plugin_id: "p".into(),
                tx,
            },
        );
        e.eval::<()>(r#"emit_message("progress", "halfway")"#)
            .unwrap();
        let m = rx.recv().unwrap();
        assert_eq!(m.plugin_id, "p");
        assert_eq!(m.channel, "progress");
        assert_eq!(m.payload, serde_json::Value::String("halfway".into()));
    }
}
