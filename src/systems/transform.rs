use bevy::prelude::*;
use tracing::{debug, trace, warn};

use crate::{
    app::{Clock, ExecutionResultReceiver, HarnessSettings, MemoryConfig},
    domain::{
        Agent, AgentExecutionOutput, AgentExecutionRequest, AgentExecutionRequestMessage,
        AgentExecutionResultMessage, AgentRequestKind, BrainDecisionError, ConversationMessage,
        CreateTaskMessage, EntryMetadata, EntryRole, FailureReason, FinishTaskMessage,
        OutputContent, RetryReadyMessage, ShortTermMemory, Signal, SignalPayload,
        SubTaskBatchCreatedMessage, SubTaskCompletedMessage, SubTaskConfig,
        SummarizationRequestMessage, SummarizationTrigger, Task, TaskStatus, TaskTerminatedMessage,
        ToolCallingState, ToolDefinition, ToolExecutionRequestMessage, ToolExecutionResultMessage,
        UserInputMessage, UserOutputMessage, WaitingReason,
    },
    llm::parse_brain_decision,
};

type TaskTerminationQuery<'a> = (
    &'a Task,
    Option<&'a ShortTermMemory>,
    Option<&'a SubTaskConfig>,
);

pub(crate) fn signal_ingest_system(mut commands: Commands, signals: Query<(Entity, &Signal)>) {
    for (entity, signal) in &signals {
        match &signal.payload {
            SignalPayload::UserInput(content) => {
                debug!(
                    event = "SignalIngested",
                    signal_type = ?signal.kind,
                    payload_type = "UserInput",
                    content = %content,
                    content_len = content.len(),
                    "signal converted to UserInputMessage"
                );
                commands.spawn(UserInputMessage {
                    content: content.clone(),
                });
            }
            SignalPayload::RetryWakeup(task_id) => {
                debug!(
                    event = "SignalIngested",
                    signal_type = ?signal.kind,
                    payload_type = "RetryWakeup",
                    task_id = %task_id,
                    "signal converted to RetryReadyMessage"
                );
                commands.spawn(RetryReadyMessage { task_id: *task_id });
            }
            SignalPayload::SystemWakeup => {
                debug!(
                    event = "SignalIngested",
                    signal_type = ?signal.kind,
                    payload_type = "SystemWakeup",
                    "system wakeup signal received"
                );
            }
        }

        commands.entity(entity).despawn();
    }
}

pub(crate) fn user_message_to_task_system(
    mut commands: Commands,
    settings: Res<HarnessSettings>,
    messages: Query<(Entity, &CreateTaskMessage)>,
) {
    for (entity, message) in &messages {
        // 创建多轮对话任务（Pending 状态）并附带 ShortTermMemory
        let mut stm = ShortTermMemory::default();
        stm.add_entry(EntryRole::User, &message.content, EntryMetadata::default());
        let stm_tokens = stm.estimated_tokens;

        let task = Task::from_user_input(message.content.clone(), settings.0.max_retries);
        debug!(
            event = "TaskCreated",
            task_id = %task.id,
            content = %message.content,
            content_len = message.content.len(),
            multi_turn = task.multi_turn,
            max_retries = task.max_retries,
            stm_initial_entries = 1,
            stm_initial_tokens = stm_tokens,
            "new task spawned from user message"
        );

        commands.spawn((task, stm));
        commands.entity(entity).despawn();
    }
}

pub(crate) fn ingest_execution_results_system(
    mut commands: Commands,
    mut receiver: ResMut<ExecutionResultReceiver>,
) {
    while let Ok(result) = receiver.0.try_recv() {
        commands.spawn(AgentExecutionResultMessage { result });
    }
}

pub(crate) fn brain_decision_system(
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
                            && agent.kind == crate::domain::AgentKind::Persistent
                    });

                    let Some(selected_agent) = selected_agent else {
                        let fallback = agents.iter().find(|agent| {
                            !agent.capabilities.tags.contains(&"brain".to_string())
                                && agent.kind == crate::domain::AgentKind::Persistent
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
                        let request = AgentExecutionRequest {
                            task_id: task.id,
                            agent_id: fallback.id,
                            request_kind: AgentRequestKind::LlmCompletion,
                            prompt: decision.delegate_prompt,
                            system_prompt: None,
                            tools,
                            conversation: None,
                        };

                        task.delegate = Some(fallback.id);
                        task.status = TaskStatus::Waiting(crate::domain::WaitingReason::Agent);
                        task.updated_at = clock.0;
                        commands.spawn(AgentExecutionRequestMessage { request });
                        commands.entity(entity).despawn();
                        continue;
                    };

                    let tools = build_tools_for_agent(&registry, selected_agent);
                    let request = AgentExecutionRequest {
                        task_id: task.id,
                        agent_id: selected_agent.id,
                        request_kind: AgentRequestKind::LlmCompletion,
                        prompt: decision.delegate_prompt,
                        system_prompt: None,
                        tools,
                        conversation: None,
                    };

                    task.delegate = Some(selected_agent.id);
                    task.status = TaskStatus::Waiting(crate::domain::WaitingReason::Agent);
                    task.updated_at = clock.0;
                    commands.spawn(AgentExecutionRequestMessage { request });
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
                task.last_error =
                    Some("brain decision returned tool calls, not supported yet".to_string());
                task.status = TaskStatus::Failed(FailureReason::AgentError);
                task.updated_at = clock.0;
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

pub(crate) fn llm_response_system(
    clock: Res<Clock>,
    settings: Res<HarnessSettings>,
    mut commands: Commands,
    mut tasks: Query<(&mut Task, Option<&mut ShortTermMemory>)>,
    results: Query<(Entity, &AgentExecutionResultMessage)>,
    calling_states: Query<(Entity, &ToolCallingState)>,
) {
    // Pre-collect ToolCallingState info to avoid mutable borrow conflicts
    struct CallingStateInfo {
        entity: Entity,
        task_id: crate::domain::TaskId,
        iteration: u32,
        max_iterations: u32,
        conversation: Vec<ConversationMessage>,
        tools: Vec<ToolDefinition>,
    }
    let state_info: Vec<CallingStateInfo> = calling_states
        .iter()
        .map(|(e, s)| CallingStateInfo {
            entity: e,
            task_id: s.task_id,
            iteration: s.iteration,
            max_iterations: s.max_iterations,
            conversation: s.conversation.clone(),
            tools: s.tools.clone(),
        })
        .collect();

    for (entity, result_message) in &results {
        if result_message.result.request_kind != AgentRequestKind::LlmCompletion {
            continue;
        }

        let result = &result_message.result;

        for (mut task, short_term) in &mut tasks {
            if task.id != result.task_id {
                continue;
            }

            debug!(
                event = "LlmResponseReceived",
                task_id = %task.id,
                agent_id = %result.agent_id,
                request_kind = ?result.request_kind,
                success = result.result.is_ok(),
                response_content = ?result.result.as_ref().ok(),
                multi_turn = task.multi_turn,
                "llm response received"
            );

            match &result.result {
                Ok(AgentExecutionOutput {
                    content: OutputContent::Text(content),
                    ..
                }) => {
                    // Despawn any ToolCallingState for this task (loop completed with text)
                    if let Some(info) = state_info.iter().find(|i| i.task_id == task.id) {
                        debug!(
                            event = "ToolCallingStateCleaned",
                            task_id = %task.id,
                            "tool calling completed with text response, cleaning up state"
                        );
                        commands.entity(info.entity).despawn();
                    }

                    let stm_len = short_term.as_ref().map(|s| s.entries.len()).unwrap_or(0);
                    let stm_tokens_before =
                        short_term.as_ref().map(|s| s.estimated_tokens).unwrap_or(0);

                    if let Some(mut stm) = short_term {
                        stm.add_entry(EntryRole::Assistant, content, EntryMetadata::default());
                    }
                    let stm_tokens_after =
                        stm_tokens_before + crate::domain::estimate_tokens(content);

                    if task.multi_turn {
                        let old_status = task.status.clone();
                        task.status = TaskStatus::Waiting(WaitingReason::User);
                        task.input_summary = content.clone();
                        task.updated_at = clock.0;
                        debug!(
                            event = "TaskStatusTransition",
                            task_id = %task.id,
                            from_status = ?old_status,
                            to_status = ?task.status,
                            reason = "multi_turn_response",
                            response_len = content.len(),
                            response_content = %content,
                            stm_entries = stm_len + 1,
                            stm_tokens_before = stm_tokens_before,
                            stm_tokens_after = stm_tokens_after,
                            "multi_turn: task now waiting for user"
                        );
                        commands.spawn(UserOutputMessage {
                            content: content.clone(),
                        });
                    } else {
                        debug!(
                            event = "TaskStatusTransition",
                            task_id = %task.id,
                            from_status = ?task.status,
                            to_status = ?TaskStatus::Done,
                            reason = "single_turn_complete",
                            response_len = content.len(),
                            response_content = %content,
                            "single_turn: marking task Done"
                        );
                        task.mark_done(content.clone(), clock.0);
                        commands.spawn(UserOutputMessage {
                            content: content.clone(),
                        });
                    }
                }
                Ok(AgentExecutionOutput {
                    content: OutputContent::ToolCalls(calls),
                    reasoning_content,
                    ..
                }) => {
                    // Check for existing ToolCallingState (follow-up iteration)
                    let existing = state_info.iter().find(|i| i.task_id == task.id);

                    if let Some(info) = existing {
                        let new_iteration = info.iteration + 1;
                        if new_iteration > info.max_iterations {
                            warn!(
                                event = "ToolCallingLimitExceeded",
                                task_id = %task.id,
                                iteration = new_iteration,
                                max_iterations = info.max_iterations,
                                "tool calling exceeded max iterations"
                            );
                            task.last_error = Some(format!(
                                "tool calling exceeded max iterations ({})",
                                info.max_iterations
                            ));
                            task.status = TaskStatus::Failed(FailureReason::AgentError);
                            task.updated_at = clock.0;
                            commands.entity(info.entity).despawn();
                            break;
                        }

                        // Despawn old state and create updated one
                        let mut new_conversation = info.conversation.clone();
                        new_conversation.push(ConversationMessage::Assistant {
                            content: None,
                            tool_calls: calls.clone(),
                            reasoning_content: reasoning_content.clone(),
                        });

                        let pending_ids: Vec<String> = calls.iter().map(|c| c.id.clone()).collect();

                        debug!(
                            event = "ToolCallingStateUpdated",
                            task_id = %task.id,
                            iteration = new_iteration,
                            pending_count = calls.len(),
                            tools = ?calls.iter().map(|c| &c.name).collect::<Vec<_>>(),
                            "tool calling state updated for follow-up iteration"
                        );

                        commands.entity(info.entity).despawn();
                        commands.spawn(ToolCallingState {
                            task_id: task.id,
                            agent_id: result.agent_id,
                            pending_tool_call_ids: pending_ids,
                            iteration: new_iteration,
                            max_iterations: info.max_iterations,
                            conversation: new_conversation,
                            tools: info.tools.clone(),
                        });
                    } else {
                        // First iteration: create new ToolCallingState
                        let mut conversation = Vec::new();
                        if let Some(sp) = &result.system_prompt {
                            conversation.push(ConversationMessage::System {
                                content: sp.clone(),
                            });
                        }
                        conversation.push(ConversationMessage::User {
                            content: result.prompt.clone(),
                        });
                        conversation.push(ConversationMessage::Assistant {
                            content: None,
                            tool_calls: calls.clone(),
                            reasoning_content: reasoning_content.clone(),
                        });

                        let pending_ids: Vec<String> = calls.iter().map(|c| c.id.clone()).collect();
                        let max_iterations = settings.0.max_tool_iterations;

                        debug!(
                            event = "ToolCallingStateCreated",
                            task_id = %task.id,
                            agent_id = %result.agent_id,
                            iteration = 1,
                            pending_count = pending_ids.len(),
                            tools = ?calls.iter().map(|c| &c.name).collect::<Vec<_>>(),
                            max_iterations = max_iterations,
                            "created tool calling state"
                        );

                        commands.spawn(ToolCallingState {
                            task_id: task.id,
                            agent_id: result.agent_id,
                            pending_tool_call_ids: pending_ids,
                            iteration: 1,
                            max_iterations,
                            conversation,
                            tools: result.tools.clone(),
                        });
                    }

                    // Spawn ToolExecutionRequestMessage for each call
                    for call in calls {
                        let tool_input: serde_json::Value = serde_json::from_str(&call.arguments)
                            .unwrap_or(serde_json::Value::Null);
                        commands.spawn(ToolExecutionRequestMessage {
                            request: AgentExecutionRequest {
                                task_id: task.id,
                                agent_id: result.agent_id,
                                request_kind: AgentRequestKind::ToolExecution {
                                    tool_name: call.name.clone(),
                                },
                                prompt: String::new(),
                                system_prompt: None,
                                tools: vec![],
                                conversation: None,
                            },
                            tool_name: call.name.clone(),
                            tool_input,
                            pending_confirmation_id: None,
                            tool_call_id: Some(call.id.clone()),
                            pending_confirmation_options: None,
                        });
                    }

                    // Set task to Waiting(ToolExecution)
                    let old_status = task.status.clone();
                    task.status = TaskStatus::Waiting(WaitingReason::ToolExecution);
                    task.updated_at = clock.0;
                    debug!(
                        event = "TaskStatusTransition",
                        task_id = %task.id,
                        from_status = ?old_status,
                        to_status = ?task.status,
                        reason = "tool_calls_received",
                        tool_count = calls.len(),
                        "task waiting for tool execution"
                    );
                }
                Err(error) if error.is_retryable() && task.retry_count < task.max_retries => {
                    // Clean up ToolCallingState before retry
                    if let Some(info) = state_info.iter().find(|i| i.task_id == task.id) {
                        commands.entity(info.entity).despawn();
                    }
                    let stm_entries = short_term.as_ref().map(|s| s.entries.len()).unwrap_or(0);
                    let stm_tokens = short_term.as_ref().map(|s| s.estimated_tokens).unwrap_or(0);
                    let stm_recent: Option<Vec<_>> = short_term.as_ref().map(|s| {
                        s.entries
                            .iter()
                            .rev()
                            .take(3)
                            .map(|e| (e.role, e.content.clone()))
                            .collect()
                    });
                    debug!(
                        event = "TaskRetryScheduled",
                        task_id = %task.id,
                        retry_count = task.retry_count,
                        max_retries = task.max_retries,
                        error = %error.message(),
                        error_type = std::any::type_name_of_val(error),
                        stm_entries = stm_entries,
                        stm_tokens = stm_tokens,
                        stm_recent = ?stm_recent,
                        "scheduling retry for task"
                    );
                    task.schedule_retry(error, clock.0);
                }
                Err(error) => {
                    // Clean up ToolCallingState before marking task failed
                    if let Some(info) = state_info.iter().find(|i| i.task_id == task.id) {
                        commands.entity(info.entity).despawn();
                    }
                    let stm_entries = short_term.as_ref().map(|s| s.entries.len()).unwrap_or(0);
                    let stm_tokens = short_term.as_ref().map(|s| s.estimated_tokens).unwrap_or(0);
                    let stm_recent: Option<Vec<_>> = short_term.as_ref().map(|s| {
                        s.entries
                            .iter()
                            .rev()
                            .take(3)
                            .map(|e| (e.role, e.content.clone()))
                            .collect()
                    });
                    debug!(
                        event = "TaskFailed",
                        task_id = %task.id,
                        error = %error.message(),
                        error_type = std::any::type_name_of_val(error),
                        retry_count = task.retry_count,
                        max_retries = task.max_retries,
                        stm_entries = stm_entries,
                        stm_tokens = stm_tokens,
                        stm_recent = ?stm_recent,
                        "task failed with non-retryable error"
                    );
                    task.mark_failed(error, clock.0);
                    commands.spawn(UserOutputMessage {
                        content: format!(
                            "任务执行失败（{:?}）：{}",
                            task_status_failure_reason(&task).unwrap_or(FailureReason::Unknown),
                            error.message()
                        ),
                    });
                }
            }

            break;
        }

        commands.entity(entity).despawn();
    }
}

/// Tool 调用循环协调器
///
/// 收集 Tool 执行结果，构建对话历史，生成后续 LLM 请求。
pub(crate) fn tool_calling_orchestrator_system(
    clock: Res<Clock>,
    mut commands: Commands,
    mut calling_states: Query<(Entity, &mut ToolCallingState)>,
    tool_results: Query<(Entity, &ToolExecutionResultMessage)>,
    mut tasks: Query<&mut Task>,
) {
    for (state_entity, mut state) in &mut calling_states {
        if state.pending_tool_call_ids.is_empty() {
            continue;
        }

        // 仅在任务处于 Waiting(ToolExecution) 状态时继续，否则跳过
        // （如 tool 需要用户确认时，任务状态会变为 Waiting(User)）
        let task_is_waiting = tasks.iter().any(|t| {
            t.id == state.task_id
                && matches!(t.status, TaskStatus::Waiting(WaitingReason::ToolExecution))
        });
        if !task_is_waiting {
            continue;
        }

        // Collect matching tool results
        let mut collected: Vec<(Entity, String, String)> = Vec::new();
        let mut remaining_ids: Vec<String> = state.pending_tool_call_ids.clone();

        for (result_entity, result) in &tool_results {
            if let Some(ref call_id) = result.tool_call_id
                && remaining_ids.contains(call_id)
            {
                let content = match &result.tool_output {
                    Ok(val) => serde_json::to_string(val).unwrap_or_else(|_| val.to_string()),
                    Err(e) => format!("error: {}", e),
                };
                collected.push((result_entity, call_id.clone(), content));
                remaining_ids.retain(|id| id != call_id);
            }
        }

        // Not all results ready yet
        if !remaining_ids.is_empty() {
            trace!(
                event = "ToolCallingPending",
                task_id = %state.task_id,
                iteration = state.iteration,
                pending_count = remaining_ids.len(),
                total_count = state.pending_tool_call_ids.len(),
                "waiting for remaining tool results"
            );
            continue;
        }

        // All results collected — add to conversation
        debug!(
            event = "ToolCallingResultsCollected",
            task_id = %state.task_id,
            iteration = state.iteration,
            result_count = collected.len(),
            "all tool results collected, building follow-up request"
        );

        for (_, call_id, content) in &collected {
            state.conversation.push(ConversationMessage::Tool {
                tool_call_id: call_id.clone(),
                content: content.clone(),
            });
        }

        // Clear pending IDs (all collected)
        state.pending_tool_call_ids.clear();

        // Despawn consumed result entities
        for (entity, _, _) in &collected {
            commands.entity(*entity).despawn();
        }

        // Check iteration limit
        if state.iteration >= state.max_iterations {
            warn!(
                event = "ToolCallingLimitExceeded",
                task_id = %state.task_id,
                iteration = state.iteration,
                max_iterations = state.max_iterations,
                "tool calling reached max iterations"
            );
            if let Some(mut task) = tasks.iter_mut().find(|t| t.id == state.task_id) {
                task.last_error = Some(format!(
                    "tool calling reached max iterations ({})",
                    state.max_iterations
                ));
                task.status = TaskStatus::Failed(FailureReason::AgentError);
                task.updated_at = clock.0;
            }
            commands.entity(state_entity).despawn();
            continue;
        }

        // Spawn follow-up LLM request with conversation
        let request = AgentExecutionRequest {
            task_id: state.task_id,
            agent_id: state.agent_id,
            request_kind: AgentRequestKind::LlmCompletion,
            prompt: String::new(),
            system_prompt: None,
            tools: state.tools.clone(),
            conversation: Some(state.conversation.clone()),
        };

        debug!(
            event = "ToolCallingFollowUp",
            task_id = %state.task_id,
            agent_id = %state.agent_id,
            iteration = state.iteration,
            conversation_messages = state.conversation.len(),
            tools_count = state.tools.len(),
            "spawning follow-up LLM request with tool results"
        );

        commands.spawn(AgentExecutionRequestMessage { request });

        // Set task back to Waiting(Agent)
        if let Some(mut task) = tasks.iter_mut().find(|t| t.id == state.task_id)
            && matches!(
                task.status,
                TaskStatus::Waiting(WaitingReason::ToolExecution)
            )
        {
            let old_status = task.status.clone();
            task.status = TaskStatus::Waiting(WaitingReason::Agent);
            task.updated_at = clock.0;
            debug!(
                event = "TaskStatusTransition",
                task_id = %task.id,
                from_status = ?old_status,
                to_status = ?task.status,
                reason = "tool_results_collected",
                "task waiting for follow-up LLM response"
            );
        }
    }
}

pub(crate) fn retry_ready_system(
    clock: Res<Clock>,
    mut commands: Commands,
    messages: Query<(Entity, &RetryReadyMessage)>,
    mut tasks: Query<&mut Task>,
) {
    for (entity, message) in &messages {
        for mut task in &mut tasks {
            if task.id == message.task_id {
                debug!(
                    event = "RetryReady",
                    task_id = %task.id,
                    retry_count = task.retry_count,
                    max_retries = task.max_retries,
                    last_error = ?task.last_error,
                    "marking task ready for retry"
                );
                task.mark_ready_for_retry(clock.0);
                break;
            }
        }

        commands.entity(entity).despawn();
    }
}

pub(crate) fn task_termination_system(
    mut commands: Commands,
    config: Res<MemoryConfig>,
    tasks: Query<TaskTerminationQuery, Changed<Task>>,
    calling_states: Query<(Entity, &ToolCallingState)>,
) {
    for (task, memory, sub_task_config) in &tasks {
        if task.status.is_terminal() {
            // Clean up any ToolCallingState for this task
            for (cs_entity, cs) in &calling_states {
                if cs.task_id == task.id {
                    debug!(
                        event = "ToolCallingStateTerminated",
                        task_id = %task.id,
                        iteration = cs.iteration,
                        "cleaning up tool calling state on task termination"
                    );
                    commands.entity(cs_entity).despawn();
                }
            }

            debug!(
                event = "TaskTerminated",
                task_id = %task.id,
                task_status = ?task.status,
                task_content = %task.content,
                result_summary = %task.result_summary,
                has_stm = memory.is_some(),
                "task reached terminal state"
            );
            commands.spawn(TaskTerminatedMessage { task_id: task.id });

            // 子任务完成时产出 SubTaskCompletedMessage
            if let Some(parent_id) = task.parent_task_id {
                let child_name = sub_task_config
                    .map(|c| c.child_agent_name.clone())
                    .unwrap_or_else(|| "unknown".to_string());
                debug!(
                    event = "SubTaskTerminated",
                    task_id = %task.id,
                    parent_task_id = %parent_id,
                    batch_id = ?task.batch_id,
                    child_name = %child_name,
                    success = matches!(task.status, TaskStatus::Done),
                    result_summary = %task.result_summary,
                    "child task reached terminal state, notifying parent"
                );
                commands.spawn(SubTaskCompletedMessage {
                    parent_task_id: parent_id,
                    batch_id: task.batch_id.unwrap_or_default(),
                    child_task_id: task.id,
                    child_task_name: child_name,
                    result_summary: task.result_summary.clone(),
                    success: matches!(task.status, TaskStatus::Done),
                });
            }

            // 任务完成时触发摘要
            if let Some(stm) = memory
                && !stm.entries.is_empty()
            {
                let content: String = stm
                    .entries
                    .iter()
                    .map(|e| format!("{:?}: {}", e.role, e.content))
                    .collect::<Vec<_>>()
                    .join("\n");

                debug!(
                    event = "SummarizationTriggered",
                    task_id = %task.id,
                    trigger = "TaskComplete",
                    stm_entries = stm.entries.len(),
                    stm_tokens = stm.estimated_tokens,
                    content_len = content.len(),
                    target_tokens = config.summary_target_tokens,
                    "triggering summarization on task completion"
                );
                commands.spawn(SummarizationRequestMessage {
                    task_id: task.id,
                    content_to_summarize: content,
                    target_tokens: config.summary_target_tokens,
                    trigger: SummarizationTrigger::TaskComplete,
                });
            }
        }
    }
}

/// 从 registry 构建 Agent 可用的工具列表（非 Deny）
fn build_tools_for_agent(
    registry: &crate::domain::SpaceToolRegistry,
    agent: &crate::domain::Agent,
) -> Vec<crate::domain::ToolDefinition> {
    use crate::domain::ToolPermission;
    registry
        .tools
        .values()
        .filter(|td| {
            !matches!(
                agent.tool_permissions.get_permission(&td.name),
                ToolPermission::Deny
            )
        })
        .cloned()
        .collect()
}

fn task_status_failure_reason(task: &Task) -> Option<FailureReason> {
    match &task.status {
        TaskStatus::Failed(reason) => Some(reason.clone()),
        _ => None,
    }
}

/// 处理 /finish 命令，将任务标记为 Done。
pub(crate) fn finish_task_system(
    clock: Res<Clock>,
    mut commands: Commands,
    messages: Query<(Entity, &FinishTaskMessage)>,
    mut tasks: Query<&mut Task>,
) {
    for (entity, msg) in &messages {
        if let Some(mut task) = tasks.iter_mut().find(|t| t.id == msg.task_id) {
            debug!(
                event = "TaskFinished",
                task_id = %task.id,
                task_status = ?task.status,
                task_content = %task.content,
                "finishing task via /finish command"
            );
            task.mark_done("finished by user", clock.0);
        }
        commands.entity(entity).despawn();
    }
}

/// 处理 SubTaskBatchCreatedMessage：将父 Task 阻塞等待所有子任务完成
pub(crate) fn sub_task_batch_block_system(
    mut commands: Commands,
    clock: Res<Clock>,
    messages: Query<(Entity, &SubTaskBatchCreatedMessage)>,
    mut tasks: Query<&mut Task>,
) {
    for (entity, msg) in &messages {
        if let Some(mut parent_task) = tasks.iter_mut().find(|t| t.id == msg.parent_task_id) {
            debug!(
                event = "ParentTaskBlocked",
                parent_task_id = %msg.parent_task_id,
                batch_id = %msg.batch_id,
                task_count = msg.tasks.len(),
                "parent task blocked waiting for sub-task batch completion"
            );
            parent_task.status = TaskStatus::Waiting(WaitingReason::SubTaskBatch {
                batch_id: msg.batch_id,
            });
            parent_task.updated_at = clock.0;
        }
        commands.entity(entity).despawn();
    }
}

/// 处理 SubTaskCompletedMessage：更新 SubTaskBatchState，检查是否全部完成
pub(crate) fn sub_task_completion_system(
    mut commands: Commands,
    messages: Query<(Entity, &SubTaskCompletedMessage)>,
    mut tasks: Query<&mut Task>,
    mut batch_states: Query<(Entity, &mut crate::domain::SubTaskBatchState)>,
    calling_states: Query<&ToolCallingState>,
) {
    for (entity, msg) in &messages {
        debug!(
            event = "SubTaskCompleted",
            parent_task_id = %msg.parent_task_id,
            batch_id = %msg.batch_id,
            child_task_id = %msg.child_task_id,
            child_name = %msg.child_task_name,
            success = msg.success,
            result_summary = %msg.result_summary,
            "sub-task completed, updating batch state"
        );

        // 更新 SubTaskBatchState
        let (batch_complete, batch_entity) = if let Some((bs_entity, mut batch_state)) =
            batch_states
                .iter_mut()
                .find(|(_, bs)| bs.batch_id == msg.batch_id)
        {
            let new_state = if msg.success {
                crate::domain::BatchTaskState::Done
            } else {
                crate::domain::BatchTaskState::Failed
            };
            batch_state.update_task_state(
                &msg.child_task_name,
                new_state,
                Some(msg.result_summary.clone()),
            );
            debug!(
                event = "BatchStateUpdated",
                batch_id = %msg.batch_id,
                completed = batch_state.completed_count,
                total = batch_state.total_count,
                "batch progress updated"
            );
            (batch_state.all_done(), Some(bs_entity))
        } else {
            warn!(
                event = "SubTaskBatchStateNotFound",
                batch_id = %msg.batch_id,
                child_task_id = %msg.child_task_id,
                "SubTaskBatchState not found for completed sub-task"
            );
            commands.entity(entity).despawn();
            continue;
        };

        if batch_complete {
            debug!(
                event = "SubTaskBatchComplete",
                parent_task_id = %msg.parent_task_id,
                batch_id = %msg.batch_id,
                "all sub-tasks in batch completed, unblocking parent"
            );

            // 清理 SubTaskBatchState
            if let Some(bs_entity) = batch_entity {
                commands.entity(bs_entity).despawn();
            }

            // 恢复父 Task 状态
            if let Some(mut parent_task) = tasks.iter_mut().find(|t| t.id == msg.parent_task_id) {
                let has_calling_state =
                    calling_states.iter().any(|cs| cs.task_id == parent_task.id);
                parent_task.status = if has_calling_state {
                    TaskStatus::Waiting(WaitingReason::ToolExecution)
                } else {
                    TaskStatus::Ready
                };
                debug!(
                    event = "ParentTaskUnblocked",
                    parent_task_id = %msg.parent_task_id,
                    new_status = ?parent_task.status,
                    "parent task unblocked after batch completion"
                );
            }
        }

        commands.entity(entity).despawn();
    }
}
