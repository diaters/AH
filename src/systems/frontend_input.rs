use crate::prelude::*;
use tracing::debug;

use crate::{
    app::FrontendRegistry,
    domain::{Signal, ToolConfirmationResponseMessage, UserAction},
};

/// 从前端拉取用户动作，转为 ECS 内部消息
pub(crate) fn frontend_input_system(registry: Res<FrontendRegistry>, mut commands: Commands) {
    for frontend in &registry.frontends {
        for action in frontend.poll_actions() {
            match action {
                UserAction::Text { channel, content } => {
                    debug!(
                        event = "FrontendInputText",
                        content_len = content.len(),
                        "received text from frontend"
                    );
                    commands.spawn(Signal::user_input_with_channel(content, channel));
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
                    });
                }
            }
        }
    }
}
