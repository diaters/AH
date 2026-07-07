use crate::prelude::*;
use tracing::{debug, warn};

use crate::app::FrontendRegistry;
use crate::domain::{
    Agent, AgentStatusKind, EngineEvent, EventTarget, FailureReason, FrontendKind, MessageRole,
    SystemOutputMessage, Task, TaskStatus, TaskStatusKind, ToolConfirmationRequestMessage,
    UserOutputMessage,
};

/// 将 ECS 状态变化转为 EngineEvent 推送给所有前端
#[allow(clippy::too_many_arguments)]
pub(crate) fn frontend_output_system(
    registry: Res<FrontendRegistry>,
    mut commands: Commands,
    outputs: Query<(Entity, &UserOutputMessage)>,
    system_outputs: Query<(Entity, &SystemOutputMessage)>,
    all_tasks: Query<(Entity, &Task)>,
    tasks: Query<&Task, Changed<Task>>,
    agents: Query<&Agent, Changed<Agent>>,
    confirmations: Query<
        (Entity, &ToolConfirmationRequestMessage),
        Added<ToolConfirmationRequestMessage>,
    >,
) {
    // 用户可见文本输出
    for (entity, output) in &outputs {
        let Some(target) = all_tasks
            .iter()
            .find(|(_, t)| t.id == output.task_id)
            .and_then(|(_, t)| t.routing_policy.output_channel.clone())
            .map(|channel| EventTarget::Directed(vec![channel]))
        else {
            debug!(
                event = "FrontendOutputDroppedNoChannel",
                task_id = %output.task_id,
                "dropping output because task has no output channel"
            );
            commands.entity(entity).despawn();
            continue;
        };
        debug!(
            event = "FrontendOutputText",
            task_id = %output.task_id,
            content_len = output.content.len(),
            "pushing text to frontends"
        );
        let event = EngineEvent::Text {
            target,
            role: MessageRole::Agent,
            content: output.content.clone(),
        };
        for frontend in &registry.frontends {
            frontend.push_event(event.clone());
        }
        commands.entity(entity).despawn();
    }

    // 系统通知输出（不进入 STM，路由到 task 的 output_channel）
    for (entity, output) in &system_outputs {
        let Some(target) = all_tasks
            .iter()
            .find(|(_, t)| t.id == output.task_id)
            .and_then(|(_, t)| t.routing_policy.output_channel.clone())
            .map(|channel| EventTarget::Directed(vec![channel]))
        else {
            debug!(
                event = "FrontendSystemOutputDroppedNoChannel",
                task_id = %output.task_id,
                "dropping system output because task has no output channel"
            );
            commands.entity(entity).despawn();
            continue;
        };

        debug!(
            event = "FrontendSystemOutput",
            task_id = %output.task_id,
            content_len = output.content.len(),
            target = ?target,
            "pushing system notification to frontends"
        );
        let event = EngineEvent::Text {
            target,
            role: MessageRole::System,
            content: output.content.clone(),
        };
        for frontend in &registry.frontends {
            frontend.push_event(event.clone());
        }
        commands.entity(entity).despawn();
    }

    // Task 状态变化
    for task in &tasks {
        let Some(target) = task
            .routing_policy
            .output_channel
            .clone()
            .map(|channel| EventTarget::Directed(vec![channel]))
        else {
            debug!(
                event = "FrontendTaskStatusDroppedNoChannel",
                task_id = %task.id,
                "dropping task status event because task has no output channel"
            );
            continue;
        };
        let status = task_status_to_kind(&task.status);
        let result = if task.status.is_terminal() {
            Some(task.result_summary.clone())
        } else {
            None
        };
        let event = EngineEvent::TaskStatusChanged {
            target,
            task_id: task.id,
            name: task.input_summary.clone(),
            status,
            result,
            parent_id: task.parent_task_id,
        };
        for frontend in &registry.frontends {
            frontend.push_event(event.clone());
        }
    }

    // Agent 状态变化
    for agent in &agents {
        let event = EngineEvent::AgentStatusChanged {
            target: EventTarget::Broadcast,
            agent_id: agent.id,
            name: agent.profile.name.clone(),
            status: AgentStatusKind::Idle,
        };
        for frontend in &registry.frontends {
            frontend.push_event(event.clone());
        }
    }

    // 审批请求
    for (entity, confirmation) in &confirmations {
        // 事件任务的审批必须走路由策略中显式配置的 approval_channel。
        // 普通聊天任务的 approval_channel 与 output_channel 相同，由
        // TaskRoutingPolicy::conversational 构造时设置。
        let Some(approval_channel) = all_tasks
            .iter()
            .find(|(_, t)| t.id == confirmation.task_id)
            .and_then(|(_, t)| t.routing_policy.approval_channel.clone())
        else {
            // 缺少审批通道时，显式标记任务失败，避免任务卡在等待态
            if let Some((task_entity, task)) =
                all_tasks.iter().find(|(_, t)| t.id == confirmation.task_id)
            {
                let mut failed_task = task.clone();
                failed_task.status = TaskStatus::Failed(FailureReason::Unknown);
                failed_task.last_error =
                    Some("missing approval channel for event task approval request".to_string());
                commands.entity(task_entity).insert(failed_task);
            }
            warn!(
                event = "FrontendApprovalRouteMissing",
                task_id = %confirmation.task_id,
                request_id = %confirmation.request_id,
                "marking task failed because approval channel is missing"
            );
            commands.entity(entity).despawn();
            continue;
        };

        if !registry.has_frontend(approval_channel.frontend.clone()) {
            if let Some((task_entity, task)) =
                all_tasks.iter().find(|(_, t)| t.id == confirmation.task_id)
            {
                let frontend_name = match approval_channel.frontend {
                    FrontendKind::Tui => "tui",
                    FrontendKind::Telegram => "telegram",
                    FrontendKind::Web => "web",
                    FrontendKind::QQ => "qq",
                    FrontendKind::Feishu => "feishu",
                };
                let mut failed_task = task.clone();
                failed_task.status = TaskStatus::Failed(FailureReason::Unknown);
                failed_task.last_error = Some(format!(
                    "approval channel frontend '{}' is not enabled",
                    frontend_name
                ));
                commands.entity(task_entity).insert(failed_task);
            }
            warn!(
                event = "FrontendApprovalRouteInvalid",
                task_id = %confirmation.task_id,
                request_id = %confirmation.request_id,
                frontend = ?approval_channel.frontend,
                "marking task failed because approval channel frontend is not enabled"
            );
            commands.entity(entity).despawn();
            continue;
        }

        let target = EventTarget::Directed(vec![approval_channel]);

        debug!(
            event = "FrontendOutputApprovalRequest",
            task_id = %confirmation.task_id,
            agent_id = %confirmation.agent_id,
            request_id = %confirmation.request_id,
            tool_name = %confirmation.tool_name,
            option_count = confirmation.options.len(),
            "pushing approval request to frontends"
        );

        let options: Vec<crate::domain::ApprovalOption> = confirmation
            .options
            .iter()
            .map(|opt| crate::domain::ApprovalOption {
                id: opt.id.clone(),
                label: opt.label.clone(),
                description: if opt.id == "deny" {
                    "拒绝".to_string()
                } else {
                    match opt.mode {
                        crate::domain::GrantMode::Once => "仅本次允许".to_string(),
                        crate::domain::GrantMode::Permanent => "永久允许此工具".to_string(),
                    }
                },
            })
            .collect();

        let event = EngineEvent::ApprovalRequest {
            target,
            request_id: confirmation.request_id,
            agent_name: String::new(),
            tool_name: confirmation.tool_name.clone(),
            tool_input: confirmation.tool_input.clone(),
            options,
            approval_context: confirmation.approval_context.clone(),
        };
        for frontend in &registry.frontends {
            frontend.push_event(event.clone());
        }

        commands.entity(entity).despawn();
    }
}

fn task_status_to_kind(status: &crate::domain::TaskStatus) -> TaskStatusKind {
    match status {
        crate::domain::TaskStatus::Pending => TaskStatusKind::Pending,
        crate::domain::TaskStatus::Ready => TaskStatusKind::Pending,
        crate::domain::TaskStatus::Running => TaskStatusKind::Running,
        crate::domain::TaskStatus::Waiting(_) => TaskStatusKind::Waiting,
        crate::domain::TaskStatus::Done => TaskStatusKind::Done,
        crate::domain::TaskStatus::Failed(_) => TaskStatusKind::Failed,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use crate::prelude::*;
    use uuid::Uuid;

    use crate::app::FrontendRegistry;
    use crate::domain::{
        ChannelId, ConfirmationOption, ConfirmationSource, EngineEvent, EventTarget, Frontend,
        FrontendKind, Task, TaskRoutingPolicy, ToolConfirmationRequestMessage, UserAction,
        UserOutputMessage,
    };

    use super::frontend_output_system;

    struct MockFrontend {
        kind: FrontendKind,
        events: Arc<Mutex<Vec<EngineEvent>>>,
    }

    impl Frontend for MockFrontend {
        fn kind(&self) -> FrontendKind {
            self.kind.clone()
        }

        fn push_event(&self, event: EngineEvent) {
            self.events.lock().unwrap().push(event);
        }

        fn poll_actions(&self) -> Vec<UserAction> {
            vec![]
        }
    }

    #[test]
    fn approval_request_targeted_to_task_origin_channel() {
        let mut app = App::new();
        let events = Arc::new(Mutex::new(Vec::new()));
        let frontend = MockFrontend {
            kind: FrontendKind::Telegram,
            events: events.clone(),
        };
        app.insert_resource(FrontendRegistry {
            frontends: vec![Box::new(frontend)],
        });
        app.add_systems(Update, frontend_output_system);

        let origin_channel = ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "u1".to_string(),
            thread_id: Some("t1".to_string()),
        };
        let task = Task::from_user_input("test", 3, origin_channel.clone());
        let task_id = task.id;
        app.world_mut().spawn(task);

        app.world_mut().spawn(ToolConfirmationRequestMessage {
            request_id: Uuid::new_v4(),
            task_id,
            agent_id: Uuid::nil(),
            tool_name: "shell_exec".to_string(),
            tool_input: serde_json::Value::Null,
            options: ConfirmationOption::default_options(),
            source: ConfirmationSource::User,
            parent_agent_id: None,
            approval_context: None,
        });

        app.update();

        let events = events.lock().unwrap();
        let approval = events
            .iter()
            .find_map(|e| match e {
                EngineEvent::ApprovalRequest { target, .. } => Some(target.clone()),
                _ => None,
            })
            .expect("should emit ApprovalRequest");

        match approval {
            EventTarget::Directed(channels) => {
                assert_eq!(channels, vec![origin_channel]);
            }
            EventTarget::Broadcast => panic!("approval should be routed to task origin channel"),
        }
    }

    #[test]
    fn event_task_approval_routes_to_default_approval_channel() {
        let mut app = App::new();
        let events = Arc::new(Mutex::new(Vec::new()));
        let frontend = MockFrontend {
            kind: FrontendKind::Telegram,
            events: events.clone(),
        };
        app.insert_resource(FrontendRegistry {
            frontends: vec![Box::new(frontend)],
        });
        app.add_systems(Update, frontend_output_system);

        let approval_channel = ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "reviewer".to_string(),
            thread_id: Some("ops".to_string()),
        };
        let task = Task::from_trigger(
            "nightly summary".to_string(),
            3,
            TaskRoutingPolicy::event(
                Some(approval_channel.clone()),
                Some("nightly summary timer".to_string()),
            ),
        );
        let task_id = task.id;
        app.world_mut().spawn(task);

        app.world_mut().spawn(ToolConfirmationRequestMessage {
            request_id: Uuid::new_v4(),
            task_id,
            agent_id: Uuid::nil(),
            tool_name: "shell_exec".to_string(),
            tool_input: serde_json::json!({"command": "date"}),
            options: ConfirmationOption::default_options(),
            source: ConfirmationSource::User,
            parent_agent_id: None,
            approval_context: Some("nightly summary timer".to_string()),
        });

        app.update();

        let events = events.lock().unwrap();
        let approval_target = events
            .iter()
            .find_map(|event| match event {
                EngineEvent::ApprovalRequest { target, .. } => Some(target.clone()),
                _ => None,
            })
            .expect("approval request should be emitted");

        match approval_target {
            EventTarget::Directed(channels) => {
                assert_eq!(channels, vec![approval_channel]);
            }
            EventTarget::Broadcast => panic!("approval should route to configured channel"),
        }
    }

    #[test]
    fn event_task_user_output_is_dropped_when_output_channel_is_none() {
        let mut app = App::new();
        let events = Arc::new(Mutex::new(Vec::new()));
        let frontend = MockFrontend {
            kind: FrontendKind::Telegram,
            events: events.clone(),
        };
        app.insert_resource(FrontendRegistry {
            frontends: vec![Box::new(frontend)],
        });
        app.add_systems(Update, frontend_output_system);

        let task = Task::from_trigger(
            "nightly summary".to_string(),
            3,
            TaskRoutingPolicy::event(None, Some("nightly summary timer".to_string())),
        );
        let task_id = task.id;
        app.world_mut().spawn(task);
        app.world_mut().spawn(UserOutputMessage {
            task_id,
            content: "should not be sent".to_string(),
        });

        app.update();

        let events = events.lock().unwrap();
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, EngineEvent::Text { .. }))
        );
    }

    #[test]
    fn missing_approval_channel_marks_event_task_failed() {
        let mut app = App::new();
        let events = Arc::new(Mutex::new(Vec::new()));
        let frontend = MockFrontend {
            kind: FrontendKind::Telegram,
            events: events.clone(),
        };
        app.insert_resource(FrontendRegistry {
            frontends: vec![Box::new(frontend)],
        });
        app.add_systems(Update, frontend_output_system);

        let task = Task::from_trigger(
            "nightly summary".to_string(),
            3,
            TaskRoutingPolicy::event(None, Some("nightly summary timer".to_string())),
        );
        let task_id = task.id;
        app.world_mut().spawn(task);
        app.world_mut().spawn(ToolConfirmationRequestMessage {
            request_id: Uuid::new_v4(),
            task_id,
            agent_id: Uuid::nil(),
            tool_name: "shell_exec".to_string(),
            tool_input: serde_json::json!({"command": "date"}),
            options: ConfirmationOption::default_options(),
            source: ConfirmationSource::User,
            parent_agent_id: None,
            approval_context: Some("nightly summary timer".to_string()),
        });

        app.update();

        let task = app
            .world_mut()
            .query::<&Task>()
            .iter(app.world())
            .find(|task| task.id == task_id)
            .expect("task should remain for failure inspection");
        assert!(matches!(
            task.status,
            crate::domain::TaskStatus::Failed(crate::domain::FailureReason::Unknown)
        ));
        assert_eq!(
            task.last_error.as_deref(),
            Some("missing approval channel for event task approval request")
        );
    }

    #[test]
    fn approval_request_with_disabled_frontend_marks_task_failed() {
        let mut app = App::new();
        let events = Arc::new(Mutex::new(Vec::new()));
        // 只注册 Telegram frontend，QQ 未注册
        let frontend = MockFrontend {
            kind: FrontendKind::Telegram,
            events: events.clone(),
        };
        app.insert_resource(FrontendRegistry {
            frontends: vec![Box::new(frontend)],
        });
        app.add_systems(Update, frontend_output_system);

        let approval_channel = ChannelId {
            frontend: FrontendKind::QQ,
            user_id: "reviewer".to_string(),
            thread_id: None,
        };
        let task = Task::from_trigger(
            "nightly summary".to_string(),
            3,
            TaskRoutingPolicy::event(
                Some(approval_channel),
                Some("nightly summary timer".to_string()),
            ),
        );
        let task_id = task.id;
        app.world_mut().spawn(task);
        app.world_mut().spawn(ToolConfirmationRequestMessage {
            request_id: Uuid::new_v4(),
            task_id,
            agent_id: Uuid::nil(),
            tool_name: "shell_exec".to_string(),
            tool_input: serde_json::json!({"command": "date"}),
            options: ConfirmationOption::default_options(),
            source: ConfirmationSource::User,
            parent_agent_id: None,
            approval_context: Some("nightly summary timer".to_string()),
        });

        app.update();

        let task = app
            .world_mut()
            .query::<&Task>()
            .iter(app.world())
            .find(|task| task.id == task_id)
            .expect("task should remain for failure inspection");
        assert!(matches!(
            task.status,
            crate::domain::TaskStatus::Failed(crate::domain::FailureReason::Unknown)
        ));
        assert_eq!(
            task.last_error.as_deref(),
            Some("approval channel frontend 'qq' is not enabled")
        );

        let events = events.lock().unwrap();
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, EngineEvent::ApprovalRequest { .. })),
            "should not emit ApprovalRequest for disabled frontend"
        );
    }

    #[test]
    fn scheduled_task_approval_request_routes_to_output_channel() {
        let mut app = App::new();
        let events = Arc::new(Mutex::new(Vec::new()));
        let frontend = MockFrontend {
            kind: FrontendKind::QQ,
            events: events.clone(),
        };
        app.insert_resource(FrontendRegistry {
            frontends: vec![Box::new(frontend)],
        });
        app.add_systems(Update, frontend_output_system);

        let output_channel = ChannelId {
            frontend: FrontendKind::QQ,
            user_id: "reviewer".to_string(),
            thread_id: None,
        };
        let task = Task::from_trigger(
            "scheduled task".to_string(),
            3,
            TaskRoutingPolicy::scheduled_task(Some(output_channel.clone()), "scheduled task"),
        );
        let task_id = task.id;
        app.world_mut().spawn(task);
        app.world_mut().spawn(ToolConfirmationRequestMessage {
            request_id: Uuid::new_v4(),
            task_id,
            agent_id: Uuid::nil(),
            tool_name: "shell_exec".to_string(),
            tool_input: serde_json::Value::Null,
            options: ConfirmationOption::default_options(),
            source: ConfirmationSource::User,
            parent_agent_id: None,
            approval_context: Some("scheduled task".to_string()),
        });

        app.update();

        let events = events.lock().unwrap();
        let approval_target = events
            .iter()
            .find_map(|event| match event {
                EngineEvent::ApprovalRequest { target, .. } => Some(target.clone()),
                _ => None,
            })
            .expect("approval request should be emitted");

        match approval_target {
            EventTarget::Directed(channels) => {
                assert_eq!(channels, vec![output_channel]);
            }
            EventTarget::Broadcast => {
                panic!("approval should route to scheduled task output channel")
            }
        }
    }
}
