use bevy::prelude::*;
use tracing::warn;

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
        if let Err(error) = sender.0.send(OutputMessage::new(output.content.clone())) {
            warn!(?error, "failed to forward output to external channel");
        }

        commands.entity(entity).despawn();
    }
}
