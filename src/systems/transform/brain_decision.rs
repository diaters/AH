//! Brain 决策 System
//!
//! 处理 Brain Agent 的决策结果。

use crate::prelude::*;

use crate::{
    app::{Clock, HarnessSettings},
    domain::{
        Agent, AgentExecutionOutput, AgentExecutionRequest, AgentExecutionRequestMessage,
        AgentExecutionResultMessage, AgentKind, AgentRequestKind, BrainDecisionError,
        FailureReason, MessageDispatchedHookPending, OutputContent, Task, TaskStatus,
        ToolDefinition, WaitingReason,
    },
    llm::parse_brain_decision,
};

/// 从 registry 构建 Agent 可用的工具列表（非 Deny）
fn build_tools_for_agent(
    registry: &crate::domain::SpaceToolRegistry,
    agent: &crate::domain::Agent,
) -> Vec<ToolDefinition> {
    use crate::domain::ToolPermission;
    registry
        .iter()
        .filter(|td| {
            !matches!(
                agent.tool_permissions.get_permission(&td.name),
                ToolPermission::Deny
            )
        })
        .cloned()
        .collect()
}

/// 在 Brain 生成的 delegate prompt 前追加当前通道上下文，
/// 确保被委派 Agent 知道通过哪个 IM 通道回发文件/消息。
fn augment_delegate_prompt(
    delegate_prompt: &str,
    origin_channel: &crate::domain::ChannelId,
) -> String {
    let context = origin_channel.to_prompt_context();
    format!("{context}\n\n{delegate_prompt}")
}

/// Brain 决策 System
///
/// 处理 Brain Agent 的决策结果，选择合适的 Agent 执行任务。
pub fn brain_decision_system(
    clock: Res<Clock>,
    settings: Res<HarnessSettings>,
    mut commands: Commands,
    mut tasks: Query<&mut Task>,
    agents: Query<&Agent>,
    registry: Res<crate::domain::SpaceToolRegistry>,
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

        let Some(mut task) = tasks.iter_mut().find(|t| t.id == result.task_id) else {
            commands.entity(entity).despawn();
            continue;
        };

        match &result.result {
            Ok(AgentExecutionOutput {
                content: OutputContent::Text(content),
                ..
            }) => match parse_brain_decision(content) {
                Ok(decision) => {
                    let selected_agent = agents.iter().find(|agent| {
                        agent.profile.name == decision.selected_agent_name
                            && agent.kind == AgentKind::Persistent
                    });

                    let Some(selected_agent) = selected_agent else {
                        let fallback = agents.iter().find(|agent| {
                            !agent.capabilities.tags.contains(&"brain".to_string())
                                && agent.kind == AgentKind::Persistent
                        });

                        let Some(fallback) = fallback else {
                            task.last_error = Some(format!(
                                "brain selected agent '{}' but no agent available",
                                decision.selected_agent_name
                            ));
                            task.status = TaskStatus::Failed(FailureReason::AgentError);
                            task.updated_at = clock.0;
                            commands.entity(entity).despawn();
                            continue;
                        };

                        let tools = build_tools_for_agent(&registry, fallback);
                        let prompt = augment_delegate_prompt(
                            &decision.delegate_prompt,
                            &task.origin_channel,
                        );
                        let request = AgentExecutionRequest {
                            task_id: task.id,
                            agent_id: fallback.id,
                            request_kind: AgentRequestKind::LlmCompletion,
                            prompt,
                            system_prompt: None,
                            tools,
                            conversation: None,
                            work_item_id: None,
                        };

                        task.delegate = Some(fallback.id);
                        task.status = TaskStatus::Waiting(WaitingReason::Agent);
                        task.updated_at = clock.0;
                        commands.spawn((
                            AgentExecutionRequestMessage { request },
                            MessageDispatchedHookPending,
                        ));
                        commands.entity(entity).despawn();
                        continue;
                    };

                    let tools = build_tools_for_agent(&registry, selected_agent);
                    let prompt =
                        augment_delegate_prompt(&decision.delegate_prompt, &task.origin_channel);
                    let request = AgentExecutionRequest {
                        task_id: task.id,
                        agent_id: selected_agent.id,
                        request_kind: AgentRequestKind::LlmCompletion,
                        prompt,
                        system_prompt: None,
                        tools,
                        conversation: None,
                        work_item_id: None,
                    };

                    task.delegate = Some(selected_agent.id);
                    task.status = TaskStatus::Waiting(WaitingReason::Agent);
                    task.updated_at = clock.0;
                    commands.spawn((
                        AgentExecutionRequestMessage { request },
                        MessageDispatchedHookPending,
                    ));
                }
                Err(BrainDecisionError::ParseFailed(msg)) => {
                    task.last_error = Some(format!("brain decision parse failed: {msg}"));
                    task.status = TaskStatus::Failed(FailureReason::AgentError);
                    task.updated_at = clock.0;
                }
                Err(BrainDecisionError::EmptyResponse) => {
                    task.last_error = Some("brain returned empty response".to_string());
                    task.status = TaskStatus::Failed(FailureReason::AgentError);
                    task.updated_at = clock.0;
                }
                Err(BrainDecisionError::UnknownAgent(name)) => {
                    task.last_error = Some(format!("brain selected unknown agent: {name}"));
                    task.status = TaskStatus::Failed(FailureReason::AgentError);
                    task.updated_at = clock.0;
                }
            },
            Ok(AgentExecutionOutput {
                content: OutputContent::ToolCalls(_),
                ..
            }) => {
                // Tool calls are handled by llm_response_system
                // Skip entity despawn here so llm_response_system can process it
                continue;
            }
            Err(error) if error.is_retryable() && task.retry_count < task.max_retries => {
                task.schedule_retry(error, clock.0);
            }
            Err(error) => {
                task.mark_failed(error, clock.0);
            }
        }

        commands.entity(entity).despawn();
    }
}
