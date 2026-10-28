//! 终态任务回收
//!
//! 终态 Task 此前只在 `/clear` 时销毁，长期运行（尤其是周期定时任务）会让 World 中的
//! Task 实体与 `TaskTerminalDispatched` 去重集合无限增长。本系统按"任务树"回收：
//! 树内所有任务都已终态且超过保留时长、树上没有未终态的 WorkItem 时，整树销毁。
//!
//! 多轮会话的根任务一直停在等待用户的状态，整棵树永远不会进入终态。
//! `conversation_idle_system` 把空闲超过阈值的会话按 `/finish` 路径结束，之后由终态回收清理。

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Duration, Utc};
use tracing::info;

use crate::contracts::Clock;
use crate::domain::{
    FinishTaskMessage, SystemOutputMessage, Task, TaskId, TaskStatus, WaitingReason, WorkItem,
};
use crate::ecs::EntityIndex;
use crate::prelude::*;
use crate::systems::HarnessSettings;
use crate::systems::TaskTerminalDispatched;

/// 两次回收扫描的最小间隔。
const SWEEP_INTERVAL_SECS: i64 = 30;
/// 沿父链查找根任务的步数上限，防御异常数据形成环。
const MAX_TASK_DEPTH: usize = 64;

/// 回收保留期已过的终态任务树。
#[allow(clippy::too_many_arguments)]
pub fn terminal_task_retention_system(
    mut commands: Commands,
    mut index: ResMut<EntityIndex>,
    clock: Res<Clock>,
    settings: Res<HarnessSettings>,
    tasks: Query<&Task>,
    work_items: Query<&WorkItem>,
    dispatched: Option<ResMut<TaskTerminalDispatched>>,
    pending: Query<&crate::domain::ToolRequestPending>,
    inflight_llm: Option<Res<crate::domain::InflightLlmRequests>>,
    backend: Option<Res<crate::systems::NativeProcessBackend>>,
    mut last_sweep: Local<Option<DateTime<Utc>>>,
) {
    let now = clock.0;
    if last_sweep.is_some_and(|last| now - last < Duration::seconds(SWEEP_INTERVAL_SECS)) {
        return;
    }
    *last_sweep = Some(now);

    let retention = Duration::seconds(settings.0.terminal_task_retention_secs as i64);
    let parents: HashMap<TaskId, Option<TaskId>> =
        tasks.iter().map(|t| (t.id, t.parent_task_id)).collect();
    let root_of = |id: TaskId| -> TaskId {
        let mut current = id;
        for _ in 0..MAX_TASK_DEPTH {
            match parents.get(&current).copied().flatten() {
                // 父任务已不存在（如被 /clear）时，当前任务即为根
                Some(parent) if parents.contains_key(&parent) => current = parent,
                _ => break,
            }
        }
        current
    };

    // 树上任一任务未终态 / 仍在保留期内，或仍有未终态 WorkItem，则整树保留
    let mut busy_roots: HashSet<TaskId> = tasks
        .iter()
        .filter(|t| !t.status.is_terminal() || now - t.updated_at < retention)
        .map(|t| root_of(t.id))
        .collect();
    busy_roots.extend(
        work_items
            .iter()
            .filter(|w| !w.is_terminal() && parents.contains_key(&w.task_id))
            .map(|w| root_of(w.task_id)),
    );

    busy_roots.extend(
        pending
            .iter()
            .map(|request| root_of(request.original_request.task_id)),
    );
    if let Some(inflight) = inflight_llm.as_ref() {
        busy_roots.extend(
            tasks
                .iter()
                .filter(|task| inflight.in_flight(task.id) > 0)
                .map(|task| root_of(task.id)),
        );
    }
    if let Some(backend) = backend.as_ref() {
        use crate::domain::SessionBackend;
        busy_roots.extend(
            tasks
                .iter()
                .filter(|task| {
                    backend
                        .list_task_sessions(task.id)
                        .map_or(true, |sessions| !sessions.is_empty())
                })
                .map(|task| root_of(task.id)),
        );
    }

    let reclaim: Vec<TaskId> = tasks
        .iter()
        .map(|t| t.id)
        .filter(|id| !busy_roots.contains(&root_of(*id)))
        .collect();
    if reclaim.is_empty() {
        return;
    }

    let mut dispatched = dispatched;
    for id in &reclaim {
        crate::ecs::despawn_task(&mut commands, &mut index, *id);
        if let Some(dispatched) = dispatched.as_mut() {
            dispatched.0.remove(id);
        }
    }
    info!(
        event = "TerminalTasksReclaimed",
        count = reclaim.len(),
        retention_secs = settings.0.terminal_task_retention_secs,
        "reclaimed terminal task trees past retention"
    );
}

/// 结束空闲超过阈值的顶层多轮会话，并向会话通道推送提示。
///
/// 只处理等待用户输入（`Waiting(User)` / `Waiting(AskUser)`）的顶层任务；
/// 等待工具审批的任务（带 `pending_confirmation_id`）由审批流程负责，不在此结束。
pub fn conversation_idle_system(
    mut commands: Commands,
    clock: Res<Clock>,
    settings: Res<HarnessSettings>,
    tasks: Query<&Task>,
    mut last_sweep: Local<Option<DateTime<Utc>>>,
) {
    let idle_secs = settings.0.conversation_idle_secs;
    if idle_secs == 0 {
        return;
    }
    let now = clock.0;
    if last_sweep.is_some_and(|last| now - last < Duration::seconds(SWEEP_INTERVAL_SECS)) {
        return;
    }
    *last_sweep = Some(now);

    let idle = Duration::seconds(idle_secs as i64);
    for task in tasks.iter().filter(|t| {
        t.parent_task_id.is_none()
            && t.pending_confirmation_id.is_none()
            && matches!(
                t.status,
                TaskStatus::Waiting(WaitingReason::User | WaitingReason::AskUser)
            )
            && now - t.updated_at >= idle
    }) {
        info!(
            event = "ConversationIdleFinished",
            task_id = %task.id,
            idle_secs,
            "conversation idle past threshold, finishing"
        );
        commands.spawn(SystemOutputMessage {
            task_id: task.id,
            content: format!(
                "会话已空闲超过 {}，已自动结束；发送新消息将开启新会话。",
                format_idle(idle_secs)
            ),
        });
        commands.spawn(FinishTaskMessage { task_id: task.id });
    }
}

/// 把空闲阈值格式化为便于阅读的时长。
fn format_idle(secs: u64) -> String {
    match secs {
        s if s >= 3600 && s % 3600 == 0 => format!("{} 小时", s / 3600),
        s if s >= 60 && s % 60 == 0 => format!("{} 分钟", s / 60),
        s => format!("{s} 秒"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        ChannelId, FinishTaskMessage, FrontendKind, SystemOutputMessage, TaskRoutingPolicy,
        TaskStatus, WaitingReason, WorkItemInput, WorkItemOrigin, WorkItemType,
        WorkItemWritebackTarget,
    };
    use crate::systems::HarnessConfig;

    fn task(status: TaskStatus, updated_at: DateTime<Utc>, parent: Option<TaskId>) -> Task {
        let channel = ChannelId {
            frontend: FrontendKind::Tui,
            user_id: "u".to_string(),
            thread_id: None,
        };
        Task {
            id: TaskId::new(),
            content: "t".to_string(),
            creator: crate::domain::AgentId::new(),
            delegate: None,
            status,
            pending_confirmation_id: None,
            input_summary: String::new(),
            result_summary: String::new(),
            priority: 0,
            created_at: updated_at,
            updated_at,
            retry_count: 0,
            max_retries: 3,
            next_retry_at: None,
            last_error: None,
            multi_turn: false,
            parent_task_id: parent,
            batch_id: None,
            origin_channel: Some(channel.clone()),
            routing_policy: TaskRoutingPolicy::conversational(channel),
            last_evaluated_turn: None,
        }
    }

    fn world_at(now: DateTime<Utc>) -> World {
        let mut world = World::new();
        world.insert_resource(Clock(now));
        world.insert_resource(HarnessSettings(HarnessConfig {
            terminal_task_retention_secs: 3600,
            conversation_idle_secs: 86400,
            ..HarnessConfig::default()
        }));
        world.init_resource::<EntityIndex>();
        world.init_resource::<TaskTerminalDispatched>();
        world
    }

    fn spawn(world: &mut World, task: Task) -> TaskId {
        let id = task.id;
        let entity = world.spawn(task).id();
        world.resource_mut::<EntityIndex>().tasks.insert(id, entity);
        world.resource_mut::<TaskTerminalDispatched>().0.insert(id);
        id
    }

    fn run(world: &mut World) {
        let mut schedule = Schedule::default();
        schedule.add_systems(terminal_task_retention_system);
        schedule.run(world);
    }

    fn alive(world: &World, id: TaskId) -> bool {
        world.resource::<EntityIndex>().get_task(&id).is_some()
    }

    #[test]
    fn reclaims_terminal_tree_past_retention() {
        let now = Utc::now();
        let mut world = world_at(now);
        let old = now - Duration::hours(2);
        let root = spawn(&mut world, task(TaskStatus::Done, old, None));
        let child = spawn(&mut world, task(TaskStatus::Done, old, Some(root)));

        run(&mut world);

        assert!(!alive(&world, root) && !alive(&world, child));
        assert_eq!(world.query::<&Task>().iter(&world).count(), 0);
        assert!(world.resource::<TaskTerminalDispatched>().0.is_empty());
    }

    #[test]
    fn keeps_recent_or_unfinished_trees() {
        let now = Utc::now();
        let mut world = world_at(now);
        let old = now - Duration::hours(2);
        let recent = spawn(&mut world, task(TaskStatus::Done, now, None));
        let parent = spawn(&mut world, task(TaskStatus::Done, old, None));
        let running_child = spawn(&mut world, task(TaskStatus::Running, old, Some(parent)));
        let waiting = spawn(
            &mut world,
            task(
                TaskStatus::Waiting(crate::domain::WaitingReason::User),
                old,
                None,
            ),
        );

        run(&mut world);

        for id in [recent, parent, running_child, waiting] {
            assert!(alive(&world, id));
        }
    }

    #[test]
    fn keeps_tree_with_pending_work_item() {
        let now = Utc::now();
        let mut world = world_at(now);
        let root = spawn(
            &mut world,
            task(TaskStatus::Done, now - Duration::hours(2), None),
        );
        world.spawn(WorkItem::new(
            root,
            WorkItemType::Evaluation,
            WorkItemInput::new("eval".to_string()),
            WorkItemOrigin::Evaluation,
            WorkItemWritebackTarget::TaskResult,
        ));

        run(&mut world);

        assert!(alive(&world, root));
    }

    fn run_idle(world: &mut World) -> Vec<TaskId> {
        let mut schedule = Schedule::default();
        schedule.add_systems(conversation_idle_system);
        schedule.run(world);
        let mut finished: Vec<TaskId> = world
            .query::<&FinishTaskMessage>()
            .iter(world)
            .map(|m| m.task_id)
            .collect();
        finished.sort_by_key(|id| id.to_string());
        finished
    }

    fn notices(world: &mut World) -> Vec<(TaskId, String)> {
        world
            .query::<&SystemOutputMessage>()
            .iter(world)
            .map(|m| (m.task_id, m.content.clone()))
            .collect()
    }

    #[test]
    fn finishes_idle_top_level_conversations_and_notifies() {
        let now = Utc::now();
        let mut world = world_at(now);
        let old = now - Duration::hours(25);
        let waiting = spawn(
            &mut world,
            task(TaskStatus::Waiting(WaitingReason::User), old, None),
        );
        let asking = spawn(
            &mut world,
            task(TaskStatus::Waiting(WaitingReason::AskUser), old, None),
        );

        let mut expected = vec![waiting, asking];
        expected.sort_by_key(|id| id.to_string());
        assert_eq!(run_idle(&mut world), expected);
        let notices = notices(&mut world);
        assert_eq!(notices.len(), 2);
        assert!(notices.iter().all(|(_, text)| text.contains("自动结束")));
    }

    #[test]
    fn keeps_recent_pending_approval_child_and_running_tasks() {
        let now = Utc::now();
        let mut world = world_at(now);
        let old = now - Duration::hours(25);
        spawn(
            &mut world,
            task(TaskStatus::Waiting(WaitingReason::User), now, None),
        );
        let mut approval = task(TaskStatus::Waiting(WaitingReason::User), old, None);
        approval.pending_confirmation_id = Some(uuid::Uuid::new_v4());
        spawn(&mut world, approval);
        let parent = spawn(&mut world, task(TaskStatus::Running, old, None));
        spawn(
            &mut world,
            task(TaskStatus::Waiting(WaitingReason::User), old, Some(parent)),
        );

        assert!(run_idle(&mut world).is_empty());
        assert!(notices(&mut world).is_empty());
    }

    #[test]
    fn idle_finish_disabled_when_zero() {
        let now = Utc::now();
        let mut world = world_at(now);
        world
            .resource_mut::<HarnessSettings>()
            .0
            .conversation_idle_secs = 0;
        spawn(
            &mut world,
            task(
                TaskStatus::Waiting(WaitingReason::User),
                now - Duration::days(30),
                None,
            ),
        );

        assert!(run_idle(&mut world).is_empty());
    }
}
