use std::collections::{HashMap, HashSet};

use crate::prelude::*;
use tracing::{debug, warn};

use crate::contracts::FrontendRegistry;
use crate::domain::{
    Agent, AgentStatusKind, EngineEvent, EventTarget, FailureReason, MessageRole,
    SystemOutputMessage, Task, TaskId, TaskStatus, TaskStatusKind, ToolConfirmationRequestMessage,
    UserOutputMessage, WaitingReason, WaitingReasonKind,
};
use crate::ecs::EntityIndex;

/// 将 ECS 状态变化转为 EngineEvent 推送给所有前端
#[allow(clippy::too_many_arguments)]
pub(crate) fn frontend_output_system(
    registry: Res<FrontendRegistry>,
    mut commands: Commands,
    index: Res<EntityIndex>,
    outputs: Query<(Entity, &UserOutputMessage)>,
    system_outputs: Query<(Entity, &SystemOutputMessage)>,
    all_tasks: Query<(Entity, &Task)>,
    tasks: Query<&Task, Changed<Task>>,
    agents: Query<&Agent, Changed<Agent>>,
    all_agents: Query<&Agent>,
    confirmations: Query<
        (Entity, &ToolConfirmationRequestMessage),
        Added<ToolConfirmationRequestMessage>,
    >,
    mut last_status: Local<HashMap<TaskId, TaskStatusKind>>,
    mut reported_terminal: Local<HashSet<TaskId>>,
    scheduled_runs: Query<&crate::domain::ScheduledRun>,
    channel_manager: Option<Res<crate::channels::ChannelManager>>,
    mut scheduled_deliveries: Option<ResMut<crate::systems::ScheduleDeliveryTracker>>,
) {
    // 用户可见文本输出
    for (entity, output) in &outputs {
        // UUID 寻址改用 EntityIndex O(1) 解析
        let Some(target) = index
            .get_task(&output.task_id)
            .and_then(|e| all_tasks.get(e).ok())
            .and_then(|(_, t)| t.routing_policy.output_channel().cloned())
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
            task_id: Some(output.task_id),
        };
        let run = index
            .get_task(&output.task_id)
            .and_then(|entity| scheduled_runs.get(entity).ok());
        if let Some(run) = run {
            if run.notify == crate::domain::NotifyPolicy::Reply
                && let Some((_, task)) = index
                    .get_task(&output.task_id)
                    .and_then(|entity| all_tasks.get(entity).ok())
                && let Some(channel) = task.delivery_channel()
                && let Some(tracker) = scheduled_deliveries.as_deref_mut()
            {
                let callback = tracker.begin(output.task_id);
                if channel.frontend == crate::domain::FrontendKind::Tui {
                    if registry.has_frontend(crate::domain::FrontendKind::Tui) {
                        callback(Ok(None));
                    } else {
                        callback(Err("TUI frontend unavailable".into()));
                    }
                } else if let Some(manager) = channel_manager.as_deref() {
                    let message = crate::channels::ChannelOutboundMessage {
                        recipient: channel.user_id.clone(),
                        thread_id: channel.thread_id.clone(),
                        content: format!(
                            "[{}] 助手: {}",
                            &task.id.to_string()[..8],
                            output.content
                        ),
                        parse_mode: None,
                        reply_markup: None,
                        attachments: vec![],
                        message_kind: crate::channels::MessageKind::LLMReply,
                    };
                    if let Err(error) = manager.send_with_result(
                        channel.frontend.channel_name().into(),
                        message,
                        callback,
                    ) {
                        tracker.reject(output.task_id, error.to_string());
                    }
                } else {
                    callback(Err("channel manager unavailable".into()));
                }
            }
            // 定时输出保留TUI可见；IM自动推送只在reply策略中经真实回执路径执行。
            for frontend in &registry.frontends {
                if frontend.kind() == crate::domain::FrontendKind::Tui {
                    frontend.push_event(event.clone());
                }
            }
        } else {
            for frontend in &registry.frontends {
                if frontend.kind() == crate::domain::FrontendKind::Client {
                    frontend.push_event(EngineEvent::AgentOutput {
                        target: event.target().clone(),
                        task_id: output.task_id,
                        content: output.content.clone(),
                        progress: output.progress,
                    });
                } else {
                    frontend.push_event(event.clone());
                }
            }
        }
        commands.entity(entity).despawn();
    }

    // 系统通知输出（不进入 STM，路由到 task 的 output_channel）
    for (entity, output) in &system_outputs {
        // UUID 寻址改用 EntityIndex O(1) 解析
        let Some(target) = index
            .get_task(&output.task_id)
            .and_then(|e| all_tasks.get(e).ok())
            .and_then(|(_, t)| t.routing_policy.output_channel().cloned())
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
            task_id: Some(output.task_id),
        };
        for frontend in &registry.frontends {
            frontend.push_event(event.clone());
        }
        commands.entity(entity).despawn();
    }

    // Task 状态变化
    for task in &tasks {
        if reported_terminal.contains(&task.id) {
            continue;
        }

        let Some(target) = task
            .routing_policy
            .output_channel()
            .cloned()
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
        let old_status = last_status.get(&task.id).copied();
        if old_status == Some(status) {
            continue;
        }
        let result = if task.status.is_terminal() {
            Some(task.result_summary.clone())
        } else {
            None
        };
        // 通过 delegate agent_id 解析 agent name
        let agent_name = task.delegate.and_then(|agent_id| {
            index
                .get_agent(&agent_id)
                .and_then(|e| all_agents.get(e).ok())
                .map(|a| a.profile.name.clone())
        });
        let waiting_reason = match &task.status {
            TaskStatus::Waiting(reason) => Some(waiting_reason_to_kind(reason)),
            _ => None,
        };
        let event = EngineEvent::TaskStatusChanged {
            target,
            task_id: task.id,
            name: task.input_summary.clone(),
            status,
            old_status,
            result,
            parent_id: task.parent_task_id,
            origin_channel: task.origin_channel.clone(),
            agent_name,
            waiting_reason,
        };
        if task.status.is_terminal() {
            last_status.remove(&task.id);
            reported_terminal.insert(task.id);
        } else {
            last_status.insert(task.id, status);
        }
        let scheduled = index
            .get_task(&task.id)
            .is_some_and(|entity| scheduled_runs.get(entity).is_ok());
        for frontend in &registry.frontends {
            if !scheduled || frontend.kind() == crate::domain::FrontendKind::Tui {
                frontend.push_event(event.clone());
            }
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
        // UUID 寻址改用 EntityIndex O(1) 解析
        let Some((task_entity, task)) = index
            .get_task(&confirmation.task_id)
            .and_then(|e| all_tasks.get(e).ok())
        else {
            commands.entity(entity).despawn();
            continue;
        };

        let Some(approval_channel) = task.routing_policy.approval_channel().cloned() else {
            let mut failed_task = task.clone();
            failed_task.mark_failed_reason(
                FailureReason::Unknown,
                "missing approval channel for event task approval request",
                chrono::Utc::now(),
            );
            commands.entity(task_entity).insert(failed_task);
            warn!(
                event = "FrontendApprovalRouteMissing",
                task_id = %confirmation.task_id,
                request_id = %confirmation.request_id,
                "marking task failed because approval channel is missing"
            );
            commands.entity(entity).despawn();
            continue;
        };

        // 仅对事件任务检查 frontend 是否注册
        if task.origin_channel.is_none()
            && !registry.has_frontend(approval_channel.frontend.clone())
        {
            let frontend_name = approval_channel.frontend.channel_name();
            let mut failed_task = task.clone();
            failed_task.mark_failed_reason(
                FailureReason::Unknown,
                format!(
                    "approval channel frontend '{}' is not enabled",
                    frontend_name
                ),
                chrono::Utc::now(),
            );
            commands.entity(task_entity).insert(failed_task);
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
                        crate::domain::GrantMode::Permanent => {
                            "本次运行内对该会话允许此工具".to_string()
                        }
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

fn waiting_reason_to_kind(reason: &WaitingReason) -> WaitingReasonKind {
    match reason {
        WaitingReason::Agent => WaitingReasonKind::Agent,
        WaitingReason::ToolExecution
        | WaitingReason::Session { .. }
        | WaitingReason::SubTaskBatch { .. }
        | WaitingReason::BackgroundJobs => WaitingReasonKind::Tool,
        WaitingReason::User | WaitingReason::Approval | WaitingReason::AskUser => {
            WaitingReasonKind::User
        }
        WaitingReason::RetryBackoff => WaitingReasonKind::Retry,
        WaitingReason::Evaluator | WaitingReason::Summarization | WaitingReason::ChatAgent => {
            WaitingReasonKind::Other
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use crate::prelude::*;
    use uuid::Uuid;

    use crate::contracts::FrontendRegistry;
    use crate::domain::{
        Agent, AgentCapabilities, AgentKind, AgentProfile, AgentToolPermissions, ChannelId,
        ConfirmationOption, ConfirmationSource, EngineEvent, EventTarget, Frontend, FrontendKind,
        Task, TaskRoutingPolicy, TaskStatus, TaskStatusKind, ToolConfirmationRequestMessage,
        UserAction, UserOutputMessage, WaitingReason, WaitingReasonKind,
    };
    use crate::ecs::EntityIndex;

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
        app.insert_resource(EntityIndex::default());
        app.add_systems(Update, frontend_output_system);

        let origin_channel = ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "u1".to_string(),
            thread_id: Some("t1".to_string()),
        };
        let task = Task::from_user_input("test", 3, origin_channel.clone());
        let task_id = task.id;
        let task_entity = app.world_mut().spawn(task).id();
        app.world_mut()
            .resource_mut::<EntityIndex>()
            .tasks
            .insert(task_id, task_entity);

        app.world_mut().spawn(ToolConfirmationRequestMessage {
            request_id: Uuid::new_v4(),
            task_id,
            agent_id: crate::domain::AgentId::nil(),
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
        app.insert_resource(EntityIndex::default());
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
        let task_entity = app.world_mut().spawn(task).id();
        app.world_mut()
            .resource_mut::<EntityIndex>()
            .tasks
            .insert(task_id, task_entity);

        app.world_mut().spawn(ToolConfirmationRequestMessage {
            request_id: Uuid::new_v4(),
            task_id,
            agent_id: crate::domain::AgentId::nil(),
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
        app.insert_resource(EntityIndex::default());
        app.add_systems(Update, frontend_output_system);

        let task = Task::from_trigger(
            "nightly summary".to_string(),
            3,
            TaskRoutingPolicy::event(None, Some("nightly summary timer".to_string())),
        );
        let task_id = task.id;
        let task_entity = app.world_mut().spawn(task).id();
        app.world_mut()
            .resource_mut::<EntityIndex>()
            .tasks
            .insert(task_id, task_entity);
        app.world_mut().spawn(UserOutputMessage {
            progress: false,
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
    fn user_output_text_event_includes_task_id() {
        let mut app = App::new();
        let events = Arc::new(Mutex::new(Vec::new()));
        let frontend = MockFrontend {
            kind: FrontendKind::Telegram,
            events: events.clone(),
        };
        app.insert_resource(FrontendRegistry {
            frontends: vec![Box::new(frontend)],
        });
        app.insert_resource(EntityIndex::default());
        app.add_systems(Update, frontend_output_system);

        let origin_channel = ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "u1".to_string(),
            thread_id: None,
        };
        let task = Task::from_user_input("test", 3, origin_channel);
        let task_id = task.id;
        let task_entity = app.world_mut().spawn(task).id();
        app.world_mut()
            .resource_mut::<EntityIndex>()
            .tasks
            .insert(task_id, task_entity);
        app.world_mut().spawn(UserOutputMessage {
            progress: false,
            task_id,
            content: "hello".to_string(),
        });

        app.update();

        let events = events.lock().unwrap();
        let text_task_id = events
            .iter()
            .find_map(|e| match e {
                EngineEvent::Text { task_id, .. } => *task_id,
                _ => None,
            })
            .expect("should emit Text event with task_id");
        assert_eq!(text_task_id, task_id);
    }

    #[test]
    fn client_output_preserves_progress_phase() {
        let mut app = App::new();
        let events = Arc::new(Mutex::new(Vec::new()));
        app.insert_resource(FrontendRegistry {
            frontends: vec![Box::new(MockFrontend {
                kind: FrontendKind::Client,
                events: events.clone(),
            })],
        });
        app.insert_resource(EntityIndex::default());
        app.add_systems(Update, frontend_output_system);
        let channel = ChannelId {
            frontend: FrontendKind::Client,
            user_id: "u1".into(),
            thread_id: None,
        };
        let task = Task::from_user_input("test", 3, channel);
        let task_id = task.id;
        let entity = app.world_mut().spawn(task).id();
        app.world_mut()
            .resource_mut::<EntityIndex>()
            .tasks
            .insert(task_id, entity);
        app.world_mut().spawn(UserOutputMessage {
            progress: true,
            task_id,
            content: "checking".into(),
        });
        app.update();
        assert!(events.lock().unwrap().iter().any(|event| matches!(event,
            EngineEvent::AgentOutput { task_id: id, progress: true, content, .. } if *id == task_id && content == "checking")));
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
        app.insert_resource(EntityIndex::default());
        app.add_systems(Update, frontend_output_system);

        let task = Task::from_trigger(
            "nightly summary".to_string(),
            3,
            TaskRoutingPolicy::event(None, Some("nightly summary timer".to_string())),
        );
        let task_id = task.id;
        let task_entity = app.world_mut().spawn(task).id();
        app.world_mut()
            .resource_mut::<EntityIndex>()
            .tasks
            .insert(task_id, task_entity);
        app.world_mut().spawn(ToolConfirmationRequestMessage {
            request_id: Uuid::new_v4(),
            task_id,
            agent_id: crate::domain::AgentId::nil(),
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
        app.insert_resource(EntityIndex::default());
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
        let task_entity = app.world_mut().spawn(task).id();
        app.world_mut()
            .resource_mut::<EntityIndex>()
            .tasks
            .insert(task_id, task_entity);
        app.world_mut().spawn(ToolConfirmationRequestMessage {
            request_id: Uuid::new_v4(),
            task_id,
            agent_id: crate::domain::AgentId::nil(),
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
        app.insert_resource(EntityIndex::default());
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
        let task_entity = app.world_mut().spawn(task).id();
        app.world_mut()
            .resource_mut::<EntityIndex>()
            .tasks
            .insert(task_id, task_entity);
        app.world_mut().spawn(ToolConfirmationRequestMessage {
            request_id: Uuid::new_v4(),
            task_id,
            agent_id: crate::domain::AgentId::nil(),
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

    #[test]
    fn task_status_changed_event_includes_old_status() {
        let mut app = App::new();
        let events = Arc::new(Mutex::new(Vec::new()));
        let frontend = MockFrontend {
            kind: FrontendKind::Telegram,
            events: events.clone(),
        };
        app.insert_resource(FrontendRegistry {
            frontends: vec![Box::new(frontend)],
        });
        app.insert_resource(EntityIndex::default());
        app.add_systems(Update, frontend_output_system);

        let origin_channel = ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "u1".to_string(),
            thread_id: None,
        };
        let task = Task::from_user_input("test", 3, origin_channel);
        let task_id = task.id;
        let task_entity = app.world_mut().spawn(task).id();
        app.world_mut()
            .resource_mut::<EntityIndex>()
            .tasks
            .insert(task_id, task_entity);

        // First update: task status change from Pending -> Running
        {
            let mut task = app
                .world_mut()
                .query::<&mut Task>()
                .iter_mut(app.world_mut())
                .find(|t| t.id == task_id)
                .unwrap();
            task.mark_running(chrono::Utc::now());
        }
        app.update();

        // Second update: Running -> Done
        {
            let mut task = app
                .world_mut()
                .query::<&mut Task>()
                .iter_mut(app.world_mut())
                .find(|t| t.id == task_id)
                .unwrap();
            task.mark_done("done", chrono::Utc::now());
        }
        app.update();

        let events = events.lock().unwrap();
        let status_events: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                EngineEvent::TaskStatusChanged {
                    task_id: id,
                    status,
                    old_status,
                    ..
                } if *id == task_id => Some((*old_status, *status)),
                _ => None,
            })
            .collect();

        assert_eq!(status_events.len(), 2);
        assert_eq!(status_events[0], (None, TaskStatusKind::Running));
        assert_eq!(
            status_events[1],
            (Some(TaskStatusKind::Running), TaskStatusKind::Done)
        );
    }

    #[test]
    fn terminal_task_status_is_not_re_emitted_after_subsequent_changes() {
        let mut app = App::new();
        let events = Arc::new(Mutex::new(Vec::new()));
        let frontend = MockFrontend {
            kind: FrontendKind::Telegram,
            events: events.clone(),
        };
        app.insert_resource(FrontendRegistry {
            frontends: vec![Box::new(frontend)],
        });
        app.insert_resource(EntityIndex::default());
        app.add_systems(Update, frontend_output_system);

        let origin_channel = ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "u1".to_string(),
            thread_id: None,
        };
        let task = Task::from_user_input("test", 3, origin_channel);
        let task_id = task.id;
        let task_entity = app.world_mut().spawn(task).id();
        app.world_mut()
            .resource_mut::<EntityIndex>()
            .tasks
            .insert(task_id, task_entity);

        for status in [
            TaskStatus::Running,
            TaskStatus::Done,
            TaskStatus::Running,
            TaskStatus::Failed(crate::domain::FailureReason::Unknown),
            TaskStatus::Running,
        ] {
            {
                let mut task = app
                    .world_mut()
                    .query::<&mut Task>()
                    .iter_mut(app.world_mut())
                    .find(|t| t.id == task_id)
                    .unwrap();
                task.status = status;
            }
            app.update();
        }

        let events = events.lock().unwrap();
        let status_events: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                EngineEvent::TaskStatusChanged {
                    task_id: id,
                    status,
                    old_status,
                    ..
                } if *id == task_id => Some((*old_status, *status)),
                _ => None,
            })
            .collect();

        // 只有 Pending -> Running 和 Running -> Done 会被发送，
        // Done 之后的变更因已进入 reported_terminal 而被忽略。
        assert_eq!(status_events.len(), 2);
        assert_eq!(status_events[0], (None, TaskStatusKind::Running));
        assert_eq!(
            status_events[1],
            (Some(TaskStatusKind::Running), TaskStatusKind::Done)
        );
    }

    #[test]
    fn terminal_task_status_is_not_duplicated_when_other_fields_change() {
        let mut app = App::new();
        let events = Arc::new(Mutex::new(Vec::new()));
        let frontend = MockFrontend {
            kind: FrontendKind::Telegram,
            events: events.clone(),
        };
        app.insert_resource(FrontendRegistry {
            frontends: vec![Box::new(frontend)],
        });
        app.insert_resource(EntityIndex::default());
        app.add_systems(Update, frontend_output_system);

        let origin_channel = ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "u1".to_string(),
            thread_id: None,
        };
        let task = Task::from_user_input("test", 3, origin_channel);
        let task_id = task.id;
        let task_entity = app.world_mut().spawn(task).id();
        app.world_mut()
            .resource_mut::<EntityIndex>()
            .tasks
            .insert(task_id, task_entity);

        // Pending -> Running
        {
            let mut task = app
                .world_mut()
                .query::<&mut Task>()
                .iter_mut(app.world_mut())
                .find(|t| t.id == task_id)
                .unwrap();
            task.mark_running(chrono::Utc::now());
        }
        app.update();

        // Running -> Done
        {
            let mut task = app
                .world_mut()
                .query::<&mut Task>()
                .iter_mut(app.world_mut())
                .find(|t| t.id == task_id)
                .unwrap();
            task.mark_done("done", chrono::Utc::now());
        }
        app.update();

        // Done 之后更新其他字段（如 result_summary），不应再次触发 TaskStatusChanged
        {
            let mut task = app
                .world_mut()
                .query::<&mut Task>()
                .iter_mut(app.world_mut())
                .find(|t| t.id == task_id)
                .unwrap();
            task.result_summary = "final result".to_string();
        }
        app.update();

        let events = events.lock().unwrap();
        let status_events: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                EngineEvent::TaskStatusChanged {
                    task_id: id,
                    status,
                    ..
                } if *id == task_id => Some(*status),
                _ => None,
            })
            .collect();

        assert_eq!(status_events.len(), 2);
        assert_eq!(status_events[0], TaskStatusKind::Running);
        assert_eq!(status_events[1], TaskStatusKind::Done);
    }

    #[test]
    fn task_status_changed_event_includes_origin_channel() {
        let mut app = App::new();
        let events = Arc::new(Mutex::new(Vec::new()));
        let frontend = MockFrontend {
            kind: FrontendKind::Telegram,
            events: events.clone(),
        };
        app.insert_resource(FrontendRegistry {
            frontends: vec![Box::new(frontend)],
        });
        app.insert_resource(EntityIndex::default());
        app.add_systems(Update, frontend_output_system);

        let origin_channel = ChannelId {
            frontend: FrontendKind::QQ,
            user_id: "qq_user".to_string(),
            thread_id: None,
        };
        let task = Task::from_user_input("test", 3, origin_channel.clone());
        let task_id = task.id;
        let task_entity = app.world_mut().spawn(task).id();
        app.world_mut()
            .resource_mut::<EntityIndex>()
            .tasks
            .insert(task_id, task_entity);

        // Update task status to trigger event
        {
            let mut task = app
                .world_mut()
                .query::<&mut Task>()
                .iter_mut(app.world_mut())
                .find(|t| t.id == task_id)
                .unwrap();
            task.mark_running(chrono::Utc::now());
        }
        app.update();

        let events = events.lock().unwrap();
        let origin = events
            .iter()
            .find_map(|e| match e {
                EngineEvent::TaskStatusChanged { origin_channel, .. } => {
                    Some(origin_channel.clone())
                }
                _ => None,
            })
            .expect("should emit TaskStatusChanged with origin_channel");
        assert_eq!(origin, Some(origin_channel));
    }

    #[test]
    fn task_status_changed_includes_agent_name_and_waiting_reason() {
        let mut app = App::new();
        let events = Arc::new(Mutex::new(Vec::new()));
        let frontend = MockFrontend {
            kind: FrontendKind::Telegram,
            events: events.clone(),
        };
        app.insert_resource(FrontendRegistry {
            frontends: vec![Box::new(frontend)],
        });
        app.insert_resource(EntityIndex::default());
        app.add_systems(Update, frontend_output_system);

        let origin_channel = ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "u1".to_string(),
            thread_id: None,
        };
        let mut task = Task::from_user_input("test", 3, origin_channel);
        task.delegate = Some(crate::domain::AgentId::nil());
        task.mark_waiting(WaitingReason::ToolExecution, chrono::Utc::now());
        let task_id = task.id;
        let task_entity = app.world_mut().spawn(task).id();
        app.world_mut()
            .resource_mut::<EntityIndex>()
            .tasks
            .insert(task_id, task_entity);

        let agent = Agent {
            id: crate::domain::AgentId::nil(),
            profile: AgentProfile {
                name: "TestAgent".to_string(),
                model: "test-model".to_string(),
            },
            capabilities: AgentCapabilities {
                tags: vec![],
                description: String::new(),
            },
            kind: AgentKind::Persistent,
            parent_id: None,
            bound_task_id: None,
            tool_permissions: AgentToolPermissions::default(),
            system_prompt: None,
        };
        let agent_entity = app.world_mut().spawn(agent).id();
        app.world_mut()
            .resource_mut::<EntityIndex>()
            .agents
            .insert(crate::domain::AgentId::nil(), agent_entity);

        app.update();

        let events = events.lock().unwrap();
        let (agent_name, waiting_reason) = events
            .iter()
            .find_map(|e| match e {
                EngineEvent::TaskStatusChanged {
                    agent_name,
                    waiting_reason,
                    ..
                } => Some((agent_name.clone(), *waiting_reason)),
                _ => None,
            })
            .expect("should emit TaskStatusChanged with agent_name and waiting_reason");
        assert_eq!(agent_name.as_deref(), Some("TestAgent"));
        assert_eq!(waiting_reason, Some(WaitingReasonKind::Tool));
    }

    #[test]
    fn task_status_changed_agent_name_none_when_no_delegate() {
        let mut app = App::new();
        let events = Arc::new(Mutex::new(Vec::new()));
        let frontend = MockFrontend {
            kind: FrontendKind::Telegram,
            events: events.clone(),
        };
        app.insert_resource(FrontendRegistry {
            frontends: vec![Box::new(frontend)],
        });
        app.insert_resource(EntityIndex::default());
        app.add_systems(Update, frontend_output_system);

        let origin_channel = ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "u1".to_string(),
            thread_id: None,
        };
        let mut task = Task::from_user_input("test", 3, origin_channel);
        // 不设置 delegate（保持 None），也不 spawn agent entity
        task.mark_waiting(WaitingReason::Agent, chrono::Utc::now());
        let task_id = task.id;
        let task_entity = app.world_mut().spawn(task).id();
        app.world_mut()
            .resource_mut::<EntityIndex>()
            .tasks
            .insert(task_id, task_entity);

        app.update();

        let events = events.lock().unwrap();
        let agent_name = events
            .iter()
            .find_map(|e| match e {
                EngineEvent::TaskStatusChanged { agent_name, .. } => Some(agent_name.clone()),
                _ => None,
            })
            .expect("should emit TaskStatusChanged");
        assert_eq!(agent_name, None);
    }

    #[test]
    fn waiting_reason_to_kind_mappings() {
        use super::waiting_reason_to_kind;

        let cases = [
            (WaitingReason::Agent, WaitingReasonKind::Agent),
            (WaitingReason::User, WaitingReasonKind::User),
            (WaitingReason::Approval, WaitingReasonKind::User),
            (WaitingReason::AskUser, WaitingReasonKind::User),
            (WaitingReason::RetryBackoff, WaitingReasonKind::Retry),
            (WaitingReason::Evaluator, WaitingReasonKind::Other),
            (
                WaitingReason::Session {
                    handle_id: Uuid::new_v4(),
                },
                WaitingReasonKind::Tool,
            ),
            (
                WaitingReason::SubTaskBatch {
                    batch_id: Uuid::new_v4(),
                },
                WaitingReasonKind::Tool,
            ),
        ];

        for (reason, expected) in cases {
            assert_eq!(waiting_reason_to_kind(&reason), expected);
        }
    }
    #[test]
    fn explicit_scheduled_run_keeps_tui_reply_and_hides_im_status_and_reply() {
        let mut app = App::new();
        let im = Arc::new(Mutex::new(Vec::new()));
        let tui = Arc::new(Mutex::new(Vec::new()));
        app.insert_resource(FrontendRegistry {
            frontends: vec![
                Box::new(MockFrontend {
                    kind: FrontendKind::QQ,
                    events: im.clone(),
                }),
                Box::new(MockFrontend {
                    kind: FrontendKind::Tui,
                    events: tui.clone(),
                }),
            ],
        });
        app.init_resource::<EntityIndex>();
        app.init_resource::<crate::systems::ScheduleDeliveryTracker>();
        app.add_systems(Update, frontend_output_system);
        let channel = ChannelId {
            frontend: FrontendKind::QQ,
            user_id: "schedule-test".into(),
            thread_id: None,
        };
        let mut task = Task::from_user_input("monitor", 3, channel.clone());
        task.origin_channel = None;
        task.routing_policy = TaskRoutingPolicy::scheduled_task(Some(channel), "scheduled test");
        task.mark_done("no change", chrono::Utc::now());
        let task_id = task.id;
        let entity = app
            .world_mut()
            .spawn((
                task,
                crate::domain::ScheduledRun {
                    schedule_id: Uuid::new_v4(),
                    fired_at: None,
                    notify: crate::domain::NotifyPolicy::Explicit,
                },
            ))
            .id();
        app.world_mut()
            .resource_mut::<EntityIndex>()
            .tasks
            .insert(task_id, entity);
        app.world_mut().spawn(UserOutputMessage {
            progress: false,
            task_id,
            content: "no change".into(),
        });
        app.update();
        assert!(!im.lock().unwrap().iter().any(|event| matches!(
            event,
            EngineEvent::Text { .. } | EngineEvent::TaskStatusChanged { .. }
        )));
        assert!(tui.lock().unwrap().iter().any(
            |event| matches!(event, EngineEvent::Text { content, .. } if content == "no change")
        ));
        assert_eq!(
            app.world()
                .resource::<crate::systems::ScheduleDeliveryTracker>()
                .notified_for(task_id),
            0
        );
    }
    /// 覆盖真实 ChannelManager 队列 -> 平台 send -> 回执 -> 账本，不把入队当成功。
    struct GatedScheduleChannel {
        gate: Arc<tokio::sync::Semaphore>,
        started: crossbeam_channel::Sender<crate::channels::ChannelOutboundMessage>,
        fail: bool,
    }
    #[async_trait::async_trait]
    impl crate::channels::Channel for GatedScheduleChannel {
        fn name(&self) -> &str {
            "qq"
        }
        async fn send(
            &self,
            message: &crate::channels::ChannelOutboundMessage,
        ) -> Result<Option<String>, crate::channels::ChannelError> {
            self.started.send(message.clone()).unwrap();
            self.gate.acquire().await.unwrap().forget();
            if self.fail {
                Err(crate::channels::ChannelError::NotConfigured)
            } else {
                Ok(Some("platform-id".into()))
            }
        }
        async fn listen(
            &self,
            _: crossbeam_channel::Sender<crate::channels::ChannelInboundMessage>,
        ) -> Result<(), crate::channels::ChannelError> {
            Ok(())
        }
    }

    async fn schedule_receipt_pipeline(fail: bool, explicit_send: bool) {
        use crate::domain::{
            NotifyPolicy, RunOutcome, ScheduleDefinition, ScheduleExpr, ScheduledRun,
        };
        use crate::infrastructure::schedules::ScheduleStore;
        use crate::systems::{
            ScheduleDeliveryTracker, ScheduleRunRecorded, schedule_run_accounting_system,
        };
        use crate::triggers::{SchedulerState, SchedulerStateWatcher};
        let dir = tempfile::tempdir().unwrap();
        let store = ScheduleStore::new(dir.path().join("schedules.json"));
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        let (started_tx, started_rx) = crossbeam_channel::unbounded();
        let channel = Arc::new(GatedScheduleChannel {
            gate: gate.clone(),
            started: started_tx,
            fail,
        });
        let (input_tx, _) = crossbeam_channel::unbounded();
        let (manager, _, _) =
            crate::channels::ChannelManager::new(vec![channel], input_tx).unwrap();
        let output_channel = crate::domain::ChannelId {
            frontend: FrontendKind::QQ,
            user_id: "group:scheduled".into(),
            thread_id: Some("topic".into()),
        };
        let mut def = ScheduleDefinition::new(
            Uuid::new_v4(),
            Some("pipeline".into()),
            "check".into(),
            ScheduleExpr::parse("cron:*/5 * * * *").unwrap(),
            Some(output_channel.clone()),
            chrono::Utc::now(),
        );
        def.notify = if explicit_send {
            NotifyPolicy::Explicit
        } else {
            NotifyPolicy::Reply
        };
        let id = def.id;
        let policy = def.notify;
        let mut state = SchedulerState::default();
        state.dynamic_tasks_mut().push(def);
        let mut app = App::new();
        app.insert_resource(state);
        app.insert_resource(SchedulerStateWatcher::default());
        app.insert_resource(store.clone());
        app.insert_resource(ScheduleDeliveryTracker::default());
        app.insert_resource(manager);
        app.insert_resource(EntityIndex::default());
        let events = Arc::new(Mutex::new(Vec::new()));
        app.insert_resource(FrontendRegistry {
            frontends: vec![Box::new(MockFrontend {
                kind: FrontendKind::Tui,
                events: events.clone(),
            })],
        });
        app.add_systems(
            Update,
            (
                frontend_output_system,
                crate::systems::channel_send_dispatch_system,
                schedule_run_accounting_system,
            )
                .chain(),
        );
        let mut task = Task::from_trigger(
            "check",
            0,
            TaskRoutingPolicy::scheduled_task(Some(output_channel), "scheduled task"),
        );
        task.mark_done("done", chrono::Utc::now());
        let task_id = task.id;
        let entity = app
            .world_mut()
            .spawn((
                task,
                ScheduledRun {
                    schedule_id: id,
                    fired_at: None,
                    notify: policy,
                },
            ))
            .id();
        app.world_mut()
            .resource_mut::<EntityIndex>()
            .tasks
            .insert(task_id, entity);
        // explicit 最终回复留在 TUI，只有 channel_send 会产生真实通知。
        app.world_mut().spawn(UserOutputMessage {
            progress: false,
            task_id,
            content: "final reply".into(),
        });
        if explicit_send {
            let request_entity = app.world_mut().spawn_empty().id();
            app.world_mut().spawn(crate::domain::PendingChannelSend {
                channel: "qq".into(),
                recipient: None,
                content: "explicit notification".into(),
                attachments: vec![],
                tool_call_id: None,
                task_id,
                agent_id: crate::domain::AgentId::nil(),
                request_entity,
            });
        }
        app.update();
        let message = tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                if let Ok(message) = started_rx.try_recv() {
                    break message;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(message.recipient, "group:scheduled");
        assert_eq!(message.thread_id.as_deref(), Some("topic"));
        if explicit_send {
            assert_eq!(message.content, "explicit notification");
        } else {
            assert!(
                message
                    .content
                    .starts_with(&format!("[{}] 助手: ", &task_id.to_string()[..8]))
            );
        }
        for _ in 0..3 {
            app.update();
        }
        assert_eq!(
            app.world()
                .resource::<ScheduleDeliveryTracker>()
                .pending_for(task_id),
            1
        );
        assert!(app.world().get::<ScheduleRunRecorded>(entity).is_none());
        assert_eq!(
            store
                .run_summary(id, chrono::DateTime::<chrono::Utc>::MIN_UTC)
                .unwrap()
                .total,
            0,
            "queue acceptance is never a completed run"
        );
        gate.add_permits(1);
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                app.update();
                if app.world().get::<ScheduleRunRecorded>(entity).is_some() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let raw = std::fs::read_to_string(store.state_dir(id).parent().unwrap().join("runs.jsonl"))
            .unwrap();
        let record: crate::domain::ScheduleRunRecord = serde_json::from_str(raw.trim()).unwrap();
        assert_eq!(
            record.outcome,
            if fail {
                RunOutcome::Failed
            } else {
                RunOutcome::Notified
            }
        );
        assert_eq!(record.notification_count, u32::from(!fail));
        assert_eq!(
            store.load().unwrap()[0].health.consecutive_failures,
            u32::from(fail)
        );
        assert_eq!(
            app.world()
                .resource::<ScheduleDeliveryTracker>()
                .pending_for(task_id),
            0
        );
        assert!(
            started_rx.try_recv().is_err(),
            "explicit final reply must not enqueue a second notification"
        );
        app.world()
            .resource::<crate::channels::ChannelManager>()
            .shutdown();
    }

    #[tokio::test]
    async fn scheduled_reply_waits_for_real_success_and_failure_receipts() {
        schedule_receipt_pipeline(false, false).await;
        schedule_receipt_pipeline(true, false).await;
    }

    #[tokio::test]
    async fn scheduled_explicit_channel_send_waits_for_real_receipts() {
        schedule_receipt_pipeline(false, true).await;
        schedule_receipt_pipeline(true, true).await;
    }
}
