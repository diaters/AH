use crossbeam_channel::unbounded;
use harness::channels::{
    Channel, ChannelInboundMessage, ChannelOutboundMessage, TelegramChannel, config::TelegramConfig,
};
use std::time::Duration;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn telegram_send_message() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/botTOKEN/sendMessage"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"ok": true, "result": {"message_id": 1}})),
        )
        .mount(&mock_server)
        .await;

    let cfg = TelegramConfig {
        bot_token: "TOKEN".to_string(),
        allowed_users: vec!["u".to_string()],
    };
    let channel = TelegramChannel::new(cfg).with_base_url(mock_server.uri());

    channel
        .send(&ChannelOutboundMessage {
            recipient: "123".to_string(),
            thread_id: None,
            content: "hello".to_string(),
            parse_mode: None,
            reply_markup: None,
            attachments: vec![],
        })
        .await
        .expect("send");
}

#[tokio::test]
async fn telegram_listen_receives_update() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/botTOKEN/getUpdates"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "result": [{
                "update_id": 42,
                "message": {
                    "from": {"id": 123, "username": "alice"},
                    "chat": {"id": 456, "type": "private"},
                    "date": 0,
                    "text": "hi"
                }
            }]
        })))
        .mount(&mock_server)
        .await;

    let cfg = TelegramConfig {
        bot_token: "TOKEN".to_string(),
        allowed_users: vec!["alice".to_string()],
    };
    let channel = TelegramChannel::new(cfg).with_base_url(mock_server.uri());

    let (tx, rx) = unbounded::<ChannelInboundMessage>();
    let listen_handle = tokio::spawn(async move {
        let _ = channel.listen(tx).await;
    });

    // Poll for the message since crossbeam recv may race with tokio spawn
    let msg = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(msg) = rx.try_recv() {
                return msg;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("receive timeout");
    assert_eq!(msg.sender_id, "123");
    assert_eq!(msg.content, "hi");

    listen_handle.abort();
}
