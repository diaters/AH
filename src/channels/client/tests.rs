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
