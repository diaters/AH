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
    app::{Clock, HarnessSettings},
    domain::{
        Agent, AgentExecutionOutput, AgentExecutionResultMessage, AgentKind, AgentRequestKind,
        AwaitingBrainDecision, DispatchHint, DispatchKind, DispatchStrategy, FailureReason,
        OutputContent, PendingDispatch, Task, TaskStatus,
    },
    infrastructure::skills::SkillRegistry,
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
/// - Brain 选的 skill 不属于该 Agent → 降级为 None + warn 日志（设计 gap：Brain prompt 当前未包含 skills 列表）
/// - Brain LLM 调用返回可重试错误且未超 retry 上限 → schedule_retry
/// - Brain LLM 调用返回不可重试错误或超 retry → mark_failed
pub fn brain_decision_system(
    clock: Res<Clock>,
    settings: Res<HarnessSettings>,
    mut commands: Commands,
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

        let Some((task_entity, mut task, awaiting)) =
            tasks.iter_mut().find(|(_, t, _)| t.id == result.task_id)
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
                        task.last_error = Some(format!(
                            "brain selected agent '{}' but no such persistent agent",
                            agent_name
                        ));
                        task.status = TaskStatus::Failed(FailureReason::AgentError);
                        task.updated_at = clock.0;
                        commands
                            .entity(task_entity)
                            .remove::<AwaitingBrainDecision>();
                        commands.entity(entity).despawn();
                        continue;
                    }

                    // 解析 skill_id（如有）
                    // 校验 skill 归属：通过 SkillRegistry.list_by_owner 查找
                    // 设计 gap：Brain prompt 当前未包含 skills 列表，Brain 输出的 skill_name
                    // 可能不准确。校验失败时降级为 None + warn，而非 Failed。
                    // 未来 Brain prompt 改造后可收紧为 Failed。
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
                    task.last_error = Some(format!("brain skill selection parse failed: {:?}", e));
                    task.status = TaskStatus::Failed(FailureReason::AgentError);
                    task.updated_at = clock.0;
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
