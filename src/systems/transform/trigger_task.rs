//! 事件触发任务路由 System
//!
//! 消费 `TriggerTaskMessage`，根据 `SignalTriggerRegistry` 中注册的路由，
//! 把事件触发转换为 `CreateTaskMessage`，使事件任务进入与普通用户输入相同的任务创建链路。

use crate::prelude::*;
use tracing::{debug, warn};

use crate::domain::{
    CreateTaskMessage, SignalTriggerRegistry, TaskRoutingPolicy, TriggerTaskMessage,
};

/// 将事件触发消息路由为 `CreateTaskMessage`。
///
/// - 未注册的触发器会被丢弃并记录结构化日志
/// - `build_task_input` 失败的触发器会被丢弃
/// - 成功路由后产出 `CreateTaskMessage`，`origin_channel` 为 `None`，
///   `routing_policy` 使用 `TaskRoutingPolicy::event`，由路由配置提供审批通道与上下文
pub fn trigger_task_routing_system(
    mut commands: Commands,
    registry: Res<SignalTriggerRegistry>,
    messages: Query<(Entity, &TriggerTaskMessage)>,
) {
    for (entity, message) in &messages {
        let Some(route) = registry.route(&message.trigger) else {
            warn!(
                event = "SignalTriggerRouteMissing",
                source = %message.source.0,
                trigger = ?message.trigger,
                "dropping unregistered signal trigger"
            );
            commands.entity(entity).despawn();
            continue;
        };

        let Ok(content) = route.build_task_input(&message.trigger) else {
            warn!(
                event = "SignalTriggerPromptBuildFailed",
                source = %message.source.0,
                trigger = ?message.trigger,
                "dropping signal trigger after prompt build failure"
            );
            commands.entity(entity).despawn();
            continue;
        };

        let approval_context = route.build_approval_context(&message.trigger);
        debug!(
            event = "SignalTriggerMatched",
            source = %message.source.0,
            trigger = ?message.trigger,
            content_len = content.len(),
            "signal trigger routed to CreateTaskMessage"
        );

        commands.spawn(CreateTaskMessage {
            content,
            origin_channel: None,
            routing_policy: TaskRoutingPolicy::event(
                route.approval_channel.clone(),
                Some(approval_context),
            ),
        });
        commands.entity(entity).despawn();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{ChannelId, EventTaskRoute, FrontendKind, SignalSource, TaskTrigger};

    #[test]
    fn registered_webhook_route_creates_create_task_message() {
        let mut app = App::new();
        let mut registry = SignalTriggerRegistry::default();
        registry.register_webhook(
            "github.issue_opened",
            EventTaskRoute {
                prompt_template: "请分析这个 issue".to_string(),
                approval_channel: Some(ChannelId {
                    frontend: FrontendKind::Telegram,
                    user_id: "reviewer".to_string(),
                    thread_id: None,
                }),
                approval_context: "GitHub issue opened".to_string(),
            },
        );
        app.insert_resource(registry);
        app.add_systems(Update, trigger_task_routing_system);
        app.world_mut().spawn(TriggerTaskMessage {
            source: SignalSource("external:test".to_string()),
            trigger: TaskTrigger::Webhook {
                kind: "github.issue_opened".to_string(),
                body: serde_json::json!({"title": "bug"}),
            },
        });
        app.update();
        let mut query = app.world_mut().query::<&CreateTaskMessage>();
        let messages: Vec<_> = query.iter(app.world()).collect();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content, "请分析这个 issue");
        assert_eq!(messages[0].origin_channel, None);
        assert_eq!(
            messages[0].routing_policy.approval_context.as_deref(),
            Some("GitHub issue opened")
        );
    }

    #[test]
    fn unregistered_timer_route_is_dropped() {
        let mut app = App::new();
        app.insert_resource(SignalTriggerRegistry::default());
        app.add_systems(Update, trigger_task_routing_system);
        app.world_mut().spawn(TriggerTaskMessage {
            source: SignalSource("scheduler:test".to_string()),
            trigger: TaskTrigger::Timer {
                kind: "nightly".to_string(),
            },
        });
        app.update();
        let mut query = app.world_mut().query::<&CreateTaskMessage>();
        assert_eq!(query.iter(app.world()).count(), 0);
    }
}
