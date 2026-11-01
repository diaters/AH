//! Tool 调用循环编排
//!
//! 自 `transform/llm_response.rs` 按知识域归位至此（P2 重组，纯搬家）。
//! 承载工具调用循环的编排知识：`ToolCallingState` 的查找与严格匹配协议、
//! 工具预算耗尽 / 收尾要求的合成结果生成、结果收集与 follow-up LLM 请求生成。
//!
//! 用户顶层任务受轮次预算约束（到达后请 LLM 总结并询问用户）；批次子任务没有轮次预算，
//! 程序侧只产出检查点、重复调用、连续失败等巡检触发信号，由 watchdog 判断是否干预。
//! 唯一的硬指标是子任务运行时长安全熔断：到达后先要求收尾，宽限轮次内仍不收尾才判失败。

use crate::prelude::*;
use tracing::{debug, info, trace, warn};

use crate::{
    contracts::Clock,
    domain::{
        AgentExecutionOutput, AgentExecutionRequest, AgentExecutionRequestMessage,
        AgentExecutionResult, AgentId, AgentRequestKind, ConversationMessage, FailureReason,
        FinalizeRequest, LlmToolCall, MessageDispatchedHookPending, OutputContent, Task, TaskId,
        TaskStatus, ToolCalledHookPending, ToolCallingState, ToolDefinition,
        ToolExecutionRequestMessage, ToolExecutionResultMessage, ToolReturnedHookPending,
        WaitingReason, WatchdogReviewTrigger, consecutive_error_rounds, repeated_call_streak,
    },
    ecs::EntityIndex,
};

/// 绝对硬上限倍数：iteration 超过此值 × max_iterations 时强制失败任务
pub(crate) const HARD_LIMIT_MULTIPLIER: u32 = 2;

/// 收到收尾要求后仍继续调用工具的宽限轮数；超过即判失败。
pub(crate) const FINALIZE_GRACE_ROUNDS: u32 = 2;

/// 连续相同调用 / 连续全部失败达到该轮数时请求 watchdog 立即巡检。
pub(crate) const LOOP_SIGNAL_THRESHOLD: u32 = 3;

/// 工具循环对话的上下文预算（估算 token）：超过后省略较早的工具输出正文。
/// 与派发期对话裁剪使用同一安全上限。
pub(crate) const TOOL_LOOP_CONTEXT_BUDGET_TOKENS: u32 = 100_000;

/// 预算裁剪时保留原文的最近工具结果条数。
const KEEP_RECENT_TOOL_RESULTS: usize = 6;

/// 已省略工具输出的标记前缀。
const PRUNED_PREFIX: &str = "[早期工具输出已省略";

/// `ToolCallingState` 的快照，用于在 `llm_response_system` 内避开 Bevy 借用冲突。
///
/// 字段语义与 `ToolCallingState` 一致；`work_item_id` 区分 Task 级（None）与
/// WorkItem 级（Some）调用循环，是 `(task_id, work_item_id)` 索引键的一部分。
pub(crate) struct CallingStateInfo {
    pub(crate) entity: Entity,
    pub(crate) task_id: TaskId,
    pub(crate) iteration: u32,
    pub(crate) max_iterations: Option<u32>,
    pub(crate) finalize: Option<FinalizeRequest>,
    pub(crate) conversation: Vec<ConversationMessage>,
    pub(crate) tools: Vec<ToolDefinition>,
    pub(crate) request_kind: AgentRequestKind,
    pub(crate) work_item_id: Option<uuid::Uuid>,
}

/// 工具循环策略：轮次预算与子任务监督参数。
#[derive(Debug, Clone, Copy)]
pub(crate) struct ToolLoopPolicy {
    pub(crate) max_tool_iterations: u32,
    pub(crate) subtask_checkpoint_iterations: u32,
    pub(crate) subtask_max_runtime_secs: u64,
}

impl ToolLoopPolicy {
    pub(crate) fn from_config(config: &crate::systems::HarnessConfig) -> Self {
        Self {
            max_tool_iterations: config.max_tool_iterations,
            subtask_checkpoint_iterations: config.subtask_checkpoint_iterations,
            subtask_max_runtime_secs: config.subtask_max_runtime_secs,
        }
    }
}

/// 批次子任务（`create_tasks` 派出）不受轮次预算约束，由 watchdog 监督。
pub(crate) fn is_supervised_subtask(task: &Task, work_item_id: Option<uuid::Uuid>) -> bool {
    work_item_id.is_none() && task.parent_task_id.is_some() && task.batch_id.is_some()
}

/// 在 `state_info` 中按 `(task_id, work_item_id)` 严格匹配查找 ToolCallingState。
///
/// 设计原则：WorkItem 是执行单位，Task 是组织单位。同一 Task 下不同 WorkItem
/// 的工具循环互不复用 State。这避免了"collector 残留 State 被 skill-updater
/// 误复用"类的循环 bug。
///
/// `work_item_id = None` 表示 Task 级调用，仅匹配 `work_item_id = None` 的 State；
/// `work_item_id = Some(x)` 表示 WorkItem 级调用，仅匹配 `work_item_id = Some(x)`
/// 的 State。跨 Task / 跨 WorkItem 都不会误匹配。
pub(crate) fn find_calling_state(
    state_info: &[CallingStateInfo],
    task_id: TaskId,
    work_item_id: Option<uuid::Uuid>,
) -> Option<&CallingStateInfo> {
    state_info
        .iter()
        .find(|i| i.task_id == task_id && i.work_item_id == work_item_id)
}

/// 生成合成 ToolExecutionResultMessage：不执行真实工具，直接把说明作为工具结果回填。
///
/// 合成结果跳过 ToolCalledHookPending（不 spawn），因此不会触发 on_tool_called hook。
/// 合成结果附加 ToolReturnedHookPending，会进入 on_tool_returned hook 流水线。
fn spawn_synthetic_result(
    commands: &mut Commands,
    task_id: TaskId,
    agent_id: AgentId,
    call: &LlmToolCall,
    status: &str,
    output: String,
) {
    let result = AgentExecutionResult {
        task_id,
        agent_id,
        request_kind: AgentRequestKind::ToolExecution {
            tool_name: call.name.clone(),
        },
        result: Ok(AgentExecutionOutput {
            content: OutputContent::Text(String::new()),
            reasoning_content: None,
        }),
        prompt: String::new(),
        system_prompt: None,
        tools: vec![],
        reasoning_content: None,
        work_item_id: None,
        conversation: None,
        turn_generation: None,
    };

    commands.spawn((
        ToolExecutionResultMessage {
            result,
            tool_name: call.name.clone(),
            tool_output: Ok(serde_json::json!({
                "exit_code": 1,
                "status": status,
                "output": output,
            })),
            tool_call_id: Some(call.id.clone()),
            processed: false,
            original_tool_output: None,
        },
        ToolReturnedHookPending,
    ));
}

fn budget_exhausted_text(iteration: u32, max_iterations: u32) -> String {
    format!(
        "[TOOL_BUDGET_EXHAUSTED] 本轮工具调用次数已达上限 ({iteration}/{max_iterations})。请总结你目前取得的进展，并向用户说明下一步需要什么，等待用户决策是否继续。"
    )
}

fn finalize_text(reason: &str) -> String {
    format!(
        "[FINALIZE_REQUESTED] 本工具调用未执行：子任务已被要求收尾（{reason}）。请停止调用工具，总结已完成的工作、未完成的部分及原因，并用 <<<RESULT>>>…<<</RESULT>>> 标记交付当前结果。"
    )
}

/// 子任务的巡检触发信号：检查点、重复相同调用、连续失败。
fn loop_signal(
    conversation: &[ConversationMessage],
    iteration: u32,
    checkpoint: u32,
) -> Option<String> {
    let streak = repeated_call_streak(conversation);
    if streak >= LOOP_SIGNAL_THRESHOLD {
        return Some(format!("连续 {streak} 轮发出完全相同的工具调用"));
    }
    let errors = consecutive_error_rounds(conversation);
    if errors >= LOOP_SIGNAL_THRESHOLD {
        return Some(format!("连续 {errors} 轮工具调用全部失败"));
    }
    (checkpoint > 0 && iteration.is_multiple_of(checkpoint))
        .then(|| format!("检查点：已执行 {iteration} 轮工具调用"))
}

/// 超过上下文预算时，从最早的工具结果开始省略正文（保留最近若干条与调用配对结构）。
///
/// 返回省略的条数。只改写 Tool 内容，不删除消息，保证 tool_call_id 配对完整。
pub(crate) fn prune_tool_outputs(
    conversation: &mut [ConversationMessage],
    budget_tokens: u32,
) -> usize {
    let mut total: u64 = conversation
        .iter()
        .map(|m| u64::from(m.estimated_tokens()))
        .sum();
    if total <= u64::from(budget_tokens) {
        return 0;
    }
    let tool_positions: Vec<usize> = conversation
        .iter()
        .enumerate()
        .filter(|(_, m)| matches!(m, ConversationMessage::Tool { .. }))
        .map(|(i, _)| i)
        .collect();
    let prunable = tool_positions
        .len()
        .saturating_sub(KEEP_RECENT_TOOL_RESULTS);
    let mut pruned = 0;
    for &position in &tool_positions[..prunable] {
        if total <= u64::from(budget_tokens) {
            break;
        }
        let ConversationMessage::Tool { content, .. } = &mut conversation[position] else {
            continue;
        };
        if content.starts_with(PRUNED_PREFIX) {
            continue;
        }
        let before = u64::from(crate::domain::estimate_tokens(content));
        let head: String = content.chars().take(200).collect();
        *content = format!(
            "{PRUNED_PREFIX}以节省上下文：原 {} 字符，开头：{head}…]",
            content.chars().count()
        );
        let after = u64::from(crate::domain::estimate_tokens(content));
        total = total.saturating_sub(before.saturating_sub(after));
        pruned += 1;
    }
    pruned
}

/// 以合成结果回填本轮全部调用，并把循环状态更新为"等待合成结果"。
#[allow(clippy::too_many_arguments)]
fn respond_synthetic(
    commands: &mut Commands,
    task: &mut Task,
    info: &CallingStateInfo,
    agent_id: AgentId,
    calls: &[LlmToolCall],
    conversation: Vec<ConversationMessage>,
    iteration: u32,
    finalize: Option<FinalizeRequest>,
    status: &str,
    output: &str,
    now: chrono::DateTime<chrono::Utc>,
) {
    for call in calls {
        spawn_synthetic_result(
            commands,
            task.id,
            agent_id,
            call,
            status,
            output.to_string(),
        );
    }
    commands.entity(info.entity).despawn();
    commands.spawn(ToolCallingState {
        task_id: task.id,
        agent_id,
        pending_tool_call_ids: calls.iter().map(|c| c.id.clone()).collect(),
        iteration,
        max_iterations: info.max_iterations,
        finalize,
        conversation,
        tools: info.tools.clone(),
        request_kind: info.request_kind.clone(),
        work_item_id: info.work_item_id,
    });
    // 不生成真实 ToolExecutionRequestMessage；置 Waiting(ToolExecution) 使
    // tool_calling_orchestrator_system 继续收集合成结果。
    if info.work_item_id.is_none() && !task.status.is_terminal() {
        task.mark_waiting(WaitingReason::ToolExecution, now);
    }
}

/// 处理 LLM 响应中的 ToolCalls 分支（自 `llm_response.rs` 迁入）。
///
/// 职责：更新/创建 `ToolCallingState`、执行循环约束（收尾要求 → 安全熔断 →
/// 轮次预算），子任务产出巡检触发信号，并为每个调用 spawn `ToolExecutionRequestMessage`。
#[allow(clippy::too_many_arguments)]
pub(crate) fn handle_tool_calls_response(
    commands: &mut Commands,
    task: &mut Task,
    state_info: &[CallingStateInfo],
    result: &AgentExecutionResult,
    calls: &[LlmToolCall],
    reasoning_content: &Option<String>,
    now: chrono::DateTime<chrono::Utc>,
    policy: ToolLoopPolicy,
) {
    // 按 (task_id, work_item_id) 严格匹配：避免误复用同 Task 下其他 WorkItem 残留的 State。
    let existing = find_calling_state(state_info, task.id, result.work_item_id);
    let assistant_text = result
        .result
        .as_ref()
        .ok()
        .and_then(|output| output.content.text())
        .map(str::to_string);

    if let Some(info) = existing {
        let new_iteration = info.iteration + 1;
        let mut new_conversation = info.conversation.clone();
        new_conversation.push(ConversationMessage::Assistant {
            content: assistant_text.clone(),
            tool_calls: calls.to_vec(),
            reasoning_content: reasoning_content.clone(),
        });

        // 子任务运行时长安全熔断：到达后转为收尾要求。
        let mut finalize = info.finalize.clone();
        if finalize.is_none()
            && info.max_iterations.is_none()
            && policy.subtask_max_runtime_secs > 0
            && (now - task.created_at).num_seconds()
                >= i64::try_from(policy.subtask_max_runtime_secs).unwrap_or(i64::MAX)
        {
            warn!(
                event = "SubTaskRuntimeFuseTripped",
                task_id = %task.id,
                max_runtime_secs = policy.subtask_max_runtime_secs,
                "sub-task reached runtime safety fuse; requesting finalize"
            );
            finalize = Some(FinalizeRequest::new(format!(
                "运行时长已达安全上限 {}s",
                policy.subtask_max_runtime_secs
            )));
        }

        if let Some(mut request) = finalize {
            request.rounds += 1;
            if request.rounds > FINALIZE_GRACE_ROUNDS {
                warn!(
                    event = "FinalizeIgnored",
                    task_id = %task.id,
                    rounds = request.rounds,
                    reason = %request.reason,
                    "tool loop kept calling tools after finalize request; failing task"
                );
                if info.work_item_id.is_none() {
                    task.mark_failed_reason(
                        FailureReason::AgentError,
                        format!("要求收尾后仍持续调用工具（{}）", request.reason),
                        now,
                    );
                }
                commands.entity(info.entity).despawn();
                return;
            }
            debug!(
                event = "FinalizeRequestedSyntheticResult",
                task_id = %task.id,
                rounds = request.rounds,
                "finalize requested, returning synthetic results"
            );
            let output = finalize_text(&request.reason);
            respond_synthetic(
                commands,
                task,
                info,
                result.agent_id,
                calls,
                new_conversation,
                new_iteration,
                Some(request),
                "finalize_requested",
                &output,
                now,
            );
            return;
        }

        if let Some(max_iterations) = info.max_iterations
            && new_iteration > max_iterations
        {
            // 绝对硬上限：任何情况下都强制失败，防止无限循环
            if new_iteration > HARD_LIMIT_MULTIPLIER * max_iterations {
                warn!(
                    event = "ToolCallingHardLimitExceeded",
                    task_id = %task.id,
                    iteration = new_iteration,
                    max_iterations,
                    "tool calling exceeded absolute hard limit"
                );
                if info.work_item_id.is_none() {
                    task.mark_failed_reason(
                        FailureReason::AgentError,
                        format!(
                            "tool calling exceeded absolute hard limit ({new_iteration}/{max_iterations})"
                        ),
                        now,
                    );
                }
                commands.entity(info.entity).despawn();
                return;
            }

            if info.work_item_id.is_some() {
                // WorkItem 保持硬失败语义，但不修改原任务状态
                warn!(
                    event = "ToolCallingLimitExceeded",
                    task_id = %task.id,
                    work_item_id = ?info.work_item_id,
                    iteration = new_iteration,
                    max_iterations,
                    "work item tool calling exceeded max iterations"
                );
                commands.entity(info.entity).despawn();
                return;
            }

            // 用户顶层任务：生成合成 tool result，请 LLM 总结并询问用户
            debug!(
                event = "ToolBudgetExhausted",
                task_id = %task.id,
                iteration = new_iteration,
                max_iterations,
                "tool budget exhausted, returning synthetic result"
            );
            let output = budget_exhausted_text(info.iteration, max_iterations);
            respond_synthetic(
                commands,
                task,
                info,
                result.agent_id,
                calls,
                new_conversation,
                new_iteration,
                None,
                "tool_budget_exhausted",
                &output,
                now,
            );
            return;
        }

        if info.max_iterations.is_none()
            && let Some(reason) = loop_signal(
                &new_conversation,
                new_iteration,
                policy.subtask_checkpoint_iterations,
            )
        {
            info!(
                event = "WatchdogReviewTriggered",
                task_id = %task.id,
                iteration = new_iteration,
                reason = %reason,
                "sub-task loop signal requests watchdog review"
            );
            commands.spawn(WatchdogReviewTrigger {
                task_id: task.id,
                reason,
            });
        }

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
            finalize: None,
            conversation: new_conversation,
            tools: info.tools.clone(),
            request_kind: info.request_kind.clone(),
            work_item_id: info.work_item_id,
        });
    } else {
        // First iteration: create new ToolCallingState
        // 优先使用 request.conversation（结构化路径），仅在为空时构造纯文本路径
        let conversation = if result.conversation.as_ref().is_some_and(|c| !c.is_empty()) {
            // 结构化路径：使用已有的 conversation（从 STM 还原），追加本轮 Assistant
            let mut conv = result.conversation.clone().unwrap();
            conv.push(ConversationMessage::Assistant {
                content: assistant_text.clone(),
                tool_calls: calls.to_vec(),
                reasoning_content: reasoning_content.clone(),
            });
            conv
        } else {
            // 纯文本路径：现有逻辑
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
                content: assistant_text.clone(),
                tool_calls: calls.to_vec(),
                reasoning_content: reasoning_content.clone(),
            });
            conversation
        };

        let pending_ids: Vec<String> = calls.iter().map(|c| c.id.clone()).collect();
        let max_iterations = (!is_supervised_subtask(task, result.work_item_id))
            .then_some(policy.max_tool_iterations);

        debug!(
            event = "ToolCallingStateCreated",
            task_id = %task.id,
            agent_id = %result.agent_id,
            iteration = 1,
            pending_count = pending_ids.len(),
            tools = ?calls.iter().map(|c| &c.name).collect::<Vec<_>>(),
            max_iterations = ?max_iterations,
            "created tool calling state"
        );

        commands.spawn(ToolCallingState {
            task_id: task.id,
            agent_id: result.agent_id,
            pending_tool_call_ids: pending_ids,
            iteration: 1,
            max_iterations,
            finalize: None,
            conversation,
            tools: result.tools.clone(),
            request_kind: result.request_kind.clone(),
            work_item_id: result.work_item_id,
        });
    }

    // Spawn ToolExecutionRequestMessage for each call
    for call in calls {
        let tool_input: serde_json::Value =
            serde_json::from_str(&call.arguments).unwrap_or(serde_json::Value::Null);
        commands.spawn((
            ToolCalledHookPending,
            ToolExecutionRequestMessage {
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
                    work_item_id: result.work_item_id,
                    model_override: None,
                    turn_generation: None,
                    vision: false,
                    trusted_images: Vec::new(),
                },
                tool_name: call.name.clone(),
                tool_input,
                pending_confirmation_id: None,
                tool_call_id: Some(call.id.clone()),
                pending_confirmation_options: None,
                confirmed_once: false,
            },
        ));
    }

    // Set task to Waiting(ToolExecution) — but not for ExperienceCollection WorkItems
    // since their original task is already in terminal state
    if result.work_item_id.is_none() {
        task.mark_waiting(WaitingReason::ToolExecution, now);
        debug!(
            event = "ToolCallsReceived",
            task_id = %task.id,
            tool_count = calls.len(),
            "task waiting for tool execution"
        );
    }
}

/// Tool 调用循环协调器
///
/// 收集 Tool 执行结果，构建对话历史，生成后续 LLM 请求。
pub fn tool_calling_orchestrator_system(
    clock: Res<Clock>,
    mut commands: Commands,
    index: Res<EntityIndex>,
    mut calling_states: Query<(Entity, &mut ToolCallingState)>,
    tool_results: Query<(Entity, &ToolExecutionResultMessage)>,
    mut tasks: Query<&mut Task>,
) {
    for (state_entity, mut state) in &mut calling_states {
        if state.pending_tool_call_ids.is_empty() {
            continue;
        }

        // 仅在任务处于”由 tool calling loop 驱动的等待态”时继续，否则跳过
        // ExperienceCollection WorkItem 的原任务可能已是终态，但仍需继续 tool calling
        let is_work_item = state.work_item_id.is_some();
        let task_is_waiting = index
            .get_task(&state.task_id)
            .and_then(|e| tasks.get(e).ok())
            .map(|t| {
                matches!(
                    t.status,
                    TaskStatus::Waiting(
                        WaitingReason::ToolExecution
                            | WaitingReason::Session { .. }
                            | WaitingReason::SubTaskBatch { .. }
                    )
                )
            })
            .unwrap_or(false);
        if !task_is_waiting && !is_work_item {
            continue;
        }
        // For WorkItem tool calls, skip if there are no tool results yet even if task isn't waiting
        if is_work_item && !task_is_waiting {
            // Check if there are any collected results for this state
            let has_results = tool_results.iter().any(|r| {
                r.1.tool_call_id
                    .as_ref()
                    .is_some_and(|id| state.pending_tool_call_ids.contains(id))
            });
            if !has_results {
                continue;
            }
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

        // Check iteration limit（批次子任务无轮次预算，由 watchdog 监督）
        if let Some(max_iterations) = state.max_iterations
            && state.iteration >= max_iterations
        {
            // 绝对硬上限
            if state.iteration > HARD_LIMIT_MULTIPLIER * max_iterations {
                warn!(
                    event = "ToolCallingHardLimitExceeded",
                    task_id = %state.task_id,
                    iteration = state.iteration,
                    max_iterations,
                    "tool calling exceeded absolute hard limit on result collection"
                );
                // WorkItem 不修改原任务状态
                if state.work_item_id.is_none()
                    && let Some(mut task) = index
                        .get_task(&state.task_id)
                        .and_then(|e| tasks.get_mut(e).ok())
                {
                    task.mark_failed_reason(
                        crate::domain::FailureReason::AgentError,
                        format!(
                            "tool calling exceeded absolute hard limit ({}/{})",
                            state.iteration, max_iterations
                        ),
                        clock.0,
                    );
                }
                commands.entity(state_entity).despawn();
                continue;
            }

            if state.work_item_id.is_some() {
                // WorkItem 保持现有隔离语义：不修改原任务状态，停止 follow-up
                warn!(
                    event = "ToolCallingLimitExceeded",
                    task_id = %state.task_id,
                    work_item_id = ?state.work_item_id,
                    iteration = state.iteration,
                    max_iterations,
                    "work item tool calling reached max iterations"
                );
                commands.entity(state_entity).despawn();
                continue;
            } else {
                // 普通任务：允许 LLM 再响应一次，用于总结和询问用户
                debug!(
                    event = "ToolBudgetExhaustedAllowingSummary",
                    task_id = %state.task_id,
                    iteration = state.iteration,
                    max_iterations,
                    "tool budget exhausted, allowing LLM to summarize"
                );
            }
        }

        // 长时间运行的工具循环：超过上下文预算时省略较早的工具输出正文。
        let pruned = prune_tool_outputs(&mut state.conversation, TOOL_LOOP_CONTEXT_BUDGET_TOKENS);
        if pruned > 0 {
            info!(
                event = "ToolLoopContextPruned",
                task_id = %state.task_id,
                iteration = state.iteration,
                pruned_results = pruned,
                budget_tokens = TOOL_LOOP_CONTEXT_BUDGET_TOKENS,
                "pruned early tool outputs to stay within context budget"
            );
        }

        // 在 follow-up 中保留当前通道上下文，避免多轮 tool calling 后丢失来源信息。
        let system_prompt = index
            .get_task(&state.task_id)
            .and_then(|e| tasks.get(e).ok())
            .and_then(|task| task.origin_channel.as_ref())
            .map(|ch| ch.to_prompt_context());

        // Spawn follow-up LLM request with conversation
        let request = AgentExecutionRequest {
            task_id: state.task_id,
            agent_id: state.agent_id,
            request_kind: state.request_kind.clone(),
            prompt: String::new(),
            system_prompt,
            tools: state.tools.clone(),
            conversation: Some(state.conversation.clone()),
            work_item_id: state.work_item_id,
            model_override: None,
            turn_generation: None,
            vision: false,
            trusted_images: Vec::new(),
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

        commands.spawn((
            AgentExecutionRequestMessage { request },
            MessageDispatchedHookPending,
        ));

        // Set task back to Waiting(Agent) — 但不修改 ExperienceCollection 关联的原任务状态
        if state.work_item_id.is_none()
            && let Some(mut task) = index
                .get_task(&state.task_id)
                .and_then(|e| tasks.get_mut(e).ok())
            && matches!(
                task.status,
                TaskStatus::Waiting(
                    WaitingReason::ToolExecution
                        | WaitingReason::Session { .. }
                        | WaitingReason::SubTaskBatch { .. }
                )
            )
        {
            task.mark_waiting(WaitingReason::Agent, clock.0);
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn prune_tool_outputs_keeps_recent_results_and_pairing() {
        let big = "x ".repeat(4000);
        let mut conversation = Vec::new();
        for i in 0..10 {
            conversation.push(ConversationMessage::Assistant {
                content: None,
                tool_calls: vec![LlmToolCall {
                    id: format!("c{i}"),
                    name: "shell_exec".into(),
                    arguments: "{}".into(),
                }],
                reasoning_content: None,
            });
            conversation.push(ConversationMessage::Tool {
                tool_call_id: format!("c{i}"),
                content: big.clone(),
            });
        }
        let len = conversation.len();
        assert_eq!(prune_tool_outputs(&mut conversation, 1_000_000), 0);
        let pruned = prune_tool_outputs(&mut conversation, 1_000);
        assert_eq!(
            pruned, 4,
            "only results older than the recent window are pruned"
        );
        assert_eq!(
            conversation.len(),
            len,
            "messages are rewritten, not removed"
        );
        let contents: Vec<&str> = conversation
            .iter()
            .filter_map(|m| match m {
                ConversationMessage::Tool { content, .. } => Some(content.as_str()),
                _ => None,
            })
            .collect();
        assert!(contents[..4].iter().all(|c| c.starts_with(PRUNED_PREFIX)));
        assert!(contents[4..].iter().all(|c| *c == big));
        // 已省略的内容不会被重复改写。
        assert_eq!(prune_tool_outputs(&mut conversation, 1_000), 0);
    }

    #[test]
    fn loop_signal_prefers_repeat_then_errors_then_checkpoint() {
        let round = |args: &str, result: &str| {
            vec![
                ConversationMessage::Assistant {
                    content: None,
                    tool_calls: vec![LlmToolCall {
                        id: args.into(),
                        name: "shell_exec".into(),
                        arguments: args.into(),
                    }],
                    reasoning_content: None,
                },
                ConversationMessage::Tool {
                    tool_call_id: args.into(),
                    content: result.into(),
                },
            ]
        };
        let same: Vec<_> = (0..3).flat_map(|_| round("same", "ok")).collect();
        assert!(loop_signal(&same, 3, 10).unwrap().contains("完全相同"));
        let errors: Vec<_> = (0..3)
            .flat_map(|i| round(&i.to_string(), "error: boom"))
            .collect();
        assert!(loop_signal(&errors, 3, 10).unwrap().contains("全部失败"));
        let fine: Vec<_> = (0..2).flat_map(|i| round(&i.to_string(), "ok")).collect();
        assert!(loop_signal(&fine, 10, 10).unwrap().contains("检查点"));
        assert!(loop_signal(&fine, 9, 10).is_none());
        assert!(loop_signal(&fine, 10, 0).is_none());
    }

    use super::*;

    /// 构造一个最小 CallingStateInfo 用于查找测试。
    fn make_state_info(
        task_id: crate::domain::TaskId,
        work_item_id: Option<uuid::Uuid>,
    ) -> CallingStateInfo {
        CallingStateInfo {
            entity: Entity::PLACEHOLDER,
            task_id,
            iteration: 1,
            max_iterations: Some(10),
            finalize: None,
            conversation: vec![],
            tools: vec![],
            request_kind: AgentRequestKind::LlmCompletion,
            work_item_id,
        }
    }

    /// 同 Task 下不同 WorkItem 的 ToolCallingState 不应互相复用。
    /// 这是本次 bug 修复的核心：skill-updater 不应误用 collector 残留的 State。
    #[test]
    fn find_calling_state_strict_matches_work_item_id() {
        let task_id = crate::domain::TaskId::new();
        let collector_wi = uuid::Uuid::new_v4();
        let skill_updater_wi = uuid::Uuid::new_v4();

        // 模拟 collector 残留 State（work_item_id = collector_wi）
        // 与 skill-updater 当前响应（work_item_id = skill_updater_wi）
        let state_info = vec![
            make_state_info(task_id, Some(collector_wi)),
            make_state_info(task_id, Some(skill_updater_wi)),
        ];

        // 查找 skill-updater 的 State：必须严格匹配 work_item_id
        let found = find_calling_state(&state_info, task_id, Some(skill_updater_wi));
        assert!(
            found.is_some(),
            "should find state for matching work_item_id"
        );
        assert_eq!(
            found.unwrap().work_item_id,
            Some(skill_updater_wi),
            "must not reuse state from a different work item"
        );

        // 查找 collector 的 State：仍能找到
        let found_collector = find_calling_state(&state_info, task_id, Some(collector_wi));
        assert!(found_collector.is_some());
        assert_eq!(found_collector.unwrap().work_item_id, Some(collector_wi));

        // 查找不存在的 work_item_id：返回 None
        let other_wi = uuid::Uuid::new_v4();
        let found_other = find_calling_state(&state_info, task_id, Some(other_wi));
        assert!(
            found_other.is_none(),
            "should not return state for non-matching work_item_id"
        );
    }

    /// Task 级调用（work_item_id = None）应能找到 Task 级 State（work_item_id = None），
    /// 但不应误匹配 WorkItem 级 State。
    #[test]
    fn find_calling_state_task_level_excludes_work_item_states() {
        let task_id = crate::domain::TaskId::new();
        let work_item_id = uuid::Uuid::new_v4();

        // 混合：一个 WorkItem 级 State + 一个 Task 级 State
        let state_info = vec![
            make_state_info(task_id, Some(work_item_id)),
            make_state_info(task_id, None),
        ];

        // Task 级查找：返回 Task 级 State，不返回 WorkItem 级
        let found = find_calling_state(&state_info, task_id, None);
        assert!(found.is_some(), "should find task-level state");
        assert_eq!(
            found.unwrap().work_item_id,
            None,
            "must not return work-item state when looking for task-level state"
        );
    }

    /// 只存在 WorkItem 级 State 时，Task 级查找不应误匹配。
    /// 这防止"Task 级调用复用 WorkItem 残留 State"的反向 bug。
    #[test]
    fn find_calling_state_task_level_returns_none_if_only_work_item_states_exist() {
        let task_id = crate::domain::TaskId::new();
        let work_item_id = uuid::Uuid::new_v4();

        let state_info = vec![make_state_info(task_id, Some(work_item_id))];

        let found = find_calling_state(&state_info, task_id, None);
        assert!(
            found.is_none(),
            "must not return work-item state for task-level lookup"
        );
    }
}
