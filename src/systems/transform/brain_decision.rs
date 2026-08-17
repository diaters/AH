//! Brain 决策 System
//!
//! 处理 Brain Agent 的决策结果。
//!
//! ## 改造说明（任务 3.1）
//!
//! 本 system 已从"直接 spawn AgentExecutionRequestMessage + fallback"改造为
//! "产出 PendingDispatch(DirectDelegate)"。实际派发由 `dispatch_system` 处理。
//!
//! 移除了 fallback 到第一个非 brain Persistent Agent 的逻辑——Brain 失败
//! 现在直接 Task Failed（语义诚实，符合 AGENTS.md "避免伪精细控制面"原则）。

use crate::prelude::*;
use tracing::{debug, warn};

use crate::{
    contracts::Clock,
    domain::{
        Agent, AgentExecutionOutput, AgentExecutionResultMessage, AgentKind, AgentRequestKind,
        AwaitingBrainDecision, DispatchHint, DispatchKind, DispatchStrategy, FailureReason,
        OutputContent, PendingDispatch, Task, TaskStatus,
    },
    ecs::EntityIndex,
    infrastructure::skills::SkillRegistry,
    systems::HarnessSettings,
    systems::dispatch::parse_brain_skill_selection,
};

/// Brain 决策 System
///
/// 处理 Brain Agent 的决策结果，解析出 `{agent_name, skill_name?}`，
/// 产出 `PendingDispatch + DirectDelegate` 由 `dispatch_system` 派发。
///
/// ## 失败语义
///
/// - Brain 输出 JSON 解析失败 → Task Failed
/// - Brain 选的 Agent 不存在或非 Persistent → Task Failed
/// - Brain 选的 skill 不属于该 Agent → 降级为 None + warn 日志（防御性处理，Brain prompt 已含候选 Agent skills 清单）
/// - Brain LLM 调用返回可重试错误且未超 retry 上限 → schedule_retry
/// - Brain LLM 调用返回不可重试错误或超 retry → mark_failed
#[allow(clippy::too_many_arguments)]
pub fn brain_decision_system(
    clock: Res<Clock>,
    settings: Res<HarnessSettings>,
    mut commands: Commands,
    index: Res<EntityIndex>,
    mut tasks: Query<(Entity, &mut Task, Option<&AwaitingBrainDecision>)>,
    agents: Query<&Agent>,
    skill_registry: Res<SkillRegistry>,
    results: Query<(Entity, &AgentExecutionResultMessage)>,
) {
    let Some(brain_config) = &settings.0.brain else {
        return;
    };
    if !brain_config.enabled {
        return;
    }

    for (entity, result_message) in &results {
        if result_message.result.request_kind != AgentRequestKind::BrainDecision {
            continue;
        }

        let result = &result_message.result;

        let Some((task_entity, mut task, awaiting)) = index
            .get_task(&result.task_id)
            .and_then(|e| tasks.get_mut(e).ok())
        else {
            commands.entity(entity).despawn();
            continue;
        };

        match &result.result {
            Ok(AgentExecutionOutput {
                content: OutputContent::Text(content),
                ..
            }) => match parse_brain_skill_selection(content) {
                Ok((agent_name, skill_name)) => {
                    // 校验 agent 存在且为 Persistent
                    let agent_exists = agents
                        .iter()
                        .any(|a| a.profile.name == agent_name && a.kind == AgentKind::Persistent);

                    if !agent_exists {
                        // Brain 选了不存在的 Agent，直接 Failed（不 fallback）
                        let old_status = task.status.clone();
                        task.last_error = Some(format!(
                            "brain selected agent '{}' but no such persistent agent",
                            agent_name
                        ));
                        task.status = TaskStatus::Failed(FailureReason::AgentError);
                        task.updated_at = clock.0;
                        warn!(
                            event = "BrainDecisionAgentNotFound",
                            task_id = %task.id,
                            selected_agent = %agent_name,
                            from_status = ?old_status,
                            to_status = ?TaskStatus::Failed(FailureReason::AgentError),
                            "brain selected non-existent agent, marking task as failed"
                        );
                        commands
                            .entity(task_entity)
                            .remove::<AwaitingBrainDecision>();
                        commands.entity(entity).despawn();
                        continue;
                    }

                    // 解析 skill_id（如有）
                    // 校验 skill 归属：通过 SkillRegistry.list_by_owner 查找
                    // Brain prompt 已含候选 Agent 名下 skills 清单（build_agent_descriptions），
                    // 此处仍保留降级防御：LLM 输出偶发不准确时降级为 None + warn，而非 Failed。
                    let skill_id = if let Some(skill_name) = skill_name {
                        let owner_skills = skill_registry.list_by_owner(&agent_name);
                        let matched = owner_skills.iter().find(|entry| {
                            entry.name == skill_name || entry.skill_id.skill_name == skill_name
                        });
                        match matched {
                            Some(entry) => Some(entry.skill_id.clone()),
                            None => {
                                warn!(
                                    event = "BrainSelectedSkillNotOwned",
                                    task_id = %task.id,
                                    agent_name = %agent_name,
                                    skill_name = %skill_name,
                                    "brain selected skill not owned by agent, downgrading to None"
                                );
                                None
                            }
                        }
                    } else {
                        None
                    };

                    // 携带原 awaiting 的 spawn_spec
                    let spawn_spec = awaiting.and_then(|a| a.spawn_spec.clone());

                    let has_skill = skill_id.is_some();
                    let selected_agent_name = agent_name.clone();

                    // 移除 AwaitingBrainDecision，加 PendingDispatch + DirectDelegate
                    commands
                        .entity(task_entity)
                        .remove::<AwaitingBrainDecision>();
                    commands.entity(task_entity).insert(PendingDispatch {
                        kind: DispatchKind::Task,
                        hint: DispatchHint {
                            strategy: DispatchStrategy::DirectDelegate,
                            preferred_agent_name: Some(agent_name),
                            required_skill_id: skill_id,
                            agent_spawn_spec: spawn_spec,
                        },
                    });

                    debug!(
                        event = "BrainDecisionResolved",
                        task_id = %task.id,
                        selected_agent = %selected_agent_name,
                        has_skill = has_skill,
                        "brain decision resolved, task re-queued for direct dispatch"
                    );
                }
                Err(e) => {
                    // JSON 解析失败，直接 Failed
                    let old_status = task.status.clone();
                    task.last_error = Some(format!("brain skill selection parse failed: {:?}", e));
                    task.status = TaskStatus::Failed(FailureReason::AgentError);
                    task.updated_at = clock.0;
                    warn!(
                        event = "BrainDecisionParseFailed",
                        task_id = %task.id,
                        error = ?e,
                        raw_len = content.len(),
                        raw_preview = %content.chars().take(200).collect::<String>(),
                        from_status = ?old_status,
                        to_status = ?TaskStatus::Failed(FailureReason::AgentError),
                        "brain skill selection JSON parse failed, marking task as failed"
                    );
                    commands
                        .entity(task_entity)
                        .remove::<AwaitingBrainDecision>();
                }
            },
            Ok(AgentExecutionOutput {
                content: OutputContent::ToolCalls(_),
                ..
            }) => {
                // Tool calls 由 llm_response_system 处理，跳过
                continue;
            }
            Err(error) if error.is_retryable() && task.retry_count < task.max_retries => {
                task.schedule_retry(error, clock.0);
            }
            Err(error) => {
                task.mark_failed(error, clock.0);
                commands
                    .entity(task_entity)
                    .remove::<AwaitingBrainDecision>();
            }
        }

        commands.entity(entity).despawn();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        AgentCapabilities, AgentProfile, AgentToolPermissions, ChannelId, FrontendKind,
        ToolPermission, WaitingReason,
    };
    use crate::ecs::EntityIndex;
    use crate::systems::{BrainConfig, HarnessConfig, HarnessSettings};
    use std::collections::HashMap;
    use uuid::Uuid;

    /// 构造最小 Bevy App，注册 brain_decision_system 及其所需资源。
    fn make_brain_decision_test_app() -> App {
        let mut app = App::new();
        let config = HarnessConfig {
            brain: Some(BrainConfig { enabled: true }),
            ..HarnessConfig::default()
        };
        app.insert_resource(Clock::default());
        app.insert_resource(HarnessSettings(config));
        app.init_resource::<EntityIndex>();
        app.init_resource::<SkillRegistry>();
        app.add_systems(Update, brain_decision_system);
        app
    }

    /// 在 app world 中 spawn 一个 task（Waiting(Agent)）+ AwaitingBrainDecision，
    /// 返回 task_id。
    fn spawn_brain_awaiting_task(app: &mut App) -> Uuid {
        let task = Task::from_user_input(
            "test subtask".to_string(),
            3,
            ChannelId {
                frontend: FrontendKind::Tui,
                user_id: "test".to_string(),
                thread_id: None,
            },
        );
        let task_id = task.id;
        let entity = app
            .world_mut()
            .spawn((
                task,
                AwaitingBrainDecision {
                    task_id,
                    spawn_spec: None,
                },
            ))
            .id();
        app.world_mut()
            .resource_mut::<EntityIndex>()
            .tasks
            .insert(task_id, entity);
        task_id
    }

    /// 在 app world 中 spawn 一个 Persistent Agent，返回其 entity。
    fn spawn_persistent_agent(app: &mut App, name: &str) -> Entity {
        let agent = Agent {
            id: Uuid::new_v4(),
            profile: AgentProfile {
                name: name.to_string(),
                model: "test-model".to_string(),
            },
            capabilities: AgentCapabilities {
                tags: vec![name.to_string()],
                description: format!("{} agent", name),
            },
            kind: AgentKind::Persistent,
            parent_id: None,
            bound_task_id: None,
            tool_permissions: AgentToolPermissions {
                default_permission: ToolPermission::Confirm,
                default_permission_explicit: true,
                overrides: HashMap::new(),
            },
            system_prompt: None,
        };
        let entity = app.world_mut().spawn(agent).id();
        app.world_mut()
            .resource_mut::<EntityIndex>()
            .agents
            .insert(Uuid::new_v4(), entity);
        entity
    }

    /// 构造一个 BrainDecision 类型的 AgentExecutionResultMessage（Text 内容）。
    fn make_brain_text_result(task_id: Uuid, content: &str) -> AgentExecutionResultMessage {
        AgentExecutionResultMessage {
            result: crate::domain::AgentExecutionResult {
                task_id,
                agent_id: Uuid::nil(),
                request_kind: AgentRequestKind::BrainDecision,
                result: Ok(AgentExecutionOutput {
                    content: OutputContent::Text(content.to_string()),
                    reasoning_content: None,
                }),
                prompt: "test".to_string(),
                system_prompt: None,
                tools: vec![],
                reasoning_content: None,
                work_item_id: None,
                conversation: None,
            },
        }
    }

    /// 获取指定 task 的当前状态。
    fn get_task_status(app: &mut App, task_id: Uuid) -> Option<TaskStatus> {
        app.world_mut()
            .query::<&Task>()
            .iter(app.world())
            .find(|t| t.id == task_id)
            .map(|t| t.status.clone())
    }

    /// 获取指定 task 的 last_error。
    fn get_task_last_error(app: &mut App, task_id: Uuid) -> Option<String> {
        app.world_mut()
            .query::<&Task>()
            .iter(app.world())
            .find(|t| t.id == task_id)
            .and_then(|t| t.last_error.clone())
    }

    /// 检查指定 task 是否仍有 AwaitingBrainDecision 组件。
    fn has_awaiting_brain_decision(app: &mut App, task_id: Uuid) -> bool {
        let Some(entity) = app.world().resource::<EntityIndex>().get_task(&task_id) else {
            return false;
        };
        app.world_mut()
            .entity(entity)
            .contains::<AwaitingBrainDecision>()
    }

    #[test]
    fn parse_failure_marks_task_failed() {
        let mut app = make_brain_decision_test_app();
        let task_id = spawn_brain_awaiting_task(&mut app);

        // Spawn result with non-JSON text (simulates LLM returning only reasoning)
        app.world_mut().spawn(make_brain_text_result(
            task_id,
            "I think the best agent for this task would be...",
        ));

        app.update();

        // Task should be Failed
        let status = get_task_status(&mut app, task_id).expect("task should exist");
        assert!(
            matches!(status, TaskStatus::Failed(FailureReason::AgentError)),
            "expected Failed(AgentError), got {:?}",
            status
        );

        // AwaitingBrainDecision should be removed
        assert!(
            !has_awaiting_brain_decision(&mut app, task_id),
            "AwaitingBrainDecision should be removed on parse failure"
        );

        // last_error should be set
        let last_error = get_task_last_error(&mut app, task_id);
        assert!(
            last_error
                .as_ref()
                .is_some_and(|e| e.contains("parse failed")),
            "last_error should mention parse failure, got {:?}",
            last_error
        );
    }

    #[test]
    fn agent_not_found_marks_task_failed() {
        let mut app = make_brain_decision_test_app();
        let task_id = spawn_brain_awaiting_task(&mut app);

        // Spawn a persistent agent, but the LLM output references a different one
        spawn_persistent_agent(&mut app, "coder");

        // LLM output selects a non-existent agent
        app.world_mut().spawn(make_brain_text_result(
            task_id,
            r#"{"agent_name": "nonexistent-agent"}"#,
        ));

        app.update();

        // Task should be Failed
        let status = get_task_status(&mut app, task_id).expect("task should exist");
        assert!(
            matches!(status, TaskStatus::Failed(FailureReason::AgentError)),
            "expected Failed(AgentError), got {:?}",
            status
        );

        // AwaitingBrainDecision should be removed
        assert!(
            !has_awaiting_brain_decision(&mut app, task_id),
            "AwaitingBrainDecision should be removed on agent not found"
        );

        // last_error should mention the agent name
        let last_error = get_task_last_error(&mut app, task_id);
        assert!(
            last_error
                .as_ref()
                .is_some_and(|e| e.contains("nonexistent-agent")),
            "last_error should mention the non-existent agent, got {:?}",
            last_error
        );
    }

    #[test]
    fn valid_brain_decision_resolves_successfully() {
        let mut app = make_brain_decision_test_app();
        let task_id = spawn_brain_awaiting_task(&mut app);

        // Spawn the agent that Brain will select
        spawn_persistent_agent(&mut app, "coder");

        // LLM output selects the existing agent
        app.world_mut().spawn(make_brain_text_result(
            task_id,
            r#"{"agent_name": "coder"}"#,
        ));

        app.update();

        // Task should NOT be Failed — it should have PendingDispatch
        let status = get_task_status(&mut app, task_id).expect("task should exist");
        assert!(
            !status.is_terminal(),
            "task should not be terminal after successful brain decision, got {:?}",
            status
        );

        // AwaitingBrainDecision should be removed
        assert!(
            !has_awaiting_brain_decision(&mut app, task_id),
            "AwaitingBrainDecision should be removed on successful resolution"
        );

        // PendingDispatch should be attached
        let entity = app
            .world()
            .resource::<EntityIndex>()
            .get_task(&task_id)
            .expect("task entity should exist");
        assert!(
            app.world_mut().entity(entity).contains::<PendingDispatch>(),
            "PendingDispatch should be attached after successful brain decision"
        );
    }

    #[test]
    fn brain_disabled_skips_processing() {
        let mut app = App::new();
        let config = HarnessConfig {
            brain: None,
            ..HarnessConfig::default()
        };
        app.insert_resource(Clock::default());
        app.insert_resource(HarnessSettings(config));
        app.init_resource::<EntityIndex>();
        app.init_resource::<SkillRegistry>();
        app.add_systems(Update, brain_decision_system);

        let mut task = Task::from_user_input(
            "test".to_string(),
            3,
            ChannelId {
                frontend: FrontendKind::Tui,
                user_id: "test".to_string(),
                thread_id: None,
            },
        );
        // Simulate a task that is waiting for brain decision
        task.status = TaskStatus::Waiting(WaitingReason::Agent);
        let task_id = task.id;
        let entity = app
            .world_mut()
            .spawn((
                task,
                AwaitingBrainDecision {
                    task_id,
                    spawn_spec: None,
                },
            ))
            .id();
        app.world_mut()
            .resource_mut::<EntityIndex>()
            .tasks
            .insert(task_id, entity);

        // Spawn a result that would cause parse failure
        app.world_mut()
            .spawn(make_brain_text_result(task_id, "not json"));

        app.update();

        // Task should still be Waiting(Agent) — brain disabled, system no-ops
        let status = get_task_status(&mut app, task_id).expect("task should exist");
        assert!(
            matches!(status, TaskStatus::Waiting(_)),
            "task should remain Waiting when brain is disabled, got {:?}",
            status
        );
    }
}
