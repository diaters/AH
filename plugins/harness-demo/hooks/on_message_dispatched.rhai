// on_message_dispatched: 观察 hook — 演示 emit_message
log_info("[demo] on_message_dispatched: message has been dispatched");

// 演示 emit_message：向 "demo-events" 频道发送跨插件消息
emit_message("demo-events", "message dispatched observed");
