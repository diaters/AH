//! 单实例客户端状态；先持久化再确认，损坏时拒绝启动，不回退为空身份库。
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::ClientError;
#[path = "files.rs"]
mod files;
#[path = "notifications.rs"]
mod notifications;
pub use files::ClientFile;
pub(super) use files::MAX_FILE_BYTES;
pub use notifications::MeowConfig;
pub(super) use notifications::notification_worker;

pub const EVENT_RETENTION: usize = 2048;
const COMMAND_LIMIT: usize = 10_000;

#[derive(Clone, Serialize, Deserialize)]
pub struct Device {
    pub id: Uuid,
    pub user_id: String,
    pub name: String,
    pub expires_at: i64,
    pub revoked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) token_hash: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Pairing {
    user_id: String,
    expires_at: i64,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ClientEvent {
    pub sequence: u64,
    pub event_id: Uuid,
    pub session_id: Uuid,
    pub object_version: u64,
    pub kind: String,
    pub data: Value,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: Uuid,
    pub user_id: String,
    pub title: String,
    pub version: u64,
    pub tasks: BTreeMap<String, Value>,
    pub approvals: BTreeMap<Uuid, Value>,
    #[serde(default)]
    pub questions: BTreeMap<String, Value>,
    #[serde(default)]
    pub observations: Vec<Value>,
    #[serde(default)]
    pub observations_truncated: bool,
    #[serde(default)]
    pub progress: BTreeMap<String, Value>,
    /// 进行中或待确认的经验草稿；保存或丢弃后移除。
    #[serde(default)]
    pub experience_drafts: BTreeMap<String, Value>,
    pub messages: Vec<Value>,
    pub history_truncated: bool,
    /// 最近一次变更时的实例事件序号，供客户端按最近活动排序。
    #[serde(default)]
    pub updated_at: u64,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct CommandReceipt {
    pub command_id: Uuid,
    /// 只表示进入统一输入队列，不表示 Agent 已消费或任务执行成功。
    pub status: String,
    #[serde(default)]
    pub result_code: Option<String>,
    pub session_id: Uuid,
    pub(super) payload_hash: String,
    pub(super) user_id: String,
}

#[derive(Clone, Serialize, Deserialize)]
struct State {
    format: u32,
    instance_id: Uuid,
    sequence: u64,
    devices: Vec<Device>,
    pairings: BTreeMap<String, Pairing>,
    sessions: BTreeMap<Uuid, Session>,
    events: Vec<ClientEvent>,
    commands: BTreeMap<Uuid, CommandReceipt>,
    #[serde(default)]
    automation: BTreeMap<String, Value>,
    #[serde(default)]
    notifications: Vec<notifications::Notification>,
    #[serde(default)]
    notification_quota: BTreeMap<String, u32>,
    #[serde(default)]
    notification_overflow: BTreeMap<String, u64>,
    #[serde(default)]
    files: BTreeMap<Uuid, ClientFile>,
}

pub struct Store {
    path: PathBuf,
    state: State,
    // 跨进程独占，避免管理命令和运行实例同时覆盖状态。
    _lock: File,
    failed: bool,
}

pub(super) struct ApprovalSelection<'a> {
    pub request_id: Uuid,
    pub expected_version: u64,
    pub option_id: &'a str,
}

pub fn secret() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}
pub fn hash(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

fn private_file(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

impl Store {
    #[cfg(test)]
    pub(super) fn test_expire_cursor(&mut self, session_id: Uuid) {
        self.change(|state| {
            for _ in 0..=EVENT_RETENTION {
                append_event(state, session_id, "inbox", serde_json::json!({"pending":1}));
            }
            Ok(())
        })
        .unwrap();
    }
    pub fn open(path: PathBuf) -> Result<Self, ClientError> {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        fs::create_dir_all(parent)?;
        let mut options = OpenOptions::new();
        options.create(true).truncate(false).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock = options.open(path.with_extension("lock"))?;
        lock.try_lock()
            .map_err(|_| ClientError::Conflict("client state is already in use"))?;
        let mut state: State = match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => State {
                format: 1,
                instance_id: Uuid::new_v4(),
                sequence: 0,
                devices: vec![],
                pairings: BTreeMap::new(),
                sessions: BTreeMap::new(),
                events: vec![],
                commands: BTreeMap::new(),
                automation: BTreeMap::new(),
                notifications: vec![],
                notification_quota: BTreeMap::new(),
                notification_overflow: BTreeMap::new(),
                files: BTreeMap::new(),
            },
            Err(e) => return Err(e.into()),
        };
        if state.format != 1 {
            return Err(ClientError::Invalid("unsupported client state format"));
        }
        // ECS 任务还不具备跨进程恢复能力。保留历史，显式失效运行态及未决命令。
        for receipt in state.commands.values_mut() {
            if receipt.status == "queued" {
                receipt.status = "unknown_after_restart".into();
            }
        }
        for session in state.sessions.values_mut() {
            session.approvals.clear();
            session.questions.clear();
            for task in session.tasks.values_mut() {
                if !matches!(task["status"].as_str(), Some("done" | "failed")) {
                    task["status"] = "recovery_required".into();
                    task["can_stop"] = false.into();
                    task["can_finish"] = false.into();
                }
            }
        }
        for view in state.automation.values_mut() {
            view["stale"] = true.into();
        }
        for notification in &mut state.notifications {
            if notification.state == "sending" {
                notification.state = "unknown".into();
                notification.result_code = Some("interrupted_while_sending".into());
            }
        }
        let store = Self {
            path,
            state,
            _lock: lock,
            failed: false,
        };
        store.persist(&store.state)?;
        Ok(store)
    }

    fn persist(&self, state: &State) -> Result<(), ClientError> {
        let tmp = self.path.with_extension(format!("{}.tmp", Uuid::new_v4()));
        let result = (|| {
            let mut file = private_file(&tmp)?;
            file.write_all(&serde_json::to_vec(state)?)?;
            file.sync_all()?;
            fs::rename(&tmp, &self.path)?;
            #[cfg(unix)]
            File::open(
                self.path
                    .parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .unwrap_or(Path::new(".")),
            )?
            .sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(tmp);
        }
        result
    }

    fn change<T>(
        &mut self,
        f: impl FnOnce(&mut State) -> Result<T, ClientError>,
    ) -> Result<T, ClientError> {
        if self.failed {
            return Err(ClientError::StorageUnavailable);
        }
        let mut next = self.state.clone();
        let result = f(&mut next)?;
        if let Err(error) = self.persist(&next) {
            // rename/fsync 失败可能发生在替换后；禁止继续用旧内存状态接受命令。
            self.failed = true;
            return Err(error);
        }
        self.state = next;
        Ok(result)
    }

    pub(super) fn diagnostics(&self, user: &str) -> Value {
        let sessions: Vec<_> = self
            .state
            .sessions
            .values()
            .filter(|s| s.user_id == user)
            .collect();
        serde_json::json!({"instance_id":self.instance_id(),"version":env!("CARGO_PKG_VERSION"),"protocol_version":1,
            "storage_available":!self.failed,"cursor":self.state.sequence,"event_retention":EVENT_RETENTION,
            "sessions":sessions.len(),"pending_approvals":sessions.iter().map(|s|s.approvals.len()).sum::<usize>(),
            "pending_questions":sessions.iter().map(|s|s.questions.len()).sum::<usize>(),
            "unknown_commands":self.state.commands.values().filter(|r|r.user_id==user&&r.status=="unknown_after_restart").count(),
            "files":self.state.files.values().filter(|f|f.user_id==user).count(),
            "notification_overflow":self.state.notification_overflow.get(user).copied().unwrap_or(0),
            "task_restart_recovery":false})
    }
    pub fn instance_id(&self) -> Uuid {
        self.state.instance_id
    }

    pub fn issue_pairing(&mut self, user_id: &str, now: i64) -> Result<String, ClientError> {
        if user_id.is_empty() || user_id.len() > 128 {
            return Err(ClientError::Invalid("invalid user ID"));
        }
        let code = secret();
        self.change(|state| {
            state.pairings.retain(|_, p| p.expires_at > now);
            if state.pairings.len() >= 32 {
                return Err(ClientError::Capacity);
            }
            state.pairings.insert(
                hash(&code),
                Pairing {
                    user_id: user_id.into(),
                    expires_at: now + 300,
                },
            );
            Ok(())
        })?;
        Ok(code)
    }

    pub fn pair(
        &mut self,
        code: &str,
        name: &str,
        now: i64,
    ) -> Result<(Device, String), ClientError> {
        if name.trim().is_empty() || name.len() > 128 {
            return Err(ClientError::Invalid("invalid device name"));
        }
        let token = secret();
        let device = self.change(|state| {
            let pairing = state
                .pairings
                .remove(&hash(code))
                .filter(|p| p.expires_at > now)
                .ok_or(ClientError::Unauthorized)?;
            if state.devices.len() >= 256 {
                return Err(ClientError::Capacity);
            }
            let device = Device {
                id: Uuid::new_v4(),
                user_id: pairing.user_id,
                name: name.into(),
                expires_at: now + 90 * 86400,
                revoked: false,
                token_hash: Some(hash(&token)),
            };
            state.devices.push(device.clone());
            Ok(device)
        })?;
        Ok((device.public(), token))
    }

    pub fn authenticate(&self, token: &str, now: i64) -> Result<Device, ClientError> {
        if self.failed {
            return Err(ClientError::StorageUnavailable);
        }
        let token_hash = hash(token);
        self.state
            .devices
            .iter()
            .find(|d| {
                !d.revoked && d.expires_at > now && d.token_hash.as_deref() == Some(&token_hash)
            })
            .cloned()
            .map(Device::public)
            .ok_or(ClientError::Unauthorized)
    }

    pub fn devices(&self, user: &str) -> Vec<Device> {
        self.state
            .devices
            .iter()
            .filter(|d| d.user_id == user)
            .cloned()
            .map(Device::public)
            .collect()
    }

    pub fn revoke(&mut self, user: &str, id: Uuid) -> Result<(), ClientError> {
        self.change(|state| {
            let device = state
                .devices
                .iter_mut()
                .find(|d| d.user_id == user && d.id == id)
                .ok_or(ClientError::NotFound)?;
            device.revoked = true;
            device.token_hash = None;
            Ok(())
        })
    }

    pub fn snapshot(&self, user: &str) -> Value {
        serde_json::json!({ "cursor": self.state.sequence, "instance_id": self.state.instance_id,
            "sessions": self.state.sessions.values().filter(|s| s.user_id == user).collect::<Vec<_>>(),
            "automation":self.state.automation.get(user).cloned().unwrap_or(serde_json::json!({"available":false,"schedules":[],"stale":true})),
            "notifications":self.state.notifications.iter().rev().filter(|n|n.user_id==user).take(200).map(|n|n.public()).collect::<Vec<_>>(),
            "notifications_truncated":self.state.notifications.iter().filter(|n|n.user_id==user).count()>200,
            "notification_overflow":self.state.notification_overflow.get(user).copied().unwrap_or(0),
            "files":self.state.files.values().filter(|f|f.user_id==user).collect::<Vec<_>>() })
    }

    pub fn events(&self, user: &str, after: u64) -> Result<Value, ClientError> {
        if after > self.state.sequence
            || self
                .state
                .events
                .first()
                .is_some_and(|e| after < e.sequence - 1)
        {
            return Err(ClientError::Resync);
        }
        Ok(serde_json::json!({"cursor": self.state.sequence,
            "events": self.state.events.iter().filter(|e| e.sequence > after && self.state.sessions.get(&e.session_id).is_some_and(|s| s.user_id == user)).collect::<Vec<_>>() }))
    }

    pub fn session(&self, user: &str, id: Uuid) -> Result<&Session, ClientError> {
        self.state
            .sessions
            .get(&id)
            .filter(|s| s.user_id == user)
            .ok_or(ClientError::NotFound)
    }

    /// 一个命令 ID 在实例中只接受一种内容；会话创建也使用该账本去重。
    pub fn command(
        &self,
        user: &str,
        id: Uuid,
        payload: &str,
    ) -> Result<Option<CommandReceipt>, ClientError> {
        match self.state.commands.get(&id) {
            Some(old) if old.user_id == user && old.payload_hash == hash(payload) => {
                Ok(Some(old.clone()))
            }
            Some(_) => Err(ClientError::Conflict("command ID was already used")),
            None => Ok(None),
        }
    }

    pub fn command_status(&self, user: &str, id: Uuid) -> Result<Value, ClientError> {
        let receipt = self
            .state
            .commands
            .get(&id)
            .filter(|r| r.user_id == user)
            .ok_or(ClientError::NotFound)?;
        Ok(
            serde_json::json!({"command_id":id,"session_id":receipt.session_id,"status":receipt.status,"result_code":receipt.result_code}),
        )
    }

    pub fn create_session(
        &mut self,
        user: &str,
        id: Uuid,
        title: &str,
        payload: &str,
    ) -> Result<CommandReceipt, ClientError> {
        self.change(|state| {
            if state.sessions.len() >= 256 || state.commands.len() >= COMMAND_LIMIT {
                return Err(ClientError::Capacity);
            }
            let session = Session {
                id: Uuid::new_v4(),
                user_id: user.into(),
                title: title.into(),
                version: 0,
                tasks: BTreeMap::new(),
                approvals: BTreeMap::new(),
                questions: BTreeMap::new(),
                observations: vec![],
                observations_truncated: false,
                progress: BTreeMap::new(),
                experience_drafts: BTreeMap::new(),
                messages: vec![],
                history_truncated: false,
                updated_at: state.sequence,
            };
            let receipt = CommandReceipt {
                command_id: id,
                status: "processed".into(),
                result_code: None,
                session_id: session.id,
                payload_hash: hash(payload),
                user_id: user.into(),
            };
            state.sessions.insert(session.id, session);
            state.commands.insert(id, receipt.clone());
            Ok(receipt)
        })
    }

    pub fn accept_text(
        &mut self,
        user: &str,
        id: Uuid,
        session_id: Uuid,
        text: &str,
        attachments: &[Uuid],
        payload: &str,
    ) -> Result<CommandReceipt, ClientError> {
        self.session(user, session_id)?;
        self.change(|state| {
            if state.commands.len() >= COMMAND_LIMIT {
                return Err(ClientError::Capacity);
            }
            let receipt = CommandReceipt {
                command_id: id,
                session_id,
                status: "queued".into(),
                result_code: None,
                payload_hash: hash(payload),
                user_id: user.into(),
            };
            state.commands.insert(id, receipt.clone());
            append_event(
                state,
                session_id,
                "text",
                serde_json::json!({"role":"user", "content":text, "command_id":id, "attachment_ids":attachments}),
            );
            Ok(receipt)
        })
    }

    pub fn reject_command(&mut self, id: Uuid) -> Result<(), ClientError> {
        self.change(|state| {
            state
                .commands
                .get_mut(&id)
                .ok_or(ClientError::NotFound)?
                .status = "runtime_unavailable".into();
            Ok(())
        })
    }

    pub(super) fn accept_command(
        &mut self,
        user: &str,
        id: Uuid,
        session_id: Uuid,
        payload: &str,
    ) -> Result<(), ClientError> {
        self.session(user, session_id)?;
        self.change(|state| {
            if state.commands.len() >= COMMAND_LIMIT {
                return Err(ClientError::Capacity);
            }
            state.commands.insert(
                id,
                CommandReceipt {
                    command_id: id,
                    session_id,
                    status: "queued".into(),
                    result_code: None,
                    payload_hash: hash(payload),
                    user_id: user.into(),
                },
            );
            Ok(())
        })
    }

    pub(super) fn accept_interaction(
        &mut self,
        user: &str,
        id: Uuid,
        session_id: Uuid,
        interaction: &crate::domain::TaskInteraction,
        payload: &str,
    ) -> Result<(), ClientError> {
        use crate::domain::TaskInteraction;
        let session = self.session(user, session_id)?;
        match interaction {
            TaskInteraction::Stop {
                task_id,
                expected_revision,
            }
            | TaskInteraction::Finish {
                task_id,
                expected_revision,
            } => {
                let task = session
                    .tasks
                    .get(&task_id.to_string())
                    .ok_or(ClientError::NotFound)?;
                let operation = if matches!(interaction, TaskInteraction::Stop { .. }) {
                    "can_stop"
                } else {
                    "can_finish"
                };
                if task["revision"].as_str() != Some(expected_revision)
                    || task[operation].as_bool() != Some(true)
                {
                    return Err(ClientError::Conflict(
                        "task changed or operation unavailable",
                    ));
                }
            }
            TaskInteraction::Answer {
                task_id,
                question_id,
                answer,
            } => {
                let question = session
                    .questions
                    .get(&format!("{task_id}/{question_id}"))
                    .ok_or(ClientError::NotFound)?;
                if question["claimed"].as_bool() == Some(true) {
                    return Err(ClientError::Conflict("question already claimed"));
                }
                if answer.trim().is_empty() || answer.len() > 32 * 1024 {
                    return Err(ClientError::Invalid("invalid answer"));
                }
            }
        }
        self.change(|state| {
            if state.commands.len() >= COMMAND_LIMIT { return Err(ClientError::Capacity); }
            state.commands.insert(id,CommandReceipt {command_id:id,session_id,status:"queued".into(),result_code:None,
                payload_hash:hash(payload),user_id:user.into()});
            if let TaskInteraction::Answer {task_id,question_id,answer} = interaction {
                state.sessions.get_mut(&session_id).unwrap().questions.get_mut(&format!("{task_id}/{question_id}")).unwrap()["claimed"] = true.into();
                append_event(state,session_id,"text",serde_json::json!({"role":"user","content":answer,"command_id":id,"question_id":question_id}));
            }
            Ok(())
        })
    }

    pub(super) fn accept_approval(
        &mut self,
        user: &str,
        id: Uuid,
        session_id: Uuid,
        selection: ApprovalSelection<'_>,
        payload: &str,
    ) -> Result<CommandReceipt, ClientError> {
        let ApprovalSelection {
            request_id,
            expected_version,
            option_id,
        } = selection;
        let session = self.session(user, session_id)?;
        let request = session
            .approvals
            .get(&request_id)
            .ok_or(ClientError::NotFound)?;
        if request["version"].as_u64() != Some(expected_version)
            || request["claimed"].as_bool() == Some(true)
        {
            return Err(ClientError::Conflict(
                "approval is stale or already claimed",
            ));
        }
        if !request["options"].as_array().is_some_and(|options| {
            options
                .iter()
                .any(|option| option["id"].as_str() == Some(option_id))
        }) {
            return Err(ClientError::Invalid("unknown approval option"));
        }
        self.change(|state| {
            if state.commands.len() >= COMMAND_LIMIT {
                return Err(ClientError::Capacity);
            }
            let receipt = CommandReceipt {
                command_id: id,
                session_id,
                status: "queued".into(),
                result_code: None,
                payload_hash: hash(payload),
                user_id: user.into(),
            };
            state.commands.insert(id, receipt.clone());
            let request = state
                .sessions
                .get_mut(&session_id)
                .unwrap()
                .approvals
                .get_mut(&request_id)
                .unwrap();
            request["claimed"] = true.into();
            append_event(
                state,
                session_id,
                "approval_claimed",
                serde_json::json!({"request_id":request_id,"command_id":id}),
            );
            Ok(receipt)
        })
    }

    pub fn append(
        &mut self,
        user: &str,
        session_id: Uuid,
        kind: &str,
        data: Value,
    ) -> Result<(), ClientError> {
        self.append_with_notification(user, session_id, kind, data, None)
    }

    pub(super) fn append_with_notification(
        &mut self,
        user: &str,
        session_id: Uuid,
        kind: &str,
        data: Value,
        config: Option<&MeowConfig>,
    ) -> Result<(), ClientError> {
        self.session(user, session_id)?;
        self.change(|state| {
            append_event(state, session_id, kind, data.clone());
            if let Some(config) = config {
                notifications::enqueue(state, user, session_id, kind, &data, config);
            }
            Ok(())
        })
    }
}

impl Device {
    fn public(mut self) -> Self {
        self.token_hash = None;
        self
    }
}

fn append_event(state: &mut State, session_id: Uuid, kind: &str, data: Value) {
    state.sequence += 1;
    let session = state
        .sessions
        .get_mut(&session_id)
        .expect("validated session");
    session.version += 1;
    session.updated_at = state.sequence;
    if matches!(kind, "tool_started" | "tool_completed" | "permission_audit") {
        let mut observation = data.clone();
        observation["kind"] = kind.into();
        observation["sequence"] = state.sequence.into();
        session.observations.push(observation);
        if session.observations.len() > 200 {
            session.observations.remove(0);
            session.observations_truncated = true;
        }
    } else if matches!(kind, "todos" | "inbox" | "batch_progress") {
        if let Some(id) = data["task_id"]
            .as_str()
            .or_else(|| data["batch_id"].as_str())
        {
            session
                .progress
                .insert(format!("{kind}/{id}"), data.clone());
        }
    } else if kind == "automation" {
        let mut snapshot = data.clone();
        snapshot["stale"] = false.into();
        let active: std::collections::HashSet<_> = snapshot["schedules"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|s| s["id"].as_str().map(String::from))
            .collect();
        let mut archived = std::collections::BTreeMap::new();
        if let Some(previous) = state.automation.get(&session.user_id) {
            for def in previous["archived"]
                .as_array()
                .into_iter()
                .flatten()
                .chain(previous["schedules"].as_array().into_iter().flatten())
            {
                if let Some(id) = def["id"].as_str().filter(|id| !active.contains(*id)) {
                    let mut def = def.clone();
                    def["archived"] = true.into();
                    def["enabled"] = false.into();
                    def["next_trigger"] = Value::Null;
                    archived.insert(id.to_string(), def);
                }
            }
        }
        snapshot["archive_truncated"] = (archived.len() > 256).into();
        snapshot["archived"] =
            serde_json::json!(archived.into_values().take(256).collect::<Vec<_>>());
        if snapshot.get("history").is_none()
            && let Some(history) = state
                .automation
                .get(&session.user_id)
                .and_then(|v| v.get("history"))
        {
            snapshot["history"] = history.clone();
        }
        state.automation.insert(session.user_id.clone(), snapshot);
    } else if kind == "command_result" {
        if let Some(id) = data["command_id"]
            .as_str()
            .and_then(|s| s.parse::<Uuid>().ok())
            && let Some(receipt) = state.commands.get_mut(&id).filter(|r| {
                r.session_id == session_id && r.user_id == session.user_id && r.status == "queued"
            })
        {
            receipt.status = data["status"].as_str().unwrap_or("rejected").into();
            receipt.result_code = data["result_code"].as_str().map(String::from);
        }
    } else if kind == "task_interaction" {
        if let Some(id) = data["task_id"].as_str() {
            if let Some(task) = session.tasks.get_mut(id) {
                for key in ["revision", "can_stop", "can_finish"] {
                    task[key] = data[key].clone();
                }
            }
            session.questions.retain(|_, q| {
                q["task_id"].as_str() != Some(id) || q["question_id"] == data["question_id"]
            });
        }
    } else if kind == "question" {
        if let (Some(task), Some(id)) = (data["task_id"].as_str(), data["question_id"].as_str()) {
            session
                .questions
                .insert(format!("{task}/{id}"), data.clone());
        }
    } else if kind == "task_status" {
        if let Some(id) = data["task_id"].as_str() {
            let mut next = data.clone();
            if let Some(previous) = session.tasks.get(id) {
                for key in ["revision", "can_stop", "can_finish"] {
                    next[key] = previous[key].clone();
                }
            }
            if matches!(data["status"].as_str(), Some("done" | "failed")) {
                next["can_stop"] = false.into();
                next["can_finish"] = false.into();
            }
            session.tasks.insert(id.into(), next);
        }
    } else if kind == "task_cleared" {
        if let Some(id) = data["task_id"].as_str() {
            session.tasks.remove(id);
            session
                .questions
                .retain(|_, q| q["task_id"].as_str() != Some(id));
            session
                .progress
                .retain(|_, p| p["task_id"].as_str() != Some(id));
        }
    } else if kind == "text" {
        let mut message = data.clone();
        message["sequence"] = state.sequence.into();
        session.messages.push(message);
        if session.messages.len() > 500 {
            session.messages.remove(0);
            session.history_truncated = true;
        }
    } else if kind == "experience_draft" {
        if let Some(id) = data["draft_id"].as_str() {
            if matches!(data["status"].as_str(), Some("saved" | "discarded")) {
                session.experience_drafts.remove(id);
            } else {
                let mut draft = session
                    .experience_drafts
                    .get(id)
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({}));
                if let (Some(target), Some(fields)) = (draft.as_object_mut(), data.as_object()) {
                    for (key, value) in fields {
                        target.insert(key.clone(), value.clone());
                    }
                }
                session.experience_drafts.insert(id.into(), draft);
            }
        }
    } else if kind == "approval_required" {
        if let Some(id) = data["request_id"].as_str().and_then(|id| id.parse().ok()) {
            let mut request = data.clone();
            request["version"] = session.version.into();
            request["claimed"] = false.into();
            session.approvals.insert(id, request);
        }
    } else if kind == "approval_result"
        && let Some(id) = data["request_id"]
            .as_str()
            .and_then(|id| id.parse::<Uuid>().ok())
    {
        session.approvals.remove(&id);
    }
    state.events.push(ClientEvent {
        sequence: state.sequence,
        event_id: Uuid::new_v4(),
        session_id,
        object_version: session.version,
        kind: kind.into(),
        data,
    });
    if state.events.len() > EVENT_RETENTION {
        state.events.remove(0);
    }
}
