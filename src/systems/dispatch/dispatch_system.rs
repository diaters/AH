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
//! 阶段 5：`task_dispatch_system` 与 `workitem_dispatch_system` 已退役，
//! 所有派发请求统一通过 `PendingDispatch` 流入本 system 处理。
//! DirectDelegate 分支复用 `prompt_builder::build_prompt_with_context`，
//! 与原 `task_dispatch_system` 行为对齐（LTM/STM/通道上下文 + skills 系统提示）。

use crate::prelude::*;
use tracing::{debug, warn};

use crate::{
    app::Clock,
    domain::{
        Agent, AgentExecutionRequest, AgentExecutionRequestMessage, AgentKind, AgentRequestKind,
        AgentSpawnRequestMessage, AwaitingBrainDecision, DispatchHint, DispatchKind,
        DispatchStrategy, ExecutionError, LongTermMemory, MessageDispatchedHookPending,
        PendingDispatch, ShortTermMemory, SpaceToolRegistry, Task, TaskInjectedSkill, TaskStatus,
        ToolPermission, WaitingReason, WorkItem, WorkItemLifecycleHookPending, WorkItemType,
    },
    infrastructure::skills::{PluginSkillContributions, SkillLoader},
    user_plugins::hook_point::HookPoint,
};

use super::{build_brain_execution_request, prompt_builder::build_prompt_with_context};

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
                // 调用 brain_llm_builder 构造 Brain LLM 执行请求
                let brain_request =
                    build_brain_execution_request(&task, short_term, &agent_refs, &registry);

                let Some((request_message, hook_pending)) = brain_request else {
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

                let brain_agent_id = request_message.request.agent_id;

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
                                agent.tool_permissions.get_permission(&tool_def.name),
                                ToolPermission::Deny
                            )
                        })
                        .cloned()
                        .collect();

                    // 构建 prompt：完整 LTM/STM/通道上下文（与原 task_dispatch 行为对齐）
                    let prompt = build_prompt_with_context(
                        &task.content,
                        short_term,
                        long_term,
                        task.origin_channel.as_ref(),
                    );

                    // 构建 skills 系统提示（内置 + 插件贡献）
                    let mut skills = skill_loader.load_skills(&agent.profile.name);
                    skills.extend(
                        skill_loader.load_plugin_skills(&plugin_skills, &agent.profile.name),
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
                        conversation: None,
                        work_item_id: None,
                        model_override: None,
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
                    let parent_agent_id = spec.parent_agent_id.unwrap_or(uuid::Uuid::nil());
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
                    task.status = TaskStatus::Waiting(WaitingReason::Agent);
                    task.updated_at = clock.0;

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
