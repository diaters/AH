use tokio::sync::mpsc::UnboundedSender;
use tracing::{error, trace};

use crate::domain::{
    ChannelId, EngineEvent, EventTarget, Frontend, FrontendKind, MessageRole, TaskId,
    TaskStatusKind, UserAction,
};

use super::ChannelOutboundMessage;
use super::traits::{ChannelParseMode, InlineKeyboardButton, ReplyMarkup};

/// 将 EngineEvent 路由到对应 IM 通道出向发送队列的 Frontend 实现。
pub struct ChannelFrontend {
    kind: FrontendKind,
    channel_name: String,
    outbound_tx: UnboundedSender<(String, ChannelOutboundMessage)>,
}

impl ChannelFrontend {
    pub fn new(
        kind: FrontendKind,
        channel_name: impl Into<String>,
        outbound_tx: UnboundedSender<(String, ChannelOutboundMessage)>,
    ) -> Self {
        Self {
            kind,
            channel_name: channel_name.into(),
            outbound_tx,
        }
    }

    fn matches(&self, channel_id: &ChannelId) -> bool {
        channel_id.frontend == self.kind
    }

    fn send_message(&self, msg: ChannelOutboundMessage) {
        if let Err(e) = self.outbound_tx.send((self.channel_name.clone(), msg)) {
            error!(event = "ChannelFrontendSendFailed", error = %e, channel = %self.channel_name);
        }
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
                for channel_id in recipients {
                    let msg = ChannelOutboundMessage {
                        recipient: channel_id.user_id,
                        thread_id: channel_id.thread_id,
                        content: prefixed_content.clone(),
                        parse_mode: None,
                        reply_markup: None,
                        attachments: vec![],
                    };
                    self.send_message(msg);
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
                    };
                    self.send_message(msg);
                }
            }
            EngineEvent::TaskStatusChanged {
                target,
                task_id,
                name,
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
                // task_name 截断到 30 字符（按字符截断，UTF-8 安全）
                let short_name: String = name.chars().take(30).collect();
                let transition = match old_status {
                    Some(old) => {
                        format!("{} → {}", status_label(old), status_label(status))
                    }
                    None => status_label(status).to_string(),
                };
                let content = match agent_name.as_deref() {
                    Some(agent) => format!(
                        "[{}] {}: {} @{}",
                        task_short_id(task_id),
                        short_name,
                        transition,
                        agent
                    ),
                    None => format!(
                        "[{}] {}: {}",
                        task_short_id(task_id),
                        short_name,
                        transition
                    ),
                };
                for channel_id in recipients {
                    let msg = ChannelOutboundMessage {
                        recipient: channel_id.user_id,
                        thread_id: channel_id.thread_id,
                        content: content.clone(),
                        parse_mode: None,
                        reply_markup: None,
                        attachments: vec![],
                    };
                    self.send_message(msg);
                }
            }
            EngineEvent::ToolCallStarted {
                target,
                task_id,
                agent_name,
                tool_name,
                tool_input_summary,
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
                let content = if tool_input_summary.is_empty() {
                    format!(
                        "[{}] 🔧 {} 调用 {}",
                        task_short_id(task_id),
                        agent_name,
                        tool_name
                    )
                } else {
                    format!(
                        "[{}] 🔧 {} 调用 {}: {}",
                        task_short_id(task_id),
                        agent_name,
                        tool_name,
                        tool_input_summary
                    )
                };
                for channel_id in recipients {
                    let msg = ChannelOutboundMessage {
                        recipient: channel_id.user_id,
                        thread_id: channel_id.thread_id,
                        content: content.clone(),
                        parse_mode: None,
                        reply_markup: None,
                        attachments: vec![],
                    };
                    self.send_message(msg);
                }
            }
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
    use tokio::sync::mpsc;

    fn make_frontend(
        kind: FrontendKind,
    ) -> (
        ChannelFrontend,
        mpsc::UnboundedReceiver<(String, ChannelOutboundMessage)>,
    ) {
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
        let (_, msg) = rx.try_recv().expect("one outbound message");
        assert_eq!(msg.content, "[a1b2c3d4] 助手: hello");
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
        let (_, msg) = rx.try_recv().expect("one outbound message");
        assert_eq!(msg.content, "hello");
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
        let (_, msg) = rx.try_recv().expect("one outbound message");
        assert_eq!(msg.content, "[a1b2c3d4] 系统: summary done");
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
        let (_, msg) = rx.try_recv().expect("one outbound message");
        assert_eq!(msg.content, "[a1b2c3d4] test: 运行中 → 已完成");
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
        let (name, msg) = rx.try_recv().expect("one outbound message");
        assert_eq!(name, "test");
        assert_eq!(msg.recipient, "u1");
        assert_eq!(msg.content, "hello");
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn approval_request_escapes_html_in_tool_input() {
        use crate::domain::ApprovalOption;
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
        let (_, msg) = rx.try_recv().expect("one outbound message");
        assert!(matches!(msg.parse_mode, Some(ChannelParseMode::Html)));
        assert!(
            msg.content.contains("&lt;script&gt; &amp; text"),
            "HTML special chars should be escaped: {}",
            msg.content
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
        let (_, msg) = rx.try_recv().expect("one outbound message");
        assert_eq!(msg.content, "[00000000] 助手: hello");
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
        let (_, msg) = rx.try_recv().expect("one outbound message");
        assert_eq!(msg.content, "[00000000] task: 运行中 → 已完成");
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
        let (_, msg) = rx.try_recv().expect("one outbound message");
        assert_eq!(msg.content, "[00000000] task: 运行中");
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
    fn renders_tool_call_started() {
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
        let (_, msg) = rx.try_recv().expect("one outbound message");
        assert_eq!(
            msg.content,
            "[a1b2c3d4] 🔧 TestAgent 调用 shell_exec: ls -la"
        );
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn renders_tool_call_started_without_summary() {
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
            tool_input_summary: String::new(),
        });
        let (_, msg) = rx.try_recv().expect("one outbound message");
        assert_eq!(msg.content, "[a1b2c3d4] 🔧 TestAgent 调用 shell_exec");
        assert!(rx.try_recv().is_err());
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
        let (_, msg) = rx.try_recv().expect("one outbound message");
        assert_eq!(
            msg.content,
            "[a1b2c3d4] build feature: 运行中 → 已完成 @TestAgent"
        );
        assert!(rx.try_recv().is_err());
    }
}
