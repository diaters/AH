//! 统一派发 System
//!
//! 扫描带 `PendingDispatch` Component 的 Task / WorkItem Entity，执行派发决策。
//!
//! ## 设计假设
//!
//! Task 和 WorkItem 是不同 entity，两个 mut Query 不会冲突。
//!
//! ## 当前状态
//!
//! 所有派发请求统一通过 `PendingDispatch` 流入本 system 处理：
//! - WorkItem 按 `required_tag` 交给对应服务角色（摘要员 / evaluator / 经验管理员）；
//! - Task 交给 `preferred_agent_name` 指定的 Agent，未指定时交给主 Agent
//!   （`HarnessConfig::main_agent`）。Task 派发复用 `prompt_builder::build_prompt_with_context`，
//!   构建 LTM/STM/通道上下文 + skills 系统提示。

use crate::prelude::*;
use tracing::{debug, warn};

use crate::{
    contracts::Clock,
    domain::HookPoint,
    domain::{
        Agent, AgentExecutionRequest, AgentExecutionRequestMessage, AgentRequestKind,
        ConversationMessage, DispatchKind, ExecutionError, LongTermMemory,
        MessageDispatchedHookPending, PendingDispatch, ShortTermMemory, SpaceToolRegistry, Task,
        TaskStatus, ToolPermission, WorkItem, WorkItemLifecycleHookPending, WorkItemType,
    },
    infrastructure::skills::{LoadedSkill, PluginSkillContributions, SkillCatalog, SkillLoader},
    systems::HarnessSettings,
};

use super::prompt_builder::build_prompt_with_context;

/// 仅恢复运行时实际协议。展示/压缩记忆走文本提示，不伪造工具参数与轮次。
fn build_structured_conversation(stm: &ShortTermMemory) -> Option<Vec<ConversationMessage>> {
    stm.protocol.restore()
}

/// 一次计算成本，按 Assistant/tool 配对组整体裁剪，避免悬空工具结果。
fn truncate_conversation_by_budget(messages: &mut Vec<ConversationMessage>, budget_tokens: u32) {
    let costs: Vec<u64> = messages
        .iter()
        .map(|message| u64::from(message.estimated_tokens()))
        .collect();
    let mut total: u64 = costs.iter().sum();
    let mut end = 0;
    while end < messages.len() && total > u64::from(budget_tokens) {
        let anchor = matches!(&messages[end], ConversationMessage::Assistant { tool_calls, .. } if !tool_calls.is_empty());
        total -= costs[end];
        end += 1;
        if anchor {
            while end < messages.len() && matches!(&messages[end], ConversationMessage::Tool { .. })
            {
                total -= costs[end];
                end += 1;
            }
        }
    }
    messages.drain(..end);
}

/// 构建派发时的技能清单：加载全局与插件 skill 并刷新 `SkillCatalog`，
/// 返回对该 Agent 可见的部分（未声明 agents 的 skill 仅主 Agent 可见）。
fn visible_skills(
    skill_loader: &SkillLoader,
    catalog: &SkillCatalog,
    plugin_skills: &PluginSkillContributions,
    agent_name: &str,
    main_agent: &str,
) -> Vec<LoadedSkill> {
    let all = skill_loader.load_all(plugin_skills);
    let visible = all
        .iter()
        .filter(|s| s.visible_to(agent_name, main_agent))
        .cloned()
        .collect();
    catalog.replace(all);
    visible
}

/// Agent 自身的系统提示与技能清单拼接，两者都可能为空。
fn compose_system_prompt(agent_prompt: Option<&str>, skills_catalog: &str) -> Option<String> {
    let parts: Vec<&str> = [agent_prompt.unwrap_or_default(), skills_catalog]
        .into_iter()
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    (!parts.is_empty()).then(|| parts.join("\n\n"))
}

/// 统一派发 System
///
/// 扫描带 `PendingDispatch` 的 Task / WorkItem Entity，按 `DispatchHint`
/// 执行派发决策。详见模块文档。
#[allow(clippy::too_many_arguments)]
pub fn dispatch_system(
    clock: Res<Clock>,
    mut commands: Commands,
    agents: Query<(&Agent, Option<&LongTermMemory>)>,
    registry: Res<SpaceToolRegistry>,
    settings: Option<Res<HarnessSettings>>,
    skill_loader: Res<SkillLoader>,
    skill_catalog: Res<SkillCatalog>,
    plugin_skills: Res<PluginSkillContributions>,
    mut tasks: Query<(
        Entity,
        &mut Task,
        Option<&ShortTermMemory>,
        Option<&PendingDispatch>,
    )>,
    mut work_items: Query<(Entity, &mut WorkItem, Option<&PendingDispatch>)>,
) {
    let main_agent_name = settings
        .as_deref()
        .map(|s| s.0.main_agent.as_str())
        .unwrap_or(crate::systems::DEFAULT_MAIN_AGENT);
    // 收集 agents 引用，便于按 tag/name 查找
    let agent_refs: Vec<&Agent> = agents.iter().map(|(a, _)| a).collect();

    // ---------- 处理 WorkItem 派发 ----------
    for (entity, mut work_item, pending) in &mut work_items {
        let Some(pending) = pending else {
            continue;
        };

        let DispatchKind::WorkItem(work_type) = &pending.kind else {
            // Task kind 不在 WorkItem 处理路径
            continue;
        };

        let work_type = *work_type;
        let required_tag = work_type.required_tag();

        // 通过 tag 查找匹配的 Persistent Agent
        let agent = agent_refs
            .iter()
            .copied()
            .find(|agent| agent.capabilities.tags.contains(&required_tag.to_string()));

        let Some(agent) = agent else {
            // 找不到 Agent → fail + 派发 OnWorkItemFailed hook
            warn!(
                event = "DispatchWorkItemNoAgentFound",
                work_item_id = %work_item.id,
                task_id = %work_item.task_id,
                work_type = ?work_type,
                required_tag = required_tag,
                "no suitable agent found for work item, marking as failed"
            );
            work_item.fail();
            commands
                .entity(entity)
                .insert(WorkItemLifecycleHookPending(HookPoint::OnWorkItemFailed))
                .remove::<PendingDispatch>();
            continue;
        };

        // 状态转换：Pending -> Assigned -> Running
        work_item.assign(agent.id);
        work_item.start();

        // 标记 WorkItem 已启动，等待 companion 系统派发 OnWorkItemStarted hook
        commands
            .entity(entity)
            .insert(WorkItemLifecycleHookPending(HookPoint::OnWorkItemStarted));

        // request_kind 映射：Evaluation/Summarization 专用，其他走 LlmCompletion
        let request_kind = match work_type {
            WorkItemType::Evaluation => AgentRequestKind::Evaluation,
            WorkItemType::Summarization => AgentRequestKind::Summarization,
            // 管理员请求不得把所属任务标记为 Running（检索期间父任务仍在等待工具结果）。
            WorkItemType::ExperienceLookup | WorkItemType::ExperienceDraft => {
                AgentRequestKind::Experience
            }
            WorkItemType::WatchdogReview => AgentRequestKind::Watchdog,
            _ => AgentRequestKind::LlmCompletion,
        };

        // spawn AgentExecutionRequestMessage
        commands.spawn((
            AgentExecutionRequestMessage {
                request: AgentExecutionRequest {
                    task_id: work_item.task_id,
                    agent_id: agent.id,
                    request_kind,
                    prompt: work_item.input.prompt.clone(),
                    system_prompt: work_item
                        .input
                        .context
                        .system_prompt
                        .clone()
                        .or_else(|| agent.system_prompt.clone()),
                    tools: work_item.input.context.tools.clone(),
                    conversation: work_item.input.context.conversation.clone(),
                    work_item_id: Some(work_item.id),
                    model_override: None,
                    turn_generation: None,
                    vision: false,
                    trusted_images: Vec::new(),
                },
            },
            MessageDispatchedHookPending,
        ));

        // 派发完成，移除 PendingDispatch
        commands.entity(entity).remove::<PendingDispatch>();

        debug!(
            event = "DispatchWorkItemDispatched",
            task_id = %work_item.task_id,
            work_item_id = %work_item.id,
            work_type = ?work_type,
            agent_id = %agent.id,
            agent_name = %agent.profile.name,
            "work item dispatched via unified dispatch_system"
        );
    }

    // ---------- 处理 Task 派发 ----------
    for (task_entity, mut task, short_term, pending) in &mut tasks {
        let Some(pending) = pending else {
            continue;
        };

        if !matches!(pending.kind, DispatchKind::Task) {
            // WorkItem kind 不在 Task 处理路径
            continue;
        }

        let hint = &pending.hint;

        // 未指定 Agent 的初次派发：仅处理 Ready/Pending 且无 delegate 的 Task，
        // 避免重复派发正在执行的任务。
        if hint.preferred_agent_name.is_none()
            && ((task.status != TaskStatus::Ready && task.status != TaskStatus::Pending)
                || task.delegate.is_some())
        {
            commands.entity(task_entity).remove::<PendingDispatch>();
            continue;
        }

        // 按 preferred_agent_name（未指定时为主 Agent）查找 Agent，同时取出 LongTermMemory
        let target_name = hint
            .preferred_agent_name
            .as_deref()
            .unwrap_or(main_agent_name);
        let agent_with_ltm = agents.iter().find(|(a, _)| a.profile.name == target_name);

        if let Some((agent, long_term)) = agent_with_ltm {
            // 构建 tools 列表：从 registry 中筛选 Agent 有权限的工具（非 Deny）
            let tools: Vec<_> = registry
                .iter()
                .filter(|tool_def| {
                    !matches!(
                        agent
                            .tool_permissions
                            .effective_permission(&tool_def.name, Some(&registry))
                            .0,
                        ToolPermission::Deny
                    )
                })
                .cloned()
                .collect();

            // 判断是否走结构化路径
            let (prompt, conversation) = if let Some(stm) = short_term {
                if let Some(conv) = build_structured_conversation(stm) {
                    // 结构化路径：prompt 为空，conversation 为还原结果
                    let mut conv = conv;
                    // 硬截断兜底：按模型窗口预算裁剪
                    // TODO: budget_tokens 应从模型配置获取，暂用 100000 作为安全上限
                    truncate_conversation_by_budget(&mut conv, 100_000);
                    (String::new(), Some(conv))
                } else {
                    // 纯文本路径
                    let prompt = build_prompt_with_context(
                        &task.content,
                        Some(stm),
                        long_term,
                        task.origin_channel.as_ref(),
                    );
                    (prompt, None)
                }
            } else {
                let prompt = build_prompt_with_context(
                    &task.content,
                    None,
                    long_term,
                    task.origin_channel.as_ref(),
                );
                (prompt, None)
            };

            // 系统提示 = Agent 自身提示 + 技能清单（完整说明由 load_skill 按需读取）
            let skills = visible_skills(
                &skill_loader,
                &skill_catalog,
                &plugin_skills,
                &agent.profile.name,
                main_agent_name,
            );
            let system_prompt = compose_system_prompt(
                agent.system_prompt.as_deref(),
                &SkillLoader::format_skills_catalog(&skills),
            );

            let stm_entries = short_term.map(|s| s.entries.len()).unwrap_or(0);
            let stm_tokens = short_term.map(|s| s.estimated_tokens).unwrap_or(0);
            let ltm_entries = long_term.map(|l| l.entries.len()).unwrap_or(0);

            let request = AgentExecutionRequest {
                task_id: task.id,
                agent_id: agent.id,
                request_kind: AgentRequestKind::LlmCompletion,
                prompt,
                system_prompt,
                tools,
                conversation,
                work_item_id: None,
                model_override: None,
                turn_generation: None,
                vision: false,
                trusted_images: Vec::new(),
            };

            task.mark_waiting_for_agent(agent.id, clock.0);
            commands.spawn((
                AgentExecutionRequestMessage { request },
                MessageDispatchedHookPending,
            ));
            commands.entity(task_entity).remove::<PendingDispatch>();

            debug!(
                event = "DispatchTaskDelegated",
                task_id = %task.id,
                agent_id = %agent.id,
                agent_name = %agent.profile.name,
                stm_entries = stm_entries,
                stm_tokens = stm_tokens,
                ltm_entries = ltm_entries,
                "task delegated to persistent agent"
            );
            continue;
        }

        // 找不到目标 Agent → Task Failed
        let error = ExecutionError::Unknown(format!(
            "no persistent agent named '{target_name}' found for task dispatch"
        ));
        task.mark_failed(&error, clock.0);
        commands.entity(task_entity).remove::<PendingDispatch>();

        warn!(
            event = "DispatchTaskNoAgent",
            task_id = %task.id,
            agent_name = %target_name,
            "no agent for task dispatch, marking task as failed"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{EntryMetadata, EntryRole, ToolCall};
    use tempfile::TempDir;

    #[test]
    fn build_structured_conversation_from_stm() {
        let mut stm = ShortTermMemory::default();
        stm.add_entry(EntryRole::User, "list files", EntryMetadata::default());
        let mut metadata = EntryMetadata::default();
        metadata.tool_calls.push(ToolCall {
            id: Some("call_1".to_string()),
            tool_name: "shell_exec".to_string(),
            input: "ls".to_string(),
            output: "file1.txt\nfile2.txt".to_string(),
            timestamp: chrono::Utc::now(),
        });
        stm.add_entry(EntryRole::Assistant, "done", metadata);
        stm.add_entry(EntryRole::User, "next question", EntryMetadata::default());
        stm.add_entry(EntryRole::Assistant, "answer", EntryMetadata::default());

        assert!(
            build_structured_conversation(&stm).is_none(),
            "display metadata is lossy and must never be replayed as tool protocol"
        );
    }

    #[test]
    fn budget_truncation_counts_reasoning_and_removes_whole_tool_groups() {
        let mut messages = vec![
            ConversationMessage::Assistant {
                content: None,
                reasoning_content: Some("long reasoning ".repeat(100)),
                tool_calls: vec![crate::domain::LlmToolCall {
                    id: "list".into(),
                    name: "shell_list".into(),
                    arguments: "{}".into(),
                }],
            },
            ConversationMessage::Tool {
                tool_call_id: "list".into(),
                content: "[]".into(),
            },
            ConversationMessage::User {
                content: "now".into(),
            },
        ];
        truncate_conversation_by_budget(&mut messages, crate::domain::estimate_tokens("now"));
        assert_eq!(
            messages,
            vec![ConversationMessage::User {
                content: "now".into()
            }]
        );
    }

    // ---------- skills ----------

    #[test]
    fn visible_skills_filters_by_agent_and_refreshes_catalog() {
        let tmp = TempDir::new().unwrap();
        let skills_dir = tmp.path().join("skills");
        for (dir, front) in [
            ("main-only", "name: main-only"),
            ("shared", "name: shared\nagents: [summarizer, main]"),
        ] {
            std::fs::create_dir_all(skills_dir.join(dir)).unwrap();
            std::fs::write(
                skills_dir.join(dir).join("SKILL.md"),
                format!("---\n{front}\ndescription: d\n---\nBody.\n"),
            )
            .unwrap();
        }
        let loader = SkillLoader::new(skills_dir);
        let catalog = SkillCatalog::default();
        let plugins = PluginSkillContributions::default();

        let names = |skills: Vec<LoadedSkill>| -> Vec<String> {
            skills.into_iter().map(|s| s.name).collect()
        };
        let main = visible_skills(&loader, &catalog, &plugins, "main", "main");
        assert_eq!(names(main), ["main-only", "shared"]);
        let summarizer = visible_skills(&loader, &catalog, &plugins, "summarizer", "main");
        assert_eq!(names(summarizer), ["shared"]);
        // 目录不包含的 Agent 不可见，但 catalog 仍保存全部 skill 供 load_skill 查找
        assert!(visible_skills(&loader, &catalog, &plugins, "watchdog", "main").is_empty());
        assert!(catalog.find("main-only").is_some());
    }

    #[test]
    fn compose_system_prompt_keeps_agent_prompt_and_appends_catalog() {
        assert_eq!(
            compose_system_prompt(Some("你是助手"), "## 可用技能\n- `a`：d\n"),
            Some("你是助手\n\n## 可用技能\n- `a`：d".to_string())
        );
        assert_eq!(
            compose_system_prompt(Some("你是助手"), ""),
            Some("你是助手".into())
        );
        assert_eq!(compose_system_prompt(None, "清单"), Some("清单".into()));
        assert_eq!(compose_system_prompt(None, ""), None);
        assert_eq!(compose_system_prompt(Some("  "), ""), None);
    }
}
