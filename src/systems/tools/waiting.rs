//! 等待注册、目标完成和到期唤醒收敛为单一完成入口。
use super::orchestrator::{collect_task_results, spawn_wait_result_message};
use crate::prelude::*;
use crate::systems::demand::DemandSchedule;
use crate::{
    contracts::Clock,
    domain::{Task, TaskId, WaitingForTasksInfo},
    ecs::EntityIndex,
};
use std::collections::{HashMap, HashSet};

/// 可重建的反向索引。移除/替换等待时同时撤销旧目标，状态仍以组件为准。
#[derive(Resource, Default)]
pub struct WaitingIndex {
    by_child: HashMap<TaskId, HashSet<Entity>>,
    by_waiter: HashMap<Entity, Vec<TaskId>>,
    checks: u64,
}
impl WaitingIndex {
    fn remove(&mut self, entity: Entity) {
        if let Some(targets) = self.by_waiter.remove(&entity) {
            for target in targets {
                if let Some(waiters) = self.by_child.get_mut(&target) {
                    waiters.remove(&entity);
                    if waiters.is_empty() {
                        self.by_child.remove(&target);
                    }
                }
            }
        }
    }
    fn register(&mut self, entity: Entity, targets: &[TaskId]) {
        self.remove(entity);
        self.by_waiter.insert(entity, targets.to_vec());
        for target in targets {
            self.by_child.entry(*target).or_default().insert(entity);
        }
    }
}

/// 新等待检查现状；后续仅检查目标发生终态变化或超时的等待者。
#[allow(clippy::too_many_arguments)]
pub fn check_waiting_tasks_system(
    clock: Res<Clock>,
    mut commands: Commands,
    waiting_tasks: Query<(Entity, &Task, &WaitingForTasksInfo)>,
    changed_waits: Query<(Entity, &WaitingForTasksInfo), Changed<WaitingForTasksInfo>>,
    changed_tasks: Query<&Task, Changed<Task>>,
    all_tasks: Query<&Task>,
    index: Res<EntityIndex>,
    schedule: Option<Res<DemandSchedule>>,
    mut waiting_index: ResMut<WaitingIndex>,
    mut removed: RemovedComponents<WaitingForTasksInfo>,
) {
    for entity in removed.read() {
        waiting_index.remove(entity);
    }
    let mut candidates = HashSet::new();
    for (entity, info) in &changed_waits {
        waiting_index.register(entity, &info.target_task_ids);
        candidates.insert(entity);
    }
    for task in &changed_tasks {
        if task.status().is_terminal()
            && let Some(waiters) = waiting_index.by_child.get(&task.id)
        {
            candidates.extend(waiters);
        }
    }
    if let Some(schedule) = schedule.as_deref() {
        candidates.extend(schedule.waiters_due());
    } else {
        // 独立 system 夹具无主循环索引，用其假时钟检查到期，仍不遍历每个目标列表。
        candidates.extend(
            waiting_tasks
                .iter()
                .filter(|(_, _, info)| clock.0 >= info.timeout_at)
                .map(|(entity, _, _)| entity),
        );
    }
    for entity in candidates {
        let Ok((_, task, info)) = waiting_tasks.get(entity) else {
            waiting_index.remove(entity);
            continue;
        };
        waiting_index.checks = waiting_index.checks.saturating_add(1);
        let timed_out = schedule
            .as_deref()
            .map_or(clock.0 >= info.timeout_at, |schedule| {
                schedule.wait_expired(entity, info, clock.0)
            });
        let all_terminal = info.target_task_ids.iter().all(|id| {
            index
                .get_task(id)
                .and_then(|e| all_tasks.get(e).ok())
                .is_some_and(|t| t.status().is_terminal())
        });
        if timed_out || all_terminal {
            let results = collect_task_results(&info.target_task_ids, &all_tasks, &index);
            spawn_wait_result_message(&mut commands, task.id, info, results, timed_out);
            waiting_index.remove(entity);
            commands.entity(entity).remove::<WaitingForTasksInfo>();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{AgentId, ChannelId, FrontendKind};
    use bevy_app::{App, Update};
    fn task() -> Task {
        Task::from_user_input(
            "test",
            3,
            ChannelId {
                frontend: FrontendKind::Tui,
                user_id: "test".into(),
                thread_id: None,
            },
        )
    }
    #[test]
    fn completion_checks_only_related_waiter_and_completes_once() {
        let mut app = App::new();
        app.insert_resource(Clock::default());
        app.init_resource::<EntityIndex>();
        app.init_resource::<WaitingIndex>();
        app.add_systems(Update, check_waiting_tasks_system);
        let mut children = Vec::new();
        let mut waiters = Vec::new();
        for n in 0..100 {
            let child = task();
            let id = child.id;
            let child_entity = app.world_mut().spawn(child).id();
            app.world_mut()
                .resource_mut::<EntityIndex>()
                .tasks
                .insert(id, child_entity);
            let waiter = task();
            let entity = app
                .world_mut()
                .spawn((
                    waiter,
                    WaitingForTasksInfo {
                        target_task_ids: vec![id],
                        timeout_at: Utc::now() + chrono::Duration::hours(1),
                        tool_call_id: format!("wait-{n}"),
                        agent_id: AgentId::new(),
                    },
                ))
                .id();
            children.push(child_entity);
            waiters.push(entity);
        }
        app.update();
        let checks = app.world().resource::<WaitingIndex>().checks;
        app.update();
        assert_eq!(app.world().resource::<WaitingIndex>().checks, checks);
        app.world_mut()
            .get_mut::<Task>(children[50])
            .unwrap()
            .mark_done("done", Utc::now());
        app.update();
        assert_eq!(app.world().resource::<WaitingIndex>().checks, checks + 1);
        assert!(
            app.world()
                .get::<WaitingForTasksInfo>(waiters[50])
                .is_none()
        );
        let count = app
            .world_mut()
            .query::<&crate::domain::ToolExecutionResultMessage>()
            .iter(app.world())
            .count();
        app.update();
        assert_eq!(
            app.world_mut()
                .query::<&crate::domain::ToolExecutionResultMessage>()
                .iter(app.world())
                .count(),
            count
        );
    }
    use chrono::Utc;
}
