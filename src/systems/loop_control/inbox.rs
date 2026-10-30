//! 回合收件箱：投递写入与空闲唤醒
//!
//! - `inbox_ingest_system`：`TaskInbox` 的唯一写入点，消费 `InboxDeliveryMessage`。
//! - `inbox_wake_system`：任务空闲时取出积压，经 `ContinueTaskMessage` 走既有续轮链路。

use std::collections::HashMap;

use tracing::debug;

use crate::contracts::{Clock, FrontendRegistry};
use crate::domain::{
    AskUserPending, AutoWakeBudget, ContinueTaskMessage, DeferredResult, EngineEvent, EventTarget,
    InboxDeliveryMessage, InboxItem, LoopControlConfig, OwnedJobs, Task, TaskInbox, TaskStatus,
    WaitingReason, render_inbox_items,
};
use crate::ecs::EntityIndex;
use crate::prelude::*;

/// 投递写入：`TaskInbox` 的唯一 push 入口。
///
/// 同帧多条投递到同一个尚未挂载 `TaskInbox` 的任务时先在本地聚合，
/// 避免多次 `insert` 互相覆盖。
pub(crate) fn inbox_ingest_system(
    mut commands: Commands,
    index: Res<EntityIndex>,
    deliveries: Query<(
        Entity,
        &InboxDeliveryMessage,
        Option<&crate::domain::InputSequence>,
    )>,
    mut tasks: Query<(&Task, Option<&mut TaskInbox>)>,
) {
    let mut new_inboxes: HashMap<Entity, TaskInbox> = HashMap::new();

    let mut deliveries: Vec<_> = deliveries.iter().collect();
    deliveries.sort_by_key(|(entity, _, seq)| seq.map_or(entity.to_bits(), |seq| seq.0));
    for (message_entity, delivery, _) in deliveries {
        commands.entity(message_entity).despawn();

        let Some(task_entity) = index.get_task(&delivery.task_id) else {
            debug!(
                event = "InboxDeliveryDropped",
                task_id = %delivery.task_id,
                reason = "task_not_found",
                "inbox delivery dropped"
            );
            continue;
        };
        let Ok((task, inbox)) = tasks.get_mut(task_entity) else {
            continue;
        };
        if task.status.is_terminal() {
            debug!(
                event = "InboxDeliveryDropped",
                task_id = %delivery.task_id,
                reason = "task_terminal",
                "inbox delivery dropped"
            );
            continue;
        }

        debug!(
            event = "InboxDelivered",
            task_id = %delivery.task_id,
            is_user_steer = delivery.item.is_user_steer(),
            "inbox item delivered"
        );
        match inbox {
            Some(mut inbox) => inbox.push(delivery.item.clone()),
            None => new_inboxes
                .entry(task_entity)
                .or_default()
                .push(delivery.item.clone()),
        }
    }

    for (task_entity, inbox) in new_inboxes {
        commands.entity(task_entity).insert(inbox);
    }
}

/// 任务是否处于可被收件箱唤醒的空闲态。
fn is_idle_for_wake(task: &Task) -> bool {
    match &task.status {
        TaskStatus::Waiting(WaitingReason::User) => task.pending_confirmation_id.is_none(),
        // 有输出/steer 时立即处理；后台任务是否结束只约束自然最终完成。
        TaskStatus::Waiting(WaitingReason::BackgroundJobs) => true,
        _ => false,
    }
}

/// 空闲唤醒：任务空闲且收件箱有积压时续一轮。
///
/// - 含用户 steer 的积压立即唤醒，不占用或清空自动拉起额度；
/// - 仅含后台通知的积压从首条开始等待固定合并窗口，并受滚动窗口额度约束；
///   超限只在内部记录延期，额度到期后自动恢复。
#[allow(clippy::type_complexity)]
pub(crate) fn inbox_wake_system(
    mut commands: Commands,
    clock: Res<Clock>,
    config: Res<LoopControlConfig>,
    mut tasks: Query<
        (
            Entity,
            &Task,
            &mut TaskInbox,
            Option<&mut AutoWakeBudget>,
            Option<&OwnedJobs>,
            Option<&DeferredResult>,
        ),
        Without<AskUserPending>,
    >,
) {
    for (task_entity, task, mut inbox, budget, owned_jobs, deferred) in &mut tasks {
        if inbox.is_empty() || !is_idle_for_wake(task) {
            continue;
        }

        let has_steer = inbox.has_user_steer();
        let mut budget_value = budget.as_deref().cloned().unwrap_or_default();

        if !has_steer {
            let coalesce = chrono::Duration::milliseconds(
                i64::try_from(config.notification_coalesce_ms).unwrap_or(i64::MAX),
            );
            if inbox
                .first_received_at()
                .is_some_and(|first| clock.0 - first < coalesce)
            {
                continue;
            }
            if !budget_value.try_record_wake(clock.0, &config) {
                if !budget_value.exhausted_notified {
                    budget_value.exhausted_notified = true;
                    debug!(
                        event = "AutoWakeBudgetExhausted",
                        task_id = %task.id,
                        pending = inbox.len(),
                        max_auto_wakes = config.max_auto_wakes,
                        window_secs = config.auto_wake_window_secs,
                        "auto wake rate limit reached; holding notifications until the window expires"
                    );
                }
                write_budget(&mut commands, task_entity, budget, budget_value);
                continue;
            }
        }
        let auto_wakes = budget_value.recent_wakes.len();
        write_budget(&mut commands, task_entity, budget, budget_value);

        let items: Vec<InboxItem> = inbox.drain_all();
        let mut user_input = String::new();
        if let Some(DeferredResult(result)) = deferred {
            let instruction = if owned_jobs.is_some_and(|jobs| !jobs.jobs().is_empty()) {
                "后台任务仍运行，请结合新通知继续处理；最终完成仍需等待后台任务结束"
            } else {
                "请结合下列后台结果给出最终结果"
            };
            user_input.push_str(&format!(
                "[你此前在后台任务未结束时给出过候选结果，{instruction}]\n候选结果：\n{result}\n\n"
            ));
            commands.entity(task_entity).remove::<DeferredResult>();
        }
        user_input.push_str(&render_inbox_items(&items));

        debug!(
            event = "InboxWake",
            task_id = %task.id,
            item_count = items.len(),
            has_steer = has_steer,
            auto_wakes,
            "waking idle task with inbox items"
        );
        let mut message = commands.spawn(ContinueTaskMessage {
            task_id: task.id,
            user_input,
        });
        if !has_steer {
            message.insert(crate::domain::BackgroundReportTurn);
        }
    }
}

/// 收件箱积压数量同步到前端（TUI 展示排队数量）。
///
/// 依赖 `Changed<TaskInbox>`：push（ingest）与 drain（注入管道 / 唤醒）都经 `&mut` 访问，
/// 均会触发变更检测，单点覆盖所有出入口。
pub(crate) fn inbox_status_sync_system(
    registry: Option<Res<FrontendRegistry>>,
    inboxes: Query<(&Task, &TaskInbox), Changed<TaskInbox>>,
) {
    let Some(registry) = registry else {
        return;
    };
    for (task, inbox) in &inboxes {
        let Some(channel) = task.routing_policy.output_channel().cloned() else {
            continue;
        };
        let event = EngineEvent::InboxChanged {
            target: EventTarget::Directed(vec![channel]),
            task_id: task.id,
            pending: inbox.len(),
        };
        for frontend in &registry.frontends {
            frontend.push_event(event.clone());
        }
    }
}

fn write_budget(
    commands: &mut Commands,
    task_entity: Entity,
    budget: Option<Mut<AutoWakeBudget>>,
    value: AutoWakeBudget,
) {
    match budget {
        Some(mut existing) => *existing = value,
        None => {
            commands.entity(task_entity).insert(value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{ChannelId, FrontendKind, SystemOutputMessage, TaskId};

    fn channel() -> ChannelId {
        ChannelId {
            frontend: FrontendKind::Tui,
            user_id: "local".to_string(),
            thread_id: None,
        }
    }

    fn steer(content: &str) -> InboxItem {
        InboxItem::UserSteer {
            content: content.to_string(),
            received_at: chrono::Utc::now(),
        }
    }

    fn notification(at: chrono::DateTime<chrono::Utc>) -> InboxItem {
        InboxItem::TaskNotification {
            job_id: uuid::Uuid::new_v4(),
            source: "shell".to_string(),
            status: "exited(0)".to_string(),
            summary: "ok".to_string(),
            received_at: at,
        }
    }

    fn app_with_task(status: TaskStatus) -> (App, Entity, TaskId) {
        let mut app = App::new();
        app.insert_resource(Clock::default());
        app.insert_resource(LoopControlConfig::default());
        app.insert_resource(EntityIndex::default());
        let mut task = Task::from_user_input("goal", 3, channel());
        let now = chrono::Utc::now();
        match status {
            TaskStatus::Waiting(reason) => task.mark_waiting(reason, now),
            TaskStatus::Running => task.mark_running(now),
            TaskStatus::Done => task.mark_done("done", now),
            other => panic!("unsupported test status {other:?}"),
        }
        let task_id = task.id;
        let entity = app.world_mut().spawn(task).id();
        app.world_mut()
            .resource_mut::<EntityIndex>()
            .tasks
            .insert(task_id, entity);
        (app, entity, task_id)
    }

    fn continue_inputs(app: &mut App) -> Vec<String> {
        app.world_mut()
            .query::<&ContinueTaskMessage>()
            .iter(app.world())
            .map(|m| m.user_input.clone())
            .collect()
    }

    #[test]
    fn inbox_status_sync_reports_pending_count_on_push_and_drain() {
        use std::sync::{Arc, Mutex};

        use crate::domain::{Frontend, UserAction};

        struct RecordingFrontend(Arc<Mutex<Vec<EngineEvent>>>);
        impl Frontend for RecordingFrontend {
            fn kind(&self) -> FrontendKind {
                FrontendKind::Tui
            }
            fn push_event(&self, event: EngineEvent) {
                self.0.lock().unwrap().push(event);
            }
            fn poll_actions(&self) -> Vec<UserAction> {
                vec![]
            }
        }

        let (mut app, entity, task_id) =
            app_with_task(TaskStatus::Waiting(WaitingReason::ToolExecution));
        let events = Arc::new(Mutex::new(Vec::new()));
        app.insert_resource(FrontendRegistry {
            frontends: vec![Box::new(RecordingFrontend(events.clone()))],
        });
        app.add_systems(
            Update,
            (inbox_ingest_system, inbox_status_sync_system).chain(),
        );
        app.world_mut().spawn(InboxDeliveryMessage {
            task_id,
            item: steer("queued"),
        });
        app.update();
        app.world_mut()
            .get_mut::<TaskInbox>(entity)
            .unwrap()
            .drain_all();
        app.update();
        app.update();

        let pending: Vec<usize> = events
            .lock()
            .unwrap()
            .iter()
            .filter_map(|e| match e {
                EngineEvent::InboxChanged { pending, .. } => Some(*pending),
                _ => None,
            })
            .collect();
        assert_eq!(
            pending,
            vec![1, 0],
            "push then drain, no event without change"
        );
    }

    #[test]
    fn ingest_aggregates_same_frame_deliveries_without_overwrite() {
        let (mut app, entity, task_id) =
            app_with_task(TaskStatus::Waiting(WaitingReason::ToolExecution));
        app.add_systems(Update, inbox_ingest_system);
        for content in ["a", "b"] {
            app.world_mut().spawn(InboxDeliveryMessage {
                task_id,
                item: steer(content),
            });
        }
        app.update();

        let inbox = app
            .world()
            .get::<TaskInbox>(entity)
            .expect("inbox inserted");
        assert_eq!(inbox.len(), 2);
        let remaining = app
            .world_mut()
            .query::<&InboxDeliveryMessage>()
            .iter(app.world())
            .count();
        assert_eq!(remaining, 0, "delivery messages are consumed");
    }

    #[test]
    fn ingest_drops_delivery_for_terminal_task() {
        let (mut app, entity, task_id) = app_with_task(TaskStatus::Done);
        app.add_systems(Update, inbox_ingest_system);
        app.world_mut().spawn(InboxDeliveryMessage {
            task_id,
            item: steer("late"),
        });
        app.update();
        assert!(app.world().get::<TaskInbox>(entity).is_none());
    }

    #[test]
    fn wake_continues_idle_task_with_steer_immediately() {
        let (mut app, entity, _) = app_with_task(TaskStatus::Waiting(WaitingReason::User));
        app.add_systems(Update, inbox_wake_system);
        let mut inbox = TaskInbox::default();
        inbox.push(steer("queued after final reply"));
        app.world_mut().entity_mut(entity).insert(inbox);
        app.update();

        let inputs = continue_inputs(&mut app);
        assert_eq!(inputs.len(), 1);
        assert!(inputs[0].contains("queued after final reply"));
        assert!(app.world().get::<TaskInbox>(entity).unwrap().is_empty());

        app.update();
        assert_eq!(continue_inputs(&mut app).len(), 1, "woken exactly once");
    }

    #[test]
    fn wake_skips_task_still_in_loop() {
        let (mut app, entity, _) = app_with_task(TaskStatus::Running);
        app.add_systems(Update, inbox_wake_system);
        let mut inbox = TaskInbox::default();
        inbox.push(steer("x"));
        app.world_mut().entity_mut(entity).insert(inbox);
        app.update();
        assert!(continue_inputs(&mut app).is_empty());
        assert_eq!(app.world().get::<TaskInbox>(entity).unwrap().len(), 1);
    }

    #[test]
    fn notification_waits_for_coalesce_window() {
        let (mut app, entity, _) = app_with_task(TaskStatus::Waiting(WaitingReason::User));
        app.add_systems(Update, inbox_wake_system);
        let now = app.world().resource::<Clock>().0;
        let mut inbox = TaskInbox::default();
        inbox.push(notification(now));
        app.world_mut().entity_mut(entity).insert(inbox);
        app.update();
        assert!(
            continue_inputs(&mut app).is_empty(),
            "inside coalesce window"
        );

        app.world_mut().resource_mut::<Clock>().0 = now + chrono::Duration::seconds(1);
        app.update();
        let inputs = continue_inputs(&mut app);
        assert_eq!(inputs.len(), 1);
        assert!(inputs[0].contains("<task-notification"));
        assert_eq!(
            app.world()
                .get::<AutoWakeBudget>(entity)
                .unwrap()
                .recent_wakes
                .len(),
            1
        );
    }

    #[test]
    fn notification_wake_respects_budget_without_user_status_noise() {
        let (mut app, entity, _) = app_with_task(TaskStatus::Waiting(WaitingReason::User));
        app.add_systems(Update, inbox_wake_system);
        let max = app.world().resource::<LoopControlConfig>().max_auto_wakes;
        let past = app.world().resource::<Clock>().0 - chrono::Duration::seconds(5);
        let mut inbox = TaskInbox::default();
        inbox.push(notification(past));
        app.world_mut().entity_mut(entity).insert((
            inbox,
            AutoWakeBudget {
                recent_wakes: std::iter::repeat_n(past, max as usize).collect(),
                exhausted_notified: false,
            },
        ));
        app.update();
        app.update();

        assert!(continue_inputs(&mut app).is_empty());
        let notices = app
            .world_mut()
            .query::<&SystemOutputMessage>()
            .iter(app.world())
            .count();
        assert_eq!(
            notices, 0,
            "rate limiting stays internal while notifications await automatic recovery"
        );
        assert_eq!(app.world().get::<TaskInbox>(entity).unwrap().len(), 1);
    }

    #[test]
    fn steer_does_not_consume_or_clear_budget_and_includes_held_notifications() {
        let (mut app, entity, _) = app_with_task(TaskStatus::Waiting(WaitingReason::User));
        app.add_systems(Update, inbox_wake_system);
        let past = app.world().resource::<Clock>().0 - chrono::Duration::seconds(5);
        let mut inbox = TaskInbox::default();
        inbox.push(notification(past));
        inbox.push(steer("now continue"));
        app.world_mut().entity_mut(entity).insert((
            inbox,
            AutoWakeBudget {
                recent_wakes: std::iter::repeat_n(past, 5).collect(),
                exhausted_notified: true,
            },
        ));
        app.update();

        let inputs = continue_inputs(&mut app);
        assert_eq!(inputs.len(), 1);
        assert!(inputs[0].contains("<task-notification") && inputs[0].contains("now continue"));
        assert_eq!(
            app.world()
                .get::<AutoWakeBudget>(entity)
                .unwrap()
                .recent_wakes
                .len(),
            5
        );
    }

    #[test]
    fn background_jobs_wait_includes_deferred_result_once_jobs_finish() {
        let (mut app, entity, _) =
            app_with_task(TaskStatus::Waiting(WaitingReason::BackgroundJobs));
        app.add_systems(Update, inbox_wake_system);
        let past = app.world().resource::<Clock>().0 - chrono::Duration::seconds(5);
        let mut inbox = TaskInbox::default();
        inbox.push(notification(past));
        app.world_mut()
            .entity_mut(entity)
            .insert((inbox, DeferredResult("draft answer".to_string())));
        app.update();

        let inputs = continue_inputs(&mut app);
        assert_eq!(inputs.len(), 1);
        assert!(inputs[0].contains("draft answer"));
        assert!(app.world().get::<DeferredResult>(entity).is_none());
    }

    #[test]
    fn continuous_notifications_do_not_extend_the_first_coalesce_deadline() {
        let (mut app, entity, _) = app_with_task(TaskStatus::Waiting(WaitingReason::User));
        app.add_systems(Update, inbox_wake_system);
        let now = app.world().resource::<Clock>().0;
        let mut inbox = TaskInbox::default();
        inbox.push(notification(now));
        app.world_mut().entity_mut(entity).insert(inbox);
        app.update();
        app.world_mut()
            .get_mut::<TaskInbox>(entity)
            .unwrap()
            .push(notification(now + chrono::Duration::milliseconds(250)));
        app.world_mut().resource_mut::<Clock>().0 = now + chrono::Duration::milliseconds(300);
        app.update();
        assert_eq!(
            continue_inputs(&mut app).len(),
            1,
            "first pending notification fixes the deadline"
        );
    }

    #[test]
    fn sixth_and_tenth_notifications_resume_without_user_input() {
        let (mut app, entity, _) = app_with_task(TaskStatus::Waiting(WaitingReason::User));
        app.add_systems(Update, inbox_wake_system);
        app.world_mut()
            .resource_mut::<LoopControlConfig>()
            .notification_coalesce_ms = 0;
        let now = app.world().resource::<Clock>().0;
        app.world_mut()
            .entity_mut(entity)
            .insert(TaskInbox::default());
        for index in 0..5 {
            let at = now + chrono::Duration::seconds(index);
            app.world_mut().resource_mut::<Clock>().0 = at;
            app.world_mut()
                .get_mut::<TaskInbox>(entity)
                .unwrap()
                .push(notification(at));
            app.update();
        }
        assert_eq!(continue_inputs(&mut app).len(), 5);
        let sixth = now + chrono::Duration::seconds(5);
        app.world_mut().resource_mut::<Clock>().0 = sixth;
        app.world_mut()
            .get_mut::<TaskInbox>(entity)
            .unwrap()
            .push(notification(sixth));
        app.update();
        assert_eq!(
            continue_inputs(&mut app).len(),
            5,
            "sixth wake is held inside the window"
        );
        app.world_mut().resource_mut::<Clock>().0 = now + chrono::Duration::seconds(10);
        app.update();
        assert_eq!(
            continue_inputs(&mut app).len(),
            6,
            "held wake resumes when the oldest slot expires"
        );
        for second in 11..15 {
            let at = now + chrono::Duration::seconds(second);
            app.world_mut().resource_mut::<Clock>().0 = at;
            app.world_mut()
                .get_mut::<TaskInbox>(entity)
                .unwrap()
                .push(notification(at));
            app.update();
        }
        assert_eq!(continue_inputs(&mut app).len(), 10);
    }

    #[test]
    fn output_notification_wakes_while_the_owned_process_is_still_running() {
        use crate::domain::{BackgroundJob, JobOwner, JobSource, SessionHandleId};
        let (mut app, entity, task_id) =
            app_with_task(TaskStatus::Waiting(WaitingReason::BackgroundJobs));
        app.add_systems(Update, inbox_wake_system);
        let now = app.world().resource::<Clock>().0;
        app.world_mut().spawn((
            BackgroundJob {
                id: uuid::Uuid::new_v4(),
                owner_task_id: task_id,
                source: JobSource::Shell {
                    notify_on_exit: true,
                    handle_id: SessionHandleId::new(),
                },
                started_at: now,
                claimed: false,
            },
            JobOwner(entity),
        ));
        let mut inbox = TaskInbox::default();
        inbox.push(notification(now - chrono::Duration::seconds(1)));
        app.world_mut().entity_mut(entity).insert(inbox);
        app.update();
        assert_eq!(
            continue_inputs(&mut app).len(),
            1,
            "process completion is not a prerequisite to process an event"
        );
        assert_eq!(
            app.world().get::<OwnedJobs>(entity).unwrap().jobs().len(),
            1
        );
    }
}
