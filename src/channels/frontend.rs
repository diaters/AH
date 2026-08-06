use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use tokio::sync::{RwLock, mpsc};
use tracing::{error, trace};

use crate::domain::{
    ChannelId, EngineEvent, EventTarget, Frontend, FrontendKind, MessageRole, TaskId,
    TaskStatusKind, UserAction,
};

use super::traits::{ChannelOutboundMessage, MessageKind, OutboundEntry};
use super::traits::{ChannelParseMode, InlineKeyboardButton, ReplyMarkup};

/// 将 EngineEvent 路由到对应 IM 通道出向发送队列的 Frontend 实现。
///
/// 有状态化：维护 per-task 的状态消息 msg_id，实现滚动撤回策略。
pub struct ChannelFrontend {
    kind: FrontendKind,
    channel_name: String,
    outbound_tx: mpsc::UnboundedSender<OutboundEntry>,
    /// Per-task + per-recipient 的状态消息追踪。
    /// key = (task_id, recipient)，value = 最近一条状态消息的 msg_id。
    last_status_msg: Arc<RwLock<HashMap<(String, String), String>>>,
    /// Per-task 的最终态决策缓存。
    task_finalized: Arc<RwLock<HashSet<String>>>,
    /// (task_id, recipient) — 标记 LLMReply 已到达但 on_sent 尚未回写新 msg_id。
    /// on_sent 回调在新 msg_id 确认后，若此集合中包含该 key，
    /// 则立即发起 Recall 撤回刚发送的状态消息，并清理 last_status_msg。
    pending_reply_recall: Arc<RwLock<HashSet<(String, String)>>>,
}

impl ChannelFrontend {
    pub fn new(
        kind: FrontendKind,
        channel_name: impl Into<String>,
        outbound_tx: mpsc::UnboundedSender<OutboundEntry>,
    ) -> Self {
        Self {
            kind,
            channel_name: channel_name.into(),
            outbound_tx,
            last_status_msg: Arc::new(RwLock::new(HashMap::new())),
            task_finalized: Arc::new(RwLock::new(HashSet::new())),
            pending_reply_recall: Arc::new(RwLock::new(HashSet::new())),
        }
    }

    fn matches(&self, channel_id: &ChannelId) -> bool {
        channel_id.frontend == self.kind
    }

    fn enqueue(
        &self,
        msg: ChannelOutboundMessage,
        on_sent: Option<Box<dyn FnOnce(Option<String>) + Send + Sync>>,
    ) {
        let entry = OutboundEntry {
            channel_name: self.channel_name.clone(),
            message: msg,
            on_sent,
        };
        if let Err(e) = self.outbound_tx.send(entry) {
            error!(event = "ChannelFrontendSendFailed", error = %e, channel = %self.channel_name);
        }
    }

    /// 发送 Recall 消息（撤回指定 msg_id）。
    fn enqueue_recall(&self, recipient: String, thread_id: Option<String>, msg_id: String) {
        let msg = ChannelOutboundMessage {
            recipient,
            thread_id,
            content: msg_id,
            parse_mode: None,
            reply_markup: None,
            attachments: vec![],
            message_kind: MessageKind::Recall,
        };
        self.enqueue(msg, None);
    }
}

fn html_escape(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for c in input.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

fn task_short_id(task_id: TaskId) -> String {
    task_id
        .to_string()
        .split('-')
        .next()
        .unwrap_or("????")
        .to_string()
}

fn role_label(role: MessageRole) -> &'static str {
    match role {
        MessageRole::Agent => "助手",
        MessageRole::System => "系统",
        MessageRole::User => "用户",
    }
}

fn status_label(status: TaskStatusKind) -> &'static str {
    match status {
        TaskStatusKind::Pending => "待处理",
        TaskStatusKind::Running => "运行中",
        TaskStatusKind::Waiting => "等待中",
        TaskStatusKind::Done => "已完成",
        TaskStatusKind::Failed => "已失败",
    }
}

impl Frontend for ChannelFrontend {
    fn kind(&self) -> FrontendKind {
        self.kind.clone()
    }

    fn push_event(&self, event: EngineEvent) {
        match event {
            EngineEvent::Text {
                target,
                role,
                content,
                task_id,
                ..
            } => {
                let targets = match target {
                    EventTarget::Broadcast => return,
                    EventTarget::Directed(v) => v,
                };
                let recipients: Vec<ChannelId> = targets
                    .into_iter()
                    .filter(|cid| self.matches(cid))
                    .collect();
                if recipients.is_empty() {
                    return;
                }
                trace!(
                    event = "ChannelFrontendReceiveText",
                    channel = %self.channel_name,
                    recipients = recipients.len(),
                    content_len = content.len(),
                );
                let prefixed_content = task_id
                    .map(|id| format!("[{}] {}: {}", task_short_id(id), role_label(role), content))
                    .unwrap_or(content);
                let message_kind = match role {
                    MessageRole::Agent => MessageKind::LLMReply,
                    MessageRole::System => MessageKind::System,
                    MessageRole::User => MessageKind::Other,
                };
                for channel_id in recipients {
                    // LLM 回复到达时，撤回该 task+recipient 的最终态状态消息
                    if message_kind == MessageKind::LLMReply
                        && let Some(tid) = task_id
                    {
                        let key = (tid.to_string(), channel_id.user_id.clone());
                        if let Ok(map) = self.last_status_msg.try_read()
                            && let Some(msg_id) = map.get(&key).cloned()
                        {
                            // 正常路径：last_status_msg 有值 → 直接 Recall
                            drop(map);
                            self.enqueue_recall(
                                channel_id.user_id.clone(),
                                channel_id.thread_id.clone(),
                                msg_id,
                            );
                            if let Ok(mut map) = self.last_status_msg.try_write() {
                                map.remove(&key);
                            }
                        } else {
                            // 竞态路径：last_status_msg 为空（on_sent 尚未回写）
                            // → 设置标记，委托 on_sent 兜底 Recall
                            if let Ok(mut pending) = self.pending_reply_recall.try_write() {
                                pending.insert(key);
                            }
                        }
                        if let Ok(mut set) = self.task_finalized.try_write() {
                            set.insert(tid.to_string());
                        }
                    }
                    let msg = ChannelOutboundMessage {
                        recipient: channel_id.user_id,
                        thread_id: channel_id.thread_id,
                        content: prefixed_content.clone(),
                        parse_mode: None,
                        reply_markup: None,
                        attachments: vec![],
                        message_kind,
                    };
                    self.enqueue(msg, None);
                }
            }
            EngineEvent::ApprovalRequest {
                target,
                request_id,
                tool_name,
                tool_input,
                options,
                ..
            } => {
                let targets = match target {
                    EventTarget::Broadcast => return,
                    EventTarget::Directed(v) => v,
                };
                let recipients: Vec<ChannelId> = targets
                    .into_iter()
                    .filter(|cid| self.matches(cid))
                    .collect();
                if recipients.is_empty() {
                    return;
                }

                let tool_input_str = serde_json::to_string_pretty(&tool_input)
                    .unwrap_or_else(|_| tool_input.to_string());
                let tool_input_escaped = html_escape(&tool_input_str);
                let content = format!(
                    "🔒 需要你的确认\n\n工具：{}\n输入：<pre>{}</pre>\n\n请选择一个选项：",
                    tool_name, tool_input_escaped
                );
                let buttons: Vec<Vec<InlineKeyboardButton>> = options
                    .chunks(2)
                    .map(|chunk| {
                        chunk
                            .iter()
                            .map(|opt| InlineKeyboardButton {
                                text: opt.label.clone(),
                                callback_data: format!("{}:{}", request_id, opt.id),
                            })
                            .collect()
                    })
                    .collect();

                for channel_id in recipients {
                    let msg = ChannelOutboundMessage {
                        recipient: channel_id.user_id,
                        thread_id: channel_id.thread_id,
                        content: content.clone(),
                        parse_mode: Some(ChannelParseMode::Html),
                        reply_markup: Some(ReplyMarkup::InlineKeyboard(buttons.clone())),
                        attachments: vec![],
                        message_kind: MessageKind::ApprovalRequest,
                    };
                    self.enqueue(msg, None);
                }
            }
            EngineEvent::TaskStatusChanged {
                target,
                task_id,
                status,
                old_status,
                agent_name,
                ..
            } => {
                let targets = match target {
                    EventTarget::Broadcast => return,
                    EventTarget::Directed(v) => v,
                };
                let recipients: Vec<ChannelId> = targets
                    .into_iter()
                    .filter(|cid| self.matches(cid))
                    .collect();
                if recipients.is_empty() {
                    return;
                }
                // IM 通道不渲染 task name（即 input_summary）：用户原始输入常为长句，
                // 截断后语义不完整且含 markdown 符号，在纯文本 IM 下可读性差。
                // task_short_id 已足够识别任务。
                let transition = match old_status {
                    Some(old) => {
                        format!("{} → {}", status_label(old), status_label(status))
                    }
                    None => status_label(status).to_string(),
                };
                let content = match agent_name.as_deref() {
                    Some(agent) => {
                        format!("[{}]: {} @{}", task_short_id(task_id), transition, agent)
                    }
                    None => format!("[{}]: {}", task_short_id(task_id), transition),
                };

                for channel_id in recipients {
                    let key = (task_id.to_string(), channel_id.user_id.clone());
                    // 滚动撤回：发新状态消息前撤回上一条
                    // Failed 状态不撤回（保留错误信息作为最终态）
                    if status != TaskStatusKind::Failed
                        && let Ok(map) = self.last_status_msg.try_read()
                        && let Some(old_msg_id) = map.get(&key).cloned()
                    {
                        drop(map);
                        self.enqueue_recall(
                            channel_id.user_id.clone(),
                            channel_id.thread_id.clone(),
                            old_msg_id,
                        );
                        if let Ok(mut map) = self.last_status_msg.try_write() {
                            map.remove(&key);
                        }
                    }

                    // 准备 on_sent 回调：更新 last_status_msg + 检查 pending_reply_recall
                    let last_status_msg = self.last_status_msg.clone();
                    let pending_reply_recall = self.pending_reply_recall.clone();
                    let outbound_tx = self.outbound_tx.clone();
                    let channel_name = self.channel_name.clone();
                    let recipient = channel_id.user_id.clone();
                    let thread_id = channel_id.thread_id.clone();
                    let on_sent: Option<Box<dyn FnOnce(Option<String>) + Send + Sync>> =
                        Some(Box::new(move |msg_id: Option<String>| {
                            // 1. 保存新 msg_id（现行为）
                            if let Some(ref id) = msg_id
                                && let Ok(mut map) = last_status_msg.try_write()
                            {
                                map.insert(key.clone(), id.clone());
                            }
                            // 2. 检查 pending_reply_recall → 撤回刚发送的状态消息
                            if let Ok(mut pending) = pending_reply_recall.try_write()
                                && pending.remove(&key)
                                && let Some(id) = msg_id
                            {
                                // 清理 last_status_msg，避免后续 LLMReply 重复 Recall
                                if let Ok(mut map) = last_status_msg.try_write() {
                                    map.remove(&key);
                                }
                                let recall_entry = OutboundEntry {
                                    channel_name,
                                    message: ChannelOutboundMessage {
                                        recipient,
                                        thread_id,
                                        content: id,
                                        parse_mode: None,
                                        reply_markup: None,
                                        attachments: vec![],
                                        message_kind: MessageKind::Recall,
                                    },
                                    on_sent: None,
                                };
                                let _ = outbound_tx.send(recall_entry);
                            }
                        }));

                    let msg = ChannelOutboundMessage {
                        recipient: channel_id.user_id,
                        thread_id: channel_id.thread_id,
                        content: content.clone(),
                        parse_mode: None,
                        reply_markup: None,
                        attachments: vec![],
                        message_kind: MessageKind::TaskStatus,
                    };
                    self.enqueue(msg, on_sent);
                }
            }
            EngineEvent::TaskCleared { task_id, .. } => {
                let task_id_str = task_id.to_string();
                if let Ok(mut map) = self.last_status_msg.try_write() {
                    map.retain(|(tid, _), _| tid != &task_id_str);
                }
                if let Ok(mut set) = self.task_finalized.try_write() {
                    set.remove(&task_id_str);
                }
                if let Ok(mut set) = self.pending_reply_recall.try_write() {
                    set.retain(|(tid, _)| tid != &task_id_str);
                }
            }
            // ToolCallStarted 不推送到 IM 通道：
            // 面向开发者的调试信息，对 IM 用户无意义，徒增通知噪音。
            // TUI 前端仍可通过其他路径展示工具调用状态。
            EngineEvent::ToolCallStarted { .. } => {}
            _ => {}
        }
    }

    fn poll_actions(&self) -> Vec<UserAction> {
        vec![]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::ApprovalOption;

    fn make_frontend(
        kind: FrontendKind,
    ) -> (ChannelFrontend, mpsc::UnboundedReceiver<OutboundEntry>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (ChannelFrontend::new(kind, "test", tx), rx)
    }

    fn text_event(target: EventTarget) -> EngineEvent {
        EngineEvent::Text {
            target,
            role: crate::domain::MessageRole::Agent,
            content: "hello".to_string(),
            task_id: None,
        }
    }

    fn text_event_with_task(target: EventTarget, task_id: TaskId) -> EngineEvent {
        EngineEvent::Text {
            target,
            role: crate::domain::MessageRole::Agent,
            content: "hello".to_string(),
            task_id: Some(task_id),
        }
    }

    #[test]
    fn prefixes_agent_text_with_task_short_id() {
        use uuid::Uuid;
        let (fe, mut rx) = make_frontend(FrontendKind::Telegram);
        let task_id = Uuid::parse_str("a1b2c3d4-1111-2222-3333-444444444444").unwrap();
        fe.push_event(text_event_with_task(
            EventTarget::Directed(vec![ChannelId {
                frontend: FrontendKind::Telegram,
                user_id: "u1".to_string(),
                thread_id: None,
            }]),
            task_id,
        ));
        let entry = rx.try_recv().expect("one outbound message");
        assert_eq!(entry.message.content, "[a1b2c3d4] 助手: hello");
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn leaves_text_unchanged_without_task_id() {
        let (fe, mut rx) = make_frontend(FrontendKind::Telegram);
        fe.push_event(EngineEvent::Text {
            target: EventTarget::Directed(vec![ChannelId {
                frontend: FrontendKind::Telegram,
                user_id: "u1".to_string(),
                thread_id: None,
            }]),
            role: crate::domain::MessageRole::Agent,
            content: "hello".to_string(),
            task_id: None,
        });
        let entry = rx.try_recv().expect("one outbound message");
        assert_eq!(entry.message.content, "hello");
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn prefixes_system_text_with_task_short_id() {
        use uuid::Uuid;
        let (fe, mut rx) = make_frontend(FrontendKind::Telegram);
        let task_id = Uuid::parse_str("a1b2c3d4-1111-2222-3333-444444444444").unwrap();
        fe.push_event(EngineEvent::Text {
            target: EventTarget::Directed(vec![ChannelId {
                frontend: FrontendKind::Telegram,
                user_id: "u1".to_string(),
                thread_id: None,
            }]),
            role: crate::domain::MessageRole::System,
            content: "summary done".to_string(),
            task_id: Some(task_id),
        });
        let entry = rx.try_recv().expect("one outbound message");
        assert_eq!(entry.message.content, "[a1b2c3d4] 系统: summary done");
    }

    #[test]
    fn renders_task_status_change_with_transition() {
        use uuid::Uuid;
        let (fe, mut rx) = make_frontend(FrontendKind::Telegram);
        let task_id = Uuid::parse_str("a1b2c3d4-1111-2222-3333-444444444444").unwrap();
        fe.push_event(EngineEvent::TaskStatusChanged {
            target: EventTarget::Directed(vec![ChannelId {
                frontend: FrontendKind::Telegram,
                user_id: "u1".to_string(),
                thread_id: None,
            }]),
            task_id,
            name: "test".to_string(),
            status: TaskStatusKind::Done,
            old_status: Some(TaskStatusKind::Running),
            result: None,
            parent_id: None,
            origin_channel: None,
            agent_name: None,
            waiting_reason: None,
        });
        let entry = rx.try_recv().expect("one outbound message");
        assert_eq!(entry.message.content, "[a1b2c3d4]: 运行中 → 已完成");
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn ignores_broadcast() {
        let (fe, mut rx) = make_frontend(FrontendKind::Telegram);
        fe.push_event(text_event(EventTarget::Broadcast));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn ignores_non_matching_directed() {
        let (fe, mut rx) = make_frontend(FrontendKind::Telegram);
        fe.push_event(text_event(EventTarget::Directed(vec![ChannelId {
            frontend: FrontendKind::QQ,
            user_id: "u1".to_string(),
            thread_id: None,
        }])));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn queues_matching_directed() {
        let (fe, mut rx) = make_frontend(FrontendKind::Telegram);
        fe.push_event(text_event(EventTarget::Directed(vec![ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "u1".to_string(),
            thread_id: None,
        }])));
        let entry = rx.try_recv().expect("one outbound message");
        assert_eq!(entry.channel_name, "test");
        assert_eq!(entry.message.recipient, "u1");
        assert_eq!(entry.message.content, "hello");
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn approval_request_escapes_html_in_tool_input() {
        use uuid::Uuid;

        let (fe, mut rx) = make_frontend(FrontendKind::Telegram);
        let tool_input = serde_json::json!({"value": "<script> & text"});
        let event = EngineEvent::ApprovalRequest {
            target: EventTarget::Directed(vec![ChannelId {
                frontend: FrontendKind::Telegram,
                user_id: "u1".to_string(),
                thread_id: None,
            }]),
            request_id: Uuid::nil(),
            agent_name: "agent".to_string(),
            tool_name: "test_tool".to_string(),
            tool_input,
            options: vec![
                ApprovalOption {
                    id: "allow".to_string(),
                    label: "允许".to_string(),
                    description: String::new(),
                },
                ApprovalOption {
                    id: "deny".to_string(),
                    label: "拒绝".to_string(),
                    description: String::new(),
                },
            ],
            approval_context: None,
        };
        fe.push_event(event);
        let entry = rx.try_recv().expect("one outbound message");
        assert!(matches!(
            entry.message.parse_mode,
            Some(ChannelParseMode::Html)
        ));
        assert!(
            entry.message.content.contains("&lt;script&gt; &amp; text"),
            "HTML special chars should be escaped: {}",
            entry.message.content
        );
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn html_escape_works() {
        assert_eq!(
            html_escape("<script> & \"quotes\" 'single'"),
            "&lt;script&gt; &amp; &quot;quotes&quot; &#39;single&#39;"
        );
    }

    #[test]
    fn queues_text_with_task_prefix() {
        use uuid::Uuid;

        let (fe, mut rx) = make_frontend(FrontendKind::Telegram);
        let task_id: TaskId = Uuid::nil();
        fe.push_event(EngineEvent::Text {
            target: EventTarget::Directed(vec![ChannelId {
                frontend: FrontendKind::Telegram,
                user_id: "u1".to_string(),
                thread_id: None,
            }]),
            role: MessageRole::Agent,
            content: "hello".to_string(),
            task_id: Some(task_id),
        });
        let entry = rx.try_recv().expect("one outbound message");
        assert_eq!(entry.message.content, "[00000000] 助手: hello");
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn queues_status_change_with_transition() {
        use uuid::Uuid;

        let (fe, mut rx) = make_frontend(FrontendKind::Telegram);
        let task_id: TaskId = Uuid::nil();
        fe.push_event(EngineEvent::TaskStatusChanged {
            target: EventTarget::Directed(vec![ChannelId {
                frontend: FrontendKind::Telegram,
                user_id: "u1".to_string(),
                thread_id: None,
            }]),
            task_id,
            name: "task".to_string(),
            status: TaskStatusKind::Done,
            old_status: Some(TaskStatusKind::Running),
            result: None,
            parent_id: None,
            origin_channel: None,
            agent_name: None,
            waiting_reason: None,
        });
        let entry = rx.try_recv().expect("one outbound message");
        assert_eq!(entry.message.content, "[00000000]: 运行中 → 已完成");
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn queues_status_change_without_old_status() {
        use uuid::Uuid;

        let (fe, mut rx) = make_frontend(FrontendKind::Telegram);
        let task_id: TaskId = Uuid::nil();
        fe.push_event(EngineEvent::TaskStatusChanged {
            target: EventTarget::Directed(vec![ChannelId {
                frontend: FrontendKind::Telegram,
                user_id: "u1".to_string(),
                thread_id: None,
            }]),
            task_id,
            name: "task".to_string(),
            status: TaskStatusKind::Running,
            old_status: None,
            result: None,
            parent_id: None,
            origin_channel: None,
            agent_name: None,
            waiting_reason: None,
        });
        let entry = rx.try_recv().expect("one outbound message");
        assert_eq!(entry.message.content, "[00000000]: 运行中");
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn ignores_status_change_broadcast() {
        let (fe, mut rx) = make_frontend(FrontendKind::Telegram);
        fe.push_event(EngineEvent::TaskStatusChanged {
            target: EventTarget::Broadcast,
            task_id: TaskId::nil(),
            name: "task".to_string(),
            status: TaskStatusKind::Done,
            old_status: None,
            result: None,
            parent_id: None,
            origin_channel: None,
            agent_name: None,
            waiting_reason: None,
        });
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn tool_call_started_does_not_push_to_channel() {
        use uuid::Uuid;
        let (fe, mut rx) = make_frontend(FrontendKind::Telegram);
        let task_id = Uuid::parse_str("a1b2c3d4-1111-2222-3333-444444444444").unwrap();
        fe.push_event(EngineEvent::ToolCallStarted {
            target: EventTarget::Directed(vec![ChannelId {
                frontend: FrontendKind::Telegram,
                user_id: "u1".to_string(),
                thread_id: None,
            }]),
            task_id,
            agent_name: "TestAgent".to_string(),
            tool_name: "shell_exec".to_string(),
            tool_input_summary: "ls -la".to_string(),
        });
        assert!(
            rx.try_recv().is_err(),
            "ToolCallStarted should not push to IM channel"
        );
    }

    #[test]
    fn renders_task_status_change_with_name_and_agent() {
        use uuid::Uuid;
        let (fe, mut rx) = make_frontend(FrontendKind::Telegram);
        let task_id = Uuid::parse_str("a1b2c3d4-1111-2222-3333-444444444444").unwrap();
        fe.push_event(EngineEvent::TaskStatusChanged {
            target: EventTarget::Directed(vec![ChannelId {
                frontend: FrontendKind::Telegram,
                user_id: "u1".to_string(),
                thread_id: None,
            }]),
            task_id,
            name: "build feature".to_string(),
            status: TaskStatusKind::Done,
            old_status: Some(TaskStatusKind::Running),
            result: None,
            parent_id: None,
            origin_channel: None,
            agent_name: Some("TestAgent".to_string()),
            waiting_reason: None,
        });
        let entry = rx.try_recv().expect("one outbound message");
        assert_eq!(
            entry.message.content,
            "[a1b2c3d4]: 运行中 → 已完成 @TestAgent"
        );
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn renders_task_status_change_with_long_name_truncated() {
        use uuid::Uuid;
        let (fe, mut rx) = make_frontend(FrontendKind::Telegram);
        let task_id = Uuid::parse_str("a1b2c3d4-1111-2222-3333-444444444444").unwrap();
        let long_name = "测".repeat(40);
        fe.push_event(EngineEvent::TaskStatusChanged {
            target: EventTarget::Directed(vec![ChannelId {
                frontend: FrontendKind::Telegram,
                user_id: "u1".to_string(),
                thread_id: None,
            }]),
            task_id,
            name: long_name,
            status: TaskStatusKind::Done,
            old_status: Some(TaskStatusKind::Running),
            result: None,
            parent_id: None,
            origin_channel: None,
            agent_name: Some("TestAgent".to_string()),
            waiting_reason: None,
        });
        let entry = rx.try_recv().expect("one outbound message");
        // IM 通道不渲染 task name（即使很长也不应出现在输出中），仅显示 id + 状态 + agent
        assert_eq!(
            entry.message.content,
            "[a1b2c3d4]: 运行中 → 已完成 @TestAgent"
        );
        assert!(
            !entry.message.content.contains('测'),
            "消息不应包含 task name 内容，实际: {}",
            entry.message.content
        );
        assert!(rx.try_recv().is_err());
    }

    // === 滚动撤回策略测试 ===

    #[test]
    fn task_status_rolling_recall() {
        use uuid::Uuid;
        let (fe, mut rx) = make_frontend(FrontendKind::Telegram);
        let task_id: TaskId = Uuid::nil();

        // 第一条状态消息（Pending→Running）
        fe.push_event(EngineEvent::TaskStatusChanged {
            target: EventTarget::Directed(vec![ChannelId {
                frontend: FrontendKind::Telegram,
                user_id: "u1".to_string(),
                thread_id: None,
            }]),
            task_id,
            name: "task".to_string(),
            status: TaskStatusKind::Running,
            old_status: Some(TaskStatusKind::Pending),
            result: None,
            parent_id: None,
            origin_channel: None,
            agent_name: None,
            waiting_reason: None,
        });
        let entry1 = rx.try_recv().expect("first status msg");
        assert_eq!(entry1.message.message_kind, MessageKind::TaskStatus);
        assert!(rx.try_recv().is_err(), "no recall for first status");

        // 模拟 on_sent 回调，更新 last_status_msg
        (entry1.on_sent.unwrap())(Some("msg_1".to_string()));

        // 第二条状态消息（Running→Waiting）—— 应先发 Recall，再发新状态
        fe.push_event(EngineEvent::TaskStatusChanged {
            target: EventTarget::Directed(vec![ChannelId {
                frontend: FrontendKind::Telegram,
                user_id: "u1".to_string(),
                thread_id: None,
            }]),
            task_id,
            name: "task".to_string(),
            status: TaskStatusKind::Waiting,
            old_status: Some(TaskStatusKind::Running),
            result: None,
            parent_id: None,
            origin_channel: None,
            agent_name: None,
            waiting_reason: None,
        });
        let recall_entry = rx.try_recv().expect("recall msg");
        assert_eq!(recall_entry.message.message_kind, MessageKind::Recall);
        assert_eq!(recall_entry.message.content, "msg_1");
        let status_entry = rx.try_recv().expect("new status msg");
        assert_eq!(status_entry.message.message_kind, MessageKind::TaskStatus);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn llm_reply_recalls_last_status() {
        use uuid::Uuid;
        let (fe, mut rx) = make_frontend(FrontendKind::Telegram);
        let task_id: TaskId = Uuid::nil();

        // 发送状态消息
        fe.push_event(EngineEvent::TaskStatusChanged {
            target: EventTarget::Directed(vec![ChannelId {
                frontend: FrontendKind::Telegram,
                user_id: "u1".to_string(),
                thread_id: None,
            }]),
            task_id,
            name: "task".to_string(),
            status: TaskStatusKind::Done,
            old_status: Some(TaskStatusKind::Running),
            result: None,
            parent_id: None,
            origin_channel: None,
            agent_name: None,
            waiting_reason: None,
        });
        let status_entry = rx.try_recv().expect("status msg");
        (status_entry.on_sent.unwrap())(Some("msg_final".to_string()));

        // LLM 回复到达 —— 应先发 Recall，再发 LLMReply
        fe.push_event(EngineEvent::Text {
            target: EventTarget::Directed(vec![ChannelId {
                frontend: FrontendKind::Telegram,
                user_id: "u1".to_string(),
                thread_id: None,
            }]),
            role: MessageRole::Agent,
            content: "done".to_string(),
            task_id: Some(task_id),
        });
        let recall_entry = rx.try_recv().expect("recall msg");
        assert_eq!(recall_entry.message.message_kind, MessageKind::Recall);
        assert_eq!(recall_entry.message.content, "msg_final");
        let llm_entry = rx.try_recv().expect("llm reply");
        assert_eq!(llm_entry.message.message_kind, MessageKind::LLMReply);
    }

    #[test]
    fn task_failed_preserves_final_status() {
        use uuid::Uuid;
        let (fe, mut rx) = make_frontend(FrontendKind::Telegram);
        let task_id: TaskId = Uuid::nil();

        // 发送 Running 状态消息
        fe.push_event(EngineEvent::TaskStatusChanged {
            target: EventTarget::Directed(vec![ChannelId {
                frontend: FrontendKind::Telegram,
                user_id: "u1".to_string(),
                thread_id: None,
            }]),
            task_id,
            name: "task".to_string(),
            status: TaskStatusKind::Running,
            old_status: Some(TaskStatusKind::Pending),
            result: None,
            parent_id: None,
            origin_channel: None,
            agent_name: None,
            waiting_reason: None,
        });
        let status_entry = rx.try_recv().expect("status msg");
        (status_entry.on_sent.unwrap())(Some("msg_running".to_string()));

        // Failed 状态 —— 不应撤回 Running
        fe.push_event(EngineEvent::TaskStatusChanged {
            target: EventTarget::Directed(vec![ChannelId {
                frontend: FrontendKind::Telegram,
                user_id: "u1".to_string(),
                thread_id: None,
            }]),
            task_id,
            name: "task".to_string(),
            status: TaskStatusKind::Failed,
            old_status: Some(TaskStatusKind::Running),
            result: None,
            parent_id: None,
            origin_channel: None,
            agent_name: None,
            waiting_reason: None,
        });
        // 只应有 Failed 状态消息，没有 Recall
        let failed_entry = rx.try_recv().expect("failed status msg");
        assert_eq!(failed_entry.message.message_kind, MessageKind::TaskStatus);
        assert!(failed_entry.message.content.contains("已失败"));
        assert!(rx.try_recv().is_err(), "no recall for Failed status");
    }

    #[test]
    fn task_cleared_cleans_up_state() {
        use uuid::Uuid;
        let (fe, mut rx) = make_frontend(FrontendKind::Telegram);
        let task_id: TaskId = Uuid::nil();

        // 发送状态消息
        fe.push_event(EngineEvent::TaskStatusChanged {
            target: EventTarget::Directed(vec![ChannelId {
                frontend: FrontendKind::Telegram,
                user_id: "u1".to_string(),
                thread_id: None,
            }]),
            task_id,
            name: "task".to_string(),
            status: TaskStatusKind::Running,
            old_status: Some(TaskStatusKind::Pending),
            result: None,
            parent_id: None,
            origin_channel: None,
            agent_name: None,
            waiting_reason: None,
        });
        let status_entry = rx.try_recv().expect("status msg");
        (status_entry.on_sent.unwrap())(Some("msg_1".to_string()));

        // TaskCleared
        fe.push_event(EngineEvent::TaskCleared {
            target: EventTarget::Directed(vec![ChannelId {
                frontend: FrontendKind::Telegram,
                user_id: "u1".to_string(),
                thread_id: None,
            }]),
            task_id,
        });
        assert!(
            rx.try_recv().is_err(),
            "TaskCleared should not produce outbound"
        );

        // 再次发送同 task 的状态消息 —— 不应触发撤回（状态已清理）
        fe.push_event(EngineEvent::TaskStatusChanged {
            target: EventTarget::Directed(vec![ChannelId {
                frontend: FrontendKind::Telegram,
                user_id: "u1".to_string(),
                thread_id: None,
            }]),
            task_id,
            name: "task".to_string(),
            status: TaskStatusKind::Running,
            old_status: Some(TaskStatusKind::Pending),
            result: None,
            parent_id: None,
            origin_channel: None,
            agent_name: None,
            waiting_reason: None,
        });
        let new_status = rx.try_recv().expect("new status msg");
        assert_eq!(new_status.message.message_kind, MessageKind::TaskStatus);
        assert!(rx.try_recv().is_err(), "no recall after TaskCleared");
    }

    // === pending_reply_recall 竞态修复测试 ===

    #[test]
    fn normal_status_transition_does_not_recall_new_msg() {
        use uuid::Uuid;
        let (fe, mut rx) = make_frontend(FrontendKind::Telegram);
        let task_id: TaskId = Uuid::nil();
        let cid = ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "u1".to_string(),
            thread_id: None,
        };
        let target = EventTarget::Directed(vec![cid]);

        // Running 状态
        fe.push_event(EngineEvent::TaskStatusChanged {
            target: target.clone(),
            task_id,
            name: "task".to_string(),
            status: TaskStatusKind::Running,
            old_status: Some(TaskStatusKind::Pending),
            result: None,
            parent_id: None,
            origin_channel: None,
            agent_name: None,
            waiting_reason: None,
        });
        let running_entry = rx.try_recv().expect("running status");
        assert_eq!(running_entry.message.message_kind, MessageKind::TaskStatus);
        (running_entry.on_sent.unwrap())(Some("msg_1".to_string()));

        // Waiting 状态 — 应 Recall msg_1，但不应对 msg_2 设 pending 标记
        fe.push_event(EngineEvent::TaskStatusChanged {
            target: target.clone(),
            task_id,
            name: "task".to_string(),
            status: TaskStatusKind::Waiting,
            old_status: Some(TaskStatusKind::Running),
            result: None,
            parent_id: None,
            origin_channel: None,
            agent_name: None,
            waiting_reason: None,
        });
        let recall_entry = rx.try_recv().expect("recall msg_1");
        assert_eq!(recall_entry.message.message_kind, MessageKind::Recall);
        assert_eq!(recall_entry.message.content, "msg_1");
        let waiting_entry = rx.try_recv().expect("waiting status");
        assert_eq!(waiting_entry.message.message_kind, MessageKind::TaskStatus);
        // 触发 on_sent — 不应产生 Recall（无 LLMReply 到达 → 无 pending 标记）
        (waiting_entry.on_sent.unwrap())(Some("msg_2".to_string()));
        assert!(
            rx.try_recv().is_err(),
            "no recall after normal on_sent without LLMReply"
        );

        // Done 状态 — 同理
        fe.push_event(EngineEvent::TaskStatusChanged {
            target,
            task_id,
            name: "task".to_string(),
            status: TaskStatusKind::Done,
            old_status: Some(TaskStatusKind::Waiting),
            result: None,
            parent_id: None,
            origin_channel: None,
            agent_name: None,
            waiting_reason: None,
        });
        let recall_entry2 = rx.try_recv().expect("recall msg_2");
        assert_eq!(recall_entry2.message.message_kind, MessageKind::Recall);
        assert_eq!(recall_entry2.message.content, "msg_2");
        let done_entry = rx.try_recv().expect("done status");
        assert_eq!(done_entry.message.message_kind, MessageKind::TaskStatus);
        (done_entry.on_sent.unwrap())(Some("msg_3".to_string()));
        assert!(
            rx.try_recv().is_err(),
            "no recall after normal on_sent without LLMReply"
        );
    }

    #[test]
    fn llm_reply_recalls_pending_status() {
        use uuid::Uuid;
        let (fe, mut rx) = make_frontend(FrontendKind::Telegram);
        let task_id: TaskId = Uuid::nil();
        let cid = ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "u1".to_string(),
            thread_id: None,
        };
        let target = EventTarget::Directed(vec![cid]);

        // Running 状态
        fe.push_event(EngineEvent::TaskStatusChanged {
            target: target.clone(),
            task_id,
            name: "task".to_string(),
            status: TaskStatusKind::Running,
            old_status: Some(TaskStatusKind::Pending),
            result: None,
            parent_id: None,
            origin_channel: None,
            agent_name: None,
            waiting_reason: None,
        });
        let entry1 = rx.try_recv().expect("running status");
        (entry1.on_sent.unwrap())(Some("msg_1".to_string()));

        // Waiting 状态 — Recall msg_1，发送 msg_2（on_sent 尚未执行）
        fe.push_event(EngineEvent::TaskStatusChanged {
            target: target.clone(),
            task_id,
            name: "task".to_string(),
            status: TaskStatusKind::Waiting,
            old_status: Some(TaskStatusKind::Running),
            result: None,
            parent_id: None,
            origin_channel: None,
            agent_name: None,
            waiting_reason: None,
        });
        let _recall = rx.try_recv().expect("recall msg_1");
        let status_entry = rx.try_recv().expect("waiting status");
        // ⚠️ 故意不执行 on_sent

        // LLMReply 到达 — last_status_msg 为空 → 应设置 pending 标记
        fe.push_event(EngineEvent::Text {
            target: target.clone(),
            role: MessageRole::Agent,
            content: "done".to_string(),
            task_id: Some(task_id),
        });
        let llm_entry = rx.try_recv().expect("llm reply");
        assert_eq!(llm_entry.message.message_kind, MessageKind::LLMReply);
        // 无即时 Recall（last_status_msg 为空）

        // on_sent 稍后执行 → 应入队 Recall(msg_2)
        (status_entry.on_sent.unwrap())(Some("msg_2".to_string()));
        let deferred_recall = rx.try_recv().expect("deferred recall from on_sent");
        assert_eq!(deferred_recall.message.message_kind, MessageKind::Recall);
        assert_eq!(deferred_recall.message.content, "msg_2");
        assert!(rx.try_recv().is_err(), "no more messages");
    }

    #[test]
    fn pending_recall_cleaned_on_task_cleared() {
        use uuid::Uuid;
        let (fe, mut rx) = make_frontend(FrontendKind::Telegram);
        let task_id: TaskId = Uuid::nil();
        let cid = ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "u1".to_string(),
            thread_id: None,
        };
        let target = EventTarget::Directed(vec![cid]);

        // Running 状态
        fe.push_event(EngineEvent::TaskStatusChanged {
            target: target.clone(),
            task_id,
            name: "task".to_string(),
            status: TaskStatusKind::Running,
            old_status: Some(TaskStatusKind::Pending),
            result: None,
            parent_id: None,
            origin_channel: None,
            agent_name: None,
            waiting_reason: None,
        });
        let entry1 = rx.try_recv().expect("running status");
        (entry1.on_sent.unwrap())(Some("msg_1".to_string()));

        // Waiting 状态（on_sent 未执行）
        fe.push_event(EngineEvent::TaskStatusChanged {
            target: target.clone(),
            task_id,
            name: "task".to_string(),
            status: TaskStatusKind::Waiting,
            old_status: Some(TaskStatusKind::Running),
            result: None,
            parent_id: None,
            origin_channel: None,
            agent_name: None,
            waiting_reason: None,
        });
        let _recall = rx.try_recv().expect("recall msg_1");
        let status_entry = rx.try_recv().expect("waiting status");

        // LLMReply 到达 → 设置 pending 标记
        fe.push_event(EngineEvent::Text {
            target: target.clone(),
            role: MessageRole::Agent,
            content: "done".to_string(),
            task_id: Some(task_id),
        });
        let _llm = rx.try_recv().expect("llm reply");

        // TaskCleared — 应清理 pending_reply_recall
        fe.push_event(EngineEvent::TaskCleared { target, task_id });
        assert!(rx.try_recv().is_err(), "TaskCleared produces no outbound");

        // on_sent 执行 — 不应触发 Recall（标记已被清理）
        (status_entry.on_sent.unwrap())(Some("msg_2".to_string()));
        assert!(rx.try_recv().is_err(), "recall should not fire after clear");
    }

    #[test]
    fn pending_recall_not_set_if_last_status_exists() {
        use uuid::Uuid;
        let (fe, mut rx) = make_frontend(FrontendKind::Telegram);
        let task_id: TaskId = Uuid::nil();
        let cid = ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "u1".to_string(),
            thread_id: None,
        };
        let target = EventTarget::Directed(vec![cid]);

        // Running 状态 → on_sent 已执行
        fe.push_event(EngineEvent::TaskStatusChanged {
            target: target.clone(),
            task_id,
            name: "task".to_string(),
            status: TaskStatusKind::Running,
            old_status: Some(TaskStatusKind::Pending),
            result: None,
            parent_id: None,
            origin_channel: None,
            agent_name: None,
            waiting_reason: None,
        });
        let entry1 = rx.try_recv().expect("running status");
        (entry1.on_sent.unwrap())(Some("msg_1".to_string()));

        // Waiting 状态 → on_sent 已执行
        fe.push_event(EngineEvent::TaskStatusChanged {
            target: target.clone(),
            task_id,
            name: "task".to_string(),
            status: TaskStatusKind::Waiting,
            old_status: Some(TaskStatusKind::Running),
            result: None,
            parent_id: None,
            origin_channel: None,
            agent_name: None,
            waiting_reason: None,
        });
        let _recall = rx.try_recv().expect("recall msg_1");
        let status_entry = rx.try_recv().expect("waiting status");
        (status_entry.on_sent.unwrap())(Some("msg_2".to_string()));

        // LLMReply 到达 — last_status_msg 有值 → 正常 Recall，不设 pending 标记
        fe.push_event(EngineEvent::Text {
            target,
            role: MessageRole::Agent,
            content: "done".to_string(),
            task_id: Some(task_id),
        });
        let recall_entry = rx.try_recv().expect("recall msg_2");
        assert_eq!(recall_entry.message.message_kind, MessageKind::Recall);
        assert_eq!(recall_entry.message.content, "msg_2");
        let llm_entry = rx.try_recv().expect("llm reply");
        assert_eq!(llm_entry.message.message_kind, MessageKind::LLMReply);
        assert!(rx.try_recv().is_err(), "no more messages");
    }
}
