//! Webhook server 集成测试

use crossbeam_channel::unbounded;
use harness::domain::{ExternalInput, SignalSource};
use harness::triggers::{WebhookConfig, WebhookRouteConfig, run_webhook_server};

async fn spawn_server(
    auth_token: &str,
) -> (
    String,
    crossbeam_channel::Receiver<ExternalInput>,
    tokio::task::JoinHandle<()>,
) {
    let (input_tx, input_rx) = unbounded::<ExternalInput>();
    let listen_addr = "127.0.0.1:0";
    // 用 TcpListener 占位获取随机端口
    let listener = tokio::net::TcpListener::bind(listen_addr).await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let config = WebhookConfig {
        enabled: true,
        listen_addr: addr.to_string(),
        auth_token: auth_token.to_string(),
        routes: vec![],
    };
    let _ = std::marker::PhantomData::<WebhookRouteConfig>;
    let handle = tokio::spawn(async move {
        let _ = run_webhook_server(input_tx, config).await;
    });
    // 给 server 一点启动时间
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    (format!("http://{addr}"), input_rx, handle)
}

#[tokio::test]
async fn health_endpoint_returns_ok_without_auth() {
    let (base, _rx, _handle) = spawn_server("secret").await;
    let resp = reqwest::get(format!("{base}/health"))
        .await
        .expect("request");
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(body["status"], "ok");
}

#[tokio::test]
async fn webhook_with_valid_token_returns_202() {
    let (base, rx, _handle) = spawn_server("tok123").await;
    let resp = reqwest::Client::new()
        .post(format!("{base}/webhook/test.kind"))
        .header("X-Webhook-Token", "tok123")
        .json(&serde_json::json!({"title": "bug"}))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 202);
    let input = rx.recv().expect("should receive");
    match input {
        ExternalInput::Webhook { source, kind, body } => {
            assert_eq!(source, SignalSource("webhook".to_string()));
            assert_eq!(kind, "test.kind");
            assert_eq!(body["title"], "bug");
        }
        _ => panic!("expected Webhook variant"),
    }
}

#[tokio::test]
async fn webhook_with_wrong_token_returns_401() {
    let (base, _rx, _handle) = spawn_server("right").await;
    let resp = reqwest::Client::new()
        .post(format!("{base}/webhook/x"))
        .header("X-Webhook-Token", "wrong")
        .json(&serde_json::json!({}))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn webhook_with_invalid_json_returns_400() {
    let (base, _rx, _handle) = spawn_server("t").await;
    let resp = reqwest::Client::new()
        .post(format!("{base}/webhook/x"))
        .header("X-Webhook-Token", "t")
        .header("Content-Type", "application/json")
        .body("not json")
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 400);
}
