use crossbeam_channel::unbounded;
use harness::channels::{
    Channel, ChannelInboundMessage, ChannelOutboundMessage, TelegramChannel, config::TelegramConfig,
};
use std::time::Duration;
use wiremock::matchers::{body_string_contains, method, path};
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
        pairing_enabled: false,
        pairing_code: None,
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
            message_kind: harness::channels::MessageKind::Other,
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
                    "message_id": 1,
                    "from": {"id": 123, "username": "alice"},
                    "chat": {"id": 456, "type": "private"},
                    "date": 0,
                    "text": "hi"
                }
            }]
        })))
        .mount(&mock_server)
        .await;

    Mock::given(method("POST"))
        .and(path("/botTOKEN/setMessageReaction"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"ok": true, "result": true})),
        )
        .mount(&mock_server)
        .await;

    let cfg = TelegramConfig {
        bot_token: "TOKEN".to_string(),
        allowed_users: vec!["alice".to_string()],
        pairing_enabled: false,
        pairing_code: None,
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

#[tokio::test]
async fn telegram_bind_allows_user_and_replies() {
    let mock_server = MockServer::start().await;

    // First poll returns the /bind command.
    Mock::given(method("GET"))
        .and(path("/botTOKEN/getUpdates"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "result": [{
                "update_id": 1,
                "message": {
                    "message_id": 1,
                    "from": {"id": 123, "username": "alice"},
                    "chat": {"id": 456, "type": "private"},
                    "date": 0,
                    "text": "/bind secret"
                }
            }]
        })))
        .with_priority(1)
        .up_to_n_times(1)
        .mount(&mock_server)
        .await;

    // Second poll returns a normal message from the same user.
    Mock::given(method("GET"))
        .and(path("/botTOKEN/getUpdates"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "result": [{
                "update_id": 2,
                "message": {
                    "message_id": 1,
                    "from": {"id": 123, "username": "alice"},
                    "chat": {"id": 456, "type": "private"},
                    "date": 0,
                    "text": "hi"
                }
            }]
        })))
        .with_priority(2)
        .up_to_n_times(1)
        .mount(&mock_server)
        .await;

    // Subsequent polls return no updates.
    Mock::given(method("GET"))
        .and(path("/botTOKEN/getUpdates"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "result": []
        })))
        .with_priority(3)
        .mount(&mock_server)
        .await;

    Mock::given(method("POST"))
        .and(path("/botTOKEN/sendMessage"))
        .and(body_string_contains("已授权（本次运行有效）"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"ok": true, "result": {"message_id": 1}})),
        )
        .expect(1)
        .mount(&mock_server)
        .await;

    Mock::given(method("POST"))
        .and(path("/botTOKEN/setMessageReaction"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"ok": true, "result": true})),
        )
        .mount(&mock_server)
        .await;

    let cfg = TelegramConfig {
        bot_token: "TOKEN".to_string(),
        allowed_users: vec![],
        pairing_enabled: true,
        pairing_code: Some("secret".to_string()),
    };
    let channel = TelegramChannel::new(cfg).with_base_url(mock_server.uri());

    let (tx, rx) = unbounded::<ChannelInboundMessage>();
    let listen_handle = tokio::spawn(async move {
        let _ = channel.listen(tx).await;
    });

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

#[tokio::test]
async fn telegram_bind_success_does_not_send_ack_reaction() {
    let mock_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/botTOKEN/getUpdates"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "result": [{
                "update_id": 1,
                "message": {
                    "message_id": 1,
                    "from": {"id": 123, "username": "alice"},
                    "chat": {"id": 456, "type": "private"},
                    "date": 0,
                    "text": "/bind secret"
                }
            }]
        })))
        .with_priority(1)
        .up_to_n_times(1)
        .mount(&mock_server)
        .await;

    Mock::given(method("GET"))
        .and(path("/botTOKEN/getUpdates"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "result": []
        })))
        .with_priority(2)
        .mount(&mock_server)
        .await;

    Mock::given(method("POST"))
        .and(path("/botTOKEN/sendMessage"))
        .and(body_string_contains("已授权"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"ok": true, "result": {"message_id": 1}})),
        )
        .expect(1)
        .mount(&mock_server)
        .await;

    Mock::given(method("POST"))
        .and(path("/botTOKEN/setMessageReaction"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"ok": true, "result": true})),
        )
        .expect(0)
        .mount(&mock_server)
        .await;

    let cfg = TelegramConfig {
        bot_token: "TOKEN".to_string(),
        allowed_users: vec![],
        pairing_enabled: true,
        pairing_code: Some("secret".to_string()),
    };
    let channel = TelegramChannel::new(cfg).with_base_url(mock_server.uri());

    let (tx, _rx) = unbounded::<ChannelInboundMessage>();
    let listen_handle = tokio::spawn(async move {
        let _ = channel.listen(tx).await;
    });

    tokio::time::sleep(Duration::from_millis(300)).await;
    listen_handle.abort();
}

#[tokio::test]
async fn telegram_bind_wrong_code_replies_error() {
    let mock_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/botTOKEN/getUpdates"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "result": [{
                "update_id": 1,
                "message": {
                    "message_id": 1,
                    "from": {"id": 123, "username": "alice"},
                    "chat": {"id": 456, "type": "private"},
                    "date": 0,
                    "text": "/bind wrong"
                }
            }]
        })))
        .with_priority(1)
        .up_to_n_times(1)
        .mount(&mock_server)
        .await;

    Mock::given(method("GET"))
        .and(path("/botTOKEN/getUpdates"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "result": []
        })))
        .with_priority(2)
        .mount(&mock_server)
        .await;

    Mock::given(method("POST"))
        .and(path("/botTOKEN/sendMessage"))
        .and(body_string_contains("配对码错误"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"ok": true, "result": {"message_id": 1}})),
        )
        .expect(1)
        .mount(&mock_server)
        .await;

    let cfg = TelegramConfig {
        bot_token: "TOKEN".to_string(),
        allowed_users: vec![],
        pairing_enabled: true,
        pairing_code: Some("secret".to_string()),
    };
    let channel = TelegramChannel::new(cfg).with_base_url(mock_server.uri());

    let (tx, _rx) = unbounded::<ChannelInboundMessage>();
    let listen_handle = tokio::spawn(async move {
        let _ = channel.listen(tx).await;
    });

    tokio::time::sleep(Duration::from_millis(300)).await;
    listen_handle.abort();
}

#[tokio::test]
async fn telegram_bind_empty_pairing_code_replies_error() {
    let mock_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/botTOKEN/getUpdates"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "result": [{
                "update_id": 1,
                "message": {
                    "message_id": 1,
                    "from": {"id": 123, "username": "alice"},
                    "chat": {"id": 456, "type": "private"},
                    "date": 0,
                    "text": "/bind "
                }
            }]
        })))
        .with_priority(1)
        .up_to_n_times(1)
        .mount(&mock_server)
        .await;

    Mock::given(method("GET"))
        .and(path("/botTOKEN/getUpdates"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "result": []
        })))
        .with_priority(2)
        .mount(&mock_server)
        .await;

    Mock::given(method("POST"))
        .and(path("/botTOKEN/sendMessage"))
        .and(body_string_contains("配对码错误"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"ok": true, "result": {"message_id": 1}})),
        )
        .expect(1)
        .mount(&mock_server)
        .await;

    let cfg = TelegramConfig {
        bot_token: "TOKEN".to_string(),
        allowed_users: vec![],
        pairing_enabled: true,
        pairing_code: None,
    };
    let channel = TelegramChannel::new(cfg).with_base_url(mock_server.uri());

    let (tx, _rx) = unbounded::<ChannelInboundMessage>();
    let listen_handle = tokio::spawn(async move {
        let _ = channel.listen(tx).await;
    });

    tokio::time::sleep(Duration::from_millis(300)).await;
    listen_handle.abort();
}

#[tokio::test]
async fn telegram_bind_ignored_when_pairing_disabled() {
    let mock_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/botTOKEN/getUpdates"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "result": [{
                "update_id": 1,
                "message": {
                    "message_id": 1,
                    "from": {"id": 123, "username": "alice"},
                    "chat": {"id": 456, "type": "private"},
                    "date": 0,
                    "text": "/bind secret"
                }
            }]
        })))
        .mount(&mock_server)
        .await;

    Mock::given(method("POST"))
        .and(path("/botTOKEN/sendMessage"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"ok": true, "result": {"message_id": 1}})),
        )
        .expect(0)
        .mount(&mock_server)
        .await;

    let cfg = TelegramConfig {
        bot_token: "TOKEN".to_string(),
        allowed_users: vec![],
        pairing_enabled: false,
        pairing_code: Some("secret".to_string()),
    };
    let channel = TelegramChannel::new(cfg).with_base_url(mock_server.uri());

    let (tx, _rx) = unbounded::<ChannelInboundMessage>();
    let listen_handle = tokio::spawn(async move {
        let _ = channel.listen(tx).await;
    });

    tokio::time::sleep(Duration::from_millis(300)).await;
    listen_handle.abort();
}

#[tokio::test]
async fn telegram_bind_ignored_when_allowlist_not_empty() {
    let mock_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/botTOKEN/getUpdates"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "result": [{
                "update_id": 1,
                "message": {
                    "message_id": 1,
                    "from": {"id": 123, "username": "alice"},
                    "chat": {"id": 456, "type": "private"},
                    "date": 0,
                    "text": "/bind secret"
                }
            }]
        })))
        .mount(&mock_server)
        .await;

    Mock::given(method("POST"))
        .and(path("/botTOKEN/sendMessage"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"ok": true, "result": {"message_id": 1}})),
        )
        .expect(0)
        .mount(&mock_server)
        .await;

    let cfg = TelegramConfig {
        bot_token: "TOKEN".to_string(),
        allowed_users: vec!["alice".to_string()],
        pairing_enabled: true,
        pairing_code: Some("secret".to_string()),
    };
    let channel = TelegramChannel::new(cfg).with_base_url(mock_server.uri());

    let (tx, _rx) = unbounded::<ChannelInboundMessage>();
    let listen_handle = tokio::spawn(async move {
        let _ = channel.listen(tx).await;
    });

    tokio::time::sleep(Duration::from_millis(300)).await;
    listen_handle.abort();
}

#[tokio::test]
async fn telegram_callback_query_sends_confirmation_and_inbound() {
    let mock_server = MockServer::start().await;
    let request_uuid = "01912345-6789-7abc-8def-0123456789ab";
    let callback_data = format!("{}:allow_once", request_uuid);

    Mock::given(method("GET"))
        .and(path("/botTOKEN/getUpdates"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "result": [{
                "update_id": 1,
                "callback_query": {
                    "id": "cq1",
                    "from": {"id": 123, "username": "alice"},
                    "message": {"message_id": 10, "chat": {"id": 456, "type": "private"}},
                    "data": callback_data
                }
            }]
        })))
        .with_priority(1)
        .up_to_n_times(1)
        .mount(&mock_server)
        .await;

    Mock::given(method("GET"))
        .and(path("/botTOKEN/getUpdates"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "result": []
        })))
        .with_priority(2)
        .mount(&mock_server)
        .await;

    Mock::given(method("POST"))
        .and(path("/botTOKEN/answerCallbackQuery"))
        .and(body_string_contains("cq1"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"ok": true, "result": true})),
        )
        .expect(1)
        .mount(&mock_server)
        .await;

    Mock::given(method("POST"))
        .and(path("/botTOKEN/sendMessage"))
        .and(body_string_contains("已选择：allow_once"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"ok": true, "result": {"message_id": 11}})),
        )
        .expect(1)
        .mount(&mock_server)
        .await;

    let cfg = TelegramConfig {
        bot_token: "TOKEN".to_string(),
        allowed_users: vec!["alice".to_string()],
        pairing_enabled: false,
        pairing_code: None,
    };
    let channel = TelegramChannel::new(cfg).with_base_url(mock_server.uri());

    let (tx, rx) = unbounded::<ChannelInboundMessage>();
    let listen_handle = tokio::spawn(async move {
        let _ = channel.listen(tx).await;
    });

    let msg = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(m) = rx.try_recv() {
                return m;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("receive timeout");

    let confirmation = msg.confirmation.expect("confirmation");
    assert_eq!(confirmation.request_id.to_string(), request_uuid);
    assert_eq!(confirmation.option, "allow_once");

    listen_handle.abort();
}

#[tokio::test]
async fn telegram_unauthorized_callback_query_notifies_user() {
    let mock_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/botTOKEN/getUpdates"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "result": [{
                "update_id": 1,
                "callback_query": {
                    "id": "cq2",
                    "from": {"id": 999, "username": "mallory"},
                    "message": {"message_id": 20, "chat": {"id": 456, "type": "private"}},
                    "data": "01912345-6789-7abc-8def-0123456789ab:deny"
                }
            }]
        })))
        .with_priority(1)
        .up_to_n_times(1)
        .mount(&mock_server)
        .await;

    Mock::given(method("GET"))
        .and(path("/botTOKEN/getUpdates"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "result": []
        })))
        .with_priority(2)
        .mount(&mock_server)
        .await;

    Mock::given(method("POST"))
        .and(path("/botTOKEN/answerCallbackQuery"))
        .and(body_string_contains("cq2"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"ok": true, "result": true})),
        )
        .expect(1)
        .mount(&mock_server)
        .await;

    Mock::given(method("POST"))
        .and(path("/botTOKEN/sendMessage"))
        .and(body_string_contains("你没有权限操作此审批请求。"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"ok": true, "result": {"message_id": 21}})),
        )
        .expect(1)
        .mount(&mock_server)
        .await;

    let cfg = TelegramConfig {
        bot_token: "TOKEN".to_string(),
        allowed_users: vec!["alice".to_string()],
        pairing_enabled: false,
        pairing_code: None,
    };
    let channel = TelegramChannel::new(cfg).with_base_url(mock_server.uri());

    let (tx, rx) = unbounded::<ChannelInboundMessage>();
    let listen_handle = tokio::spawn(async move {
        let _ = channel.listen(tx).await;
    });

    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(rx.try_recv().is_err());

    listen_handle.abort();
}
