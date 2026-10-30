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
    pub messages: Vec<Value>,
    pub history_truncated: bool,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct CommandReceipt {
    pub command_id: Uuid,
    /// 只表示进入统一输入队列，不表示 Agent 已消费或任务执行成功。
    pub status: String,
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
            for task in session.tasks.values_mut() {
                if !matches!(task["status"].as_str(), Some("done" | "failed")) {
                    task["status"] = "recovery_required".into();
                }
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
            "sessions": self.state.sessions.values().filter(|s| s.user_id == user).collect::<Vec<_>>() })
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
            serde_json::json!({"command_id":id,"session_id":receipt.session_id,"status":receipt.status}),
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
                messages: vec![],
                history_truncated: false,
            };
            let receipt = CommandReceipt {
                command_id: id,
                status: "processed".into(),
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
                payload_hash: hash(payload),
                user_id: user.into(),
            };
            state.commands.insert(id, receipt.clone());
            append_event(
                state,
                session_id,
                "text",
                serde_json::json!({"role":"user", "content":text, "command_id":id}),
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
        self.session(user, session_id)?;
        self.change(|state| {
            append_event(state, session_id, kind, data);
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
    if kind == "task_status" {
        if let Some(id) = data["task_id"].as_str() {
            session.tasks.insert(id.into(), data.clone());
        }
    } else if kind == "task_cleared" {
        if let Some(id) = data["task_id"].as_str() {
            session.tasks.remove(id);
        }
    } else if kind == "text" {
        session.messages.push(data.clone());
        if session.messages.len() > 500 {
            session.messages.remove(0);
            session.history_truncated = true;
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
