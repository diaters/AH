//! 平台无关的原生客户端入口。网络层只鉴权和同步，输入仍进入统一运行时。
mod experience;
mod server;
mod store;
#[cfg(test)]
mod tests;
mod tls;

use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::watch;

use super::{Channel, ChannelError, ChannelInboundMessage, ChannelOutboundMessage};
use crate::domain::{
    EngineEvent, EventTarget, ExternalInput, Frontend, FrontendKind, MessageRole, UserAction,
};

pub use server::ClientServer;
pub use store::{MeowConfig, Store};

/// 离线本机管理：运行实例持有独占锁，拒绝与服务并发改写身份库。
pub fn run_admin_command() -> anyhow::Result<bool> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let Some(index) = args.iter().position(|arg| arg == "--client-pair") else {
        return Ok(false);
    };
    anyhow::ensure!(
        args.len() == 2 && index == 0,
        "usage: harness --client-pair USER_ID"
    );
    let path = std::env::var("HARNESS_CHANNELS_CONFIG")?;
    let configs: super::ChannelConfigs = toml::from_str(&std::fs::read_to_string(path)?)?;
    let config = configs
        .client
        .ok_or_else(|| anyhow::anyhow!("[client] is not configured"))?;
    let tls = tls::load(&config)?;
    let mut store = Store::open(config.state_path.clone())?;
    let code = store.issue_pairing(&args[1], chrono::Utc::now().timestamp())?;
    let addresses = tls::candidate_addresses(&config);
    anyhow::ensure!(
        !addresses.is_empty(),
        "no reachable address found; set [client] advertise"
    );
    let link = tls::pairing_link(
        store.instance_id(),
        &code,
        tls.ca_fingerprint.as_deref(),
        &addresses,
    );
    println!("{}", tls::terminal_qr(&link)?);
    let qr_path = tls::write_qr_svg(&config, &link)?;
    println!(
        "用 Harness 客户端扫码，或复制下面的配对链接（300 秒内有效，仅可使用一次）：\n\n{link}\n"
    );
    println!(
        "终端配色或字体导致扫不出时，打开二维码文件再扫：{}",
        qr_path.display()
    );
    println!("addresses={}", addresses.join(","));
    Ok(true)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientConfig {
    pub listen: SocketAddr,
    pub instance_name: String,
    /// 与 `tls_key` 同时省略时，由实例自管私有 CA 并签发服务端证书。
    #[serde(default)]
    pub tls_cert: Option<PathBuf>,
    #[serde(default)]
    pub tls_key: Option<PathBuf>,
    pub state_path: PathBuf,
    /// 客户端可达的主机名或 IP；省略时使用本机网卡地址。用于证书 SAN 与配对链接。
    #[serde(default)]
    pub advertise: Vec<String>,
    #[serde(default)]
    pub meow: Option<MeowConfig>,
}

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("authentication required")]
    Unauthorized,
    #[error("object not found")]
    NotFound,
    #[error("{0}")]
    Invalid(&'static str),
    #[error("{0}")]
    Conflict(&'static str),
    #[error("client capacity reached")]
    Capacity,
    #[error("snapshot required")]
    Resync,
    #[error("client state is unavailable; restart and inspect the persisted state")]
    StorageUnavailable,
    #[error("client state I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("client state JSON: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Clone)]
pub struct Client {
    pub(super) config: ClientConfig,
    pub(super) store: Arc<Mutex<Store>>,
    pub(super) input: crossbeam_channel::Sender<ExternalInput>,
    pub(super) changed: watch::Sender<u64>,
    /// 经验库；未注入时客户端不提供经验总结与管理。
    pub(super) experience: Option<crate::contracts::SharedExperienceLibrary>,
}

impl Client {
    pub fn new(
        config: ClientConfig,
        store: Store,
        input: crossbeam_channel::Sender<ExternalInput>,
    ) -> Self {
        let (changed, _) = watch::channel(0);
        Self {
            config,
            store: Arc::new(Mutex::new(store)),
            input,
            changed,
            experience: None,
        }
    }

    /// 启用经验总结与管理（与 ECS 共享同一经验库实例）。
    pub fn with_experience_library(
        mut self,
        library: crate::contracts::SharedExperienceLibrary,
    ) -> Self {
        self.experience = Some(library);
        self
    }

    fn wake(&self) {
        self.changed
            .send_modify(|value| *value = value.wrapping_add(1));
    }

    fn record(&self, event: &EngineEvent) -> Result<(), ClientError> {
        // 内部广播可能包含其他用户信息；绝不直接转发。
        let EventTarget::Directed(targets) = event.target() else {
            return Ok(());
        };
        let (kind, data) = match event {
            EngineEvent::ToolCallCompleted {
                task_id,
                call_id,
                agent_name,
                tool_name,
                success,
                output,
                truncated,
                late_result,
                background,
                ..
            } => (
                "tool_completed",
                json!({"task_id":task_id,"call_id":call_id,"agent_name":agent_name,"tool_name":tool_name,"success":success,"output":output,"truncated":truncated,"late_result":late_result,"background":background}),
            ),
            EngineEvent::AutomationSnapshot { snapshot, .. } => ("automation", snapshot.clone()),
            EngineEvent::ExperienceDraftUpdated {
                draft_id, outcome, ..
            } => (
                "experience_draft",
                experience::draft_event(self, *draft_id, outcome),
            ),
            EngineEvent::TaskInteractionChanged {
                task_id,
                revision,
                question_id,
                can_stop,
                can_finish,
                ..
            } => (
                "task_interaction",
                json!({"task_id":task_id,"revision":revision,"question_id":question_id,"can_stop":can_stop,"can_finish":can_finish}),
            ),
            EngineEvent::QuestionPresented {
                task_id,
                question_id,
                question,
                ..
            } => (
                "question",
                json!({"task_id":task_id,"question_id":question_id,"question":question,"claimed":false}),
            ),
            EngineEvent::CommandResult {
                command_id,
                outcome,
                ..
            } => (
                "command_result",
                json!({"command_id":command_id,"status":if outcome.is_ok(){"processed"}else{"rejected"},"result_code":outcome.as_ref().err()}),
            ),
            EngineEvent::AgentOutput {
                task_id,
                content,
                progress,
                ..
            } => (
                "text",
                json!({"role":"agent", "content":content, "task_id":task_id, "phase":if *progress { "progress" } else { "response" }}),
            ),
            EngineEvent::Text {
                role,
                content,
                task_id,
                ..
            } => (
                "text",
                json!({
                "role": match role { MessageRole::Agent => "agent", MessageRole::User => "user", MessageRole::System => "system" },
                "content":content, "task_id":task_id }),
            ),
            EngineEvent::TaskStatusChanged {
                task_id,
                name,
                status,
                result,
                parent_id,
                agent_name,
                waiting_reason,
                ..
            } => (
                "task_status",
                json!({
                "task_id":task_id, "name":name, "status":format!("{status:?}").to_lowercase(),
                "result":result, "parent_id":parent_id, "agent_name":agent_name,
                "waiting_reason":waiting_reason.map(|r| format!("{r:?}").to_lowercase()) }),
            ),
            EngineEvent::ToolCallStarted {
                task_id,
                agent_name,
                tool_name,
                tool_input_summary,
                ..
            } => (
                "tool_started",
                json!({
                "task_id":task_id, "agent_name":agent_name, "tool_name":tool_name, "input_summary":tool_input_summary }),
            ),

            EngineEvent::PermissionAudit {
                agent_id,
                agent_name,
                tool_name,
                action,
                source,
                context,
                ..
            } => (
                "permission_audit",
                json!({"agent_id":agent_id,"agent_name":agent_name,"tool_name":tool_name,"action":action,"source":source,"context":context}),
            ),
            EngineEvent::BatchProgress {
                batch_id,
                completed,
                total,
                ..
            } => (
                "batch_progress",
                json!({"batch_id":batch_id,"completed":completed,"total":total}),
            ),
            EngineEvent::TodoUpdated { task_id, items, .. } => {
                ("todos", json!({"task_id":task_id, "items":items}))
            }
            EngineEvent::InboxChanged {
                task_id, pending, ..
            } => ("inbox", json!({"task_id":task_id,"pending":pending})),
            EngineEvent::TaskCleared { task_id, .. } => {
                ("task_cleared", json!({"task_id":task_id}))
            }
            // 保留原始审批请求与选项；回包继续经过运行时权限校验。
            EngineEvent::ApprovalRequest {
                request_id,
                agent_name,
                tool_name,
                options,
                tool_input,
                approval_context,
                ..
            } => (
                "approval_required",
                json!({
                "request_id":request_id,"agent_name":agent_name,"tool_name":tool_name,
                "input_json":tool_input.to_string(),"context":approval_context,
                "options":options.iter().map(|o| json!({"id":o.id,"label":o.label,"description":o.description})).collect::<Vec<_>>() }),
            ),
            EngineEvent::ApprovalResult {
                request_id,
                decision,
                ..
            } => (
                "approval_result",
                json!({"request_id":request_id,"decision":decision}),
            ),
            _ => return Ok(()),
        };
        let mut store = self.store.lock().expect("client state mutex poisoned");
        let mut seen = std::collections::HashSet::new();
        for channel in targets
            .iter()
            .filter(|c| c.frontend == FrontendKind::Client)
        {
            let Some(session_id) = channel.thread_id.as_deref().and_then(|s| s.parse().ok()) else {
                continue;
            };
            if seen.insert((channel.user_id.clone(), session_id)) {
                store.append_with_notification(
                    &channel.user_id,
                    session_id,
                    kind,
                    data.clone(),
                    self.config.meow.as_ref(),
                )?;
            }
        }
        self.wake();
        Ok(())
    }
}

impl Frontend for Client {
    fn kind(&self) -> FrontendKind {
        FrontendKind::Client
    }
    fn push_event(&self, event: EngineEvent) {
        if let Err(error) = self.record(&event) {
            tracing::error!(event="ClientEventPersistenceFailed", %error, "client event was not persisted");
        }
    }
    fn poll_actions(&self) -> Vec<UserAction> {
        vec![]
    }
    fn requires_polling(&self) -> bool {
        false
    }
}

#[async_trait]
impl Channel for Client {
    fn name(&self) -> &str {
        "client"
    }
    fn native_frontend(&self) -> Option<Box<dyn Frontend>> {
        Some(Box::new(self.clone()))
    }

    async fn send(&self, message: &ChannelOutboundMessage) -> Result<Option<String>, ChannelError> {
        // 附件必须走后续的授权文件协议，禁止暴露服务器任意路径。
        if message.reply_markup.is_some() {
            return Err(ChannelError::NotSupported);
        }
        let session_id = message
            .thread_id
            .as_deref()
            .and_then(|id| id.parse().ok())
            .ok_or(ChannelError::NotConfigured)?;
        if message.attachments.len() > 8 {
            return Err(ChannelError::NotSupported);
        }
        let mut attachment_ids = vec![];
        {
            let mut store = self.store.lock().expect("client state mutex poisoned");
            for attachment in &message.attachments {
                let file = store
                    .import_attachment(&message.recipient, session_id, &attachment.target)
                    .map_err(|error| ChannelError::Api {
                        code: 500,
                        message: error.to_string(),
                    })?;
                attachment_ids.push(file.id);
            }
        }
        self.store
            .lock()
            .expect("client state mutex poisoned")
            .append_with_notification(
                &message.recipient,
                session_id,
                "text",
                json!({"role":"system","content":message.content,"background_notification":true,"attachment_ids":attachment_ids}),
                self.config.meow.as_ref(),
            )
            .map_err(|error| ChannelError::Api {
                code: 500,
                message: error.to_string(),
            })?;
        self.wake();
        // 回执表示本实例持久化，不表示 MeoW 或设备已接收。
        Ok(None)
    }

    async fn listen(
        &self,
        _tx: crossbeam_channel::Sender<ChannelInboundMessage>,
    ) -> Result<(), ChannelError> {
        // HTTPS 服务持有统一 ExternalInput 发送端，无额外监听/桥接线程。
        Ok(())
    }
    fn supported_attachment_kinds(&self) -> Vec<crate::domain::AttachmentKind> {
        use crate::domain::AttachmentKind::*;
        vec![Image, Document, Video, Audio, Voice]
    }
}
