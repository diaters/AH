use std::sync::{Arc, Mutex};

use harness::{
    ChannelId, EngineEvent, EventTarget, Frontend, FrontendKind, MessageRole, UserAction,
};

struct SpyFrontend {
    kind_val: FrontendKind,
    user_id: String,
    events: Mutex<Vec<EngineEvent>>,
    actions: Mutex<Vec<UserAction>>,
}

impl SpyFrontend {
    fn new(kind_val: FrontendKind) -> Self {
        Self {
            kind_val,
            user_id: "default".to_string(),
            events: Mutex::new(Vec::new()),
            actions: Mutex::new(Vec::new()),
        }
    }

    fn new_with_user(kind_val: FrontendKind, user_id: &str) -> Self {
        Self {
            kind_val,
            user_id: user_id.to_string(),
            events: Mutex::new(Vec::new()),
            actions: Mutex::new(Vec::new()),
        }
    }

    fn received_events(&self) -> Vec<EngineEvent> {
        self.events.lock().unwrap().clone()
    }

    fn push_action(&self, action: UserAction) {
        self.actions.lock().unwrap().push(action);
    }
}

impl Frontend for SpyFrontend {
    fn kind(&self) -> FrontendKind {
        self.kind_val.clone()
    }

    fn push_event(&self, event: EngineEvent) {
        let my_channels = vec![ChannelId {
            frontend: self.kind_val.clone(),
            user_id: self.user_id.clone(),
        }];
        let for_me = match event.target() {
            EventTarget::Broadcast => true,
            EventTarget::Directed(targets) => {
                targets.iter().any(|t| my_channels.contains(t))
            }
        };
        if for_me {
            self.events.lock().unwrap().push(event);
        }
    }

    fn poll_actions(&self) -> Vec<UserAction> {
        std::mem::take(&mut *self.actions.lock().unwrap())
    }
}

#[test]
fn directed_event_only_reaches_target_frontend() {
    let tui = Arc::new(SpyFrontend::new(FrontendKind::Tui));
    let telegram = Arc::new(SpyFrontend::new(FrontendKind::Telegram));

    let event = EngineEvent::Text {
        target: EventTarget::Directed(vec![ChannelId {
            frontend: FrontendKind::Tui,
            user_id: "default".to_string(),
        }]),
        role: MessageRole::Agent,
        content: "hello".to_string(),
    };

    tui.push_event(event.clone());
    telegram.push_event(event.clone());

    assert_eq!(tui.received_events().len(), 1);
    assert_eq!(telegram.received_events().len(), 0);
}

#[test]
fn broadcast_event_reaches_all_frontends() {
    let tui = Arc::new(SpyFrontend::new(FrontendKind::Tui));
    let telegram = Arc::new(SpyFrontend::new(FrontendKind::Telegram));

    let event = EngineEvent::Text {
        target: EventTarget::Broadcast,
        role: MessageRole::Agent,
        content: "hello".to_string(),
    };

    tui.push_event(event.clone());
    telegram.push_event(event.clone());

    assert_eq!(tui.received_events().len(), 1);
    assert_eq!(telegram.received_events().len(), 1);
}

#[test]
fn multi_directed_event_reaches_specified_frontends() {
    let tui = Arc::new(SpyFrontend::new(FrontendKind::Tui));
    // Create telegram frontend with the specific user_id that will be targeted
    let telegram = Arc::new(SpyFrontend::new_with_user(FrontendKind::Telegram, "chat_123"));

    let event = EngineEvent::Text {
        target: EventTarget::Directed(vec![
            ChannelId {
                frontend: FrontendKind::Tui,
                user_id: "default".to_string(),
            },
            ChannelId {
                frontend: FrontendKind::Telegram,
                user_id: "chat_123".to_string(),
            },
        ]),
        role: MessageRole::Agent,
        content: "hello".to_string(),
    };

    tui.push_event(event.clone());
    telegram.push_event(event.clone());

    assert_eq!(tui.received_events().len(), 1);
    assert_eq!(telegram.received_events().len(), 1);
}

#[test]
fn poll_actions_returns_queued_actions() {
    let spy = Arc::new(SpyFrontend::new(FrontendKind::Tui));

    spy.push_action(UserAction::Text {
        channel: ChannelId {
            frontend: FrontendKind::Tui,
            user_id: "default".to_string(),
        },
        content: "hello".to_string(),
    });

    let actions = spy.poll_actions();
    assert_eq!(actions.len(), 1);
    assert!(spy.poll_actions().is_empty());
}
