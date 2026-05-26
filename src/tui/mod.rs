pub mod app;
pub mod chat;
pub mod input;
pub mod status;

use crossbeam_channel::{Receiver, Sender};
use tracing::{debug, trace};

use crate::domain::{ChannelId, EngineEvent, EventTarget, Frontend, FrontendKind, UserAction};

pub use app::App;

/// TUI 前端实现
pub struct TuiFrontend {
    user_id: String,
    event_tx: Sender<EngineEvent>,
    action_rx: Receiver<UserAction>,
}

impl TuiFrontend {
    pub fn new(event_tx: Sender<EngineEvent>, action_rx: Receiver<UserAction>) -> Self {
        debug!(
            event = "TuiFrontendCreated",
            "TUI frontend channel created"
        );
        Self {
            user_id: "default".to_string(),
            event_tx,
            action_rx,
        }
    }

    fn my_channels(&self) -> Vec<ChannelId> {
        vec![ChannelId {
            frontend: FrontendKind::Tui,
            user_id: self.user_id.clone(),
        }]
    }
}

impl Frontend for TuiFrontend {
    fn kind(&self) -> FrontendKind {
        FrontendKind::Tui
    }

    fn push_event(&self, event: EngineEvent) {
        let my_channels = self.my_channels();
        let for_me = match event.target() {
            EventTarget::Broadcast => true,
            EventTarget::Directed(targets) => targets.iter().any(|t| my_channels.contains(t)),
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
