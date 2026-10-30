use std::{net::TcpListener, time::Duration};

use axum::{
    Json, Router,
    extract::{
        DefaultBodyLimit, Path, Query, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use super::{
    Client, ClientError,
    store::{ApprovalSelection, Device},
};
use crate::domain::{ChannelId, ExternalInput, FrontendKind};

pub struct ClientServer {
    pub address: std::net::SocketAddr,
    handle: axum_server::Handle,
    task: Option<tokio::task::JoinHandle<std::io::Result<()>>>,
}

impl ClientServer {
    pub async fn start(client: Client) -> anyhow::Result<Self> {
        let tls = axum_server::tls_rustls::RustlsConfig::from_pem_file(
            &client.config.tls_cert,
            &client.config.tls_key,
        )
        .await?;
        let listener = TcpListener::bind(client.config.listen)?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let handle = axum_server::Handle::new();
        let server = axum_server::from_tcp_rustls(listener, tls).handle(handle.clone());
        let task = tokio::spawn(server.serve(router(client).into_make_service()));
        Ok(Self {
            address,
            handle,
            task: Some(task),
        })
    }

    pub async fn shutdown(mut self) -> anyhow::Result<()> {
        self.handle.graceful_shutdown(Some(Duration::from_secs(2)));
        if let Some(task) = self.task.take() {
            task.await??;
        }
        Ok(())
    }
}

impl Drop for ClientServer {
    fn drop(&mut self) {
        self.handle.shutdown();
    }
}

pub fn router(client: Client) -> Router {
    Router::new()
        .route("/v1/identity", get(identity))
        .route("/v1/pair", post(pair))
        .route("/v1/capabilities", get(capabilities))
        .route("/v1/devices", get(devices))
        .route("/v1/devices/:id", delete(revoke))
        .route("/v1/snapshot", get(snapshot))
        .route("/v1/events", get(events))
        .route("/v1/stream", get(stream))
        .route("/v1/commands", post(command))
        .route("/v1/commands/:id", get(command_status))
        .layer(DefaultBodyLimit::max(64 * 1024))
        .with_state(client)
}

async fn identity(State(client): State<Client>) -> Json<Value> {
    Json(
        json!({"instance_id":client.store.lock().expect("client state mutex poisoned").instance_id(),
        "instance_name":client.config.instance_name,"protocol_version":1}),
    )
}

impl IntoResponse for ClientError {
    fn into_response(self) -> Response {
        let (status, code) = match &self {
            Self::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized"),
            Self::NotFound => (StatusCode::NOT_FOUND, "not_found"),
            Self::Invalid(_) => (StatusCode::BAD_REQUEST, "invalid_request"),
            Self::Conflict(_) => (StatusCode::CONFLICT, "conflict"),
            Self::Capacity => (StatusCode::TOO_MANY_REQUESTS, "capacity_reached"),
            Self::Resync => (StatusCode::CONFLICT, "snapshot_required"),
            Self::Io(_) | Self::Json(_) | Self::StorageUnavailable => {
                (StatusCode::SERVICE_UNAVAILABLE, "storage_unavailable")
            }
        };
        // 不将内部文件路径、正文和凭证写入响应或请求日志。
        if status == StatusCode::SERVICE_UNAVAILABLE {
            tracing::error!(event="ClientStorageFailed", error=%self);
        }
        (status, Json(json!({"error":code}))).into_response()
    }
}

fn token(headers: &HeaderMap) -> Result<&str, ClientError> {
    headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .filter(|t| !t.is_empty() && t.len() <= 256)
        .ok_or(ClientError::Unauthorized)
}

fn authenticate(client: &Client, headers: &HeaderMap) -> Result<Device, ClientError> {
    client
        .store
        .lock()
        .expect("client state mutex poisoned")
        .authenticate(token(headers)?, chrono::Utc::now().timestamp())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PairRequest {
    instance_id: Uuid,
    code: String,
    device_name: String,
}

async fn pair(
    State(client): State<Client>,
    Json(request): Json<PairRequest>,
) -> Result<Json<Value>, ClientError> {
    let mut store = client.store.lock().expect("client state mutex poisoned");
    if request.instance_id != store.instance_id() {
        return Err(ClientError::Unauthorized);
    }
    let (device, credential) = store.pair(
        &request.code,
        &request.device_name,
        chrono::Utc::now().timestamp(),
    )?;
    Ok(Json(
        json!({"instance_id":store.instance_id(),"device":device,"token":credential}),
    ))
}

async fn capabilities(
    State(client): State<Client>,
    headers: HeaderMap,
) -> Result<Json<Value>, ClientError> {
    let device = authenticate(&client, &headers)?;
    let store = client.store.lock().expect("client state mutex poisoned");
    Ok(Json(
        json!({"protocol_version":1,"instance_id":store.instance_id(),"instance_name":client.config.instance_name,
        "device_id":device.id,"user_id":device.user_id,
        "operations":["sessions.create","sessions.text","approvals.respond","snapshot","events","devices.list","devices.revoke"],
        "event_retention":super::store::EVENT_RETENTION,"task_restart_recovery":false,
        "delivery_receipt":"server_persisted","command_receipt":"queued_is_not_consumed"}),
    ))
}

async fn devices(
    State(client): State<Client>,
    headers: HeaderMap,
) -> Result<Json<Value>, ClientError> {
    let device = authenticate(&client, &headers)?;
    Ok(Json(
        json!({"devices":client.store.lock().expect("client state mutex poisoned").devices(&device.user_id)}),
    ))
}

async fn revoke(
    State(client): State<Client>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ClientError> {
    let device = authenticate(&client, &headers)?;
    client
        .store
        .lock()
        .expect("client state mutex poisoned")
        .revoke(&device.user_id, id)?;
    client.wake();
    Ok(StatusCode::NO_CONTENT)
}

async fn snapshot(
    State(client): State<Client>,
    headers: HeaderMap,
) -> Result<Json<Value>, ClientError> {
    let device = authenticate(&client, &headers)?;
    Ok(Json(
        client
            .store
            .lock()
            .expect("client state mutex poisoned")
            .snapshot(&device.user_id),
    ))
}

#[derive(Deserialize)]
struct Cursor {
    after: u64,
}

async fn events(
    State(client): State<Client>,
    headers: HeaderMap,
    Query(cursor): Query<Cursor>,
) -> Result<Json<Value>, ClientError> {
    let device = authenticate(&client, &headers)?;
    Ok(Json(
        client
            .store
            .lock()
            .expect("client state mutex poisoned")
            .events(&device.user_id, cursor.after)?,
    ))
}

async fn stream(
    State(client): State<Client>,
    headers: HeaderMap,
    Query(cursor): Query<Cursor>,
    ws: WebSocketUpgrade,
) -> Result<Response, ClientError> {
    authenticate(&client, &headers)?;
    let credential = token(&headers)?.to_owned();
    Ok(ws
        .max_message_size(4096)
        .max_frame_size(4096)
        .on_upgrade(move |socket| stream_events(client, credential, cursor.after, socket)))
}

async fn stream_events(client: Client, credential: String, mut cursor: u64, mut socket: WebSocket) {
    // 先订阅再读快照/补发，避免读写之间丢失唤醒；队列满不丢关键事件，由游标补读。
    let mut changed = client.changed.subscribe();
    loop {
        let result = {
            let store = client.store.lock().expect("client state mutex poisoned");
            store
                .authenticate(&credential, chrono::Utc::now().timestamp())
                .and_then(|d| store.events(&d.user_id, cursor))
        };
        match result {
            Ok(batch) => {
                cursor = batch["cursor"].as_u64().expect("event cursor");
                if !matches!(
                    tokio::time::timeout(
                        Duration::from_secs(10),
                        socket.send(Message::Text(batch.to_string()))
                    )
                    .await,
                    Ok(Ok(()))
                ) {
                    break;
                }
            }
            Err(error) => {
                let code = if matches!(error, ClientError::Resync) {
                    "snapshot_required"
                } else {
                    "unauthorized"
                };
                let _ = tokio::time::timeout(
                    Duration::from_secs(2),
                    socket.send(Message::Text(json!({"error":code}).to_string())),
                )
                .await;
                break;
            }
        }
        tokio::select! {
            _ = changed.changed() => {},
            // 及时检查凭证到期，并维持空闲连接；不轮询 ECS。
            _ = tokio::time::sleep(Duration::from_secs(30)) => {},
            message = socket.recv() => {
                match message {
                    Some(Ok(Message::Ping(bytes))) => { let _ = socket.send(Message::Pong(bytes)).await; },
                    Some(Ok(Message::Pong(_))) => {},
                    _ => break,
                }
            }
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum Command {
    CreateSession {
        command_id: Uuid,
        title: String,
    },
    SendText {
        command_id: Uuid,
        session_id: Uuid,
        content: String,
    },
    RespondApproval {
        command_id: Uuid,
        session_id: Uuid,
        request_id: Uuid,
        expected_version: u64,
        option_id: String,
        feedback: Option<String>,
    },
}

async fn command(
    State(client): State<Client>,
    headers: HeaderMap,
    Json(request): Json<Command>,
) -> Result<Json<Value>, ClientError> {
    let device = authenticate(&client, &headers)?;
    let payload = serde_json::to_string(&request)?;
    let mut store = client.store.lock().expect("client state mutex poisoned");
    store.authenticate(token(&headers)?, chrono::Utc::now().timestamp())?;
    let id = match &request {
        Command::CreateSession { command_id, .. }
        | Command::SendText { command_id, .. }
        | Command::RespondApproval { command_id, .. } => *command_id,
    };
    if store.command(&device.user_id, id, &payload)?.is_some() {
        return Ok(Json(store.command_status(&device.user_id, id)?));
    }
    match request {
        Command::CreateSession { title, .. } => {
            if title.trim().is_empty() || title.len() > 256 {
                return Err(ClientError::Invalid("invalid title"));
            }
            store.create_session(&device.user_id, id, &title, &payload)?;
        }
        Command::SendText {
            session_id,
            content,
            ..
        } => {
            if content.trim().is_empty()
                || content.len() > 32 * 1024
                || content.trim_start().starts_with('/')
            {
                return Err(ClientError::Invalid(
                    "empty, oversized or unsupported command text",
                ));
            }
            store.accept_text(&device.user_id, id, session_id, &content, &payload)?;
            // 发送者来自凭证；不会经过本地 UserAction 的 sender=None 可信旁路。
            if client
                .input
                .try_send(ExternalInput::TextWithChannel {
                    channel: ChannelId {
                        frontend: FrontendKind::Client,
                        user_id: device.user_id.clone(),
                        thread_id: Some(session_id.to_string()),
                    },
                    content,
                    sender: Some(device.user_id.clone()),
                })
                .is_err()
            {
                store.reject_command(id)?;
                return Err(ClientError::Conflict(
                    "runtime input unavailable; query command status",
                ));
            }
        }
        Command::RespondApproval {
            session_id,
            request_id,
            expected_version,
            option_id,
            feedback,
            ..
        } => {
            if feedback.as_ref().is_some_and(|f| f.len() > 8192) {
                return Err(ClientError::Invalid("feedback too long"));
            }
            store.accept_approval(
                &device.user_id,
                id,
                session_id,
                ApprovalSelection {
                    request_id,
                    expected_version,
                    option_id: &option_id,
                },
                &payload,
            )?;
            if client
                .input
                .try_send(ExternalInput::Confirmation {
                    request_id,
                    option: option_id,
                    feedback,
                    sender: Some(device.user_id.clone()),
                })
                .is_err()
            {
                store.reject_command(id)?;
                return Err(ClientError::Conflict(
                    "runtime input unavailable; query command status",
                ));
            }
        }
    };
    client.wake();
    Ok(Json(store.command_status(&device.user_id, id)?))
}

async fn command_status(
    State(client): State<Client>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ClientError> {
    let device = authenticate(&client, &headers)?;
    let store = client.store.lock().expect("client state mutex poisoned");
    Ok(Json(store.command_status(&device.user_id, id)?))
}
