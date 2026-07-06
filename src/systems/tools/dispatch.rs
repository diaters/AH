//! Tool 分发 System
//!
//! 检查 Tool 权限并决定直接执行、用户确认或父 Agent 审批。

use crate::prelude::*;
use tracing::{debug, warn};
use uuid::Uuid;

use crate::{
    app::{Clock, HarnessSettings},
    domain::{
        Agent, ApprovalRequestMessage, ApprovalRequestedHookPending, BuiltinToolExecutors,
        ChatSession, ConfirmationOption, ConfirmationSource, ExperienceStore,
        PendingExperienceHooks, SharedKnowledgeBase, ShortTermMemory, SpaceToolRegistry, Task,
        TaskStatus, ToolConfirmationRequestMessage, ToolContext, ToolError,
        ToolExecutionRequestMessage, ToolPermission, WaitingReason,
    },
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
    calling_states: Query<&crate::domain::ToolCallingState>,
    mut requests: Query<(Entity, &mut ToolExecutionRequestMessage)>,
    settings: Res<HarnessSettings>,
    backend: Res<NativeProcessBackend>,
    clock: Res<Clock>,
) {
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
        let Some(agent) = agents.iter().find(|a| a.id == request.request.agent_id) else {
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

                debug!(
                    event = "ToolExecutionAllowed",
                    tool_name = %tool_name,
                    agent_id = %agent.id,
                    "tool execution allowed"
                );

                let ctx = ToolContext {
                    knowledge: &knowledge,
                    experience_store: &experience_store,
                    default_wait_tasks_timeout_secs: settings.0.default_wait_tasks_timeout_secs,
                    shell_default_tail_lines: settings.0.shell_default_tail_lines,
                    shell_max_tail_lines: settings.0.shell_max_tail_lines,
                    shell_default_exec_timeout_secs: settings.0.shell_default_exec_timeout_secs,
                    shell_default_stop_timeout_secs: settings.0.shell_default_stop_timeout_secs,
                    current_task_id: request.request.task_id,
                    current_agent_id: request.request.agent_id,
                };
                let action = executor.execute(&request.tool_input, &ctx);

                // Find the task entity
                if let Some((task_entity, _)) =
                    tasks.iter().find(|(_, t)| t.id == request.request.task_id)
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
                        &clock,
                    );
                }

                restore_task_after_tool(&mut tasks, &calling_states, request.request.task_id);
            }
            ToolPermission::Confirm => {
                // 顺序审批：同一任务同一时间仅允许一个待确认请求
                let already_pending = tasks.iter().any(|(_, t)| {
                    t.id == request.request.task_id && t.pending_confirmation_id.is_some()
                });
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
                let task_for_approval = tasks
                    .iter()
                    .find(|(_, t)| t.id == request.request.task_id)
                    .map(|(_, t)| t.clone());

                // 统一按 task.parent_task_id 查找父 Agent
                let parent_approval = task_for_approval
                    .as_ref()
                    .and_then(|task| task.parent_task_id)
                    .and_then(|parent_task_id| {
                        tasks
                            .iter()
                            .find(|(_, t)| t.id == parent_task_id)
                            .and_then(|(_, parent_task)| parent_task.delegate)
                            .and_then(|parent_agent_id| {
                                agents.iter().find(|a| a.id == parent_agent_id)
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
        app::{Clock, HarnessConfig, HarnessSettings},
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

        world.spawn(Agent {
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
        });

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
                },
                tool_name: "shell_exec".to_string(),
                tool_input: serde_json::json!({"cmd": "echo ok"}),
                pending_confirmation_id: None,
                tool_call_id: None,
                pending_confirmation_options: None,
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
