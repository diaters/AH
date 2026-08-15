//! skill 创建 WorkItem 系统：将 SkillCreationRequestMessage 转换为 WorkItem，
//! 由 skill-creator Agent 执行创建流程；skill_creation_writeback_system 消费
//! SkillCreationWritebackMessage 完成沙箱目录 rename 写回与 SkillRegistry 注册。

use crate::prelude::*;

use tracing::{debug, warn};

use crate::domain::{
    DispatchHint, DispatchKind, DispatchStrategy, ExperienceCandidateStatus, ExperienceStore,
    PendingDispatch, SkillCreationContext, SkillCreationRequestMessage,
    SkillCreationWritebackMessage, ToolDefinition, ToolExecutorKind, ToolPermission, ToolSchema,
    WorkItem, WorkItemLifecycleHookPending, WorkItemType,
};
use crate::infrastructure::skills::{SkillEntry, SkillId, SkillLoader, SkillRegistry};
use crate::user_plugins::hook_point::HookPoint;

/// skill 创建 WorkItem 创建系统：消费 SkillCreationRequestMessage，创建 sandbox 目录，
/// 构造 prompt 与工具列表，spawn WorkItem + SkillCreationContext + PendingDispatch。
pub(crate) fn skill_creation_workitem_system(
    mut commands: Commands,
    requests: Query<(Entity, &SkillCreationRequestMessage)>,
    skill_loader: Res<SkillLoader>,
) {
    for (entity, request) in &requests {
        let agent_name = if request.agent_name.is_empty() {
            "default"
        } else {
            &request.agent_name
        };

        // 1. 创建 sandbox 目录：<base_dir>/<agent_name>/skills/.sandbox/_draft_<timestamp>/
        let timestamp = chrono::Utc::now().format("%Y%m%d%H%M%S");
        let sandbox_dir = skill_loader
            .base_dir()
            .join(agent_name)
            .join("skills")
            .join(".sandbox")
            .join(format!("_draft_{}", timestamp));

        if let Err(e) = std::fs::create_dir_all(&sandbox_dir) {
            warn!(
                event = "SkillCreationSandboxDirFailed",
                task_id = %request.task_id,
                agent_name = agent_name,
                sandbox_dir = ?sandbox_dir,
                error = %e,
                error_type = "DirectoryCreationFailed",
                "failed to create sandbox directory, skipping skill creation"
            );
            commands.entity(entity).despawn();
            continue;
        }

        // 2. 获取现有 skill 列表
        let existing_skills = skill_loader.load_skills(agent_name);
        let skills_listing = if existing_skills.is_empty() {
            "（无现有 skill）".to_string()
        } else {
            existing_skills
                .iter()
                .map(|s| format!("- {}：{}", s.name, s.description))
                .collect::<Vec<_>>()
                .join("\n")
        };

        // 3. 构造 prompt
        let prompt = format!(
            "## 任务\n\n根据以下意图创建新的 skill。\n\n\
             ## 用户意图\n\n{}\n\n\
             ## 现有 skill 列表\n\n{}\n\n\
             ## SKILL.md 模板规范\n\n\
             SKILL.md 使用 YAML frontmatter + Markdown body 格式：\n\n\
             ```markdown\n\
             ---\n\
             name: <skill-name>\n\
             description: <简要描述 skill 用途>\n\
             version: 1\n\
             self_updatable: false\n\
             ---\n\n\
             ## Instruction\n\n\
             <skill 的详细指令，描述何时触发、如何执行>\n\
             ```\n\n\
             frontmatter 要求：\n\
             - name：skill 名称，小写字母 + 连字符，全局唯一\n\
             - description：一句话描述用途\n\
             - version：初始版本为 1\n\
             - self_updatable：默认 false\n\n\
             body 要求：\n\
             - 必须包含 ## Instruction section\n\
             - 可包含其他 section（如 ## Examples、## Constraints）\n\
             - 指令应清晰、可操作、避免歧义\n\n\
             ## 工作流程\n\n\
             1. 使用 read_skill_file 读取现有 skill 文件（如需参考格式）\n\
             2. 使用 write_skill_file 创建 SKILL.md 文件\n\
             3. 调用 submit_skill 提交创建结果\n\n\
             ## 注意事项\n\n\
             - skill name 不能与现有 skill 重复\n\
             - 文件路径相对于当前 sandbox 目录\n\
             - SKILL.md 是必须创建的文件\n\
             - 可创建辅助文件（如示例文件），通过 write_skill_file 的 path 参数指定",
            request.intent, skills_listing,
        );

        // 4. 构造工具列表：submit_skill + write_skill_file
        // （read_skill_file 已移除：required_tag = "skill-updater"，skill-creator 不可用）
        let tools = vec![
            make_tool_def(
                "submit_skill",
                "提交新创建的 skill 候选。完成 skill 文件编写后调用此工具提交审核。",
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "name": {
                            "type": "string",
                            "description": "skill 名称，小写字母 + 连字符"
                        },
                        "description": {
                            "type": "string",
                            "description": "skill 用途的简要描述"
                        }
                    },
                    "required": ["name", "description"]
                }),
            ),
            make_tool_def(
                "write_skill_file",
                "创建或覆盖 skill 目录中的文件。默认创建 SKILL.md，也可通过 path 参数创建辅助文件。",
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "文件路径（相对于 skill 目录），默认为 SKILL.md"
                        },
                        "content": {
                            "type": "string",
                            "description": "文件内容（Markdown 格式）"
                        }
                    },
                    "required": ["path", "content"]
                }),
            ),
        ];

        // 5. 创建 WorkItem
        let work_item = WorkItem::skill_creation(
            request.task_id,
            prompt,
            vec![], // 无历史对话
            tools,
            request.agent_id,
        );

        // 6. 创建 SkillCreationContext
        let creation_context = SkillCreationContext {
            task_id: request.task_id,
            agent_id: request.agent_id,
            agent_name: agent_name.to_string(),
            sandbox_dir: sandbox_dir.clone(),
            skill_name: String::new(),
        };

        debug!(
            event = "SkillCreationWorkItemCreated",
            task_id = %request.task_id,
            agent_name = agent_name,
            sandbox_dir = ?sandbox_dir,
            "spawning skill creation work item"
        );

        // 7. spawn WorkItem + SkillCreationContext + PendingDispatch
        commands.spawn((
            work_item,
            creation_context,
            PendingDispatch {
                kind: DispatchKind::WorkItem(WorkItemType::SkillCreation),
                hint: DispatchHint {
                    strategy: DispatchStrategy::DirectDelegate,
                    preferred_agent_name: None,
                    required_skill_id: None,
                    agent_spawn_spec: None,
                },
            },
        ));

        // 8. despawn 请求消息
        commands.entity(entity).despawn();
    }
}

/// skill 创建写回系统：消费 SkillCreationWritebackMessage，将 sandbox 目录 rename 到正式位置，
/// 注册到 SkillRegistry，更新候选状态。
pub(crate) fn skill_creation_writeback_system(
    mut commands: Commands,
    mut store: ResMut<ExperienceStore>,
    mut skill_registry: ResMut<SkillRegistry>,
    skill_loader: Res<SkillLoader>,
    messages: Query<(
        Entity,
        &SkillCreationWritebackMessage,
        &SkillCreationContext,
        &WorkItem,
    )>,
) {
    for (entity, msg, context, _work_item) in &messages {
        // 1. 检查 skill_name 是否为空（在 rename 之前，避免 target_dir 解析到父目录）
        if context.skill_name.is_empty() {
            warn!(
                event = "SkillCreationNameEmpty",
                task_id = %msg.task_id,
                candidate_id = %msg.candidate_id,
                error = "skill_name is empty, cannot perform writeback",
                error_type = "NameEmpty",
                "skill_name is empty, skipping writeback"
            );
            if let Some(c) = store.candidates.get_mut(&msg.candidate_id) {
                c.status = ExperienceCandidateStatus::WritebackFailed;
            }
            commands.entity(entity).despawn();
            continue;
        }

        // 2. 检查目标目录是否已存在（同名冲突）
        let target_dir = skill_loader
            .base_dir()
            .join(&context.agent_name)
            .join("skills")
            .join(&context.skill_name);

        // 防御性路径安全检查：target_dir 必须在 skills/ 目录下
        if let (Ok(target_canonical), Ok(skills_canonical)) = (
            target_dir.canonicalize(),
            skill_loader
                .base_dir()
                .join(&context.agent_name)
                .join("skills")
                .canonicalize(),
        ) && !target_canonical.starts_with(&skills_canonical)
        {
            warn!(
                event = "SkillCreationPathEscape",
                task_id = %msg.task_id,
                candidate_id = %msg.candidate_id,
                skill_name = %context.skill_name,
                target_dir = ?target_dir,
                "target directory escapes skills directory, skipping writeback"
            );
            if let Some(c) = store.candidates.get_mut(&msg.candidate_id) {
                c.status = ExperienceCandidateStatus::WritebackFailed;
            }
            commands.entity(entity).despawn();
            continue;
        }

        if target_dir.exists() {
            warn!(
                event = "SkillCreationTargetDirExists",
                task_id = %msg.task_id,
                candidate_id = %msg.candidate_id,
                skill_name = %context.skill_name,
                target_dir = ?target_dir,
                error = "target skill directory already exists, skipping writeback",
                error_type = "TargetExists",
                "target skill directory already exists, skipping writeback"
            );
            if let Some(c) = store.candidates.get_mut(&msg.candidate_id) {
                c.status = ExperienceCandidateStatus::WritebackFailed;
            }
            commands.entity(entity).despawn();
            continue;
        }

        // 3. 执行 rename（原子移动）
        if let Err(e) = std::fs::rename(&context.sandbox_dir, &target_dir) {
            warn!(
                event = "SkillCreationRenameFailed",
                task_id = %msg.task_id,
                candidate_id = %msg.candidate_id,
                sandbox_dir = ?context.sandbox_dir,
                target_dir = ?target_dir,
                error = %e,
                error_type = "RenameFailed",
                "failed to rename sandbox directory to target"
            );
            if let Some(c) = store.candidates.get_mut(&msg.candidate_id) {
                c.status = ExperienceCandidateStatus::WritebackFailed;
            }
            commands.entity(entity).despawn();
            continue;
        }

        // 4. 注册到 SkillRegistry
        let skill_md_path = target_dir.join("SKILL.md");
        let skill_dir = target_dir.clone();

        if let Ok(skill_md_content) = std::fs::read_to_string(&skill_md_path) {
            if let Some(parsed) =
                crate::infrastructure::skills::loader::parse_skill_md(&skill_md_content, skill_dir)
            {
                let skill_id = SkillId::new(&context.agent_name, &context.skill_name);
                let entry = SkillEntry {
                    skill_id: skill_id.clone(),
                    name: parsed.name,
                    description: parsed.description,
                    instructions: parsed.instructions,
                    version: parsed.version,
                    owner_agent_name: context.agent_name.clone(),
                    self_updatable: parsed.self_updatable,
                };
                skill_registry.upsert(entry);
            } else {
                warn!(
                    event = "SkillCreationParseFailed",
                    task_id = %msg.task_id,
                    candidate_id = %msg.candidate_id,
                    skill_name = %context.skill_name,
                    error = "parse_skill_md returned None for new SKILL.md",
                    error_type = "ParseFailed",
                    "failed to parse new SKILL.md content, registry not updated"
                );
            }
        } else {
            warn!(
                event = "SkillCreationMdReadFailed",
                task_id = %msg.task_id,
                candidate_id = %msg.candidate_id,
                skill_name = %context.skill_name,
                skill_md_path = ?skill_md_path,
                error = "failed to read SKILL.md from target directory",
                error_type = "FileReadFailed",
                "failed to read SKILL.md from target directory after rename"
            );
        }

        // 5. 候选状态置为 Persisted
        if let Some(c) = store.candidates.get_mut(&msg.candidate_id) {
            c.status = ExperienceCandidateStatus::Persisted;
        }

        // 6. 标记 WorkItem 完成并 despawn
        commands
            .entity(entity)
            .insert(WorkItemLifecycleHookPending(HookPoint::OnWorkItemCompleted));
        commands.entity(entity).despawn();

        debug!(
            event = "SkillCreationWritebackCompleted",
            task_id = %msg.task_id,
            candidate_id = %msg.candidate_id,
            skill_name = %context.skill_name,
            "skill creation writeback completed successfully"
        );
    }
}

/// 构造 ToolDefinition 辅助函数。
fn make_tool_def(
    name: &str,
    description: &str,
    parameters_schema: serde_json::Value,
) -> ToolDefinition {
    ToolDefinition {
        name: name.to_string(),
        description: description.to_string(),
        parameters: ToolSchema {
            schema: parameters_schema,
        },
        default_permission: ToolPermission::Allow,
        executor: ToolExecutorKind::Builtin(name.to_string()),
        required_tag: Some("skill-creator".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::ExperienceCandidate;
    use bevy_ecs::system::RunSystemOnce;
    use tempfile::TempDir;
    use uuid::Uuid;

    /// 测试用 SKILL.md 模板
    const SAMPLE_SKILL_MD: &str = "---\nname: test-skill\ndescription: A test skill\nversion: 1\nself_updatable: false\n---\n\n## Instruction\n\nDo the test thing.\n";

    /// 构造 SkillCreationRequestMessage。
    fn make_request(agent_name: &str, intent: &str) -> SkillCreationRequestMessage {
        SkillCreationRequestMessage {
            task_id: Uuid::new_v4(),
            agent_id: Uuid::new_v4(),
            agent_name: agent_name.to_string(),
            intent: intent.to_string(),
        }
    }

    /// 在 ExperienceStore 中插入一个 Submitted 状态的 SkillNew 候选，返回 candidate_id。
    fn stage_skill_new_candidate(store: &mut ExperienceStore) -> Uuid {
        let c = ExperienceCandidate::skill_new(
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            "new skill title".to_string(),
            "new-skill".to_string(),
            "desc".to_string(),
            "instr".to_string(),
            Vec::new(),
        );
        let id = c.candidate_id;
        store.candidates.insert(id, c);
        id
    }

    /// 验证 workitem_system 正常路径：创建 WorkItem + SkillCreationContext + PendingDispatch。
    #[test]
    fn workitem_system_spawns_workitem_with_context() {
        let tmp = TempDir::new().unwrap();
        let loader = SkillLoader::new(tmp.path().to_path_buf());

        let mut world = World::new();
        world.insert_resource(loader);

        world.spawn(make_request("test-agent", "创建一个代码审查 skill"));

        let _ = world.run_system_once(skill_creation_workitem_system);

        // 1. SkillCreationRequestMessage 已被 despawn
        let request_count = world
            .query::<&SkillCreationRequestMessage>()
            .iter(&world)
            .count();
        assert_eq!(request_count, 0, "request should be despawned");

        // 2. 创建了 SkillCreation WorkItem
        let (work_item_count, has_skill_creation_type) = {
            let mut q = world.query::<&WorkItem>();
            let mut count = 0;
            let mut has_type = false;
            for wi in q.iter(&world) {
                count += 1;
                if wi.work_type == WorkItemType::SkillCreation {
                    has_type = true;
                }
            }
            (count, has_type)
        };
        assert_eq!(work_item_count, 1, "exactly one WorkItem should be spawned");
        assert!(
            has_skill_creation_type,
            "WorkItem should be SkillCreation type"
        );

        // 3. SkillCreationContext 附加到 WorkItem entity
        let context_attached = {
            let mut q = world.query::<(&WorkItem, &SkillCreationContext)>();
            q.iter(&world).any(|(wi, ctx)| {
                wi.work_type == WorkItemType::SkillCreation
                    && ctx.agent_name == "test-agent"
                    && ctx.skill_name.is_empty()
            })
        };
        assert!(context_attached, "SkillCreationContext should be attached");

        // 4. PendingDispatch 附加
        let has_pending_dispatch = {
            let mut q = world.query::<(&WorkItem, &PendingDispatch)>();
            q.iter(&world)
                .any(|(wi, _)| wi.work_type == WorkItemType::SkillCreation)
        };
        assert!(has_pending_dispatch, "PendingDispatch should be attached");

        // 5. sandbox 目录已创建
        let sandbox_exists = {
            let mut q = world.query::<&SkillCreationContext>();
            q.iter(&world).any(|ctx| ctx.sandbox_dir.exists())
        };
        assert!(sandbox_exists, "sandbox directory should be created");
    }

    /// agent_name 为空时，使用 "default" 作为 fallback。
    #[test]
    fn workitem_system_uses_default_when_agent_name_empty() {
        let tmp = TempDir::new().unwrap();
        let loader = SkillLoader::new(tmp.path().to_path_buf());

        let mut world = World::new();
        world.insert_resource(loader);

        world.spawn(make_request("", "创建一个测试 skill"));

        let _ = world.run_system_once(skill_creation_workitem_system);

        let uses_default = {
            let mut q = world.query::<&SkillCreationContext>();
            q.iter(&world).any(|ctx| ctx.agent_name == "default")
        };
        assert!(uses_default, "agent_name should fallback to 'default'");
    }

    /// 验证 writeback_system 正常路径：rename + registry 注册 + 候选 Persisted。
    #[test]
    fn writeback_system_renames_and_registers() {
        let tmp = TempDir::new().unwrap();
        let loader = SkillLoader::new(tmp.path().to_path_buf());

        // 预先创建 sandbox 目录和 SKILL.md
        let sandbox_dir = tmp
            .path()
            .join("test-agent")
            .join("skills")
            .join(".sandbox")
            .join("_draft_20260811120000");
        std::fs::create_dir_all(&sandbox_dir).unwrap();
        std::fs::write(sandbox_dir.join("SKILL.md"), SAMPLE_SKILL_MD).unwrap();

        let mut world = World::new();
        let mut store = ExperienceStore::default();
        let candidate_id = stage_skill_new_candidate(&mut store);
        world.insert_resource(store);
        world.insert_resource(SkillRegistry::default());
        world.insert_resource(loader);

        let task_id = Uuid::new_v4();
        let agent_id = Uuid::new_v4();
        let mut work_item =
            WorkItem::skill_creation(task_id, "prompt".to_string(), vec![], vec![], agent_id);
        work_item.start();

        world.spawn((
            work_item,
            SkillCreationContext {
                task_id,
                agent_id,
                agent_name: "test-agent".to_string(),
                sandbox_dir: sandbox_dir.clone(),
                skill_name: "test-skill".to_string(),
            },
            SkillCreationWritebackMessage {
                candidate_id,
                task_id,
            },
        ));

        let _ = world.run_system_once(skill_creation_writeback_system);

        // 1. sandbox 目录已移到正式位置
        let target_dir = tmp
            .path()
            .join("test-agent")
            .join("skills")
            .join("test-skill");
        assert!(
            target_dir.exists(),
            "target directory should exist after rename"
        );
        assert!(
            !sandbox_dir.exists(),
            "sandbox directory should be gone after rename"
        );

        // 2. SKILL.md 在正式位置
        let skill_md = target_dir.join("SKILL.md");
        assert!(
            skill_md.exists(),
            "SKILL.md should exist in target directory"
        );

        // 3. SkillRegistry 已注册
        let registry = world.resource::<SkillRegistry>();
        let skill_id = SkillId::new("test-agent", "test-skill");
        let entry = registry.get(&skill_id);
        assert!(
            entry.is_some(),
            "skill should be registered in SkillRegistry"
        );
        let entry = entry.unwrap();
        assert_eq!(entry.name, "test-skill");
        assert_eq!(entry.description, "A test skill");

        // 4. 候选状态为 Persisted
        let store = world.resource::<ExperienceStore>();
        let c = store.candidates.get(&candidate_id).unwrap();
        assert_eq!(c.status, ExperienceCandidateStatus::Persisted);

        // 5. WorkItem entity 已 despawn
        let work_item_count = world.query::<&WorkItem>().iter(&world).count();
        assert_eq!(work_item_count, 0, "WorkItem should be despawned");
    }

    /// 同名冲突：目标目录已存在时，候选置为 WritebackFailed。
    #[test]
    fn writeback_system_fails_on_target_exists() {
        let tmp = TempDir::new().unwrap();
        let loader = SkillLoader::new(tmp.path().to_path_buf());

        // 预先创建 sandbox 和目标目录
        let sandbox_dir = tmp
            .path()
            .join("test-agent")
            .join("skills")
            .join(".sandbox")
            .join("_draft_20260811120000");
        std::fs::create_dir_all(&sandbox_dir).unwrap();
        std::fs::write(sandbox_dir.join("SKILL.md"), SAMPLE_SKILL_MD).unwrap();

        let target_dir = tmp
            .path()
            .join("test-agent")
            .join("skills")
            .join("test-skill");
        std::fs::create_dir_all(&target_dir).unwrap();

        let mut world = World::new();
        let mut store = ExperienceStore::default();
        let candidate_id = stage_skill_new_candidate(&mut store);
        world.insert_resource(store);
        world.insert_resource(SkillRegistry::default());
        world.insert_resource(loader);

        let task_id = Uuid::new_v4();
        let agent_id = Uuid::new_v4();
        let mut work_item =
            WorkItem::skill_creation(task_id, "prompt".to_string(), vec![], vec![], agent_id);
        work_item.start();

        world.spawn((
            work_item,
            SkillCreationContext {
                task_id,
                agent_id,
                agent_name: "test-agent".to_string(),
                sandbox_dir,
                skill_name: "test-skill".to_string(),
            },
            SkillCreationWritebackMessage {
                candidate_id,
                task_id,
            },
        ));

        let _ = world.run_system_once(skill_creation_writeback_system);

        // 候选状态为 WritebackFailed
        let store = world.resource::<ExperienceStore>();
        let c = store.candidates.get(&candidate_id).unwrap();
        assert_eq!(c.status, ExperienceCandidateStatus::WritebackFailed);

        // sandbox 目录未被移走
        let sandbox_dir = tmp
            .path()
            .join("test-agent")
            .join("skills")
            .join(".sandbox")
            .join("_draft_20260811120000");
        assert!(sandbox_dir.exists(), "sandbox directory should still exist");
    }

    /// rename 失败（sandbox 不存在）时，候选置为 WritebackFailed。
    #[test]
    fn writeback_system_fails_on_rename_error() {
        let tmp = TempDir::new().unwrap();
        let loader = SkillLoader::new(tmp.path().to_path_buf());

        // 不创建 sandbox 目录，rename 将失败
        let sandbox_dir = tmp
            .path()
            .join("test-agent")
            .join("skills")
            .join(".sandbox")
            .join("_draft_nonexistent");

        let mut world = World::new();
        let mut store = ExperienceStore::default();
        let candidate_id = stage_skill_new_candidate(&mut store);
        world.insert_resource(store);
        world.insert_resource(SkillRegistry::default());
        world.insert_resource(loader);

        let task_id = Uuid::new_v4();
        let agent_id = Uuid::new_v4();
        let mut work_item =
            WorkItem::skill_creation(task_id, "prompt".to_string(), vec![], vec![], agent_id);
        work_item.start();

        world.spawn((
            work_item,
            SkillCreationContext {
                task_id,
                agent_id,
                agent_name: "test-agent".to_string(),
                sandbox_dir,
                skill_name: "new-skill".to_string(),
            },
            SkillCreationWritebackMessage {
                candidate_id,
                task_id,
            },
        ));

        let _ = world.run_system_once(skill_creation_writeback_system);

        // 候选状态为 WritebackFailed
        let store = world.resource::<ExperienceStore>();
        let c = store.candidates.get(&candidate_id).unwrap();
        assert_eq!(c.status, ExperienceCandidateStatus::WritebackFailed);
    }

    /// skill_name 为空时，候选置为 WritebackFailed，不执行 rename。
    #[test]
    fn writeback_system_fails_on_empty_skill_name() {
        let tmp = TempDir::new().unwrap();
        let loader = SkillLoader::new(tmp.path().to_path_buf());

        // 预先创建 sandbox 目录
        let sandbox_dir = tmp
            .path()
            .join("test-agent")
            .join("skills")
            .join(".sandbox")
            .join("_draft_20260811120000");
        std::fs::create_dir_all(&sandbox_dir).unwrap();
        std::fs::write(sandbox_dir.join("SKILL.md"), SAMPLE_SKILL_MD).unwrap();

        let mut world = World::new();
        let mut store = ExperienceStore::default();
        let candidate_id = stage_skill_new_candidate(&mut store);
        world.insert_resource(store);
        world.insert_resource(SkillRegistry::default());
        world.insert_resource(loader);

        let task_id = Uuid::new_v4();
        let agent_id = Uuid::new_v4();
        let mut work_item =
            WorkItem::skill_creation(task_id, "prompt".to_string(), vec![], vec![], agent_id);
        work_item.start();

        world.spawn((
            work_item,
            SkillCreationContext {
                task_id,
                agent_id,
                agent_name: "test-agent".to_string(),
                sandbox_dir: sandbox_dir.clone(),
                skill_name: String::new(),
            },
            SkillCreationWritebackMessage {
                candidate_id,
                task_id,
            },
        ));

        let _ = world.run_system_once(skill_creation_writeback_system);

        // 候选状态为 WritebackFailed
        let store = world.resource::<ExperienceStore>();
        let c = store.candidates.get(&candidate_id).unwrap();
        assert_eq!(c.status, ExperienceCandidateStatus::WritebackFailed);

        // sandbox 目录未被移走（rename 未执行）
        assert!(
            sandbox_dir.exists(),
            "sandbox directory should still exist when skill_name is empty"
        );

        // skills 父目录未被错误 rename
        let skills_dir = tmp.path().join("test-agent").join("skills");
        assert!(
            skills_dir.is_dir(),
            "skills parent directory should still be a directory, not corrupted by rename"
        );
    }

    #[test]
    fn workitem_system_does_not_include_read_skill_file() {
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        let loader = SkillLoader::new(tmp.path().to_path_buf());
        let mut world = World::new();
        world.insert_resource(loader);
        world.insert_resource(crate::ecs::EntityIndex::default());

        // spawn SkillCreationRequestMessage
        let request = SkillCreationRequestMessage {
            task_id: uuid::Uuid::new_v4(),
            agent_id: uuid::Uuid::new_v4(),
            agent_name: "test-agent".to_string(),
            intent: "test intent".to_string(),
        };
        world.spawn(request);

        // 运行 system
        let _ = world.run_system_once(skill_creation_workitem_system);

        // 断言：WorkItem.input.context.tools 不含 "read_skill_file"
        let mut query = world.query::<&WorkItem>();
        let work_item = query.iter(&world).next().expect("WorkItem spawned");
        let tool_names: Vec<String> = work_item
            .input
            .context
            .tools
            .iter()
            .map(|t| t.name.clone())
            .collect();

        assert!(
            !tool_names.contains(&"read_skill_file".to_string()),
            "read_skill_file should not be exposed to skill-creator; got {:?}",
            tool_names
        );
        assert!(tool_names.contains(&"write_skill_file".to_string()));
        assert!(tool_names.contains(&"submit_skill".to_string()));
    }
}
