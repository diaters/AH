//! 回合收件箱：投递写入与空闲唤醒
//!
//! - `inbox_ingest_system`：`TaskInbox` 的唯一写入点，消费 `InboxDeliveryMessage`。
//! - `inbox_wake_system`：任务空闲时取出积压，经 `ContinueTaskMessage` 走既有续轮链路。

use std::collections::HashMap;

use tracing::debug;

use crate::contracts::Clock;
use crate::domain::{
    AskUserPending, AutoWakeBudget, ContinueTaskMessage, DeferredResult, InboxDeliveryMessage,
    InboxItem, LoopControlConfig, OwnedJobs, SystemOutputMessage, Task, TaskInbox, TaskStatus,
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
    deliveries: Query<(Entity, &InboxDeliveryMessage)>,
    mut tasks: Query<(&Task, Option<&mut TaskInbox>)>,
) {
    let mut new_inboxes: HashMap<Entity, TaskInbox> = HashMap::new();

    for (message_entity, delivery) in &deliveries {
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
fn is_idle_for_wake(task: &Task, owned_jobs: Option<&OwnedJobs>) -> bool {
    match &task.status {
        TaskStatus::Waiting(WaitingReason::User) => task.pending_confirmation_id.is_none(),
        TaskStatus::Waiting(WaitingReason::BackgroundJobs) => {
            owned_jobs.is_none_or(|jobs| jobs.jobs().is_empty())
        }
        _ => false,
    }
}

/// 空闲唤醒：任务空闲且收件箱有积压时续一轮。
///
/// - 含用户 steer 的积压立即唤醒，并清零自动拉起计数；
/// - 仅含后台通知的积压需等待合并窗口，且受 `max_auto_wakes` 预算约束，
///   超出上限只提示用户，积压保留到用户下一条输入。
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
        if inbox.is_empty() || !is_idle_for_wake(task, owned_jobs) {
            continue;
        }

        let has_steer = inbox.has_user_steer();
        let mut budget_value = budget.as_deref().copied().unwrap_or_default();

        if !has_steer {
            let coalesce = chrono::Duration::milliseconds(config.notification_coalesce_ms as i64);
            if inbox
                .last_received_at()
                .is_some_and(|last| clock.0 - last < coalesce)
            {
                continue;
            }
            if budget_value.consecutive >= config.max_auto_wakes {
                if !budget_value.exhausted_notified {
                    budget_value.exhausted_notified = true;
                    commands.spawn(SystemOutputMessage {
                        task_id: task.id,
                        content: format!(
                            "有 {} 条后台通知待处理，已达到连续自动拉起上限，将随你的下一条输入一并送达。",
                            inbox.len()
                        ),
                    });
                    debug!(
                        event = "AutoWakeBudgetExhausted",
                        task_id = %task.id,
                        pending = inbox.len(),
                        max_auto_wakes = config.max_auto_wakes,
                        "auto wake budget exhausted, holding notifications"
                    );
                }
                write_budget(&mut commands, task_entity, budget, budget_value);
                continue;
            }
            budget_value.consecutive += 1;
        } else {
            budget_value = AutoWakeBudget::default();
        }
        write_budget(&mut commands, task_entity, budget, budget_value);

        let items: Vec<InboxItem> = inbox.drain_all();
        let mut user_input = String::new();
        if let Some(DeferredResult(result)) = deferred {
            user_input.push_str(&format!(
                "[你此前在后台任务未结束时给出过候选结果，请结合下列后台结果给出最终结果]\n候选结果：\n{result}\n\n"
            ));
            commands.entity(task_entity).remove::<DeferredResult>();
        }
        user_input.push_str(&render_inbox_items(&items));

        debug!(
            event = "InboxWake",
            task_id = %task.id,
            item_count = items.len(),
            has_steer = has_steer,
            auto_wakes = budget_value.consecutive,
            "waking idle task with inbox items"
        );
        commands.spawn(ContinueTaskMessage {
            task_id: task.id,
            user_input,
        });
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
    use crate::domain::{ChannelId, FrontendKind, TaskId};

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
                .consecutive,
            1
        );
    }

    #[test]
    fn notification_wake_respects_budget_and_notifies_once() {
        let (mut app, entity, _) = app_with_task(TaskStatus::Waiting(WaitingReason::User));
        app.add_systems(Update, inbox_wake_system);
        let max = app.world().resource::<LoopControlConfig>().max_auto_wakes;
        let past = app.world().resource::<Clock>().0 - chrono::Duration::seconds(5);
        let mut inbox = TaskInbox::default();
        inbox.push(notification(past));
        app.world_mut().entity_mut(entity).insert((
            inbox,
            AutoWakeBudget {
                consecutive: max,
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
        assert_eq!(notices, 1, "exhaustion notice sent once");
        assert_eq!(app.world().get::<TaskInbox>(entity).unwrap().len(), 1);
    }

    #[test]
    fn steer_resets_budget_and_includes_held_notifications() {
        let (mut app, entity, _) = app_with_task(TaskStatus::Waiting(WaitingReason::User));
        app.add_systems(Update, inbox_wake_system);
        let past = app.world().resource::<Clock>().0 - chrono::Duration::seconds(5);
        let mut inbox = TaskInbox::default();
        inbox.push(notification(past));
        inbox.push(steer("now continue"));
        app.world_mut().entity_mut(entity).insert((
            inbox,
            AutoWakeBudget {
                consecutive: 99,
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
                .consecutive,
            0
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
}
