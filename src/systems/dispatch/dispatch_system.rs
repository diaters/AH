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
//! 所有派发请求统一通过 `PendingDispatch` 流入本 system 处理。
//! DirectDelegate 分支复用 `prompt_builder::build_prompt_with_context`，
//! 构建 LTM/STM/通道上下文 + skills 系统提示。

use crate::prelude::*;
use tracing::{debug, warn};

use crate::{
    contracts::Clock,
    domain::HookPoint,
    domain::{
        Agent, AgentExecutionRequest, AgentExecutionRequestMessage, AgentKind, AgentRequestKind,
        AgentSpawnRequestMessage, AwaitingBrainDecision, ConversationMessage, DispatchHint,
        DispatchKind, DispatchStrategy, ExecutionError, LongTermMemory,
        MessageDispatchedHookPending, PendingDispatch, ShortTermMemory, SpaceToolRegistry, Task,
        TaskInjectedSkill, TaskStatus, ToolPermission, WaitingReason, WorkItem,
        WorkItemLifecycleHookPending, WorkItemType,
    },
    ecs::EntityIndex,
    infrastructure::skills::{
        LoadedSkill, PluginSkillContributions, SkillId, SkillLoader, SkillRegistry,
        resolve_skill_closure,
    },
};

use super::prompt_builder::build_prompt_with_context;
use crate::systems::brain::{build_brain_execution_request, find_brain_agent};

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

/// 构建 DirectDelegate 注入的 skill 列表：
/// - Brain 选中 skill 时，内置 skill 收窄为依赖闭包（拓扑序：依赖在前、选中最后），
///   插件 skill 维持全量追加（设计 D4 / 边界 B2）
/// - 未选中（None）时，维持 agent 名下全量注入（决策 Q1，行为不变）
fn build_injected_skills(
    skill_loader: &SkillLoader,
    agent_name: &str,
    required_skill_id: Option<&SkillId>,
    plugin_skills: &PluginSkillContributions,
) -> Vec<LoadedSkill> {
    let all_skills = skill_loader.load_skills(agent_name);
    let mut skills = match required_skill_id {
        Some(skill_id) => {
            // Brain 选中 skill：内置 skill 收窄为依赖闭包
            let closure = resolve_skill_closure(&all_skills, &skill_id.skill_name);
            if !closure.is_empty() {
                closure.into_iter().cloned().collect()
            } else {
                // 闭包为空（root 缺失等）：降级为全量注入，避免行为回退（设计 G4）
                all_skills
            }
        }
        None => all_skills,
    };
    // 插件 skill 维持全量追加（设计文档 B2）
    skills.extend(skill_loader.load_plugin_skills(plugin_skills, agent_name));
    skills
}

/// 统一派发 System
///
/// 扫描带 `PendingDispatch` 的 Task / WorkItem Entity，按 `DispatchHint`
/// 执行派发决策。详见模块文档。
#[allow(clippy::too_many_arguments)]
pub fn dispatch_system(
    clock: Res<Clock>,
    mut commands: Commands,
    index: Res<EntityIndex>,
    agents: Query<(&Agent, Option<&LongTermMemory>)>,
    registry: Res<SpaceToolRegistry>,
    skill_registry: Res<SkillRegistry>,
    skill_loader: Res<SkillLoader>,
    plugin_skills: Res<PluginSkillContributions>,
    mut tasks: Query<(
        Entity,
        &mut Task,
        Option<&ShortTermMemory>,
        Option<&PendingDispatch>,
    )>,
    mut work_items: Query<(Entity, &mut WorkItem, Option<&PendingDispatch>)>,
) {
    // 收集 agents 引用，便于复用 brain_llm_builder 与按 tag/name 查找
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
        let agent = agent_refs.iter().copied().find(|agent| {
            agent.kind == AgentKind::Persistent
                && agent.capabilities.tags.contains(&required_tag.to_string())
        });

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

        // BrainLlm 策略：仅处理 Ready/Pending 且无 delegate 的 Task（初始派发）
        // DirectDelegate 策略：由 brain_decision_system 产出，task 可能处于 Waiting(Agent)
        // 且有 delegate（指向 Brain agent），不检查状态与 delegate
        if matches!(pending.hint.strategy, DispatchStrategy::BrainLlm) {
            // 跳过非 Ready/Pending 状态
            if task.status != TaskStatus::Ready && task.status != TaskStatus::Pending {
                commands.entity(task_entity).remove::<PendingDispatch>();
                continue;
            }

            // 跳过已有 delegate 的 Task
            if task.delegate.is_some() {
                commands.entity(task_entity).remove::<PendingDispatch>();
                continue;
            }
        }

        let hint: &DispatchHint = &pending.hint;

        match hint.strategy {
            DispatchStrategy::BrainLlm => {
                // 1. 过滤 brain candidates 并选择 brain_agent_id（复合查询，保留遍历）
                let Some(brain_agent_id) = find_brain_agent(&agent_refs) else {
                    // 未找到 Brain Agent → Task Failed
                    let error = ExecutionError::Unknown(
                        "no brain agent found for BrainLlm dispatch".to_string(),
                    );
                    task.mark_failed(&error, clock.0);
                    commands.entity(task_entity).remove::<PendingDispatch>();
                    warn!(
                        event = "DispatchTaskBrainLlmNoBrainAgent",
                        task_id = %task.id,
                        "no brain agent available, marking task as failed"
                    );
                    continue;
                };

                // 2. 经 EntityIndex O(1) 解析 AgentId → Entity → &Agent（ADR-005 §3）
                let Some(brain_agent) = index
                    .get_agent(&brain_agent_id)
                    .and_then(|e| agents.get(e).ok())
                    .map(|(a, _)| a)
                else {
                    // 索引不一致 → Task Failed
                    let error = ExecutionError::Unknown(format!(
                        "brain agent {brain_agent_id} not found in EntityIndex"
                    ));
                    task.mark_failed(&error, clock.0);
                    commands.entity(task_entity).remove::<PendingDispatch>();
                    warn!(
                        event = "DispatchTaskBrainLlmIndexMiss",
                        task_id = %task.id,
                        brain_agent_id = %brain_agent_id,
                        "brain agent id not in EntityIndex, marking task as failed"
                    );
                    continue;
                };

                // 3. 构建 Brain LLM 执行请求（brain_agent 已解析，不再返回 Option）
                let (request_message, hook_pending) = build_brain_execution_request(
                    &task,
                    short_term,
                    brain_agent,
                    &agent_refs,
                    &registry,
                    &skill_registry,
                );

                // 附加 AwaitingBrainDecision，由 brain_decision_system 处理 Brain 输出后移除
                commands.entity(task_entity).insert(AwaitingBrainDecision {
                    task_id: task.id,
                    spawn_spec: hint.agent_spawn_spec.clone(),
                });

                // 切换 Task 到 Waiting(Agent)，spawn Brain LLM 调用
                task.mark_waiting_for_agent(brain_agent_id, clock.0);
                commands.spawn((request_message, hook_pending));

                commands.entity(task_entity).remove::<PendingDispatch>();

                debug!(
                    event = "DispatchTaskBrainLlmDispatched",
                    task_id = %task.id,
                    brain_agent_id = %brain_agent_id,
                    "task dispatched via BrainLlm strategy"
                );
            }
            DispatchStrategy::DirectDelegate => {
                // 按 preferred_agent_name 查找 Persistent Agent（同时取出 LongTermMemory）
                let preferred_name = hint.preferred_agent_name.as_deref();
                let agent_with_ltm = preferred_name.and_then(|name| {
                    agents
                        .iter()
                        .find(|(a, _)| a.kind == AgentKind::Persistent && a.profile.name == name)
                });

                if let Some((agent, long_term)) = agent_with_ltm {
                    // 注入 TaskInjectedSkill（如果 hint 携带 required_skill_id）
                    if let Some(skill_id) = &hint.required_skill_id {
                        commands.entity(task_entity).insert(TaskInjectedSkill {
                            skill_id: Some(skill_id.clone()),
                        });
                        debug!(
                            event = "DispatchTaskDirectDelegateSkillInjected",
                            task_id = %task.id,
                            agent_name = %agent.profile.name,
                            skill_id = %skill_id.as_string(),
                            "injected skill for direct delegated task"
                        );
                    }

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

                    // 构建 skills 系统提示（内置 + 插件贡献；Brain 选中 skill 时收窄为依赖闭包）
                    let skills = build_injected_skills(
                        &skill_loader,
                        &agent.profile.name,
                        hint.required_skill_id.as_ref(),
                        &plugin_skills,
                    );
                    let skills_prompt = SkillLoader::format_skills_prompt(&skills);
                    // 优先级：skills_prompt 非空时用 skills_prompt，否则用 agent.system_prompt
                    let system_prompt = if !skills_prompt.is_empty() {
                        Some(skills_prompt)
                    } else {
                        agent.system_prompt.clone()
                    };

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
                        event = "DispatchTaskDirectDelegated",
                        task_id = %task.id,
                        agent_id = %agent.id,
                        agent_name = %agent.profile.name,
                        stm_entries = stm_entries,
                        stm_tokens = stm_tokens,
                        ltm_entries = ltm_entries,
                        "task directly delegated to existing agent"
                    );
                    continue;
                }

                // 找不到 Agent → 检查 spawn_spec
                if let Some(spec) = &hint.agent_spawn_spec {
                    // spawn 新 Agent（参考 brain_dispatch.rs 中 SubTask 路径）
                    let parent_agent_id = spec
                        .parent_agent_id
                        .unwrap_or(crate::domain::AgentId::nil());
                    commands.spawn(AgentSpawnRequestMessage {
                        parent_agent_id,
                        task_id: task.id,
                        name: spec.name.clone(),
                        model: spec.model.clone(),
                        description: spec.name.clone(),
                        tools: spec.allowed_tools.clone(),
                        task_prompt: task.content.clone(),
                        task_system_prompt: None,
                    });

                    // Agent 尚未生成，无法设置 delegate；仅切到 Waiting(Agent)
                    task.mark_waiting(WaitingReason::Agent, clock.0);

                    commands.entity(task_entity).remove::<PendingDispatch>();

                    debug!(
                        event = "DispatchTaskDirectDelegateSpawn",
                        task_id = %task.id,
                        spawn_name = %spec.name,
                        "task waiting for spawned agent"
                    );
                    continue;
                }

                // 既找不到 Agent 也无 spawn_spec → Task Failed
                let error = ExecutionError::Unknown(format!(
                    "no agent found for DirectDelegate dispatch, preferred_agent_name={:?}",
                    hint.preferred_agent_name
                ));
                task.mark_failed(&error, clock.0);
                commands.entity(task_entity).remove::<PendingDispatch>();

                warn!(
                    event = "DispatchTaskDirectDelegateNoAgent",
                    task_id = %task.id,
                    preferred_agent_name = ?hint.preferred_agent_name,
                    "no agent and no spawn_spec for DirectDelegate, marking task as failed"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{EntryMetadata, EntryRole, ToolCall};
    use crate::infrastructure::skills::PluginSkillEntry;
    use tempfile::TempDir;

    /// 写入一个 SKILL.md 到 `<agents_dir>/<agent>/skills/<skill_name>/SKILL.md`。
    /// `agents_dir` 应直接指向 `agents/` 目录本身，与 `SkillLoader::new` 语义一致。
    fn write_skill(agents_dir: &std::path::Path, agent: &str, skill_name: &str, content: &str) {
        let dir = agents_dir.join(agent).join("skills").join(skill_name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), content).unwrap();
    }

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

    // ---------- build_injected_skills ----------

    #[test]
    fn build_injected_skills_narrows_to_dependency_closure_when_selected() {
        // Brain 选中 dep-a 时：内置 skill 收窄为依赖闭包（依赖在前，选中最后），
        // 不含无关的 dep-b
        let tmp = TempDir::new().unwrap();
        let agents_dir = tmp.path().join("agents");
        write_skill(
            &agents_dir,
            "agent-a",
            "base",
            "---\nname: base\ndescription: base skill\n---\n\n## Usage\n\nDo base.\n",
        );
        write_skill(
            &agents_dir,
            "agent-a",
            "dep-a",
            "---\nname: dep-a\ndescription: dep-a skill\ndependencies: [base]\n---\n\n## Usage\n\nDo a.\n",
        );
        write_skill(
            &agents_dir,
            "agent-a",
            "dep-b",
            "---\nname: dep-b\ndescription: dep-b skill\n---\n\n## Usage\n\nDo b.\n",
        );

        let loader = SkillLoader::new(agents_dir);
        let plugin = PluginSkillContributions::default();
        let skill_id = SkillId::new("agent-a", "dep-a");
        let skills = build_injected_skills(&loader, "agent-a", Some(&skill_id), &plugin);

        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["base", "dep-a"],
            "依赖在前、选中最后，且不含无关 skill"
        );
    }

    #[test]
    fn build_injected_skills_selected_skill_without_dependencies_returns_self() {
        // 选中 skill 无依赖时：只返回选中 skill 本身
        let tmp = TempDir::new().unwrap();
        let agents_dir = tmp.path().join("agents");
        write_skill(
            &agents_dir,
            "agent-a",
            "solo",
            "---\nname: solo\ndescription: solo skill\n---\n\n## Usage\n\nSolo.\n",
        );

        let loader = SkillLoader::new(agents_dir);
        let plugin = PluginSkillContributions::default();
        let skill_id = SkillId::new("agent-a", "solo");
        let skills = build_injected_skills(&loader, "agent-a", Some(&skill_id), &plugin);

        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["solo"]);
    }

    #[test]
    fn build_injected_skills_none_keeps_full_injection() {
        // Brain 未选中 skill（None）时：维持 agent 名下全量注入（决策 Q1）
        let tmp = TempDir::new().unwrap();
        let agents_dir = tmp.path().join("agents");
        write_skill(
            &agents_dir,
            "agent-a",
            "base",
            "---\nname: base\ndescription: base skill\n---\n\n## Usage\n\nDo base.\n",
        );
        write_skill(
            &agents_dir,
            "agent-a",
            "dep-a",
            "---\nname: dep-a\ndescription: dep-a skill\n---\n\n## Usage\n\nDo a.\n",
        );
        write_skill(
            &agents_dir,
            "agent-a",
            "dep-b",
            "---\nname: dep-b\ndescription: dep-b skill\n---\n\n## Usage\n\nDo b.\n",
        );

        let loader = SkillLoader::new(agents_dir);
        let plugin = PluginSkillContributions::default();
        let skills = build_injected_skills(&loader, "agent-a", None, &plugin);

        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names.len(), 3, "未选中时全量注入");
        for n in ["base", "dep-a", "dep-b"] {
            assert!(names.contains(&n), "应包含 skill {n}");
        }
    }

    #[test]
    fn build_injected_skills_appends_plugin_skills_when_selected() {
        // 选中 skill 时：内置闭包在前，插件 skill 维持全量追加在后（设计 B2）
        let tmp = TempDir::new().unwrap();
        let agents_dir = tmp.path().join("agents");
        write_skill(
            &agents_dir,
            "agent-a",
            "base",
            "---\nname: base\ndescription: base skill\n---\n\n## Usage\n\nDo base.\n",
        );
        write_skill(
            &agents_dir,
            "agent-a",
            "dep-a",
            "---\nname: dep-a\ndescription: dep-a skill\ndependencies: [base]\n---\n\n## Usage\n\nDo a.\n",
        );

        // 插件贡献的 skill：独立 SKILL.md 文件，经命名空间化为 plugin_id:skill_name
        let plugin_md = tmp.path().join("plugin-skill.md");
        std::fs::write(
            &plugin_md,
            "---\nname: p1\ndescription: plugin one\n---\n\n## Usage\n\nPlugin.\n",
        )
        .unwrap();
        let plugin = PluginSkillContributions {
            entries: vec![PluginSkillEntry {
                plugin_id: "my-plugin".to_string(),
                skill_id: "p1".to_string(),
                path: plugin_md,
            }],
        };

        let loader = SkillLoader::new(agents_dir);
        let skill_id = SkillId::new("agent-a", "dep-a");
        let skills = build_injected_skills(&loader, "agent-a", Some(&skill_id), &plugin);

        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["base", "dep-a", "my-plugin:p1"]);
    }

    #[test]
    fn build_injected_skills_falls_back_to_full_when_closure_empty() {
        // Brain 选中的 root skill 在 load_skills 结果中找不到（如 registry 快照与磁盘漂移、
        // skill 目录被删、owner 不匹配）时，依赖闭包为空：应降级为全量注入，
        // 而非注入空列表（设计 G4 行为变更最小化）
        let tmp = TempDir::new().unwrap();
        let agents_dir = tmp.path().join("agents");
        write_skill(
            &agents_dir,
            "agent-a",
            "base",
            "---\nname: base\ndescription: base skill\n---\n\n## Usage\n\nDo base.\n",
        );
        write_skill(
            &agents_dir,
            "agent-a",
            "solo",
            "---\nname: solo\ndescription: solo skill\n---\n\n## Usage\n\nSolo.\n",
        );

        let loader = SkillLoader::new(agents_dir);
        let plugin = PluginSkillContributions::default();
        // root 不存在的 skill_id：skill_name 在 load_skills 结果中找不到 → 闭包为空
        let skill_id = SkillId::new("agent-a", "nonexistent");
        let skills = build_injected_skills(&loader, "agent-a", Some(&skill_id), &plugin);

        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names.len(), 2, "闭包为空时应降级为全量注入");
        for n in ["base", "solo"] {
            assert!(names.contains(&n), "应包含 skill {n}");
        }
    }
}
