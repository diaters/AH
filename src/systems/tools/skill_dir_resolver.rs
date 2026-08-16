//! 共享：从 WorkItem entity 解析当前 skill 目录。
//!
//! sync 路径（`tool_dispatch_system`）与 async 路径（`async_tool_dispatch_system`）
//! 都通过 `resolve_skill_dir_from_context` 统一解析，避免两份逻辑漂移（曾导致
//! async 路径硬编码 `None` 的 skill-creator 完整失败链路 bug）。

use std::path::PathBuf;

use crate::domain::{SkillCreationContext, SkillUpdateContext};
use crate::infrastructure::skills::SkillLoader;

/// 从 context 数据解析当前 skill 目录。
///
/// 解析顺序：
/// 1. SkillCreationContext.sandbox_dir（skill-creator 路径，不依赖 skill_loader）
/// 2. SkillUpdateContext.skill_id → skill_loader.skill_md_path().parent()（skill-updater 路径）
///
/// 任一命中即返回；都不命中返回 None。
/// skill_loader 为 None 时，SkillUpdateContext 分支返回 None（测试世界无 loader）。
///
/// 注意：调用方需要先从 Query 中提取 context 数据,再调用此函数。
/// 这是为了规避 Bevy Query 的生命周期 invariant 限制。
pub fn resolve_skill_dir_from_context(
    creation_ctx: Option<&SkillCreationContext>,
    update_ctx: Option<&SkillUpdateContext>,
    skill_loader: Option<&SkillLoader>,
) -> Option<PathBuf> {
    if let Some(ctx) = creation_ctx {
        return Some(ctx.sandbox_dir.clone());
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
    use crate::domain::{SkillCreationContext, SkillUpdateContext};
    use crate::infrastructure::skills::SkillId;
    use tempfile::TempDir;

    #[test]
    fn returns_none_when_no_context() {
        let result = resolve_skill_dir_from_context(None, None, None);
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
        // skill_loader = None，但 SkillCreationContext 分支不依赖 loader
        let result = resolve_skill_dir_from_context(Some(&creation_ctx), None, None);
        assert_eq!(result, Some(sandbox));
    }

    #[test]
    fn returns_skill_dir_when_skill_update_context_and_loader_present() {
        let tmp = TempDir::new().unwrap();
        let loader = SkillLoader::new(tmp.path().to_path_buf());
        // 预创建 skill 目录结构，确保 parent() 有意义
        let skill_id = SkillId::new("test-agent", "test-skill");
        std::fs::create_dir_all(
            tmp.path()
                .join("test-agent")
                .join("skills")
                .join("test-skill"),
        )
        .unwrap();
        let update_ctx = SkillUpdateContext {
            skill_id: skill_id.clone(),
            base_version: 1,
            experience_candidate_id: uuid::Uuid::new_v4(),
            governing_agent_id: uuid::Uuid::new_v4(),
        };
        let result = resolve_skill_dir_from_context(None, Some(&update_ctx), Some(&loader));
        // 期望：非空 PathBuf，指向 <base>/test-agent/skills/test-skill
        let expected = tmp
            .path()
            .join("test-agent")
            .join("skills")
            .join("test-skill");
        assert_eq!(result, Some(expected));
        // S1 回归防护：显式断言非空 PathBuf（旧 unwrap_or_default 会返回空路径）
        assert!(
            result
                .as_ref()
                .map(|p| !p.as_os_str().is_empty())
                .unwrap_or(false),
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
        // skill_loader = None，模拟测试世界未装 SkillLoader
        let result = resolve_skill_dir_from_context(None, Some(&update_ctx), None);
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
        let result =
            resolve_skill_dir_from_context(Some(&creation_ctx), Some(&update_ctx), Some(&loader));
        // 两个 context 同时存在（防御性测试，正常流程不应发生）
        // 优先返回 SkillCreationContext 的 sandbox_dir
        assert_eq!(result, Some(sandbox));
    }
}
