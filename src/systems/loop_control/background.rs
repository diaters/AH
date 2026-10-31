//! 后台任务：登记、完成通知与终态清理。
//!
//! - `background_job_monitor_system`：节流检查后台任务完成（shell 会话退出 / 子任务批次全部终态），
//!   未被 `wait_tasks` 认领的完成事件以 `TaskNotification` 投递到所属任务收件箱；
//! - `background_job_cleanup_system`：保证"终态 Task 不留存活后台任务"不变量——
//!   所属任务终态或已被删除时强制停止 shell 会话、级联取消子任务。

use tracing::debug;

use crate::contracts::Clock;
use crate::domain::{
    BackgroundJob, FailureReason, InboxDeliveryMessage, InboxItem, JobOwner, JobSource,
    LoopControlConfig, SessionBackend, SessionReadRequest, SessionStatus, Task, TaskId, TaskStatus,
    WaitingForTasksInfo,
};
use crate::ecs::EntityIndex;
use crate::prelude::*;
use crate::systems::NativeProcessBackend;
use crate::systems::tools::backend::native::is_session_not_found;

/// 通知中携带的 shell 输出末尾行数。
const NOTIFICATION_TAIL_LINES: usize = 20;

/// 为所属任务登记一个后台任务实体。
pub(crate) fn spawn_background_job(
    commands: &mut Commands,
    owner_entity: Entity,
    owner_task_id: TaskId,
    source: JobSource,
    now: chrono::DateTime<chrono::Utc>,
) {
    debug!(
        event = "BackgroundJobRegistered",
        task_id = %owner_task_id,
        source = source.label(),
        "background job registered"
    );
    commands.spawn((
        BackgroundJob {
            id: uuid::Uuid::new_v4(),
            owner_task_id,
            source,
            started_at: now,
            claimed: false,
        },
        JobOwner(owner_entity),
    ));
}

/// 子任务批次的完成摘要；任一子任务未终态时返回 None。
fn batch_completion(
    task_ids: &[TaskId],
    index: &EntityIndex,
    tasks: &Query<&Task>,
) -> Option<(String, String)> {
    let mut lines = Vec::with_capacity(task_ids.len());
    let mut failed = 0usize;
    for task_id in task_ids {
        match index.get_task(task_id).and_then(|e| tasks.get(e).ok()) {
            Some(task) if !task.status.is_terminal() => return None,
            Some(task) => {
                let name = if task.input_summary.is_empty() {
                    task.id.to_string()
                } else {
                    task.input_summary.clone()
                };
                match &task.status {
                    TaskStatus::Done => {
                        lines.push(format!("- {name} [done]: {}", task.result_summary));
                    }
                    other => {
                        failed += 1;
                        lines.push(format!(
                            "- {name} [{other:?}]: {}",
                            task.last_error().unwrap_or_default()
                        ));
                    }
                }
            }
            None => lines.push(format!("- {task_id} [unknown]")),
        }
    }
    let status = if failed == 0 {
        "completed".to_string()
    } else {
        format!("completed_with_{failed}_failed")
    };
    Some((status, lines.join("\n")))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn background_job_monitor_system(
    mut commands: Commands,
    clock: Res<Clock>,
    config: Res<LoopControlConfig>,
    mut last_poll: Local<Option<chrono::DateTime<chrono::Utc>>>,
    backend: Option<Res<NativeProcessBackend>>,
    index: Res<EntityIndex>,
    tasks: Query<&Task>,
    waiting: Query<&WaitingForTasksInfo>,
    mut jobs: Query<(Entity, &mut BackgroundJob, &JobOwner)>,
    mut demand: Option<ResMut<crate::systems::demand::DemandSchedule>>,
) {
    let interval = chrono::Duration::milliseconds(
        config.background_poll_ms.max(1).min(i64::MAX as u64) as i64,
    );
    if demand.is_none() && last_poll.is_some_and(|last| clock.0 - last < interval) {
        return;
    }
    let poll_shell = if let Some(schedule) = demand.as_deref_mut() {
        let has_shell = jobs
            .iter()
            .any(|(_, job, _)| matches!(job.source, JobSource::Shell { .. }));
        has_shell && schedule.background_ready(clock.0, interval)
    } else {
        true
    };
    if poll_shell || demand.is_none() {
        *last_poll = Some(clock.0);
    }

    for (job_entity, mut job, owner) in &mut jobs {
        if !poll_shell && matches!(job.source, JobSource::Shell { .. }) {
            continue;
        }
        // wait_tasks 认领：父任务正显式等待该批次中的任务时，结果走 wait_tasks 返回，不再通知
        let should_claim = match &job.source {
            JobSource::SubTaskBatch { task_ids, .. } => {
                !job.claimed
                    && waiting.get(owner.0).is_ok_and(|info| {
                        info.target_task_ids.iter().any(|id| task_ids.contains(id))
                    })
            }
            JobSource::Shell { .. } => false,
        };
        if should_claim {
            debug!(
                event = "BackgroundJobClaimed",
                task_id = %job.owner_task_id,
                "sub-task batch claimed by wait_tasks"
            );
            job.claimed = true;
        }

        let completion = match &job.source {
            JobSource::Shell {
                handle_id,
                notify_on_exit,
            } => {
                let Some(backend) = backend.as_ref() else {
                    continue;
                };
                match backend.read_session(SessionReadRequest {
                    handle_id: *handle_id,
                    tail_lines: NOTIFICATION_TAIL_LINES,
                }) {
                    Ok(summary) => match summary.status {
                        SessionStatus::Starting
                        | SessionStatus::Running
                        | SessionStatus::WaitingForInput => None,
                        // LLM 主动 shell_stop：无需再通知
                        SessionStatus::Stopped => Some(None),
                        SessionStatus::Completed
                        | SessionStatus::ExitedWithError
                        | SessionStatus::FailedToStart => {
                            if !notify_on_exit {
                                commands.entity(job_entity).despawn();
                                continue;
                            }
                            let status = match summary.exit_code {
                                Some(code) => format!("exited({code})"),
                                None => format!("{:?}", summary.status).to_lowercase(),
                            };
                            Some(Some((
                                status,
                                format!(
                                    "session_id: {handle_id}\ncommand: {}\n--- output (last {NOTIFICATION_TAIL_LINES} lines) ---\n{}",
                                    summary.command, summary.output.output
                                ),
                            )))
                        }
                    },
                    Err(error) => match backend.lost_session_reason(*handle_id) {
                        // 进程服务失效回收了该会话：无论 notify_on_exit，都告知所属任务监控已中断。
                        Some(reason) => Some(Some((
                            "lost".to_string(),
                            format!(
                                "session_id: {handle_id}\n进程服务故障，该后台会话已被终止且无法恢复；如仍需要请重新启动。\nreason: {reason}"
                            ),
                        ))),
                        // 会话已结束且记录被淘汰：静默清理
                        None if is_session_not_found(&error) => Some(None),
                        // 单次调用失败（超时、服务重启中）：保留任务，下次轮询再确认
                        None => {
                            debug!(
                                event = "BackgroundJobPollDeferred",
                                task_id = %job.owner_task_id,
                                error_type = "ProcessBackend",
                                error = %error,
                                "background session status unavailable, retrying on next poll"
                            );
                            None
                        }
                    },
                }
            }
            JobSource::SubTaskBatch { task_ids, .. } => {
                batch_completion(task_ids, &index, &tasks).map(Some)
            }
        };

        let Some(notification) = completion else {
            continue;
        };
        commands.entity(job_entity).despawn();

        match notification {
            Some((status, summary)) if !job.claimed => {
                debug!(
                    event = "BackgroundJobCompleted",
                    task_id = %job.owner_task_id,
                    source = job.source.label(),
                    status = %status,
                    "background job completed, notifying owner task"
                );
                commands.spawn(InboxDeliveryMessage {
                    task_id: job.owner_task_id,
                    item: InboxItem::TaskNotification {
                        job_id: job.id,
                        source: job.source.label().to_string(),
                        status,
                        summary,
                        received_at: clock.0,
                    },
                });
            }
            _ => {
                debug!(
                    event = "BackgroundJobRetiredSilently",
                    task_id = %job.owner_task_id,
                    source = job.source.label(),
                    claimed = job.claimed,
                    "background job finished without notification"
                );
            }
        }
    }
}

/// 终态清理：所属任务终态或已删除时强制停止后台任务。
pub(crate) fn background_job_cleanup_system(
    mut commands: Commands,
    clock: Res<Clock>,
    backend: Option<Res<NativeProcessBackend>>,
    index: Res<EntityIndex>,
    jobs: Query<(Entity, &BackgroundJob, Option<&JobOwner>)>,
    mut tasks: Query<&mut Task>,
) {
    for (job_entity, job, owner) in &jobs {
        let owner_alive = owner
            .and_then(|o| tasks.get(o.0).ok())
            .is_some_and(|t| !t.status.is_terminal());
        if owner_alive {
            continue;
        }

        match &job.source {
            JobSource::Shell { handle_id, .. } => {
                if let Some(backend) = backend.as_ref() {
                    let _ = backend.stop_session(*handle_id);
                }
            }
            JobSource::SubTaskBatch { task_ids, .. } => {
                for task_id in task_ids {
                    if let Some(mut child) =
                        index.get_task(task_id).and_then(|e| tasks.get_mut(e).ok())
                        && !child.status.is_terminal()
                    {
                        child.mark_failed_reason(
                            FailureReason::UserCancelled,
                            "owner task ended while background sub-task was running",
                            clock.0,
                        );
                    }
                }
            }
        }
        debug!(
            event = "BackgroundJobForceStopped",
            task_id = %job.owner_task_id,
            source = job.source.label(),
            "owner task terminal or removed; background job force-stopped"
        );
        commands.entity(job_entity).despawn();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{AgentId, ChannelId, FrontendKind, OwnedJobs, WaitingReason};

    fn channel() -> ChannelId {
        ChannelId {
            frontend: FrontendKind::Tui,
            user_id: "local".to_string(),
            thread_id: None,
        }
    }

    struct Fixture {
        app: App,
        owner: Entity,
        owner_id: TaskId,
        children: Vec<(Entity, TaskId)>,
    }

    fn fixture() -> Fixture {
        let mut app = App::new();
        app.insert_resource(Clock::default());
        app.insert_resource(LoopControlConfig::default());
        app.insert_resource(EntityIndex::default());
        let mut owner_task = Task::from_user_input("parent", 3, channel());
        owner_task.mark_waiting(WaitingReason::User, chrono::Utc::now());
        let owner_id = owner_task.id;
        let owner = app.world_mut().spawn(owner_task).id();
        app.world_mut()
            .resource_mut::<EntityIndex>()
            .tasks
            .insert(owner_id, owner);
        let mut children = Vec::new();
        for name in ["a", "b"] {
            let mut child = Task::from_user_input("child", 3, channel());
            child.parent_task_id = Some(owner_id);
            child.input_summary = name.to_string();
            child.mark_running(chrono::Utc::now());
            let id = child.id;
            let entity = app.world_mut().spawn(child).id();
            app.world_mut()
                .resource_mut::<EntityIndex>()
                .tasks
                .insert(id, entity);
            children.push((entity, id));
        }
        let task_ids = children.iter().map(|(_, id)| *id).collect();
        let mut commands_queue = app.world_mut().commands();
        spawn_background_job(
            &mut commands_queue,
            owner,
            owner_id,
            JobSource::SubTaskBatch {
                batch_id: uuid::Uuid::new_v4(),
                task_ids,
            },
            chrono::Utc::now(),
        );
        app.world_mut().flush();
        Fixture {
            app,
            owner,
            owner_id,
            children,
        }
    }

    fn deliveries(app: &mut App) -> Vec<InboxDeliveryMessage> {
        app.world_mut()
            .query::<&InboxDeliveryMessage>()
            .iter(app.world())
            .cloned()
            .collect()
    }

    fn job_count(app: &mut App) -> usize {
        app.world_mut()
            .query::<&BackgroundJob>()
            .iter(app.world())
            .count()
    }

    fn tick(app: &mut App) {
        let next = app.world().resource::<Clock>().0 + chrono::Duration::seconds(1);
        app.world_mut().resource_mut::<Clock>().0 = next;
        app.update();
    }

    #[test]
    fn spawned_job_is_tracked_by_owner_relationship() {
        let f = fixture();
        let owned = f.app.world().get::<OwnedJobs>(f.owner).unwrap();
        assert_eq!(owned.jobs().len(), 1);
    }

    #[test]
    fn batch_notifies_owner_only_when_all_children_terminal() {
        let mut f = fixture();
        f.app.add_systems(Update, background_job_monitor_system);
        tick(&mut f.app);
        assert!(deliveries(&mut f.app).is_empty());

        let (first, _) = f.children[0];
        let (second, _) = f.children[1];
        f.app
            .world_mut()
            .get_mut::<Task>(first)
            .unwrap()
            .mark_done("result a", chrono::Utc::now());
        tick(&mut f.app);
        assert!(deliveries(&mut f.app).is_empty(), "one child still running");

        f.app
            .world_mut()
            .get_mut::<Task>(second)
            .unwrap()
            .mark_failed_reason(FailureReason::AgentError, "boom", chrono::Utc::now());
        tick(&mut f.app);
        let delivered = deliveries(&mut f.app);
        assert_eq!(delivered.len(), 1);
        assert_eq!(delivered[0].task_id, f.owner_id);
        match &delivered[0].item {
            InboxItem::TaskNotification {
                source,
                status,
                summary,
                ..
            } => {
                assert_eq!(source, "subtasks");
                assert_eq!(status, "completed_with_1_failed");
                assert!(summary.contains("a [done]: result a"));
                assert!(summary.contains("boom"));
            }
            other => panic!("unexpected item {other:?}"),
        }
        assert_eq!(job_count(&mut f.app), 0);
    }

    #[test]
    fn batch_claimed_by_wait_tasks_finishes_silently() {
        let mut f = fixture();
        f.app.add_systems(Update, background_job_monitor_system);
        let targets: Vec<TaskId> = f.children.iter().map(|(_, id)| *id).collect();
        f.app
            .world_mut()
            .entity_mut(f.owner)
            .insert(WaitingForTasksInfo {
                target_task_ids: targets,
                timeout_at: chrono::Utc::now() + chrono::Duration::minutes(5),
                tool_call_id: "wait".to_string(),
                agent_id: AgentId::nil(),
            });
        tick(&mut f.app);
        for (entity, _) in f.children.clone() {
            f.app
                .world_mut()
                .get_mut::<Task>(entity)
                .unwrap()
                .mark_done("ok", chrono::Utc::now());
        }
        tick(&mut f.app);
        assert!(deliveries(&mut f.app).is_empty());
        assert_eq!(job_count(&mut f.app), 0);
    }

    #[test]
    fn cleanup_cancels_children_when_owner_terminal() {
        let mut f = fixture();
        f.app.add_systems(Update, background_job_cleanup_system);
        tick(&mut f.app);
        assert_eq!(job_count(&mut f.app), 1, "live owner keeps its job");

        f.app
            .world_mut()
            .get_mut::<Task>(f.owner)
            .unwrap()
            .mark_done("finished by user", chrono::Utc::now());
        tick(&mut f.app);
        assert_eq!(job_count(&mut f.app), 0);
        for (entity, _) in &f.children {
            assert!(matches!(
                f.app.world().get::<Task>(*entity).unwrap().status,
                TaskStatus::Failed(FailureReason::UserCancelled)
            ));
        }
    }

    fn shell_job_fixture(
        backend: NativeProcessBackend,
        handle_id: crate::domain::SessionHandleId,
    ) -> Fixture {
        let mut f = fixture();
        f.app.insert_resource(backend);
        let mut commands_queue = f.app.world_mut().commands();
        spawn_background_job(
            &mut commands_queue,
            f.owner,
            f.owner_id,
            JobSource::Shell {
                handle_id,
                notify_on_exit: false,
            },
            chrono::Utc::now(),
        );
        f.app.world_mut().flush();
        f.app.add_systems(Update, background_job_monitor_system);
        f
    }

    fn shell_notifications(app: &mut App) -> Vec<(String, String)> {
        deliveries(app)
            .into_iter()
            .filter_map(|delivery| match delivery.item {
                InboxItem::TaskNotification {
                    source,
                    status,
                    summary,
                    ..
                } if source == "shell" => Some((status, summary)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn shell_session_lost_to_process_service_failure_notifies_owner() {
        let backend = NativeProcessBackend::default();
        let handle_id = crate::domain::SessionHandleId::new();
        backend.mark_session_lost(handle_id, "process supervisor sent no response for 30s");
        let mut f = shell_job_fixture(backend, handle_id);
        tick(&mut f.app);
        let notifications = shell_notifications(&mut f.app);
        assert_eq!(
            notifications.len(),
            1,
            "notify_on_exit=false must not hide a lost session"
        );
        assert_eq!(notifications[0].0, "lost");
        assert!(notifications[0].1.contains("no response for 30s"));
        assert_eq!(
            job_count(&mut f.app),
            1,
            "only the subtask batch job remains"
        );
    }

    #[test]
    fn evicted_shell_session_retires_silently() {
        let mut f = shell_job_fixture(
            NativeProcessBackend::default(),
            crate::domain::SessionHandleId::new(),
        );
        tick(&mut f.app);
        assert!(shell_notifications(&mut f.app).is_empty());
        assert_eq!(
            job_count(&mut f.app),
            1,
            "only the subtask batch job remains"
        );
    }

    #[test]
    fn cleanup_retires_orphan_job_after_owner_despawn() {
        let mut f = fixture();
        f.app.add_systems(Update, background_job_cleanup_system);
        f.app.world_mut().entity_mut(f.owner).despawn();
        tick(&mut f.app);
        assert_eq!(job_count(&mut f.app), 0);
    }
}
