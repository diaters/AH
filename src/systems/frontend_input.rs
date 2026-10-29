use crate::prelude::*;
use tracing::debug;

use crate::{
    contracts::FrontendRegistry,
    domain::{InputSequencer, Signal, ToolConfirmationResponseMessage, UserAction},
};

/// 从前端拉取用户动作，转为 ECS 内部消息
pub(crate) fn frontend_input_system(
    registry: Res<FrontendRegistry>,
    mut sequence: ResMut<InputSequencer>,
    mut commands: Commands,
) {
    for frontend in &registry.frontends {
        for action in frontend.poll_actions() {
            match action {
                UserAction::Text { channel, content } => {
                    debug!(
                        event = "FrontendInputText",
                        content_len = content.len(),
                        "received text from frontend"
                    );
                    commands.spawn((
                        Signal::user_input_with_channel(content, channel),
                        sequence.next_sequence(),
                    ));
                }
                UserAction::Confirmation {
                    channel: _,
                    request_id,
                    option_id,
                    feedback,
                } => {
                    debug!(
                        event = "FrontendInputConfirmation",
                        request_id = %request_id,
                        option_id = %option_id,
                        "received confirmation from frontend"
                    );
                    commands.spawn(ToolConfirmationResponseMessage {
                        request_id,
                        selected_option: option_id,
                        feedback,
                        // 本地前端（TUI）的操作者视为可信审批者
                        sender: None,
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{ChannelId, EngineEvent, Frontend, FrontendKind, InputSequence};
    use std::sync::Mutex;

    struct QueuedFrontend(Mutex<Vec<UserAction>>);
    impl Frontend for QueuedFrontend {
        fn kind(&self) -> FrontendKind {
            FrontendKind::Tui
        }
        fn push_event(&self, _: EngineEvent) {}
        fn poll_actions(&self) -> Vec<UserAction> {
            std::mem::take(&mut *self.0.lock().unwrap())
        }
    }

    #[test]
    fn frontend_batch_receives_fifo_sequences_before_entity_recycling() {
        let mut app = App::new();
        app.init_resource::<InputSequencer>();
        let channel = ChannelId {
            frontend: FrontendKind::Tui,
            user_id: "default".into(),
            thread_id: None,
        };
        app.insert_resource(FrontendRegistry {
            frontends: vec![Box::new(QueuedFrontend(Mutex::new(
                ["first", "/finish", "second"]
                    .into_iter()
                    .map(|content| UserAction::Text {
                        channel: channel.clone(),
                        content: content.into(),
                    })
                    .collect(),
            )))],
        });
        app.add_systems(Update, frontend_input_system);
        app.update();
        let sequences: Vec<_> = app
            .world_mut()
            .query::<(&Signal, Option<&InputSequence>)>()
            .iter(app.world())
            .map(|(_, seq)| seq.copied())
            .collect();
        assert!(
            sequences.iter().all(Option::is_some),
            "TUI inputs need the same explicit FIFO contract as IM ingress"
        );
        let mut sequences: Vec<_> = sequences.into_iter().map(|seq| seq.unwrap().0).collect();
        sequences.sort();
        assert_eq!(sequences, vec![0, 1, 2]);
    }

    #[test]
    fn frontend_and_external_ingress_share_sequences_across_frames() {
        use crate::domain::{ExternalInput, InputReceiver, ShutdownState, SignalPayload};

        let mut app = App::new();
        app.init_resource::<InputSequencer>();
        app.init_resource::<ShutdownState>();
        let (tx, rx) = crossbeam_channel::unbounded();
        app.insert_resource(InputReceiver(rx));
        app.add_systems(
            Update,
            (frontend_input_system, crate::systems::input_ingress_system).chain(),
        );
        let channel = ChannelId {
            frontend: FrontendKind::Tui,
            user_id: "default".into(),
            thread_id: None,
        };
        for frame in 0..2 {
            app.insert_resource(FrontendRegistry {
                frontends: vec![Box::new(QueuedFrontend(Mutex::new(vec![
                    UserAction::Text {
                        channel: channel.clone(),
                        content: format!("frontend-{frame}"),
                    },
                ])))],
            });
            tx.send(ExternalInput::TextWithChannel {
                sender: None,
                channel: channel.clone(),
                content: format!("external-{frame}"),
            })
            .unwrap();
            app.update();
        }
        let mut received: Vec<_> = app
            .world_mut()
            .query::<(&Signal, &InputSequence)>()
            .iter(app.world())
            .map(|(signal, sequence)| {
                let SignalPayload::UserInput(content) = &signal.payload else {
                    panic!("unexpected signal");
                };
                (sequence.0, content.clone())
            })
            .collect();
        received.sort_by_key(|(sequence, _)| *sequence);
        assert_eq!(
            received,
            vec![
                (0, "frontend-0".to_string()),
                (1, "external-0".to_string()),
                (2, "frontend-1".to_string()),
                (3, "external-1".to_string()),
            ]
        );
    }
}
