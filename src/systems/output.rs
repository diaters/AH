use bevy::prelude::*;
use tracing::{debug, warn};

use crate::{
    app::OutputSender,
    domain::{OutputMessage, UserOutputMessage},
};

/// 将用户可见输出发往外部线程。
pub(crate) fn user_output_system(
    sender: Res<OutputSender>,
    mut commands: Commands,
    outputs: Query<(Entity, &UserOutputMessage)>,
) {
    for (entity, output) in &outputs {
        debug!(
            event = "UserOutputSent",
            content_len = output.content.len(),
            content = %output.content,
            "sending output to external channel"
        );

        if let Err(error) = sender.0.send(OutputMessage::new(output.content.clone())) {
            warn!(
                event = "OutputSendFailed",
                error = %error,
                content_len = output.content.len(),
                "failed to forward output to external channel"
            );
        }

        commands.entity(entity).despawn();
    }
}
