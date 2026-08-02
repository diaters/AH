//! Tool 分发 System
//!
//! 检查 Tool 权限并决定直接执行、用户确认或父 Agent 审批。

use crate::prelude::*;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::{
    app::{Clock, FrontendRegistry, HarnessSettings},
    domain::{
        Agent, ApprovalRequestMessage, ApprovalRequestedHookPending, BuiltinToolExecutors,
        ChatSession, ConfirmationOption, ConfirmationSource, EngineEvent, EventTarget,
        ExperienceStore, PendingExperienceHooks, ProfileGenerationContext, SharedKnowledgeBase,
        ShortTermMemory, SkillUpdateContext, SpaceToolRegistry, Task, TaskStatus,
        ToolConfirmationRequestMessage, ToolContext, ToolError, ToolExecutionRequestMessage,
        ToolPermission, WaitingReason, WorkItem,
    },
    ecs::EntityIndex,
    infrastructure::skills::SkillLoader,
    systems::NativeProcessBackend,
};

use super::orchestrator::restore_task_after_tool;
use super::orchestrator::{handle_tool_action, spawn_tool_error};

/// Tool 分发 System
///
/// 检查 Tool 权限并决定直接执行、用户确认或父 Agent 审批
#[allow(clippy::too_many_arguments)]
pub fn tool_dispatch_system(
    mut commands: Commands,
    mut tasks: Query<(Entity, &mut Task)>,
    registry: Res<SpaceToolRegistry>,
    executors: Res<BuiltinToolExecutors>,
    knowledge: Res<SharedKnowledgeBase>,
    mut experience_store: ResMut<ExperienceStore>,
    mut pending_experience_hooks: ResMut<PendingExperienceHooks>,
    agents: Query<&mut Agent>,
    mut short_term_memories: Query<&mut ShortTermMemory>,
    chat_sessions: Query<&ChatSession>,
    calling_states: Query<(Entity, &crate::domain::ToolCallingState)>,
    mut requests: Query<(Entity, &mut ToolExecutionRequestMessage)>,
    // 合并 ProfileGenerationContext 与 SkillUpdateContext 查询为单个 SystemParam，
    // 规避 Bevy 单 system 16 参数上限；两者都是与 WorkItem 同 entity 的 Component，
    // 通过 Option<&...> 区分（任一 WorkItem entity 至多只有其中之一）。
    context_queries: Query<(
        Entity,
        Option<&ProfileGenerationContext>,
        Option<&SkillUpdateContext>,
        &WorkItem,
    )>,
    settings: Res<HarnessSettings>,
    backend: Res<NativeProcessBackend>,
    // 合并 index / clock / skill_loader / frontend_registry 为单 SystemParam，规避 Bevy 单 system 16 参数上限；
    // index 用于 O(1) UUID 解析；clock/skill_loader 转发给 handle_tool_action；
    // frontend_registry 用于在 Allow 路径推送 ToolCallStarted 事件。
    index_clock_loader: (
        Res<EntityIndex>,
        Res<Clock>,
        Res<SkillLoader>,
        Res<FrontendRegistry>,
    ),
) {
    let index = &index_clock_loader.0;
    let frontend_registry = &index_clock_loader.3;
    for (entity, mut request) in &mut requests {
        // 跳过已经在等待确认的请求
        if request.pending_confirmation_id.is_some() {
            continue;
        }

        let tool_name = request.tool_name.clone();

        // 查找 Tool 定义
        let Some(tool_def) = registry.get(&tool_name) else {
            warn!(
                event = "ToolNotFound",
                tool_name = %tool_name,
                task_id = %request.request.task_id,
                agent_id = %request.request.agent_id,
                "tool not found in registry"
            );
            spawn_tool_error(
                &mut commands,
                entity,
                &request,
                ToolError::NotFound(tool_name.clone()),
            );
            continue;
        };

        // 获取 Agent 权限
        // 经 EntityIndex O(1) 解析 AgentId → Entity（替代全量线性扫描）
        let Some(agent) = index
            .get_agent(&request.request.agent_id)
            .and_then(|e| agents.get(e).ok())
        else {
            warn!(
                event = "AgentNotFound",
                agent_id = %request.request.agent_id,
                tool_name = %tool_name,
                "agent not found for tool execution"
            );
            spawn_tool_error(
                &mut commands,
                entity,
                &request,
                ToolError::NotFound(format!("agent {}", request.request.agent_id)),
            );
            continue;
        };

        // 检查 required_tag
        if let Some(required_tag) = &tool_def.required_tag
            && !agent.capabilities.tags.iter().any(|t| t == required_tag)
        {
            warn!(
                event = "ToolTagDenied",
                tool_name = %tool_name,
                agent_id = %agent.id,
                agent_name = %agent.profile.name,
                required_tag = %required_tag,
                "agent lacks required tag for tool"
            );
            spawn_tool_error(
                &mut commands,
                entity,
                &request,
                ToolError::PermissionDenied(format!(
                    "tool '{}' requires tag '{}'",
                    tool_name, required_tag
                )),
            );
            continue;
        }

        let permission = agent.tool_permissions.get_permission(&tool_name);

        debug!(
            event = "ToolDispatch",
            tool_name = %tool_name,
            agent_id = %agent.id,
            agent_name = %agent.profile.name,
            permission = ?permission,
            tool_input = ?request.tool_input,
            task_id = %request.request.task_id,
            "tool execution decision"
        );

        match permission {
            ToolPermission::Allow => {
                // 直接执行
                let Some(executor) = executors.get(&tool_name) else {
                    warn!(
                        event = "ToolExecutorNotFound",
                        tool_name = %tool_name,
                        "no executor registered for tool"
                    );
                    spawn_tool_error(
                        &mut commands,
                        entity,
                        &request,
                        ToolError::NotFound(format!(
                            "no executor for '{}' — this tool is not available, do not retry",
                            tool_name
                        )),
                    );
                    continue;
                };

                // 推送 ToolCallStarted 事件到所有前端（仅当 task 有 output_channel 时；
                // 无 output_channel 时不推送，避免向无关 IM 通道广播）
                let tool_input_summary =
                    crate::domain::summarize_tool_input(&tool_name, &request.tool_input);
                if let Some(target) = index
                    .get_task(&request.request.task_id)
                    .and_then(|e| tasks.get(e).ok())
                    .and_then(|(_, t)| t.routing_policy.output_channel.clone())
                    .map(|channel| EventTarget::Directed(vec![channel]))
                {
                    let event = EngineEvent::ToolCallStarted {
                        target,
                        task_id: request.request.task_id,
                        agent_name: agent.profile.name.clone(),
                        tool_name: tool_name.clone(),
                        tool_input_summary,
                    };
                    for frontend in &frontend_registry.frontends {
                        frontend.push_event(event.clone());
                    }
                } else {
                    debug!(
                        event = "ToolCallStartedDroppedNoChannel",
                        task_id = %request.request.task_id,
                        tool_name = %tool_name,
                        "dropping ToolCallStarted because task has no output channel"
                    );
                }

                debug!(
                    event = "ToolExecutionAllowed",
                    tool_name = %tool_name,
                    agent_id = %agent.id,
                    "tool execution allowed"
                );

                info!(
                    event = "ToolExecutionStarted",
                    tool_name = %tool_name,
                    agent_id = %agent.id,
                    agent_name = %agent.profile.name,
                    task_id = %request.request.task_id,
                    "工具执行开始：Agent [{}] 调用 {}",
                    agent.profile.name,
                    tool_name,
                );

                let ctx = ToolContext {
                    knowledge: &knowledge,
                    experience_store: &experience_store,
                    default_wait_tasks_timeout_secs: settings.0.default_wait_tasks_timeout_secs,
                    shell_default_tail_lines: settings.0.shell_default_tail_lines,
                    shell_max_tail_lines: settings.0.shell_max_tail_lines,
                    shell_default_exec_timeout_secs: settings.0.shell_default_exec_timeout_secs,
                    shell_default_stop_timeout_secs: settings.0.shell_default_stop_timeout_secs,
                    tool_inflight_timeout_secs: settings.0.tool_inflight_timeout_secs,
                    current_task_id: request.request.task_id,
                    current_agent_id: request.request.agent_id,
                    // 经 EntityIndex O(1) 解析 TaskId → Entity（替代全量线性扫描）
                    current_origin_channel: index
                        .get_task(&request.request.task_id)
                        .and_then(|e| tasks.get(e).ok())
                        .map(|(_, t)| t.origin_channel.clone())
                        .unwrap_or(None),
                };
                let action = executor.execute(&request.tool_input, &ctx);

                // Find the task entity
                // 经 EntityIndex O(1) 解析 TaskId → Entity（替代全量线性扫描）
                if let Some((task_entity, _)) = index
                    .get_task(&request.request.task_id)
                    .and_then(|e| tasks.get(e).ok())
                {
                    let parent_agent_id = agent.parent_id;
                    handle_tool_action(
                        &mut commands,
                        entity,
                        task_entity,
                        &request,
                        action,
                        &mut tasks,
                        &agents,
                        &chat_sessions,
                        &mut short_term_memories,
                        &*backend,
                        &mut experience_store,
                        &mut pending_experience_hooks,
                        parent_agent_id,
                        &index_clock_loader.1,
                        &context_queries,
                        &index_clock_loader.2,
                        &calling_states,
                        frontend_registry,
                    );
                }

                restore_task_after_tool(
                    &mut tasks,
                    &calling_states,
                    index,
                    request.request.task_id,
                );
            }
            ToolPermission::Confirm => {
                // 顺序审批：同一任务同一时间仅允许一个待确认请求
                // UUID+条件复合查询拆为 UUID 解析 + 调用方断言两步
                let already_pending = index
                    .get_task(&request.request.task_id)
                    .and_then(|e| tasks.get(e).ok())
                    .map(|(_, t)| t.pending_confirmation_id.is_some())
                    .unwrap_or(false);
                if already_pending {
                    debug!(
                        event = "ToolConfirmationQueued",
                        queued_task_id = %request.request.task_id,
                        tool_name = %tool_name,
                        "sequential tool confirmation: sibling already pending, queuing next request"
                    );
                    continue;
                }

                // Find the task to check parent_task_id
                // 经 EntityIndex O(1) 解析 TaskId → Entity（替代全量线性扫描）
                let task_for_approval = index
                    .get_task(&request.request.task_id)
                    .and_then(|e| tasks.get(e).ok())
                    .map(|(_, t)| t.clone());

                // 统一按 task.parent_task_id 查找父 Agent
                // 经 EntityIndex O(1) 解析 parent TaskId 与 parent AgentId（替代全量线性扫描）
                let parent_approval = task_for_approval
                    .as_ref()
                    .and_then(|task| task.parent_task_id)
                    .and_then(|parent_task_id| {
                        index
                            .get_task(&parent_task_id)
                            .and_then(|e| tasks.get(e).ok())
                            .and_then(|(_, parent_task)| parent_task.delegate)
                            .and_then(|parent_agent_id| {
                                index
                                    .get_agent(&parent_agent_id)
                                    .and_then(|e| agents.get(e).ok())
                            })
                            .filter(|parent| parent.has_permission(&tool_name))
                            .map(|parent| parent.id)
                    });

                if let Some(parent_agent_id) = parent_approval {
                    debug!(
                        event = "ToolRequiresParentApproval",
                        tool_name = %tool_name,
                        agent_id = %agent.id,
                        parent_agent_id = %parent_agent_id,
                        reason = "parent task delegate has permission",
                        "tool requires parent agent approval"
                    );

                    if let Some((_, mut task)) = tasks
                        .iter_mut()
                        .find(|(_, t)| t.id == request.request.task_id)
                    {
                        task.status = TaskStatus::Waiting(WaitingReason::Approval);
                    }

                    let request_id = Uuid::new_v4();
                    commands.spawn((
                        ApprovalRequestMessage {
                            request_id,
                            tool_name: tool_name.clone(),
                            source_task_id: request.request.task_id,
                            parent_agent_id,
                            child_agent_id: agent.id,
                            tool_input: request.tool_input.clone(),
                            approval_task_id: Uuid::new_v4(),
                            context: String::new(),
                        },
                        ApprovalRequestedHookPending,
                    ));

                    if let Some((_, mut task)) = tasks
                        .iter_mut()
                        .find(|(_, t)| t.id == request.request.task_id)
                    {
                        task.pending_confirmation_id = Some(request_id);
                    }

                    request.pending_confirmation_id = Some(request_id);
                    continue;
                }

                // fallback 用户确认
                debug!(
                    event = "ToolRequiresUserConfirmation",
                    tool_name = %tool_name,
                    agent_id = %agent.id,
                    reason = "no parent task delegate or parent lacks permission",
                    "tool requires user confirmation"
                );

                if let Some((_, mut task)) = tasks
                    .iter_mut()
                    .find(|(_, t)| t.id == request.request.task_id)
                {
                    task.status = TaskStatus::Waiting(WaitingReason::User);
                }

                let request_id = Uuid::new_v4();
                let options = ConfirmationOption::default_options();
                commands.spawn(ToolConfirmationRequestMessage {
                    request_id,
                    task_id: request.request.task_id,
                    agent_id: agent.id,
                    tool_name: tool_name.clone(),
                    tool_input: request.tool_input.clone(),
                    options: options.clone(),
                    source: ConfirmationSource::User,
                    parent_agent_id: None,
                    approval_context: None,
                });

                if let Some((_, mut task)) = tasks
                    .iter_mut()
                    .find(|(_, t)| t.id == request.request.task_id)
                {
                    task.pending_confirmation_id = Some(request_id);
                }

                request.pending_confirmation_id = Some(request_id);
                request.pending_confirmation_options = Some(options);
            }
            ToolPermission::Deny => {
                // 拒绝执行
                warn!(
                    event = "ToolExecutionDenied",
                    tool_name = %tool_name,
                    agent_id = %agent.id,
                    "tool execution denied"
                );
                info!(
                    event = "ToolExecutionDenied",
                    tool_name = %tool_name,
                    agent_name = %agent.profile.name,
                    task_id = %request.request.task_id,
                    "工具调用被拒绝：Agent [{}] 无权使用 {}",
                    agent.profile.name,
                    tool_name,
                );
                spawn_tool_error(
                    &mut commands,
                    entity,
                    &request,
                    ToolError::PermissionDenied(tool_name.clone()),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::{Clock, FrontendRegistry, HarnessConfig, HarnessSettings},
        domain::{
            Agent, AgentCapabilities, AgentExecutionRequest, AgentKind, AgentProfile,
            AgentRequestKind, AgentToolPermissions, BuiltinToolExecutors, ChannelId,
            ExperienceStore, FrontendKind, PendingExperienceHooks, SharedKnowledgeBase,
            SpaceToolRegistry, Task, TaskStatus, ToolConfirmationRequestMessage, ToolDefinition,
            ToolExecutionRequestMessage, ToolExecutorKind, ToolPermission, ToolSchema,
            WaitingReason,
        },
        systems::NativeProcessBackend,
    };
    use bevy_ecs::prelude::*;
    use chrono::Utc;
    use std::collections::HashMap;
    use uuid::Uuid;

    #[test]
    fn sequential_confirmation_only_one_pending_at_a_time() {
        let mut world = World::new();

        // 注册必要资源
        world.init_resource::<SharedKnowledgeBase>();
        world.init_resource::<ExperienceStore>();
        world.init_resource::<PendingExperienceHooks>();
        world.insert_resource(HarnessSettings(HarnessConfig::default()));
        world.insert_resource(NativeProcessBackend::default());
        world.insert_resource(Clock::default());
        world.insert_resource(crate::ecs::EntityIndex::default());
        // tool_dispatch_system 在 Allow 路径推送 ToolCallStarted 需要 FrontendRegistry
        world.insert_resource(FrontendRegistry { frontends: vec![] });

        // 注册需要确认的测试工具
        let mut registry = SpaceToolRegistry::default();
        registry.register(ToolDefinition {
            name: "shell_exec".to_string(),
            description: "test tool".to_string(),
            parameters: ToolSchema::default(),
            default_permission: ToolPermission::Confirm,
            executor: ToolExecutorKind::Builtin("shell_exec".to_string()),
            required_tag: None,
        });
        world.insert_resource(registry);

        // 该测试不会真正执行工具，执行器注册表可为空
        world.insert_resource(BuiltinToolExecutors::default());

        // tool_dispatch_system 通过 tuple SystemParam (Res<Clock>, Res<SkillLoader>) 同时
        // 引用 Clock 与 SkillLoader，必须 init SkillLoader 才能运行。
        world.insert_resource(crate::infrastructure::skills::SkillLoader::new(
            std::path::PathBuf::from("/nonexistent_skills_root"),
        ));

        let task_id = Uuid::new_v4();
        let agent_id = Uuid::new_v4();
        let channel = ChannelId {
            frontend: FrontendKind::Tui,
            user_id: "test".to_string(),
            thread_id: None,
        };

        let task_entity = world
            .spawn(Task {
                id: task_id,
                content: "test".to_string(),
                creator: agent_id,
                delegate: Some(agent_id),
                status: TaskStatus::Waiting(WaitingReason::ToolExecution),
                pending_confirmation_id: None,
                input_summary: String::new(),
                result_summary: String::new(),
                priority: 0,
                created_at: Utc::now(),
                updated_at: Utc::now(),
                retry_count: 0,
                max_retries: 3,
                next_retry_at: None,
                last_error: None,
                multi_turn: false,
                parent_task_id: None,
                batch_id: None,
                origin_channel: Some(channel.clone()),
                routing_policy: crate::domain::TaskRoutingPolicy::conversational(channel),
                last_evaluated_turn: None,
            })
            .id();
        // 测试夹具绕过 spawn_task 封装直接 spawn，需手动写入 EntityIndex
        world
            .resource_mut::<crate::ecs::EntityIndex>()
            .tasks
            .insert(task_id, task_entity);

        let agent_entity = world
            .spawn(Agent {
                id: agent_id,
                profile: AgentProfile {
                    name: "test-agent".to_string(),
                    model: "test".to_string(),
                },
                capabilities: AgentCapabilities {
                    tags: vec![],
                    description: "test".to_string(),
                },
                kind: AgentKind::TaskScoped,
                parent_id: None,
                bound_task_id: Some(task_id),
                tool_permissions: AgentToolPermissions {
                    default_permission: ToolPermission::Confirm,
                    overrides: HashMap::new(),
                },
                system_prompt: None,
            })
            .id();
        // 测试夹具绕过 spawn_agent 封装直接 spawn，需手动写入 EntityIndex
        world
            .resource_mut::<crate::ecs::EntityIndex>()
            .agents
            .insert(agent_id, agent_entity);

        // spawn 3 个需要确认的工具请求
        for _ in 0..3 {
            world.spawn(ToolExecutionRequestMessage {
                request: AgentExecutionRequest {
                    task_id,
                    agent_id,
                    request_kind: AgentRequestKind::ToolExecution {
                        tool_name: "shell_exec".to_string(),
                    },
                    prompt: String::new(),
                    system_prompt: None,
                    tools: vec![],
                    conversation: None,
                    work_item_id: None,
                    model_override: None,
                },
                tool_name: "shell_exec".to_string(),
                tool_input: serde_json::json!({"cmd": "echo ok"}),
                pending_confirmation_id: None,
                tool_call_id: None,
                pending_confirmation_options: None,
                work_item_entity: None,
                confirmed_once: false,
            });
        }

        // 运行一次 dispatch 系统
        let mut schedule = Schedule::default();
        schedule.add_systems(tool_dispatch_system);
        schedule.run(&mut world);

        let pending_count = world
            .query::<&ToolConfirmationRequestMessage>()
            .iter(&world)
            .count();
        assert_eq!(pending_count, 1, "同一时刻应只有一个确认请求");

        let task = world.query::<&Task>().get(&world, task_entity).unwrap();
        assert!(task.pending_confirmation_id.is_some());
    }
}
