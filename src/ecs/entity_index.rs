//! 中心索引：外部 UUID 稳定身份 → ECS Entity 的唯一映射（仅 Task / Agent 两类）。
//!
//! 设计依据：`docs/adr/ADR-005-ecs-relation-modeling.md`。
//!
//! 阶段 0（本文件）仅引入 `EntityIndex` Resource 与 `RemovedComponents` 兜底清理系统，
//! **运行期不改变任何既有调用**。两表的写入由阶段 1 的 `spawn_*` / `despawn_*` 中心封装负责
//! （封装内同步维护映射）；本文件的监听作为双保险之一，在组件移除的下一帧自动摘除陈旧映射。

use crate::domain::{Agent, AgentId, Task, TaskId};
use crate::prelude::*;
use std::collections::HashMap;

/// 中心索引：外部 UUID 稳定身份 → ECS Entity 的唯一映射（仅 Task / Agent 两类）。
///
/// 类型化两表比统一 `EntityId` 枚举更安全，避免把 `TaskId` 误当 `AgentId` 查询。
/// 不设 `sessions` 表 —— `SessionHandle` 是 shell 进程句柄，由 `NativeBackend` 自管（见 ADR-005）。
#[derive(Resource, Default)]
pub struct EntityIndex {
    /// `TaskId` → `Entity`
    pub tasks: HashMap<TaskId, Entity>,
    /// `AgentId` → `Entity`
    pub agents: HashMap<AgentId, Entity>,
}

impl EntityIndex {
    /// 解析 `TaskId` → `Entity`（O(1)）。
    pub fn get_task(&self, id: &TaskId) -> Option<Entity> {
        self.tasks.get(id).copied()
    }

    /// 解析 `AgentId` → `Entity`（O(1)）。
    pub fn get_agent(&self, id: &AgentId) -> Option<Entity> {
        self.agents.get(id).copied()
    }
}

/// 兜底清理（双保险之一）：`Task` 组件被移除后，下一帧自动摘除索引映射。
///
/// 即使有路径绕过阶段 1 的中心 `despawn_task` 封装直接 `commands.entity(e).despawn()`，
/// 此监听也能在组件移除的下一帧恢复索引一致性。
pub fn cleanup_index_on_task_remove(
    mut index: ResMut<EntityIndex>,
    mut removed: RemovedComponents<Task>,
) {
    for entity in removed.read() {
        index.tasks.retain(|_, v| *v != entity);
    }
}

/// 兜底清理（双保险之一）：`Agent` 组件被移除后，下一帧自动摘除索引映射。
pub fn cleanup_index_on_agent_remove(
    mut index: ResMut<EntityIndex>,
    mut removed: RemovedComponents<Agent>,
) {
    for entity in removed.read() {
        index.agents.retain(|_, v| *v != entity);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{ChannelId, FrontendKind, Task};

    /// 与运行时 app 同构：注册 Resource 与兜底清理系统（运行期在 `HarnessSet::Maintenance`）。
    fn test_app() -> App {
        let mut app = App::new();
        app.init_resource::<EntityIndex>();
        app.add_systems(
            Update,
            (cleanup_index_on_task_remove, cleanup_index_on_agent_remove),
        );
        app
    }

    fn spawn_task(world: &mut World) -> (TaskId, Entity) {
        let task = Task::from_user_input(
            "test",
            0,
            ChannelId {
                frontend: FrontendKind::Tui,
                user_id: "test".to_string(),
                thread_id: None,
            },
        );
        let id = task.id;
        let entity = world.spawn(task).id();
        (id, entity)
    }

    #[test]
    fn entity_index_resolves_and_cleanup_removes_stale_mapping() {
        let mut app = test_app();

        let (id, entity) = spawn_task(app.world_mut());

        // 写入映射（模拟阶段 1 的 spawn 封装）
        app.world_mut()
            .resource_mut::<EntityIndex>()
            .tasks
            .insert(id, entity);
        assert_eq!(
            app.world().resource::<EntityIndex>().get_task(&id),
            Some(entity)
        );

        // 直接 despawn，绕过中心封装 —— 兜底监听应在下一帧摘除映射
        app.world_mut().despawn(entity);
        app.update();

        assert_eq!(
            app.world().resource::<EntityIndex>().get_task(&id),
            None,
            "RemovedComponents 兜底应在 despawn 后摘除陈旧映射"
        );
    }

    #[test]
    fn cleanup_keeps_unrelated_mapping() {
        let mut app = test_app();

        let (id_a, entity_a) = spawn_task(app.world_mut());
        let (id_b, entity_b) = spawn_task(app.world_mut());
        let mut idx = app.world_mut().resource_mut::<EntityIndex>();
        idx.tasks.insert(id_a, entity_a);
        idx.tasks.insert(id_b, entity_b);

        // 仅 despawn A
        app.world_mut().despawn(entity_a);
        app.update();

        let idx = app.world().resource::<EntityIndex>();
        assert_eq!(idx.get_task(&id_a), None);
        assert_eq!(idx.get_task(&id_b), Some(entity_b));
    }
}
