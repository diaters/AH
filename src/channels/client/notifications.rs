//! MeoW 发件箱与任务事件共用事务；HTTP 成功不等同于设备收到。
use super::super::{Client, ClientError};
use super::{State, Store, append_event, hash};
use chrono::{Datelike, TimeZone};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, time::Duration};
use uuid::Uuid;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeowConfig {
    pub recipients: BTreeMap<String, String>,
    #[serde(default = "daily_limit")]
    pub daily_limit: u32,
    #[serde(default = "utc_offset")]
    pub utc_offset_minutes: i32,
}
fn daily_limit() -> u32 {
    3000
}
fn utc_offset() -> i32 {
    480
}
impl std::fmt::Debug for MeowConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MeowConfig")
            .field("configured_users", &self.recipients.len())
            .field("daily_limit", &self.daily_limit)
            .field("utc_offset_minutes", &self.utc_offset_minutes)
            .finish()
    }
}
impl MeowConfig {
    pub fn validate(&self) -> Result<(), ClientError> {
        if self.daily_limit == 0
            || self.daily_limit > 3000
            || !(-720..=840).contains(&self.utc_offset_minutes)
            || self.recipients.len() > 256
            || self.recipients.values().any(|s| {
                s.is_empty()
                    || s.len() > 128
                    || !s
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            })
        {
            return Err(ClientError::Invalid("invalid MeoW configuration"));
        }
        Ok(())
    }
    fn day(&self, now: i64) -> (String, i64) {
        let offset =
            chrono::FixedOffset::east_opt(self.utc_offset_minutes * 60).expect("validated offset");
        let local = offset
            .timestamp_opt(now, 0)
            .single()
            .expect("valid timestamp");
        let tomorrow = local
            .date_naive()
            .succ_opt()
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap();
        (
            format!(
                "{:04}-{:02}-{:02}",
                local.year(),
                local.month(),
                local.day()
            ),
            offset
                .from_local_datetime(&tomorrow)
                .single()
                .unwrap()
                .timestamp(),
        )
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Attempt {
    at: i64,
    finished_at: Option<i64>,
    http_status: Option<u16>,
    business_status: Option<i64>,
    result: String,
}
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Notification {
    pub(super) id: Uuid,
    pub(super) user_id: String,
    pub(super) session_id: Uuid,
    recipient_hash: String,
    title: String,
    message: String,
    pub(super) state: String,
    pub(super) result_code: Option<String>,
    created_at: i64,
    next_attempt_at: i64,
    attempts: Vec<Attempt>,
    received_at: Option<i64>,
    read_at: Option<i64>,
}
impl Notification {
    pub(super) fn public(&self) -> Value {
        json!({"id":self.id,"session_id":self.session_id,"title":self.title,"message":self.message,"state":self.state,
            "result_code":self.result_code,"created_at":self.created_at,"next_attempt_at":self.next_attempt_at,"attempts":self.attempts,
            "received_at":self.received_at,"read_at":self.read_at})
    }
}

const PREVIEW_CHARS: usize = 200;

/// 取首个非空字符串字段并按字符截断，折叠空白。
fn preview(data: &Value, key: &str) -> Option<String> {
    let text = data[key]
        .as_str()?
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if text.is_empty() {
        return None;
    }
    let mut out: String = text.chars().take(PREVIEW_CHARS).collect();
    if text.chars().count() > PREVIEW_CHARS {
        out.push('…');
    }
    Some(out)
}

fn compose(kind: &str, data: &Value) -> Option<(&'static str, String)> {
    let named = |label: &str, key: &str| {
        let name = data[key].as_str().filter(|s| !s.is_empty());
        name.map_or_else(|| label.to_string(), |n| format!("{label}（{n}）"))
    };
    let root = data["parent_id"].is_null();
    Some(match kind {
        "approval_required" => {
            let who = data["agent_name"].as_str().unwrap_or("Agent");
            let tool = data["tool_name"].as_str().unwrap_or("工具");
            (
                "Harness 待审批",
                format!("{who} 请求使用工具 {tool}，请打开 Harness 审批。"),
            )
        }
        "question" => ("Harness 待回答", preview(data, "question")?),
        "task_status" if root && data["status"] == "done" => (
            "Harness 任务完成",
            match preview(data, "result") {
                Some(r) => format!("{}：{r}", named("任务", "name")),
                None => format!("{}已完成。", named("任务", "name")),
            },
        ),
        "task_status" if root && data["status"] == "failed" => (
            "Harness 任务失败",
            match preview(data, "result") {
                Some(r) => format!("{}：{r}", named("任务", "name")),
                None => format!("{}执行失败。", named("任务", "name")),
            },
        ),
        "text" if data["background_notification"] == true => {
            ("Harness 后台结果", preview(data, "content")?)
        }
        _ => return None,
    })
}

pub(super) fn enqueue(
    state: &mut State,
    user: &str,
    session: Uuid,
    kind: &str,
    data: &Value,
    config: &MeowConfig,
) {
    let Some(recipient) = config.recipients.get(user) else {
        return;
    };
    let Some((title, message)) = compose(kind, data) else {
        return;
    };
    if state.notifications.len() >= 4000 {
        if let Some(index) = state
            .notifications
            .iter()
            .position(|n| n.read_at.is_some() && matches!(n.state.as_str(), "accepted" | "failed"))
        {
            state.notifications.remove(index);
        } else {
            *state.notification_overflow.entry(user.into()).or_default() += 1;
            return;
        }
    }
    let now = chrono::Utc::now().timestamp();
    // 只发送截断后的可读摘要；工具参数、凭证不进入外部通知，完整内容由鉴权接口读取。
    state.notifications.push(Notification {
        id: Uuid::new_v4(),
        user_id: user.into(),
        session_id: session,
        recipient_hash: hash(recipient),
        title: title.into(),
        message,
        state: "queued".into(),
        result_code: None,
        created_at: now,
        next_attempt_at: now,
        attempts: vec![],
        received_at: None,
        read_at: None,
    });
}

fn emit(state: &mut State, id: Uuid) {
    if let Some(n) = state.notifications.iter().find(|n| n.id == id) {
        append_event(state, n.session_id, "notification_status", n.public());
    }
}

impl Store {
    fn claim_notification(
        &mut self,
        config: &MeowConfig,
        now: i64,
    ) -> Result<Option<Notification>, ClientError> {
        let Some(index) = self.state.notifications.iter().position(|n| {
            matches!(n.state.as_str(), "queued" | "quota_wait") && n.next_attempt_at <= now
        }) else {
            return Ok(None);
        };
        self.change(|state| {
            let n = &mut state.notifications[index];
            let id = n.id;
            let recipient = config
                .recipients
                .get(&n.user_id)
                .filter(|s| hash(s) == n.recipient_hash);
            if recipient.is_none() || n.attempts.len() >= 20 {
                n.state = "failed".into();
                n.result_code = Some("recipient_changed_or_attempt_limit".into());
                emit(state, id);
                return Ok(None);
            }
            let (day, next_day) = config.day(now);
            let key = format!("{}:{day}", n.recipient_hash);
            let count = state.notification_quota.entry(key).or_default();
            if *count >= config.daily_limit {
                n.state = "quota_wait".into();
                n.next_attempt_at = next_day;
                n.result_code = Some("daily_quota".into());
                emit(state, id);
                return Ok(None);
            }
            // 按尝试保守预留额度，未知结果也计数；单实例不宣称协调多个发送实例。
            *count += 1;
            n.state = "sending".into();
            n.result_code = None;
            n.attempts.push(Attempt {
                at: now,
                finished_at: None,
                http_status: None,
                business_status: None,
                result: "sending".into(),
            });
            let claimed = n.clone();
            emit(state, id);
            Ok(Some(claimed))
        })
    }
    fn finish_notification(
        &mut self,
        id: Uuid,
        result: Delivery,
        now: i64,
    ) -> Result<(), ClientError> {
        self.change(|state| {
            let n = state
                .notifications
                .iter_mut()
                .find(|n| n.id == id)
                .ok_or(ClientError::NotFound)?;
            let retry = matches!(result.state, "queued") && n.attempts.len() < 3;
            n.state = if result.state == "queued" && !retry {
                "failed"
            } else {
                result.state
            }
            .into();
            n.result_code = Some(result.code.into());
            n.next_attempt_at = now + result.delay.max(1);
            if let Some(attempt) = n.attempts.last_mut() {
                attempt.finished_at = Some(now);
                attempt.http_status = result.http;
                attempt.business_status = result.business;
                attempt.result = n.state.clone();
            }
            emit(state, id);
            Ok(())
        })
    }
    pub(crate) fn acknowledge_notification(
        &mut self,
        user: &str,
        id: Uuid,
        read: bool,
        now: i64,
    ) -> Result<(), ClientError> {
        self.change(|state| {
            let n = state
                .notifications
                .iter_mut()
                .find(|n| n.id == id && n.user_id == user)
                .ok_or(ClientError::NotFound)?;
            n.received_at.get_or_insert(now);
            if read {
                n.read_at.get_or_insert(now);
            }
            emit(state, id);
            Ok(())
        })
    }
    pub(crate) fn retry_notification(
        &mut self,
        user: &str,
        id: Uuid,
        allow_unknown: bool,
        now: i64,
    ) -> Result<(), ClientError> {
        self.change(|state| {
            let n = state
                .notifications
                .iter_mut()
                .find(|n| n.id == id && n.user_id == user)
                .ok_or(ClientError::NotFound)?;
            if n.state == "unknown" && !allow_unknown {
                return Err(ClientError::Conflict(
                    "retry may duplicate an external notification",
                ));
            }
            if !matches!(n.state.as_str(), "failed" | "unknown") || n.attempts.len() >= 20 {
                return Err(ClientError::Conflict("notification cannot be retried"));
            }
            n.state = "queued".into();
            n.next_attempt_at = now;
            n.result_code = None;
            emit(state, id);
            Ok(())
        })
    }
}

struct Delivery {
    state: &'static str,
    code: &'static str,
    http: Option<u16>,
    business: Option<i64>,
    delay: i64,
}
async fn deliver(
    http: &reqwest::Client,
    base: &str,
    recipient: &str,
    n: &Notification,
) -> Delivery {
    let mut result = Delivery {
        state: "unknown",
        code: "network_result_unknown",
        http: None,
        business: None,
        delay: 60,
    };
    let response = match http
        .post(format!("{base}/{recipient}"))
        .query(&[("msgType", "text")])
        .json(&json!({"title":n.title,"msg":n.message}))
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            if error.is_connect() && !error.is_timeout() {
                result.state = "queued";
                result.code = "connection_failed";
            }
            return result;
        }
    };
    let status = response.status();
    result.http = Some(status.as_u16());
    let retry_after = response
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(60)
        .clamp(1, 3600);
    let body = response.json::<Value>().await.ok();
    result.business = body.as_ref().and_then(|v| v["status"].as_i64());
    if status.as_u16() == 429 || result.business == Some(429) {
        result.state = "queued";
        result.code = "rate_limited";
        result.delay = retry_after;
    } else if status.is_success()
        && result.business == Some(200)
        && body
            .as_ref()
            .is_some_and(|v| v["data"].is_null() || v["data"] == true)
    {
        result.state = "accepted";
        result.code = "meow_accepted";
    } else if status.is_client_error()
        || matches!(result.business, Some(400 | 403 | 404))
        || body.as_ref().is_some_and(|v| v["data"] == false)
    {
        result.state = "failed";
        result.code = "meow_rejected";
    } else {
        result.code = "response_result_unknown";
    }
    result
}

pub(crate) async fn notification_worker(
    client: Client,
    cancel: tokio_util::sync::CancellationToken,
    base: String,
) {
    let Some(config) = client.config.meow.clone() else {
        return;
    };
    let Ok(http) = reqwest::Client::builder()
        .timeout(Duration::from_secs(12))
        .redirect(reqwest::redirect::Policy::none())
        .build()
    else {
        return;
    };
    let mut changed = client.changed.subscribe();
    loop {
        let (next, mutated) = {
            let mut store = client.store.lock().expect("client state mutex poisoned");
            let sequence = store.state.sequence;
            let next = store.claim_notification(&config, chrono::Utc::now().timestamp());
            (next, sequence != store.state.sequence)
        };
        if mutated {
            client.wake();
        }
        match next {
            Ok(Some(n)) => {
                client.wake();
                let Some(recipient) = config.recipients.get(&n.user_id) else {
                    continue;
                };
                let result = tokio::select! { _=cancel.cancelled()=>break,result=deliver(&http,&base,recipient,&n)=>result };
                if let Err(error) = client
                    .store
                    .lock()
                    .expect("client state mutex poisoned")
                    .finish_notification(n.id, result, chrono::Utc::now().timestamp())
                {
                    tracing::error!(event="MeowReceiptPersistenceFailed",%error);
                    break;
                }
                client.wake();
                continue;
            }
            Err(error) => {
                tracing::error!(event="MeowOutboxUnavailable",%error);
                break;
            }
            Ok(None) => {}
        }
        let wait = client
            .store
            .lock()
            .expect("client state mutex poisoned")
            .state
            .notifications
            .iter()
            .filter(|n| matches!(n.state.as_str(), "queued" | "quota_wait"))
            .map(|n| (n.next_attempt_at - chrono::Utc::now().timestamp()).clamp(1, 60) as u64)
            .min()
            .unwrap_or(60);
        tokio::select! {_=cancel.cancelled()=>break,_=changed.changed()=>{},_=tokio::time::sleep(Duration::from_secs(wait))=>{}}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path, query_param},
    };
    fn config() -> MeowConfig {
        MeowConfig {
            recipients: BTreeMap::from([("alice".into(), "test-recipient".into())]),
            daily_limit: 3000,
            utc_offset_minutes: 480,
        }
    }
    fn setup() -> (tempfile::TempDir, Store, Uuid) {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path().join("state.json")).unwrap();
        let session = store
            .create_session("alice", Uuid::new_v4(), "test", "test-session")
            .unwrap()
            .session_id;
        (dir, store, session)
    }
    fn queue(store: &mut Store, session: Uuid, config: &MeowConfig) {
        store.append_with_notification("alice",session,"question",json!({"question":"private text must not be sent","question_id":Uuid::new_v4().to_string(),"task_id":Uuid::new_v4()}),Some(config)).unwrap();
    }
    #[test]
    fn notifications_carry_truncated_readable_preview_without_tool_input() {
        let (_dir, mut store, session) = setup();
        let config = config();
        let long = "结果".repeat(300);
        for (kind, data) in [
            (
                "text",
                json!({"content":long,"background_notification":true}),
            ),
            (
                "text",
                json!({"content":"   ","background_notification":true}),
            ),
            (
                "task_status",
                json!({"name":"日报","status":"done","result":"完成 3 项","parent_id":null}),
            ),
            (
                "task_status",
                json!({"name":"子任务","status":"done","result":"x","parent_id":"p"}),
            ),
            (
                "approval_required",
                json!({"agent_name":"coder","tool_name":"shell","input_json":"{\"token\":\"SECRET\"}"}),
            ),
        ] {
            store
                .append_with_notification("alice", session, kind, data, Some(&config))
                .unwrap();
        }
        let msgs: Vec<_> = store
            .state
            .notifications
            .iter()
            .map(|n| n.message.clone())
            .collect();
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0].chars().count(), PREVIEW_CHARS + 1);
        assert!(msgs[0].ends_with('…'));
        assert_eq!(msgs[1], "任务（日报）：完成 3 项");
        assert!(
            msgs[2].contains("coder") && msgs[2].contains("shell") && !msgs[2].contains("SECRET")
        );
    }
    #[test]
    fn quota_is_per_recipient_and_local_day_and_receipts_are_independent() {
        let (_dir, mut store, session) = setup();
        let mut config = config();
        config.daily_limit = 1;
        queue(&mut store, session, &config);
        let now = chrono::Utc::now().timestamp();
        let n = store.claim_notification(&config, now).unwrap().unwrap();
        assert!(n.message.contains("private text"));
        store
            .finish_notification(
                n.id,
                Delivery {
                    state: "accepted",
                    code: "meow_accepted",
                    http: Some(200),
                    business: Some(200),
                    delay: 1,
                },
                now,
            )
            .unwrap();
        assert!(store.state.notifications[0].received_at.is_none());
        queue(&mut store, session, &config);
        // 入队时间取自实时时钟，须不早于它，否则跨秒时尚未到期而不会进入 quota_wait。
        let now = chrono::Utc::now().timestamp();
        assert!(store.claim_notification(&config, now).unwrap().is_none());
        assert_eq!(store.state.notifications[1].state, "quota_wait");
        let next_day = config.day(now).1;
        assert!(
            store
                .claim_notification(&config, next_day + 1)
                .unwrap()
                .is_some()
        );
        assert!(
            store
                .acknowledge_notification("bob", n.id, true, now)
                .is_err()
        );
        store
            .acknowledge_notification("alice", n.id, true, now)
            .unwrap();
        assert!(store.state.notifications[0].read_at.is_some());
    }
    #[test]
    fn interrupted_attempt_is_unknown_and_not_replayed_on_restart() {
        let (dir, mut store, session) = setup();
        let config = config();
        queue(&mut store, session, &config);
        let now = chrono::Utc::now().timestamp();
        let n = store.claim_notification(&config, now).unwrap().unwrap();
        drop(store);
        let mut store = Store::open(dir.path().join("state.json")).unwrap();
        assert_eq!(store.state.notifications[0].state, "unknown");
        assert!(
            store
                .claim_notification(&config, now + 100)
                .unwrap()
                .is_none()
        );
        assert!(store.retry_notification("alice", n.id, false, now).is_err());
        store.retry_notification("alice", n.id, true, now).unwrap();
        assert!(store.claim_notification(&config, now).unwrap().is_some());
    }
    #[tokio::test]
    async fn http_and_business_status_are_both_checked_without_real_delivery() {
        let server = MockServer::start().await;
        let (_dir, mut store, session) = setup();
        let config = config();
        queue(&mut store, session, &config);
        let n = store
            .claim_notification(&config, chrono::Utc::now().timestamp())
            .unwrap()
            .unwrap();
        let http = reqwest::Client::new();
        for (body, expected) in [
            (json!({"status":200,"data":true}), "accepted"),
            (json!({"status":404}), "failed"),
            (json!({"status":429}), "queued"),
            (json!({"status":500}), "unknown"),
            (json!({"status":200,"data":false}), "failed"),
        ] {
            server.reset().await;
            Mock::given(method("POST"))
                .and(path("/test-recipient"))
                .and(query_param("msgType", "text"))
                .respond_with(ResponseTemplate::new(200).set_body_json(body))
                .expect(1)
                .mount(&server)
                .await;
            assert_eq!(
                deliver(&http, &server.uri(), "test-recipient", &n)
                    .await
                    .state,
                expected
            );
        }
        server.reset().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_secs(1))
                    .set_body_json(json!({"status":200})),
            )
            .mount(&server)
            .await;
        let http = reqwest::Client::builder()
            .timeout(Duration::from_millis(100))
            .build()
            .unwrap();
        assert_eq!(
            deliver(&http, &server.uri(), "test-recipient", &n)
                .await
                .state,
            "unknown"
        );
    }
    #[tokio::test]
    async fn runtime_event_automatically_persists_and_delivers_through_outbox() {
        use crate::domain::{ChannelId, EngineEvent, EventTarget, Frontend, FrontendKind};
        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/test-recipient"))
            .and(query_param("msgType", "text"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"status":200,"data":true})),
            )
            .expect(1)
            .mount(&mock)
            .await;
        let (dir, store, session) = setup();
        let (tx, _rx) = crossbeam_channel::unbounded();
        let client = Client::new(
            super::super::super::ClientConfig {
                listen: "127.0.0.1:0".parse().unwrap(),
                instance_name: "test".into(),
                tls_cert: Some(dir.path().join("unused-cert")),
                tls_key: Some(dir.path().join("unused-key")),
                advertise: vec![],
                state_path: dir.path().join("state.json"),
                meow: Some(config()),
            },
            store,
            tx,
        );
        let cancel = tokio_util::sync::CancellationToken::new();
        let worker = tokio::spawn(notification_worker(
            client.clone(),
            cancel.clone(),
            mock.uri(),
        ));
        client.push_event(EngineEvent::QuestionPresented {
            target: EventTarget::Directed(vec![ChannelId {
                frontend: FrontendKind::Client,
                user_id: "alice".into(),
                thread_id: Some(session.to_string()),
            }]),
            task_id: crate::domain::TaskId::new(),
            question_id: "original-call".into(),
            question: "private question".into(),
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let state = client.store.lock().unwrap().snapshot("alice");
                if state["notifications"][0]["state"] == "accepted" {
                    assert!(state["notifications"][0]["received_at"].is_null());
                    assert_eq!(
                        state["notifications"][0]["attempts"][0]["business_status"],
                        200
                    );
                    assert!(
                        client.store.lock().unwrap().snapshot("bob")["notifications"]
                            .as_array()
                            .unwrap()
                            .is_empty()
                    );
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("event did not reach mock MeoW through worker");
        cancel.cancel();
        worker.await.unwrap();
        let requests = mock.received_requests().await.unwrap();
        assert!(String::from_utf8_lossy(&requests[0].body).contains("private question"));
        drop(client);
        let recovered = Store::open(dir.path().join("state.json")).unwrap();
        assert_eq!(
            recovered.snapshot("alice")["notifications"][0]["state"],
            "accepted"
        );
    }
}
