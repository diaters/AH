pub mod app;
pub mod chat;
pub mod input;
pub mod status;

use crossbeam_channel::{Receiver, Sender};
use tracing::{debug, trace};

use crate::domain::{EngineEvent, EventTarget, Frontend, FrontendKind, UserAction};

pub use app::App;

/// TUI 前端实现
pub struct TuiFrontend {
    user_id: String,
    event_tx: Sender<EngineEvent>,
    action_rx: Receiver<UserAction>,
}

impl TuiFrontend {
    pub fn new(event_tx: Sender<EngineEvent>, action_rx: Receiver<UserAction>) -> Self {
        debug!(event = "TuiFrontendCreated", "TUI frontend channel created");
        Self {
            user_id: "default".to_string(),
            event_tx,
            action_rx,
        }
    }
}

impl Frontend for TuiFrontend {
    fn kind(&self) -> FrontendKind {
        FrontendKind::Tui
    }

    fn push_event(&self, event: EngineEvent) {
        // TaskStatusChanged 与 ToolCallStarted 始终接收（全局任务概览）
        let for_me = matches!(
            event,
            EngineEvent::TaskStatusChanged { .. } | EngineEvent::ToolCallStarted { .. }
        ) || match event.target() {
            EventTarget::Broadcast => true,
            EventTarget::Directed(targets) => targets
                .iter()
                .any(|t| t.frontend == FrontendKind::Tui && t.user_id == self.user_id),
        };
        if for_me {
            debug!(
                event = "TuiFrontendPushEvent",
                event_kind = ?event,
                "pushing engine event to TUI channel"
            );
            let _ = self.event_tx.send(event);
        } else {
            trace!(
                event = "TuiFrontendEventSkipped",
                "engine event not for this frontend, skipping"
            );
        }
    }

    fn poll_actions(&self) -> Vec<UserAction> {
        let mut actions = Vec::new();
        while let Ok(action) = self.action_rx.try_recv() {
            debug!(
                event = "TuiFrontendActionPolled",
                action_kind = ?action,
                "polled user action from TUI channel"
            );
            actions.push(action);
        }
        actions
    }
}

#[cfg(test)]
mod tests {
    use crossbeam_channel::unbounded;

    use crate::domain::{ChannelId, Frontend, MessageRole, TaskStatusKind};

    use super::*;

    #[test]
    fn tui_accepts_task_status_from_other_channels() {
        let (event_tx, event_rx) = unbounded();
        let (_action_tx, action_rx) = unbounded();
        let frontend = TuiFrontend::new(event_tx, action_rx);

        // QQ 通道的 TaskStatusChanged 事件
        let qq_channel = ChannelId {
            frontend: FrontendKind::QQ,
            user_id: "qq_user".to_string(),
            thread_id: None,
        };
        frontend.push_event(EngineEvent::TaskStatusChanged {
            target: EventTarget::Directed(vec![qq_channel]),
            task_id: crate::domain::TaskId::new(),
            name: "qq task".to_string(),
            status: TaskStatusKind::Running,
            old_status: None,
            result: None,
            parent_id: None,
            origin_channel: Some(ChannelId {
                frontend: FrontendKind::QQ,
                user_id: "qq_user".to_string(),
                thread_id: None,
            }),
            agent_name: None,
            waiting_reason: None,
        });

        let received = event_rx.try_recv();
        assert!(
            received.is_ok(),
            "TUI should accept TaskStatusChanged from QQ channel"
        );
    }

    #[test]
    fn tui_still_filters_text_for_other_channels() {
        let (event_tx, event_rx) = unbounded();
        let (_action_tx, action_rx) = unbounded();
        let frontend = TuiFrontend::new(event_tx, action_rx);

        // QQ 通道的 Text 事件应被过滤
        let qq_channel = ChannelId {
            frontend: FrontendKind::QQ,
            user_id: "qq_user".to_string(),
            thread_id: None,
        };
        frontend.push_event(EngineEvent::Text {
            target: EventTarget::Directed(vec![qq_channel]),
            role: MessageRole::Agent,
            content: "hello".to_string(),
            task_id: None,
        });

        let received = event_rx.try_recv();
        assert!(
            received.is_err(),
            "TUI should filter Text events for other channels"
        );
    }
}
