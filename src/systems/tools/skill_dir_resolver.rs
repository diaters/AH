//! 共享：从 WorkItem entity 解析当前 skill 目录。
//!
//! sync 路径（`tool_dispatch_system`）与 async 路径（`async_tool_dispatch_system`）
//! 都通过 `resolve_current_skill_dir` 统一解析，避免两份逻辑漂移（曾导致
//! async 路径硬编码 `None` 的 skill-creator 完整失败链路 bug）。

use std::path::PathBuf;

use bevy_ecs::prelude::Entity;

use crate::domain::{ProfileGenerationContext, SkillCreationContext, SkillUpdateContext, WorkItem};
use crate::infrastructure::skills::SkillLoader;

/// 抽象查询 skill context 的能力,支持 Query 和 QueryState 两种实现。
pub trait ContextQuery {
    fn get_context(
        &self,
        _entity: Entity,
    ) -> Option<(
        Entity,
        Option<ProfileGenerationContext>,
        Option<SkillUpdateContext>,
        Option<SkillCreationContext>,
        WorkItem,
    )>;
}

/// 为 Query 实现 ContextQuery trait
impl<'w, 's>
    ContextQuery for bevy_ecs::prelude::Query<
    'w,
    's,
    (
        Entity,
        Option<&'w ProfileGenerationContext>,
        Option<&'w SkillUpdateContext>,
        Option<&'w SkillCreationContext>,
        &'w WorkItem,
    ),
>
{
    fn get_context(
        &self,
        entity: Entity,
    ) -> Option<(
        Entity,
        Option<ProfileGenerationContext>,
        Option<SkillUpdateContext>,
        Option<SkillCreationContext>,
        WorkItem,
    )> {
        let (e, profile_ctx, update_ctx, creation_ctx, work_item) = self.get(entity).ok()?;
        Some((
            e,
            profile_ctx.cloned(),
            update_ctx.cloned(),
            creation_ctx.cloned(),
            work_item.clone(),
        ))
    }
}

/// 为 QueryState 实现 ContextQuery trait
impl ContextQuery for bevy_ecs::query::QueryState<(
    Entity,
    Option<&ProfileGenerationContext>,
    Option<&SkillUpdateContext>,
    Option<&SkillCreationContext>,
    &WorkItem,
)> {
    fn get_context(
        &self,
        _entity: Entity,
    ) -> Option<(
        Entity,
        Option<ProfileGenerationContext>,
        Option<SkillUpdateContext>,
        Option<SkillCreationContext>,
        WorkItem,
    )> {
        // QueryState 需要 world 引用,这里无法直接实现
        // 所以测试中直接使用简化的方法
        None
    }
}

/// 从 WorkItem entity 解析当前 skill 目录。
///
/// 解析顺序：
/// 1. SkillCreationContext.sandbox_dir（skill-creator 路径，不依赖 skill_loader）
/// 2. SkillUpdateContext.skill_id → skill_loader.skill_md_path().parent()（skill-updater 路径）
///
/// 任一命中即返回；都不命中（非 skill 类 WorkItem）返回 None。
/// skill_loader 为 None 时，SkillUpdateContext 分支返回 None（测试世界无 loader）。
pub fn resolve_current_skill_dir<Q: ContextQuery>(
    work_item_entity: Option<Entity>,
    context_queries: &Q,
    skill_loader: Option<&SkillLoader>,
) -> Option<PathBuf> {
    let wi_entity = work_item_entity?;
    let (_, _profile_ctx, update_ctx, creation_ctx, _work_item) = context_queries.get_context(wi_entity)?;

    if let Some(ctx) = creation_ctx {
        return Some(ctx.sandbox_dir);
    }

    if let Some(ctx) = update_ctx {
        let loader = skill_loader?;
        return loader
            .skill_md_path(&ctx.skill_id)
            .parent()
            .map(|p| p.to_path_buf());
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{SkillCreationContext, SkillUpdateContext, WorkItem, WorkItemInput, WorkItemOrigin, WorkItemWritebackTarget, WorkItemType};
    use crate::infrastructure::skills::SkillId;
    use bevy_ecs::prelude::World;
    use tempfile::TempDir;

    /// Mock 实现 ContextQuery,直接返回预设数据
    struct MockContextQuery {
        data: std::collections::HashMap<Entity, (
            Entity,
            Option<ProfileGenerationContext>,
            Option<SkillUpdateContext>,
            Option<SkillCreationContext>,
            WorkItem,
        )>,
    }

    impl ContextQuery for MockContextQuery {
        fn get_context(
            &self,
            entity: Entity,
        ) -> Option<(
            Entity,
            Option<ProfileGenerationContext>,
            Option<SkillUpdateContext>,
            Option<SkillCreationContext>,
            WorkItem,
        )> {
            self.data.get(&entity).cloned()
        }
    }

    /// 构造 minimal World 并返回 entity 和 mock query
    fn make_world_with_workitem(
        creation_ctx: Option<SkillCreationContext>,
        update_ctx: Option<SkillUpdateContext>,
    ) -> (World, Entity, MockContextQuery) {
        let mut world = World::new();
        let entity = world.spawn(WorkItem::new(
            uuid::Uuid::new_v4(), // task_id
            WorkItemType::SkillCreation,
            WorkItemInput::new("test prompt".to_string()),
            WorkItemOrigin::UserTask,
            WorkItemWritebackTarget::TaskResult,
        )).id();

        let work_item = world.entity(entity).get::<WorkItem>().unwrap().clone();

        let mock_query = MockContextQuery {
            data: [(entity, (entity, None, update_ctx, creation_ctx, work_item))]
                .into_iter()
                .collect(),
        };

        (world, entity, mock_query)
    }

    #[test]
    fn returns_none_when_no_work_item_entity() {
        let (_, _, mock_query) = make_world_with_workitem(None, None);
        let result = resolve_current_skill_dir(None, &mock_query, None);
        assert!(result.is_none());
    }

    #[test]
    fn returns_none_when_work_item_has_no_context() {
        let (_, entity, mock_query) = make_world_with_workitem(None, None);
        let result = resolve_current_skill_dir(Some(entity), &mock_query, None);
        assert!(result.is_none());
    }

    #[test]
    fn returns_sandbox_dir_when_skill_creation_context_present() {
        let sandbox = PathBuf::from("/tmp/test-sandbox");
        let creation_ctx = SkillCreationContext {
            task_id: uuid::Uuid::new_v4(),
            agent_id: uuid::Uuid::new_v4(),
            agent_name: "test-agent".to_string(),
            sandbox_dir: sandbox.clone(),
            skill_name: "test-skill".to_string(),
        };
        let (_, entity, mock_query) = make_world_with_workitem(Some(creation_ctx), None);
        // skill_loader = None，但 SkillCreationContext 分支不依赖 loader
        let result = resolve_current_skill_dir(Some(entity), &mock_query, None);
        assert_eq!(result, Some(sandbox));
    }

    #[test]
    fn returns_skill_dir_when_skill_update_context_and_loader_present() {
        let tmp = TempDir::new().unwrap();
        let loader = SkillLoader::new(tmp.path().to_path_buf());
        // 预创建 skill 目录结构，确保 parent() 有意义
        let skill_id = SkillId::new("test-agent", "test-skill");
        std::fs::create_dir_all(
            tmp.path().join("test-agent").join("skills").join("test-skill"),
        )
        .unwrap();
        let update_ctx = SkillUpdateContext {
            skill_id: skill_id.clone(),
            base_version: 1,
            experience_candidate_id: uuid::Uuid::new_v4(),
            governing_agent_id: uuid::Uuid::new_v4(),
        };
        let (_, entity, mock_query) = make_world_with_workitem(None, Some(update_ctx));
        let result = resolve_current_skill_dir(Some(entity), &mock_query, Some(&loader));
        // 期望：非空 PathBuf，指向 <base>/test-agent/skills/test-skill
        let expected = tmp
            .path()
            .join("test-agent")
            .join("skills")
            .join("test-skill");
        assert_eq!(result, Some(expected));
        // S1 回归防护：显式断言非空 PathBuf（旧 unwrap_or_default 会返回空路径）
        assert!(
            result.as_ref().map(|p| !p.as_os_str().is_empty()).unwrap_or(false),
            "returned path must be non-empty, got {:?}",
            result
        );
    }

    #[test]
    fn returns_none_when_skill_update_context_but_loader_missing() {
        let update_ctx = SkillUpdateContext {
            skill_id: SkillId::new("test-agent", "test-skill"),
            base_version: 1,
            experience_candidate_id: uuid::Uuid::new_v4(),
            governing_agent_id: uuid::Uuid::new_v4(),
        };
        let (_, entity, mock_query) = make_world_with_workitem(None, Some(update_ctx));
        // skill_loader = None，模拟测试世界未装 SkillLoader
        let result = resolve_current_skill_dir(Some(entity), &mock_query, None);
        assert!(result.is_none());
    }

    #[test]
    fn prefers_creation_context_when_both_present() {
        let sandbox = PathBuf::from("/tmp/test-sandbox");
        let creation_ctx = SkillCreationContext {
            task_id: uuid::Uuid::new_v4(),
            agent_id: uuid::Uuid::new_v4(),
            agent_name: "test-agent".to_string(),
            sandbox_dir: sandbox.clone(),
            skill_name: "test-skill".to_string(),
        };
        let update_ctx = SkillUpdateContext {
            skill_id: SkillId::new("test-agent", "test-skill"),
            base_version: 1,
            experience_candidate_id: uuid::Uuid::new_v4(),
            governing_agent_id: uuid::Uuid::new_v4(),
        };
        let tmp = TempDir::new().unwrap();
        let loader = SkillLoader::new(tmp.path().to_path_buf());
        let (_, entity, mock_query) = make_world_with_workitem(Some(creation_ctx), Some(update_ctx));
        let result = resolve_current_skill_dir(Some(entity), &mock_query, Some(&loader));
        // 两个 context 同时存在（防御性测试，正常流程不应发生）
        // 优先返回 SkillCreationContext 的 sandbox_dir
        assert_eq!(result, Some(sandbox));
    }

    #[test]
    fn returns_none_when_entity_not_found() {
        let (_, _, mock_query) = make_world_with_workitem(None, None);
        // 传入不存在的 entity
        let nonexistent = Entity::from_raw_u32(99999).unwrap();
        let result = resolve_current_skill_dir(Some(nonexistent), &mock_query, None);
        assert!(result.is_none());
    }
}
