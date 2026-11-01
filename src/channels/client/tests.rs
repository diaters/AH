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
            tls_cert: Some(
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/fixtures/client-tls/test-cert.pem"),
            ),
            tls_key: Some(
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/fixtures/client-tls/test-key.pem"),
            ),
            advertise: vec![],
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
        "request_id":request_id,"expected_version":1,"option_id":"allow","feedback":"不要动生产库"})
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
            feedback,
        } => {
            assert_eq!(id, request_id);
            assert_eq!(sender.as_deref(), Some("alice"));
            assert_eq!(option, "allow");
            assert_eq!(
                feedback.as_deref(),
                Some("不要动生产库"),
                "服务端必须把审批说明原样交给运行时，不能在通道层吞掉"
            );
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
    let (_, snapshot) = f.request(&alice, "GET", "/v1/snapshot", Value::Null).await;
    let sent = snapshot["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|s| s["messages"].as_array().unwrap().iter())
        .find(|m| m["content"] == "see file")
        .unwrap()
        .clone();
    assert_eq!(sent["attachment_ids"][0], id.to_string());
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
    assert_eq!(files[0]["kind"], "text");
    // 消息携带产物 ID，客户端据此在对话中内联展示。
    let messages = snapshot["sessions"][0]["messages"].as_array().unwrap();
    assert_eq!(
        messages.last().unwrap()["attachment_ids"][0],
        files[0]["id"]
    );
    // 原机产物允许超过设备上传上限（如短视频），并按类型标注供客户端选择预览方式。
    let video = f._dir.path().join("demo.mp4");
    std::fs::write(&video, vec![7u8; super::store::MAX_FILE_BYTES + 1]).unwrap();
    let mut message = message;
    message.attachments[0].target = video.to_string_lossy().into_owned();
    f.client.send(&message).await.unwrap();
    let (_, snapshot) = f.request(&token, "GET", "/v1/snapshot", Value::Null).await;
    let video = snapshot["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|file| file["name"] == "demo.mp4")
        .unwrap()
        .clone();
    assert_eq!(video["kind"], "video");
    let id: Uuid = video["id"].as_str().unwrap().parse().unwrap();
    let (_, bytes) = f
        .client
        .store
        .lock()
        .unwrap()
        .read_file("alice", id)
        .unwrap();
    assert_eq!(bytes.len(), super::store::MAX_FILE_BYTES + 1);
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

#[tokio::test]
async fn managed_ca_is_fetched_by_fingerprint_then_verifies_tls() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut f = Fixture::new();
    f.client.config.tls_cert = None;
    f.client.config.tls_key = None;
    f.client.config.listen = "127.0.0.1:0".parse().unwrap();
    let server = ClientServer::start(f.client.clone()).await.unwrap();
    let url = format!("https://127.0.0.1:{}", server.address.port());
    // 首次获取 CA 不校验证书，信任来自配对链接中的指纹。
    let insecure = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .build()
        .unwrap();
    let pem = insecure
        .get(format!("{url}/v1/ca"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let expected = super::tls::load(&f.client.config).unwrap().ca_fingerprint;
    assert_eq!(super::tls::fingerprint(&pem).ok(), expected);
    let http = reqwest::Client::builder()
        .add_root_certificate(reqwest::Certificate::from_pem(pem.as_bytes()).unwrap())
        .build()
        .unwrap();
    let identity: Value = http
        .get(format!("{url}/v1/identity"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(identity["protocol_version"], 1);
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn conversation_records_progress_tools_and_response_in_order() {
    let f = Fixture::new();
    let (_, token) = f.pair("alice");
    let session_id = f.session(&token).await;
    let task_id = crate::domain::TaskId::new();
    let target = EventTarget::Directed(vec![crate::domain::ChannelId {
        frontend: FrontendKind::Client,
        user_id: "alice".into(),
        thread_id: Some(session_id.to_string()),
    }]);
    f.client.push_event(EngineEvent::AgentOutput {
        target: target.clone(),
        task_id,
        content: "checking".into(),
        progress: true,
    });
    f.client.push_event(EngineEvent::ToolCallStarted {
        target: target.clone(),
        task_id,
        agent_name: "agent".into(),
        tool_name: "shell".into(),
        tool_input_summary: "ls".into(),
    });
    f.client.push_event(EngineEvent::ToolCallCompleted {
        target: target.clone(),
        task_id,
        call_id: Some("call".into()),
        agent_name: "agent".into(),
        tool_name: "shell".into(),
        success: true,
        output: "ok".into(),
        truncated: false,
        late_result: false,
        background: None,
    });
    f.client.push_event(EngineEvent::AgentOutput {
        target,
        task_id,
        content: "answer".into(),
        progress: false,
    });
    let (_, snapshot) = f.request(&token, "GET", "/v1/snapshot", Value::Null).await;
    let session = &snapshot["sessions"][0];
    assert_eq!(session["messages"][0]["phase"], "progress");
    assert_eq!(session["messages"][1]["phase"], "response");
    let sequence = |value: &Value| value["sequence"].as_u64().unwrap();
    assert!(sequence(&session["messages"][0]) < sequence(&session["observations"][0]));
    assert!(sequence(&session["observations"][0]) < sequence(&session["observations"][1]));
    assert!(sequence(&session["observations"][1]) < sequence(&session["messages"][1]));
}

// ============ 经验总结与经验库 ============

/// 测试用内存经验库（通道层不得依赖 infrastructure 的文件实现）。
#[derive(Default)]
struct MemoryLibrary(
    std::sync::Mutex<std::collections::BTreeMap<String, crate::domain::ExperienceEntry>>,
);

impl crate::contracts::ExperienceLibrary for MemoryLibrary {
    fn list(&self) -> anyhow::Result<Vec<crate::domain::ExperienceEntry>> {
        Ok(self.0.lock().unwrap().values().cloned().collect())
    }
    fn get(&self, id: &str) -> anyhow::Result<Option<crate::domain::ExperienceEntry>> {
        Ok(self.0.lock().unwrap().get(id).cloned())
    }
    fn save(&self, entry: &crate::domain::ExperienceEntry) -> anyhow::Result<()> {
        entry.validate()?;
        self.0
            .lock()
            .unwrap()
            .insert(entry.id.clone(), entry.clone());
        Ok(())
    }
    fn delete(&self, id: &str) -> anyhow::Result<bool> {
        Ok(self.0.lock().unwrap().remove(id).is_some())
    }
}

fn with_library(fixture: Fixture) -> (Fixture, crate::contracts::SharedExperienceLibrary) {
    let library: crate::contracts::SharedExperienceLibrary =
        std::sync::Arc::new(MemoryLibrary::default());
    let Fixture {
        client,
        input,
        _dir,
    } = fixture;
    (
        Fixture {
            client: client.with_experience_library(library.clone()),
            input,
            _dir,
        },
        library,
    )
}

fn seed_history(fixture: &Fixture, user: &str, session: Uuid) {
    let mut store = fixture.client.store.lock().unwrap();
    store
        .append(
            user,
            session,
            "text",
            json!({"role":"user","content":"hdc install 一直报 9568322"}),
        )
        .unwrap();
    store
        .append(
            user,
            session,
            "tool_started",
            json!({"task_id":"t","tool_name":"shell_exec","input_summary":"hdc install a.hap"}),
        )
        .unwrap();
    let output = format!("{}error: install failed code 9568322", "log ".repeat(2000));
    store
        .append(
            user,
            session,
            "tool_completed",
            json!({"task_id":"t","tool_name":"shell_exec","success":false,"output":output}),
        )
        .unwrap();
}

fn draft_ready(fixture: &Fixture, user: &str, session: Uuid, draft: Uuid, merge: Option<String>) {
    fixture
        .client
        .push_event(EngineEvent::ExperienceDraftUpdated {
            target: EventTarget::Directed(vec![crate::domain::ChannelId {
                frontend: FrontendKind::Client,
                user_id: user.into(),
                thread_id: Some(session.to_string()),
            }]),
            draft_id: draft,
            outcome: Ok(crate::domain::ExperienceDraftContent {
                title: "HAP 安装签名错误".into(),
                body: "## 适用场景\n9568322".into(),
                merge_target: merge,
            }),
        });
}

#[tokio::test]
async fn experience_capability_requires_library() {
    let fixture = Fixture::new();
    let (_, token) = fixture.pair("u");
    let (_, caps) = fixture
        .request(&token, "GET", "/v1/capabilities", Value::Null)
        .await;
    assert!(
        !caps["operations"]
            .as_array()
            .unwrap()
            .contains(&json!("experiences.manage"))
    );
    let (status, _) = fixture
        .request(&token, "GET", "/v1/experiences", Value::Null)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (fixture, _) = with_library(fixture);
    let (_, caps) = fixture
        .request(&token, "GET", "/v1/capabilities", Value::Null)
        .await;
    assert!(
        caps["operations"]
            .as_array()
            .unwrap()
            .contains(&json!("experiences.manage"))
    );
}

#[tokio::test]
async fn experience_draft_lifecycle_from_session_history_to_library() {
    let (fixture, library) = with_library(Fixture::new());
    let (_, token) = fixture.pair("u");
    let session = fixture.session(&token).await;
    seed_history(&fixture, "u", session);
    let drafts = format!("/v1/sessions/{session}/experience-drafts");
    let draft = Uuid::new_v4();

    let (status, body) = fixture
        .request(
            &token,
            "POST",
            &drafts,
            json!({"draft_id":draft,"note":"记下签名"}),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let Ok(ExternalInput::ExperienceDraft {
        channel,
        draft_id,
        transcript,
        note,
    }) = fixture.input.try_recv()
    else {
        panic!("draft request must reach the runtime");
    };
    assert_eq!(draft_id, draft);
    assert_eq!(
        channel.thread_id.as_deref(),
        Some(session.to_string().as_str())
    );
    assert_eq!(note.as_deref(), Some("记下签名"));
    assert!(transcript.contains("[用户] hdc install 一直报 9568322"));
    assert!(transcript.contains("[调用 shell_exec] hdc install a.hap"));
    assert!(
        transcript.contains("[失败 shell_exec]")
            && transcript.contains("install failed code 9568322"),
        "failed output tail must survive truncation: {transcript}"
    );

    // 同一草稿幂等；另起草稿在生成中被拒绝；未就绪不能保存。
    let (status, _) = fixture
        .request(&token, "POST", &drafts, json!({"draft_id":draft}))
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = fixture
        .request(&token, "POST", &drafts, json!({"draft_id":Uuid::new_v4()}))
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let save = format!("{drafts}/{draft}/save");
    let (status, _) = fixture
        .request(&token, "POST", &save, json!({"title":"t","body":"b"}))
        .await;
    assert_eq!(status, StatusCode::CONFLICT);

    draft_ready(&fixture, "u", session, draft, None);
    let (_, snapshot) = fixture
        .request(&token, "GET", "/v1/snapshot", Value::Null)
        .await;
    let card = &snapshot["sessions"][0]["experience_drafts"][draft.to_string()];
    assert_eq!(card["status"], "ready");
    assert_eq!(card["note"], "记下签名");
    assert_eq!(card["title"], "HAP 安装签名错误");

    let (status, saved) = fixture
        .request(
            &token,
            "POST",
            &save,
            json!({"title":"HAP 安装签名错误","body":"## 适用场景\n用户编辑后"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let id = saved["id"].as_str().unwrap().to_string();
    let stored = library.get(&id).unwrap().unwrap();
    assert_eq!(stored.body, "## 适用场景\n用户编辑后");
    assert_eq!(stored.source_session, Some(session.to_string()));
    let (_, snapshot) = fixture
        .request(&token, "GET", "/v1/snapshot", Value::Null)
        .await;
    assert!(
        snapshot["sessions"][0]["experience_drafts"]
            .as_object()
            .unwrap()
            .is_empty()
    );

    // 经验库管理
    let (_, list) = fixture
        .request(&token, "GET", "/v1/experiences", Value::Null)
        .await;
    assert_eq!(list["experiences"][0]["id"], id);
    assert_eq!(list["experiences"][0]["scenario"], "用户编辑后");
    let item = format!("/v1/experiences/{id}");
    let (status, entry) = fixture.request(&token, "GET", &item, Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    let stale = "2000-01-01T00:00:00Z";
    let (status, _) = fixture
        .request(
            &token,
            "PUT",
            &item,
            json!({"title":"新标题","body":"正文","expected_updated_at":stale}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, updated) = fixture
        .request(
            &token,
            "PUT",
            &item,
            json!({"title":"新标题","body":"正文","expected_updated_at":entry["updated_at"]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(library.get(&id).unwrap().unwrap().title, "新标题");
    let (status, _) = fixture.request(&token, "DELETE", &item, Value::Null).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = fixture.request(&token, "GET", &item, Value::Null).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = fixture
        .request(&token, "GET", "/v1/experiences/..%2Fstate", Value::Null)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn experience_draft_merges_into_existing_entry_and_discard_removes_card() {
    let (fixture, library) = with_library(Fixture::new());
    let existing = crate::domain::ExperienceEntry::new(
        "旧标题",
        "## 适用场景\n旧内容",
        None,
        chrono::Utc::now(),
    )
    .unwrap();
    library.save(&existing).unwrap();
    let (_, token) = fixture.pair("u");
    let session = fixture.session(&token).await;
    seed_history(&fixture, "u", session);
    let drafts = format!("/v1/sessions/{session}/experience-drafts");

    let draft = Uuid::new_v4();
    fixture
        .request(&token, "POST", &drafts, json!({"draft_id":draft}))
        .await;
    draft_ready(&fixture, "u", session, draft, Some(existing.id.clone()));
    let (_, snapshot) = fixture
        .request(&token, "GET", "/v1/snapshot", Value::Null)
        .await;
    let card = &snapshot["sessions"][0]["experience_drafts"][draft.to_string()];
    assert_eq!(card["merge_target"]["title"], "旧标题");

    let (status, _) = fixture
        .request(
            &token,
            "POST",
            &format!("{drafts}/{draft}/save"),
            json!({"title":"合并后","body":"## 适用场景\n新旧合并","target_id":existing.id}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let entries = library.list().unwrap();
    assert_eq!(entries.len(), 1, "merge must not create a new entry");
    assert_eq!(entries[0].id, existing.id);
    assert_eq!(entries[0].created_at, existing.created_at);
    assert_eq!(entries[0].body, "## 适用场景\n新旧合并");

    let second = Uuid::new_v4();
    fixture
        .request(&token, "POST", &drafts, json!({"draft_id":second}))
        .await;
    let (status, _) = fixture
        .request(&token, "DELETE", &format!("{drafts}/{second}"), Value::Null)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, snapshot) = fixture
        .request(&token, "GET", "/v1/snapshot", Value::Null)
        .await;
    assert!(
        snapshot["sessions"][0]["experience_drafts"]
            .as_object()
            .unwrap()
            .is_empty()
    );

    // 其他用户无法对该会话发起总结。
    let (_, other) = fixture.pair("v");
    let (status, _) = fixture
        .request(&other, "POST", &drafts, json!({"draft_id":Uuid::new_v4()}))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn approval_mode_is_optional_and_switchable() {
    use crate::domain::{ApprovalMode, ApprovalSetting};
    let fixture = Fixture::new();
    let (_, token) = fixture.pair("u");
    let (status, _) = fixture
        .request(&token, "GET", "/v1/approval-mode", Value::Null)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let setting = ApprovalSetting::new(ApprovalMode::Supervisor);
    let Fixture {
        client,
        input,
        _dir,
    } = fixture;
    let fixture = Fixture {
        client: client.with_approval_setting(setting.clone()),
        input,
        _dir,
    };
    let (_, caps) = fixture
        .request(&token, "GET", "/v1/capabilities", Value::Null)
        .await;
    assert!(
        caps["operations"]
            .as_array()
            .unwrap()
            .contains(&json!("approval_mode.manage"))
    );
    let (_, body) = fixture
        .request(&token, "GET", "/v1/approval-mode", Value::Null)
        .await;
    assert_eq!(body["mode"], "supervisor");

    let (status, body) = fixture
        .request(
            &token,
            "PUT",
            "/v1/approval-mode",
            json!({"mode":"allow_all"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["mode"], "allow_all");
    assert_eq!(setting.get(), ApprovalMode::AllowAll);

    let (status, _) = fixture
        .request(&token, "PUT", "/v1/approval-mode", json!({"mode":"yolo"}))
        .await;
    assert!(status.is_client_error());
    assert_eq!(setting.get(), ApprovalMode::AllowAll);
}
