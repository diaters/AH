use super::*;
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

struct Fixture {
    client: Client,
    input: crossbeam_channel::Receiver<ExternalInput>,
    _dir: tempfile::TempDir,
}

impl Fixture {
    async fn upload(
        &self,
        token: &str,
        session: Uuid,
        id: Uuid,
        bytes: Vec<u8>,
    ) -> (StatusCode, Value) {
        let response = server::router(self.client.clone())
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!("/v1/sessions/{session}/files/{id}?name=test.txt"))
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::from(bytes))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), 100000).await.unwrap();
        (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
    }
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let config = ClientConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            instance_name: "test".into(),
            tls_cert: PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/client-tls/test-cert.pem"),
            tls_key: PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/client-tls/test-key.pem"),
            state_path: dir.path().join("state.json"),
            meow: None,
        };
        let (tx, input) = crossbeam_channel::unbounded();
        let store = Store::open(config.state_path.clone()).unwrap();
        Self {
            client: Client::new(config, store, tx),
            input,
            _dir: dir,
        }
    }

    fn pair(&self, user: &str) -> (Uuid, String) {
        let now = chrono::Utc::now().timestamp();
        let mut store = self.client.store.lock().unwrap();
        let code = store.issue_pairing(user, now).unwrap();
        let (device, token) = store.pair(&code, "phone", now).unwrap();
        (device.id, token)
    }

    async fn request(
        &self,
        token: &str,
        method: &str,
        path: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        let response = server::router(self.client.clone())
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 1_000_000).await.unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn session(&self, token: &str) -> Uuid {
        let (status, body) = self
            .request(
                token,
                "POST",
                "/v1/commands",
                json!({"operation":"create_session","command_id":Uuid::new_v4(),"title":"work"}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body["session_id"].as_str().unwrap().parse().unwrap()
    }
}

#[test]
fn identity_store_is_exclusive_persistent_and_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.json");
    let mut store = Store::open(path.clone()).unwrap();
    let instance = store.instance_id();
    assert!(Store::open(path.clone()).is_err());
    let code = store.issue_pairing("alice", 100).unwrap();
    assert!(store.pair(&code, "phone", 400).is_err());
    let code = store.issue_pairing("alice", 400).unwrap();
    let (device, token) = store.pair(&code, "phone", 401).unwrap();
    assert!(store.pair(&code, "tablet", 401).is_err());
    assert!(store.authenticate(&token, device.expires_at).is_err());
    let bytes = std::fs::read_to_string(&path).unwrap();
    assert!(!bytes.contains(&code) && !bytes.contains(&token));
    drop(store);
    let mut store = Store::open(path.clone()).unwrap();
    assert_eq!(store.instance_id(), instance);
    assert_eq!(store.authenticate(&token, 402).unwrap().id, device.id);
    store.revoke("alice", device.id).unwrap();
    assert!(store.authenticate(&token, 402).is_err());
    drop(store);
    std::fs::write(&path, "broken").unwrap();
    assert!(Store::open(path.clone()).is_err());
    assert_eq!(std::fs::read_to_string(path).unwrap(), "broken");
}

#[tokio::test]
async fn pairing_requires_instance_identity_and_never_trusts_supplied_user() {
    let f = Fixture::new();
    let now = chrono::Utc::now().timestamp();
    let (code, instance) = {
        let mut store = f.client.store.lock().unwrap();
        (
            store.issue_pairing("alice", now).unwrap(),
            store.instance_id(),
        )
    };
    let (status, _) = f
        .request(
            "",
            "POST",
            "/v1/pair",
            json!({"instance_id":Uuid::new_v4(),"code":code,"device_name":"phone"}),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = f
        .request(
            "",
            "POST",
            "/v1/pair",
            json!({"instance_id":instance,"code":code,"device_name":"phone","user_id":"admin"}),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, body) = f
        .request(
            "",
            "POST",
            "/v1/pair",
            json!({"instance_id":instance,"code":code,"device_name":"phone"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["device"]["user_id"], "alice");
    assert!(body["device"].get("token_hash").is_none());
    let (status, _) = f.request("bad", "GET", "/v1/snapshot", Value::Null).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn commands_preserve_sender_scope_and_deduplicate_across_restart() {
    let f = Fixture::new();
    let (_, alice) = f.pair("alice");
    let (_, bob) = f.pair("bob");
    let session_id = f.session(&alice).await;
    let command_id = Uuid::new_v4();
    let request = json!({"operation":"send_text","command_id":command_id,"session_id":session_id,"content":"hello"});
    assert_eq!(
        f.request(&bob, "POST", "/v1/commands", request.clone())
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let (status, receipt) = f
        .request(&alice, "POST", "/v1/commands", request.clone())
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(receipt["status"], "queued");
    assert_eq!(
        f.request(&alice, "POST", "/v1/commands", request.clone())
            .await
            .1,
        receipt
    );
    match f.input.try_recv().unwrap() {
        ExternalInput::TextWithChannel {
            channel,
            sender,
            content,
        } => {
            assert_eq!(channel.frontend, FrontendKind::Client);
            assert_eq!(channel.thread_id, Some(session_id.to_string()));
            assert_eq!(channel.user_id, "alice");
            assert_eq!(sender.as_deref(), Some("alice"));
            assert_eq!(content, "hello");
        }
        _ => panic!("expected unified text input"),
    }
    assert!(f.input.try_recv().is_err());
    let mut conflicting = request.clone();
    conflicting["content"] = "different".into();
    assert_eq!(
        f.request(&alice, "POST", "/v1/commands", conflicting)
            .await
            .0,
        StatusCode::CONFLICT
    );
    let (_, snapshot) = f.request(&bob, "GET", "/v1/snapshot", Value::Null).await;
    assert!(snapshot["sessions"].as_array().unwrap().is_empty());
    let config = f.client.config.clone();
    drop(f.client);
    let store = Store::open(config.state_path.clone()).unwrap();
    assert_eq!(
        store.command_status("alice", command_id).unwrap()["status"],
        "unknown_after_restart"
    );
}

#[tokio::test]
async fn snapshot_cursor_filters_broadcast_and_retention_requires_resync() {
    let f = Fixture::new();
    let (_, token) = f.pair("alice");
    let session_id = f.session(&token).await;
    f.client.push_event(EngineEvent::Text {
        target: EventTarget::Broadcast,
        role: MessageRole::Agent,
        content: "secret broadcast".into(),
        task_id: None,
    });
    let target = EventTarget::Directed(vec![crate::domain::ChannelId {
        frontend: FrontendKind::Client,
        user_id: "alice".into(),
        thread_id: Some(session_id.to_string()),
    }]);
    f.client.push_event(EngineEvent::Text {
        target,
        role: MessageRole::Agent,
        content: "reply".into(),
        task_id: None,
    });
    let (_, snapshot) = f.request(&token, "GET", "/v1/snapshot", Value::Null).await;
    assert_eq!(snapshot["cursor"], 1);
    assert_eq!(snapshot["sessions"][0]["messages"][0]["content"], "reply");
    let (_, events) = f
        .request(&token, "GET", "/v1/events?after=0", Value::Null)
        .await;
    assert_eq!(events["events"].as_array().unwrap().len(), 1);
    let (_, events) = f
        .request(&token, "GET", "/v1/events?after=1", Value::Null)
        .await;
    assert!(events["events"].as_array().unwrap().is_empty());
    let mut store = f.client.store.lock().unwrap();
    // 大批量行为测试在单次持久化中执行，避免为测试进行数千次 fsync。
    store.test_expire_cursor(session_id);
    assert!(matches!(store.events("alice", 0), Err(ClientError::Resync)));
}

#[tokio::test]
async fn approval_is_claimed_once_and_keeps_authenticated_sender() {
    let f = Fixture::new();
    let (_, phone) = f.pair("alice");
    let (_, tablet) = f.pair("alice");
    let session_id = f.session(&phone).await;
    let request_id = Uuid::new_v4();
    f.client.push_event(EngineEvent::ApprovalRequest {
        target: EventTarget::Directed(vec![crate::domain::ChannelId {
            frontend: FrontendKind::Client,
            user_id: "alice".into(),
            thread_id: Some(session_id.to_string()),
        }]),
        request_id,
        agent_name: "agent".into(),
        tool_name: "shell_exec".into(),
        tool_input: json!({}),
        options: vec![crate::domain::ApprovalOption {
            id: "allow".into(),
            label: "允许一次".into(),
            description: "".into(),
        }],
        approval_context: None,
    });
    let command = |id| {
        json!({"operation":"respond_approval","command_id":id,"session_id":session_id,
        "request_id":request_id,"expected_version":1,"option_id":"allow","feedback":null})
    };
    let one = command(Uuid::new_v4());
    let two = command(Uuid::new_v4());
    let (a, b) = tokio::join!(
        f.request(&phone, "POST", "/v1/commands", one.clone()),
        f.request(&tablet, "POST", "/v1/commands", two)
    );
    assert_eq!(a.0, StatusCode::OK);
    assert_eq!(b.0, StatusCode::CONFLICT);
    assert_eq!(
        f.request(&phone, "POST", "/v1/commands", one).await.0,
        StatusCode::OK
    );
    match f.input.try_recv().unwrap() {
        ExternalInput::Confirmation {
            request_id: id,
            sender,
            option,
            ..
        } => {
            assert_eq!(id, request_id);
            assert_eq!(sender.as_deref(), Some("alice"));
            assert_eq!(option, "allow");
        }
        _ => panic!("expected unified confirmation"),
    }
    assert!(f.input.try_recv().is_err());
}

#[tokio::test]
async fn structured_controls_keep_target_and_runtime_receipts_are_durable() {
    let f = Fixture::new();
    let (_, alice) = f.pair("alice");
    let (_, bob) = f.pair("bob");
    let session = f.session(&alice).await;
    let task_id = crate::domain::TaskId::new();
    let target = EventTarget::Directed(vec![crate::domain::ChannelId {
        frontend: FrontendKind::Client,
        user_id: "alice".into(),
        thread_id: Some(session.to_string()),
    }]);
    f.client
        .store
        .lock()
        .unwrap()
        .append(
            "alice",
            session,
            "task_status",
            json!({"task_id":task_id,"status":"running"}),
        )
        .unwrap();
    f.client.push_event(EngineEvent::TaskInteractionChanged {
        target: target.clone(),
        task_id,
        revision: "current".into(),
        question_id: None,
        can_stop: true,
        can_finish: true,
    });
    let id = Uuid::new_v4();
    let mut request = json!({"operation":"stop_task","command_id":id,"session_id":session,"task_id":task_id,"expected_revision":"old"});
    assert_eq!(
        f.request(&alice, "POST", "/v1/commands", request.clone())
            .await
            .0,
        StatusCode::CONFLICT
    );
    request["expected_revision"] = "current".into();
    assert_eq!(
        f.request(&bob, "POST", "/v1/commands", request.clone())
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        f.request(&alice, "POST", "/v1/commands", request.clone())
            .await
            .0,
        StatusCode::OK
    );
    match f.input.try_recv().unwrap() {
        ExternalInput::Interaction {
            channel,
            command_id,
            interaction:
                crate::domain::TaskInteraction::Stop {
                    task_id: actual, ..
                },
        } => {
            assert_eq!(command_id, id);
            assert_eq!(actual, task_id);
            assert_eq!(channel.user_id, "alice");
        }
        _ => panic!("typed control required"),
    }
    f.client.push_event(EngineEvent::CommandResult {
        target,
        command_id: id,
        outcome: Err("task_changed".into()),
    });
    let (_, receipt) = f
        .request(&alice, "GET", &format!("/v1/commands/{id}"), Value::Null)
        .await;
    assert_eq!(receipt["status"], "rejected");
    assert_eq!(receipt["result_code"], "task_changed");
    let (_, retry) = f.request(&alice, "POST", "/v1/commands", request).await;
    assert_eq!(retry, receipt);
    assert!(f.input.try_recv().is_err());
}

#[tokio::test]
async fn question_answers_cannot_rebind_or_claim_twice() {
    let f = Fixture::new();
    let (_, phone) = f.pair("alice");
    let (_, tablet) = f.pair("alice");
    let session = f.session(&phone).await;
    let task_id = crate::domain::TaskId::new();
    let target = EventTarget::Directed(vec![crate::domain::ChannelId {
        frontend: FrontendKind::Client,
        user_id: "alice".into(),
        thread_id: Some(session.to_string()),
    }]);
    f.client.push_event(EngineEvent::QuestionPresented {
        target: target.clone(),
        task_id,
        question_id: "question".into(),
        question: "继续吗？".into(),
    });
    let mut request = json!({"operation":"answer_question","command_id":Uuid::new_v4(),"session_id":session,"task_id":task_id,"question_id":"expired","answer":"是"});
    assert_eq!(
        f.request(&phone, "POST", "/v1/commands", request.clone())
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    request["question_id"] = "question".into();
    assert_eq!(
        f.request(&phone, "POST", "/v1/commands", request.clone())
            .await
            .0,
        StatusCode::OK
    );
    request["command_id"] = Uuid::new_v4().to_string().into();
    assert_eq!(
        f.request(&tablet, "POST", "/v1/commands", request).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(f.input.len(), 1);
    f.client.push_event(EngineEvent::TaskInteractionChanged {
        target,
        task_id,
        revision: "answered".into(),
        question_id: None,
        can_stop: false,
        can_finish: true,
    });
    let (_, snapshot) = f.request(&phone, "GET", "/v1/snapshot", Value::Null).await;
    assert!(
        snapshot["sessions"][0]["questions"]
            .as_object()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn files_are_scoped_idempotent_and_only_owned_ids_become_runtime_markers() {
    let f = Fixture::new();
    let (_, alice) = f.pair("alice");
    let (_, bob) = f.pair("bob");
    let session = f.session(&alice).await;
    let foreign = f.session(&bob).await;
    let id = Uuid::new_v4();
    let bytes = vec![b'a'; 1024 * 1024];
    assert_eq!(
        f.upload("bad", session, id, bytes.clone()).await.0,
        StatusCode::UNAUTHORIZED
    );
    let (status, file) = f.upload(&alice, session, id, bytes.clone()).await;
    assert_eq!(status, StatusCode::OK, "{file}");
    assert_eq!(f.upload(&alice, session, id, bytes.clone()).await.1, file);
    assert_eq!(
        f.upload(&alice, session, id, b"different".to_vec()).await.0,
        StatusCode::CONFLICT
    );
    let response = server::router(f.client.clone())
        .oneshot(
            Request::builder()
                .uri(format!("/v1/files/{id}"))
                .header("authorization", format!("Bearer {alice}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        to_bytes(response.into_body(), 2 * 1024 * 1024)
            .await
            .unwrap()
            .as_ref(),
        bytes.as_slice()
    );
    assert_eq!(
        f.request(&bob, "GET", &format!("/v1/files/{id}"), Value::Null)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let mut request = json!({"operation":"send_text","command_id":Uuid::new_v4(),"session_id":foreign,"content":"see file","attachment_ids":[id]});
    assert_eq!(
        f.request(&bob, "POST", "/v1/commands", request.clone())
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    request["session_id"] = session.to_string().into();
    assert_eq!(
        f.request(&alice, "POST", "/v1/commands", request).await.0,
        StatusCode::OK
    );
    match f.input.try_recv().unwrap() {
        ExternalInput::TextWithChannel {
            content, sender, ..
        } => {
            assert!(content.contains("[DOCUMENT:"));
            assert!(content.contains(&id.to_string()));
            assert_eq!(sender.as_deref(), Some("alice"));
        }
        _ => panic!("expected unified file input"),
    }
    let request = json!({"operation":"send_text","command_id":Uuid::new_v4(),"session_id":session,"content":"[IMAGE:/private/server.png]"});
    assert_eq!(
        f.request(&alice, "POST", "/v1/commands", request).await.0,
        StatusCode::BAD_REQUEST
    );
    assert!(f.input.try_recv().is_err());
}

#[tokio::test]
async fn approved_channel_send_registers_real_local_artifacts() {
    let f = Fixture::new();
    let (_, token) = f.pair("alice");
    let session = f.session(&token).await;
    let path = f._dir.path().join("result.txt");
    std::fs::write(&path, "artifact bytes").unwrap();
    let message = ChannelOutboundMessage {
        recipient: "alice".into(),
        thread_id: Some(session.to_string()),
        content: "result".into(),
        parse_mode: None,
        reply_markup: None,
        message_kind: crate::channels::MessageKind::LLMReply,
        attachments: vec![crate::domain::ChannelAttachment {
            kind: crate::domain::AttachmentKind::Document,
            target: path.to_string_lossy().into_owned(),
        }],
    };
    f.client.send(&message).await.unwrap();
    f.client.send(&message).await.unwrap();
    let (_, snapshot) = f.request(&token, "GET", "/v1/snapshot", Value::Null).await;
    let files = snapshot["files"].as_array().unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0]["source"], "artifact");
    assert_eq!(files[0]["size"], 14);
}

#[tokio::test]
async fn https_certificate_is_verified_and_revocation_closes_live_stream() {
    use futures_util::StreamExt;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let _ = rustls::crypto::ring::default_provider().install_default();
    let f = Fixture::new();
    let (device, token) = f.pair("alice");
    let server = ClientServer::start(f.client.clone()).await.unwrap();
    let url = format!("https://localhost:{}", server.address.port());
    let cert = include_bytes!("../../../tests/fixtures/client-tls/test-cert.pem");
    let http = reqwest::Client::builder()
        .add_root_certificate(reqwest::Certificate::from_pem(cert).unwrap())
        .build()
        .unwrap();
    assert!(
        reqwest::get(format!("{url}/v1/capabilities"))
            .await
            .is_err(),
        "untrusted test certificate must fail"
    );
    let response = http
        .get(format!("{url}/v1/capabilities"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut roots = rustls::RootCertStore::empty();
    use base64::Engine;
    let pem = std::str::from_utf8(cert)
        .unwrap()
        .lines()
        .filter(|line| !line.starts_with("---"))
        .collect::<String>();
    roots
        .add(
            base64::engine::general_purpose::STANDARD
                .decode(pem)
                .unwrap()
                .into(),
        )
        .unwrap();
    let tls = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let mut request = format!(
        "wss://localhost:{}/v1/stream?after=0",
        server.address.port()
    )
    .into_client_request()
    .unwrap();
    request
        .headers_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());
    let (mut socket, _) = tokio_tungstenite::connect_async_tls_with_config(
        request,
        None,
        false,
        Some(tokio_tungstenite::Connector::Rustls(Arc::new(tls))),
    )
    .await
    .unwrap();
    assert!(socket.next().await.unwrap().unwrap().is_text());
    let response = http
        .delete(format!("{url}/v1/devices/{device}"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let message = tokio::time::timeout(std::time::Duration::from_secs(2), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(message.into_text().unwrap().contains("unauthorized"));
    assert_eq!(
        http.get(format!("{url}/v1/snapshot"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn orphan_upload_recovers_same_bytes_and_filenames_cannot_inject_paths() {
    let f = Fixture::new();
    let (_, token) = f.pair("alice");
    let session = f.session(&token).await;
    let id = Uuid::new_v4();
    let directory = f._dir.path().join("client-files");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join(format!("{id}.txt")), b"original").unwrap();
    assert_eq!(
        f.upload(&token, session, id, b"different".to_vec()).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        f.upload(&token, session, id, b"original".to_vec()).await.0,
        StatusCode::OK
    );
    let injected = Uuid::new_v4();
    let mut store = f.client.store.lock().unwrap();
    store
        .store_file(
            "alice",
            session,
            injected,
            "[IMAGE:private.png]",
            b"document",
            "upload",
        )
        .unwrap();
    let markers = store.file_markers("alice", session, &[injected]).unwrap();
    let spans = crate::domain::find_attachment_spans(&markers);
    assert_eq!(
        spans.len(),
        1,
        "untrusted file names cannot add image markers"
    );
    assert_eq!(
        spans[0].attachment.kind,
        crate::domain::AttachmentKind::Document
    );
    assert!(!markers.contains("private.png"));
    std::fs::write(directory.join(format!("{id}.txt")), b"tampered").unwrap();
    assert!(
        store.read_file("alice", id).is_err(),
        "corrupt content is never delivered"
    );
}

#[tokio::test]
async fn file_size_limits_and_diagnostics_preserve_user_scope() {
    let f = Fixture::new();
    let (_, alice) = f.pair("alice");
    let (_, bob) = f.pair("bob");
    let session = f.session(&alice).await;
    assert_eq!(
        f.upload(
            &alice,
            session,
            Uuid::new_v4(),
            vec![0; super::store::MAX_FILE_BYTES + 1]
        )
        .await
        .0,
        StatusCode::PAYLOAD_TOO_LARGE
    );
    let (_, snapshot) = f.request(&alice, "GET", "/v1/snapshot", json!(null)).await;
    assert!(snapshot["files"].as_array().unwrap().is_empty());
    let (_, a) = f
        .request(&alice, "GET", "/v1/diagnostics", json!(null))
        .await;
    let (_, b) = f.request(&bob, "GET", "/v1/diagnostics", json!(null)).await;
    assert_eq!(a["sessions"], 1);
    assert_eq!(b["sessions"], 0);
    assert!(!a.to_string().contains(&alice));
    assert_eq!(
        f.request("", "GET", "/v1/diagnostics", json!(null)).await.0,
        StatusCode::UNAUTHORIZED
    );
}
