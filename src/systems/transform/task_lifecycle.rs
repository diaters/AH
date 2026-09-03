//! 任务生命周期 System
//!
//! 处理任务终止、重试和完成。

use crate::prelude::*;
use tracing::{debug, info};

use crate::{
    contracts::{Clock, FrontendRegistry},
    domain::MemoryConfig,
    domain::SessionBackend,
    domain::{
        Agent, AwaitingBrainDecision, ClearTaskMessage, DispatchHint, DispatchKind,
        DispatchStrategy, EngineEvent, EventTarget, ExperienceCandidateStatus, ExperienceStore,
        FailureReason, FinishTaskMessage, PendingDispatch, PreviousTaskStatus, RetryReadyMessage,
        ShortTermMemory, SkillCreationContext, SubTaskConfig, Task, TaskStatus,
        TaskTerminatedMessage, ToolCallingState, ToolExecutionRequestMessage, WaitingReason,
        WorkItem,
    },
    ecs::EntityIndex,
    systems::NativeProcessBackend,
};

type TaskTerminationQuery<'a> = (
    &'a Task,
    Option<&'a ShortTermMemory>,
    Option<&'a SubTaskConfig>,
);

#[allow(dead_code)]
fn task_status_failure_reason(task: &Task) -> Option<FailureReason> {
    match &task.status {
        TaskStatus::Failed(reason) => Some(reason.clone()),
        _ => None,
    }
}

/// 重试就绪 System
///
/// 将到期重试的任务标记为 Ready，并重新附加 PendingDispatch
/// 使任务进入调度队列（重试路径不会自动进入调度）。
pub fn retry_ready_system(
    clock: Res<Clock>,
    mut commands: Commands,
    index: Res<EntityIndex>,
    agents: Query<&Agent>,
    messages: Query<(Entity, &RetryReadyMessage)>,
    mut tasks: Query<(&mut Task, Option<&AwaitingBrainDecision>)>,
) {
    for (entity, message) in &messages {
        if let Some(task_entity) = index.get_task(&message.task_id)
            && let Ok((mut task, awaiting_brain)) = tasks.get_mut(task_entity)
        {
            debug!(
                event = "RetryReady",
                task_id = %task.id,
                retry_count = task.retry_count,
                max_retries = task.max_retries,
                last_error = ?task.last_error,
                agent_delegate = ?task.delegate,
                "marking task ready for retry"
            );
            task.mark_ready_for_retry(clock.0);

            // 重新附加 PendingDispatch，使任务进入调度队列
            if let Some(agent_id) = task.delegate {
                if awaiting_brain.is_some() {
                    // 仍在等待 brain 决策（brain 可重试失败保留了 AwaitingBrainDecision）：
                    // 必须重新走 BrainLlm 重建带候选 agent 列表的 BrainDecision 请求。
                    // 若走 DirectDelegate，brain 会直接吐出路由决策 JSON，
                    // 被当作最终回复泄漏到前端（见 logs/bugs/2026-09-03-brain-decision-leak-on-retry.md）。
                    debug!(
                        event = "RetryBrainDecisionRedispatch",
                        task_id = %task.id,
                        "re-dispatching via BrainLlm on retry (still awaiting brain decision)"
                    );
                    commands.entity(task_entity).insert(PendingDispatch {
                        kind: DispatchKind::Task,
                        hint: DispatchHint {
                            strategy: DispatchStrategy::BrainLlm,
                            preferred_agent_name: None,
                            required_skill_id: None,
                            agent_spawn_spec: None,
                        },
                    });
                } else {
                    // 有 delegate：尝试 DirectDelegate 策略
                    let agent_name = agents
                        .iter()
                        .find(|a| a.id == agent_id)
                        .map(|a| a.profile.name.clone());
                    if let Some(name) = agent_name {
                        debug!(
                            event = "RetryDirectDispatch",
                            task_id = %task.id,
                            agent_name = %name,
                            "re-dispatching task via DirectDelegate on retry"
                        );
                        commands.entity(task_entity).insert(PendingDispatch {
                            kind: DispatchKind::Task,
                            hint: DispatchHint {
                                strategy: DispatchStrategy::DirectDelegate,
                                preferred_agent_name: Some(name),
                                required_skill_id: None,
                                agent_spawn_spec: None,
                            },
                        });
                    } else {
                        // delegate 指向的 agent 不存在（可能已被销毁），fallback 到 BrainLlm
                        debug!(
                            event = "RetryDelegateAgentNotFound",
                            task_id = %task.id,
                            delegate_agent_id = %agent_id,
                            "delegate agent not found, falling back to BrainLlm dispatch on retry"
                        );
                        commands.entity(task_entity).insert(PendingDispatch {
                            kind: DispatchKind::Task,
                            hint: DispatchHint {
                                strategy: DispatchStrategy::BrainLlm,
                                preferred_agent_name: None,
                                required_skill_id: None,
                                agent_spawn_spec: None,
                            },
                        });
                    }
                }
            } else {
                // 无 delegate：走 BrainLlm 重新调度
                debug!(
                    event = "RetryBrainLlm",
                    task_id = %task.id,
                    "re-dispatching task via BrainLlm on retry"
                );
                commands.entity(task_entity).insert(PendingDispatch {
                    kind: DispatchKind::Task,
                    hint: DispatchHint {
                        strategy: DispatchStrategy::BrainLlm,
                        preferred_agent_name: None,
                        required_skill_id: None,
                        agent_spawn_spec: None,
                    },
                });
            }
        }

        commands.entity(entity).despawn();
    }
}

/// 任务终止 System
///
/// 处理任务终止，清理状态并触发摘要。
///
/// 仅在 Task 状态从"非终态"转换为"终态"时 spawn `TaskTerminatedMessage`，
/// 依赖 `PreviousTaskStatus` 组件做转换检测。终态内的字段更新（如
/// `result_summary`、`updated_at` 刷新）不会重复触发。这是 `mark_done`
/// 幂等化的纵深防御层。
#[allow(clippy::too_many_arguments)]
pub fn task_termination_system(
    mut commands: Commands,
    _config: Res<MemoryConfig>,
    mut tasks: Query<(TaskTerminationQuery, &mut PreviousTaskStatus), Changed<Task>>,
    calling_states: Query<(Entity, &ToolCallingState)>,
    backend: Res<NativeProcessBackend>,
    mut experience_store: ResMut<ExperienceStore>,
    skill_creation_contexts: Query<(Entity, &SkillCreationContext, &WorkItem)>,
) {
    for ((task, memory, sub_task_config), mut prev_status) in &mut tasks {
        let prev = prev_status.0.clone();
        let curr = task.status.clone();
        let is_terminal_transition = !prev.is_terminal() && curr.is_terminal();

        // 同步 prev_status 为当前状态，无论是否触发终止处理。
        // 这保证下次 Changed<Task> 触发时，prev 反映上次观察到的状态。
        prev_status.0 = curr.clone();

        if !is_terminal_transition {
            // 非终态→终态的转换未发生：跳过终止处理。
            // 典型场景：终态内的字段更新（Done→Done、Failed→Failed），
            // 或非终态内的状态变化（Pending→Ready、Ready→Running 等）。
            continue;
        }

        // 以下逻辑仅在"非终态→终态"转换时执行
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

        // Stop all active shell sessions owned by this task
        match backend.stop_task_sessions(task.id) {
            Ok(stopped_sessions) => {
                if !stopped_sessions.is_empty() {
                    debug!(
                        event = "TaskShellSessionsStopped",
                        task_id = %task.id,
                        task_status = ?task.status,
                        stopped_sessions = ?stopped_sessions,
                        "stopped active shell sessions on task termination"
                    );
                }
            }
            Err(e) => {
                debug!(
                    event = "TaskShellSessionsStopFailed",
                    task_id = %task.id,
                    error = %e,
                    "failed to stop shell sessions on task termination"
                );
            }
        }

        debug!(
            event = "TaskTerminated",
            task_id = %task.id,
            task_status = ?task.status,
            task_content = %task.content,
            result_summary = %task.result_summary,
            has_stm = memory.is_some(),
            from_status = ?prev,
            "task reached terminal state"
        );
        info!(
            event = "TaskTerminated",
            task_id = %task.id,
            task_status = ?task.status,
            result_summary = %task.result_summary,
            "任务完成：状态={:?}，结果摘要={}",
            task.status,
            task.result_summary
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
            commands.spawn(crate::domain::SubTaskCompletedMessage {
                parent_task_id: parent_id,
                batch_id: task.batch_id.unwrap_or_default(),
                child_task_id: task.id,
                child_task_name: child_name,
                result_summary: task.result_summary.clone(),
                success: matches!(task.status, TaskStatus::Done),
            });
        }

        // TaskComplete 触发的摘要已移除：任务终态后 STM 无后续消费者，
        // 摘要写入 summary_prefix 后不会被读取，浪费 LLM tokens 并产生无用 IM 通知。
        // TokenThreshold 与 UserCommand 两种触发路径仍然保留。

        // Skill Creation 沙盒清理：任务终态后，按 candidate 状态决定是否删除沙盒
        for (wi_entity, ctx, _) in skill_creation_contexts.iter() {
            if ctx.task_id != task.id {
                continue;
            }
            let candidates = experience_store.candidates_by_producer_task(task.id);
            let candidate_status = candidates.first().map(|c| c.status.clone());
            match candidate_status {
                Some(
                    ExperienceCandidateStatus::NeedsUserApproval
                    | ExperienceCandidateStatus::GovernancePending
                    | ExperienceCandidateStatus::GovernanceResolved,
                ) => {
                    // 用户可能仍会审批，或治理/写回仍在进行中，不清理。
                    // GovernancePending 是关键：本系统在 Transform 集运行、早于
                    // Execution 集的 experience_governance_system，任务终态当帧候选
                    // 仍处于 GovernancePending，若在此 force-clean 会 despawn WorkItem
                    // 并 Discard 候选，导致治理拿不到候选、skill 永不被审核发布。
                    debug!(
                        event = "SkillSandboxPreserved",
                        task_id = %task.id,
                        sandbox_dir = %ctx.sandbox_dir.display(),
                        candidate_status = ?candidate_status,
                        "skill creation sandbox preserved (governance or approval in progress)"
                    );
                }
                Some(
                    ExperienceCandidateStatus::Persisted
                    | ExperienceCandidateStatus::Rejected
                    | ExperienceCandidateStatus::Discarded
                    | ExperienceCandidateStatus::WritebackFailed,
                ) => {
                    // 已终结：删除沙盒 + despawn WorkItem
                    debug!(
                        event = "SkillSandboxCleaned",
                        task_id = %task.id,
                        sandbox_dir = %ctx.sandbox_dir.display(),
                        candidate_status = ?candidate_status,
                        "cleaning up skill creation sandbox on task termination"
                    );
                    let _ = std::fs::remove_dir_all(&ctx.sandbox_dir);
                    commands.entity(wi_entity).despawn();
                }
                _ => {
                    // Submitted / GovernancePending / InInbox 等中间态：
                    // 删除沙盒 + despawn WorkItem + candidate 标记 Discarded
                    debug!(
                        event = "SkillSandboxForceCleaned",
                        task_id = %task.id,
                        sandbox_dir = %ctx.sandbox_dir.display(),
                        candidate_status = ?candidate_status,
                        "force cleaning skill creation sandbox and discarding candidate"
                    );
                    let _ = std::fs::remove_dir_all(&ctx.sandbox_dir);
                    // 将关联 candidate 标记为 Discarded
                    let candidate_ids: Vec<_> = experience_store
                        .candidates_by_producer_task(task.id)
                        .iter()
                        .map(|c| c.candidate_id)
                        .collect();
                    for candidate_id in candidate_ids {
                        if let Some(candidate) = experience_store.candidates.get_mut(&candidate_id)
                            && !matches!(candidate.status, ExperienceCandidateStatus::Discarded)
                        {
                            candidate.status = ExperienceCandidateStatus::Discarded;
                        }
                    }
                    commands.entity(wi_entity).despawn();
                }
            }
        }
    }
}

/// `PreviousTaskStatus` 初始化 companion System。
///
/// 在 Task entity 首次进入 ECS 时（`Added<Task>`）自动插入
/// `PreviousTaskStatus(TaskStatus::Pending)`，避免在每个 Task 创建点
/// 手动维护。`task_termination_system` 依赖该组件存在，缺失会导致
/// Query 过滤掉 Task 终止检测。
pub fn init_previous_task_status_system(mut commands: Commands, tasks: Query<Entity, Added<Task>>) {
    for entity in &tasks {
        commands
            .entity(entity)
            .insert(PreviousTaskStatus(TaskStatus::Pending));
    }
}

/// 完成任务 System
///
/// 处理 /finish 命令，将任务标记为 Done。
pub fn finish_task_system(
    clock: Res<Clock>,
    mut commands: Commands,
    index: Res<EntityIndex>,
    messages: Query<(Entity, &FinishTaskMessage)>,
    mut tasks: Query<&mut Task>,
) {
    for (entity, msg) in &messages {
        if let Some(mut task) = index
            .get_task(&msg.task_id)
            .and_then(|e| tasks.get_mut(e).ok())
        {
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

/// 清除任务 System
///
/// 处理 /clear 命令，直接 despawn task entity 及其附属组件，
/// 不触发终态处理链路（摘要、经验收集、hook 派发等）。
#[allow(clippy::too_many_arguments)]
pub fn clear_task_system(
    mut commands: Commands,
    mut index: ResMut<EntityIndex>,
    registry: Res<FrontendRegistry>,
    tasks: Query<&Task>,
    messages: Query<(Entity, &ClearTaskMessage)>,
    calling_states: Query<(Entity, &ToolCallingState)>,
    backend: Res<NativeProcessBackend>,
    mut experience_store: ResMut<ExperienceStore>,
    skill_creation_contexts: Query<(Entity, &SkillCreationContext)>,
) {
    for (entity, msg) in &messages {
        // 停止关联 shell sessions
        match backend.stop_task_sessions(msg.task_id) {
            Ok(stopped_sessions) => {
                if !stopped_sessions.is_empty() {
                    debug!(
                        event = "TaskShellSessionsStopped",
                        task_id = %msg.task_id,
                        stopped_sessions = ?stopped_sessions,
                        "stopped active shell sessions on /clear"
                    );
                }
            }
            Err(e) => {
                debug!(
                    event = "TaskShellSessionsStopFailed",
                    task_id = %msg.task_id,
                    error = %e,
                    "failed to stop shell sessions on /clear"
                );
            }
        }

        // Despawn 关联的 ToolCallingState
        for (cs_entity, cs) in &calling_states {
            if cs.task_id == msg.task_id {
                debug!(
                    event = "ToolCallingStateCleared",
                    task_id = %msg.task_id,
                    iteration = cs.iteration,
                    "despawning ToolCallingState on /clear"
                );
                commands.entity(cs_entity).despawn();
            }
        }

        debug!(
            event = "TaskCleared",
            task_id = %msg.task_id,
            "clearing task via /clear command (no termination hooks)"
        );

        // 推送前端移除通知（despawn 前读取任务路由信息）
        let target = index
            .get_task(&msg.task_id)
            .and_then(|e| tasks.get(e).ok())
            .and_then(|t| t.routing_policy.output_channel().cloned())
            .map(|channel| EventTarget::Directed(vec![channel]));
        if let Some(target) = target {
            let event = EngineEvent::TaskCleared {
                target,
                task_id: msg.task_id,
            };
            for frontend in &registry.frontends {
                frontend.push_event(event.clone());
            }
        }

        // Skill Creation 沙盒强制清理：/clear 无条件删除沙盒
        for (wi_entity, ctx) in &skill_creation_contexts {
            if ctx.task_id != msg.task_id {
                continue;
            }
            debug!(
                event = "SkillSandboxForceCleaned",
                task_id = %msg.task_id,
                sandbox_dir = %ctx.sandbox_dir.display(),
                "force cleaning skill creation sandbox on /clear"
            );
            let _ = std::fs::remove_dir_all(&ctx.sandbox_dir);
            // 将关联 candidate 标记为 Discarded
            let candidate_ids: Vec<_> = experience_store
                .candidates_by_producer_task(msg.task_id)
                .iter()
                .map(|c| c.candidate_id)
                .collect();
            for candidate_id in candidate_ids {
                if let Some(candidate) = experience_store.candidates.get_mut(&candidate_id)
                    && !matches!(candidate.status, ExperienceCandidateStatus::Discarded)
                {
                    candidate.status = ExperienceCandidateStatus::Discarded;
                }
            }
            commands.entity(wi_entity).despawn();
        }

        // 使用中心封装 despawn task（同步维护 EntityIndex）
        crate::ecs::despawn_task(&mut commands, &mut index, msg.task_id);

        commands.entity(entity).despawn();
    }
}

/// User Turn 结束时重置 ToolCallingState（安全网）
///
/// 核心重置已由 LLM 产出文本时的 ToolCallingState despawn 完成。
/// 本 system 处理边界场景：任务已进入 Waiting(User) 但 ToolCallingState
/// 仍残留（如外部信号直接修改了任务状态）。
///
/// **竞态保护**：若该 task 仍有 `ToolExecutionRequestMessage` 存在（即异步工具
/// 确认后等待 `async_tool_dispatch_system` 认领的中间态），不 despawn——
/// 否则 worker 完成后 `restore_task_after_tool` 会因找不到 `ToolCallingState`
/// 把 task 转为 `Ready` 而非 `Waiting(ToolExecution)`，LLM 调用循环无法续跑，
/// 任务永久卡死。
///
/// 时序背景：`tool_confirmation_result_system`（Dispatch 集）清除
/// `pending_confirmation_id` 后，下一帧本系统（Transform 集，先于 Dispatch 集）
/// 会看到 `Waiting(User) && pending_confirmation_id.is_none()`——若无下方保护，
/// 会错误 despawn `ToolCallingState`，然后 `async_tool_dispatch_system`（Dispatch 集）
/// 才 spawn worker。
pub fn tool_calling_turn_reset_system(
    mut commands: Commands,
    index: Res<EntityIndex>,
    tasks: Query<&Task>,
    calling_states: Query<(Entity, &ToolCallingState)>,
    tool_requests: Query<&ToolExecutionRequestMessage>,
) {
    for (state_entity, state) in &calling_states {
        if let Some(task) = index
            .get_task(&state.task_id)
            .and_then(|e| tasks.get(e).ok())
            && task.status == TaskStatus::Waiting(WaitingReason::User)
            && task.pending_confirmation_id.is_none()
        {
            // 竞态保护：若仍有属于该 task 的工具请求待认领（async 工具确认后中间态），
            // 不要 despawn ToolCallingState。
            let has_pending_tool_request = tool_requests
                .iter()
                .any(|r| r.request.task_id == state.task_id);
            if has_pending_tool_request {
                continue;
            }

            debug!(
                event = "ToolCallingStateTurnReset",
                task_id = %state.task_id,
                "despawning residual ToolCallingState on Waiting(User)"
            );
            commands.entity(state_entity).despawn();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_task_status_failure_reason() {
        let task = Task {
            status: TaskStatus::Failed(FailureReason::AgentError),
            ..Task::from_user_input(
                "test".to_string(),
                3,
                crate::domain::ChannelId {
                    frontend: crate::domain::FrontendKind::Tui,
                    user_id: "test".to_string(),
                    thread_id: None,
                },
            )
        };
        assert_eq!(
            task_status_failure_reason(&task),
            Some(FailureReason::AgentError)
        );
    }

    #[test]
    fn test_task_status_failure_reason_not_failed() {
        let task = Task {
            status: TaskStatus::Done,
            ..Task::from_user_input(
                "test".to_string(),
                3,
                crate::domain::ChannelId {
                    frontend: crate::domain::FrontendKind::Tui,
                    user_id: "test".to_string(),
                    thread_id: None,
                },
            )
        };
        assert_eq!(task_status_failure_reason(&task), None);
    }

    /// 为 task_termination_system 测试构造最小 Bevy App。
    ///
    /// 注册 `init_previous_task_status_system` 与 `task_termination_system`，
    /// 并插入所需资源（`MemoryConfig`、`NativeProcessBackend`）。
    fn make_termination_test_app() -> App {
        use crate::systems::{HarnessConfig, HarnessSettings};

        let mut app = App::new();
        app.insert_resource(MemoryConfig::default());
        app.insert_resource(HarnessSettings(HarnessConfig::default()));
        app.insert_resource(crate::systems::NativeProcessBackend::default());
        app.insert_resource(ExperienceStore::default());
        app.add_systems(
            Update,
            (
                init_previous_task_status_system,
                task_termination_system.after(init_previous_task_status_system),
            ),
        );
        app
    }

    fn count_terminated_messages(app: &mut App) -> usize {
        app.world_mut()
            .query::<&TaskTerminatedMessage>()
            .iter(app.world())
            .count()
    }

    fn spawn_pending_task(app: &mut App) -> crate::domain::TaskId {
        let task = Task::from_user_input(
            "test".to_string(),
            3,
            crate::domain::ChannelId {
                frontend: crate::domain::FrontendKind::Tui,
                user_id: "test".to_string(),
                thread_id: None,
            },
        );
        let task_id = task.id;
        app.world_mut().spawn(task);
        // 第一次 update：init_previous_task_status_system 插入 PreviousTaskStatus(Pending)。
        // task_termination_system 此时不触发（Pending 非终态）。
        app.update();
        task_id
    }

    /// 修改 task 字段，避开返回类型 `Mut<Task>` 的生命周期问题。
    /// 闭包内可对 task 进行任何 mutable 操作。
    fn with_task_mut<R>(
        app: &mut App,
        task_id: crate::domain::TaskId,
        f: impl FnOnce(&mut Task) -> R,
    ) -> R {
        let mut task_mut = app
            .world_mut()
            .query::<&mut Task>()
            .iter_mut(app.world_mut())
            .find(|t| t.id == task_id)
            .expect("task exists");
        f(&mut task_mut)
    }

    /// Pending → Done 转换应 spawn 1 个 TaskTerminatedMessage。
    #[test]
    fn pending_to_done_spawns_one_terminated_message() {
        let mut app = make_termination_test_app();
        let task_id = spawn_pending_task(&mut app);

        assert_eq!(
            count_terminated_messages(&mut app),
            0,
            "no terminated messages before any terminal transition"
        );

        // 触发 Pending → Done 转换
        let now = chrono::Utc::now();
        with_task_mut(&mut app, task_id, |t| t.mark_done("done", now));
        app.update();

        assert_eq!(
            count_terminated_messages(&mut app),
            1,
            "Pending → Done transition should spawn exactly one TaskTerminatedMessage"
        );
    }

    /// Done → Done（终态字段更新）不应 spawn 新的 TaskTerminatedMessage。
    /// 这是本次 bug 修复的核心：防止 mark_done 重复调用或终态字段更新
    /// 触发循环。
    #[test]
    fn done_to_done_does_not_spawn_terminated_message() {
        let mut app = make_termination_test_app();
        let task_id = spawn_pending_task(&mut app);

        // 第一次：Pending → Done
        let now = chrono::Utc::now();
        with_task_mut(&mut app, task_id, |t| t.mark_done("first", now));
        app.update();
        assert_eq!(count_terminated_messages(&mut app), 1);

        // 第二次：直接修改 last_error 字段（绕过 mark_done 幂等）
        // 模拟"终态 Task 被其他 system 修改非状态字段"的场景。
        with_task_mut(&mut app, task_id, |t| {
            t.last_error = Some("manual update".to_string());
        });
        app.update();

        assert_eq!(
            count_terminated_messages(&mut app),
            1,
            "Done → Done (field update) must not spawn new TaskTerminatedMessage"
        );
    }

    /// Pending → Ready → Running → Done 应只 spawn 1 个 TaskTerminatedMessage
    /// （仅最后一次非终态→终态转换触发）。
    #[test]
    fn multiple_non_terminal_transitions_then_done_spawns_one_message() {
        let mut app = make_termination_test_app();
        let task_id = spawn_pending_task(&mut app);

        // Pending → Ready
        with_task_mut(&mut app, task_id, |t| t.mark_ready(chrono::Utc::now()));
        app.update();
        assert_eq!(count_terminated_messages(&mut app), 0);

        // Ready → Running
        with_task_mut(&mut app, task_id, |t| t.mark_running(chrono::Utc::now()));
        app.update();
        assert_eq!(count_terminated_messages(&mut app), 0);

        // Running → Done
        with_task_mut(&mut app, task_id, |t| {
            t.mark_done("done", chrono::Utc::now())
        });
        app.update();
        assert_eq!(
            count_terminated_messages(&mut app),
            1,
            "only the non-terminal → terminal transition should spawn"
        );
    }

    /// 复现异步工具确认后的竞态：任务在 `Waiting(User)` + `pending_confirmation_id`
    /// 已被清除（由 `tool_confirmation_result_system` 在 Dispatch 集中清除），
    /// 但 `ToolExecutionRequestMessage` 仍存在（等待下一帧 `async_tool_dispatch_system`
    /// 认领）。此时 `tool_calling_turn_reset_system`（Transform 集，先于 Dispatch 集）
    /// 不应错误 despawn `ToolCallingState`——否则 worker 完成后 LLM 调用循环无法续跑。
    ///
    /// 时序（日志已证实）：
    ///   帧 A Dispatch: confirmation 清 pending_confirmation_id（task.status 仍 Waiting(User)）
    ///   帧 B Transform: reset 触发 → 错误 despawn ToolCallingState ← BUG
    ///   帧 B Dispatch:  async_dispatch spawn worker（ToolCallingState 已没了）
    ///   帧 C:           worker 完成 → restore_task_after_tool 发现无 ToolCallingState → task → Ready
    ///   永久卡死
    #[test]
    fn tool_calling_turn_reset_preserves_state_when_async_tool_request_pending() {
        use crate::domain::{AgentExecutionRequest, AgentRequestKind, ToolExecutionRequestMessage};

        let mut world = World::new();
        // tool_calling_turn_reset_system 经 EntityIndex O(1) 解析 TaskId → Entity，
        // 需注入资源并填充 task 映射（模拟 spawn_task 封装的索引维护）。
        world.insert_resource(crate::ecs::EntityIndex::default());

        // 构造 Task：模拟 async 工具确认后的中间态
        // - status = Waiting(User)（dispatch.rs:317 在 ToolRequiresUserConfirmation 时设置）
        // - pending_confirmation_id = None（confirmation.rs:249 在 async 分支清除）
        let mut task = Task::from_user_input(
            "test".to_string(),
            3,
            crate::domain::ChannelId {
                frontend: crate::domain::FrontendKind::Tui,
                user_id: "test".to_string(),
                thread_id: None,
            },
        );
        let task_id = task.id;
        task.mark_waiting(WaitingReason::User, chrono::Utc::now());
        task.pending_confirmation_id = None;
        let task_entity = world.spawn(task).id();
        world
            .resource_mut::<crate::ecs::EntityIndex>()
            .tasks
            .insert(task_id, task_entity);

        // 构造 ToolCallingState：工具调用循环的载体，不应被 reset 错误 despawn
        let calling_state_entity = world
            .spawn(ToolCallingState {
                task_id,
                agent_id: crate::domain::AgentId::nil(),
                pending_tool_call_ids: vec!["functions.shell_exec:0".to_string()],
                iteration: 1,
                max_iterations: 20,
                conversation: vec![],
                tools: vec![],
                request_kind: AgentRequestKind::LlmCompletion,
                work_item_id: None,
            })
            .id();

        // 构造 ToolExecutionRequestMessage：async 工具确认后的中间态
        // - pending_confirmation_id = None（已被 confirmation 清除）
        // - task_id 匹配（等待 async_tool_dispatch_system 认领）
        world.spawn(ToolExecutionRequestMessage {
            request: AgentExecutionRequest {
                task_id,
                agent_id: crate::domain::AgentId::nil(),
                request_kind: AgentRequestKind::LlmCompletion,
                prompt: String::new(),
                system_prompt: None,
                tools: vec![],
                conversation: None,
                work_item_id: None,
                model_override: None,
            },
            tool_name: "shell_exec".to_string(),
            tool_input: serde_json::Value::Null,
            pending_confirmation_id: None,
            tool_call_id: Some("functions.shell_exec:0".to_string()),
            pending_confirmation_options: None,
            work_item_entity: None,
            confirmed_once: false,
        });

        // 跑 reset 系统
        let mut schedule = Schedule::default();
        schedule.add_systems(tool_calling_turn_reset_system);
        schedule.run(&mut world);

        // 断言：ToolCallingState 应保留（async 工具请求仍 pending，不应清理）
        let state_exists = world
            .get::<ToolCallingState>(calling_state_entity)
            .is_some();
        assert!(
            state_exists,
            "ToolCallingState must be preserved when an async ToolExecutionRequestMessage \
             is still pending (Waiting(User) + pending_confirmation_id cleared by confirmation). \
             Otherwise the LLM tool-calling loop cannot resume after worker completes."
        );
    }

    #[test]
    fn clear_task_system_despawns_task_entity() {
        use crate::domain::{
            ChannelId, ClearTaskMessage, FrontendKind, PreviousTaskStatus, ShortTermMemory, Task,
            TaskStatus,
        };
        use crate::ecs::EntityIndex;

        let mut app = App::new();
        app.init_resource::<EntityIndex>();
        app.insert_resource(crate::domain::MemoryConfig::default());
        app.insert_resource(crate::contracts::FrontendRegistry { frontends: vec![] });
        app.insert_resource(crate::systems::NativeProcessBackend::default());
        app.insert_resource(ExperienceStore::default());
        app.add_systems(Update, clear_task_system);

        let channel = ChannelId {
            frontend: FrontendKind::Tui,
            user_id: "test".to_string(),
            thread_id: None,
        };
        let now = chrono::Utc::now();
        let task_id = crate::domain::TaskId::new();
        let entity = app
            .world_mut()
            .spawn((
                Task {
                    id: task_id,
                    content: "to clear".to_string(),
                    creator: crate::domain::AgentId::nil(),
                    delegate: None,
                    status: TaskStatus::Running,
                    pending_confirmation_id: None,
                    input_summary: "test".to_string(),
                    result_summary: String::new(),
                    priority: 0,
                    created_at: now,
                    updated_at: now,
                    retry_count: 0,
                    max_retries: 3,
                    next_retry_at: None,
                    last_error: None,
                    multi_turn: false,
                    parent_task_id: None,
                    batch_id: None,
                    origin_channel: Some(channel),
                    routing_policy: crate::domain::TaskRoutingPolicy::conversational(ChannelId {
                        frontend: FrontendKind::Tui,
                        user_id: "test".to_string(),
                        thread_id: None,
                    }),
                    last_evaluated_turn: None,
                },
                ShortTermMemory::default(),
                PreviousTaskStatus(TaskStatus::Pending),
            ))
            .id();

        app.world_mut()
            .resource_mut::<EntityIndex>()
            .tasks
            .insert(task_id, entity);

        app.world_mut().spawn(ClearTaskMessage { task_id });

        app.update();

        assert!(
            app.world().get::<Task>(entity).is_none(),
            "task entity should be despawned after clear_task_system"
        );
        assert!(
            app.world()
                .resource::<EntityIndex>()
                .get_task(&task_id)
                .is_none(),
            "EntityIndex mapping should be removed after clear_task_system"
        );
        let remaining: Vec<_> = app
            .world_mut()
            .query::<&ClearTaskMessage>()
            .iter(app.world())
            .collect();
        assert!(remaining.is_empty(), "ClearTaskMessage should be despawned");
    }

    #[test]
    fn clear_task_system_does_not_spawn_task_terminated_message() {
        use crate::domain::{
            ChannelId, ClearTaskMessage, FrontendKind, PreviousTaskStatus, ShortTermMemory, Task,
            TaskStatus, TaskTerminatedMessage,
        };
        use crate::ecs::EntityIndex;

        let mut app = App::new();
        app.init_resource::<EntityIndex>();
        app.insert_resource(crate::domain::MemoryConfig::default());
        app.insert_resource(crate::contracts::FrontendRegistry { frontends: vec![] });
        app.insert_resource(crate::systems::NativeProcessBackend::default());
        app.insert_resource(ExperienceStore::default());
        app.add_systems(Update, (clear_task_system, task_termination_system));

        let channel = ChannelId {
            frontend: FrontendKind::Tui,
            user_id: "test".to_string(),
            thread_id: None,
        };
        let now = chrono::Utc::now();
        let task_id = crate::domain::TaskId::new();
        let entity = app
            .world_mut()
            .spawn((
                Task {
                    id: task_id,
                    content: "to clear".to_string(),
                    creator: crate::domain::AgentId::nil(),
                    delegate: None,
                    status: TaskStatus::Running,
                    pending_confirmation_id: None,
                    input_summary: "test".to_string(),
                    result_summary: String::new(),
                    priority: 0,
                    created_at: now,
                    updated_at: now,
                    retry_count: 0,
                    max_retries: 3,
                    next_retry_at: None,
                    last_error: None,
                    multi_turn: false,
                    parent_task_id: None,
                    batch_id: None,
                    origin_channel: Some(channel),
                    routing_policy: crate::domain::TaskRoutingPolicy::conversational(ChannelId {
                        frontend: FrontendKind::Tui,
                        user_id: "test".to_string(),
                        thread_id: None,
                    }),
                    last_evaluated_turn: None,
                },
                ShortTermMemory::default(),
                PreviousTaskStatus(TaskStatus::Pending),
            ))
            .id();

        app.world_mut()
            .resource_mut::<EntityIndex>()
            .tasks
            .insert(task_id, entity);

        app.world_mut().spawn(ClearTaskMessage { task_id });

        app.update();

        let terminated: Vec<_> = app
            .world_mut()
            .query::<&TaskTerminatedMessage>()
            .iter(app.world())
            .collect();
        assert!(
            terminated.is_empty(),
            "/clear should not spawn TaskTerminatedMessage"
        );
    }

    #[test]
    fn clear_task_system_does_not_spawn_summarization_request() {
        use crate::domain::{
            ChannelId, ClearTaskMessage, EntryMetadata, EntryRole, FrontendKind,
            PreviousTaskStatus, ShortTermMemory, SummarizationRequestMessage, Task, TaskStatus,
        };
        use crate::ecs::EntityIndex;

        let mut app = App::new();
        app.init_resource::<EntityIndex>();
        app.insert_resource(crate::domain::MemoryConfig::default());
        app.insert_resource(crate::contracts::FrontendRegistry { frontends: vec![] });
        app.insert_resource(crate::systems::NativeProcessBackend::default());
        app.insert_resource(ExperienceStore::default());
        app.add_systems(Update, (clear_task_system, task_termination_system));

        let channel = ChannelId {
            frontend: FrontendKind::Tui,
            user_id: "test".to_string(),
            thread_id: None,
        };
        let now = chrono::Utc::now();
        let task_id = crate::domain::TaskId::new();

        let mut stm = ShortTermMemory::default();
        stm.add_entry(
            EntryRole::User,
            "some content to summarize",
            EntryMetadata::default(),
        );
        let entity = app
            .world_mut()
            .spawn((
                Task {
                    id: task_id,
                    content: "to clear".to_string(),
                    creator: crate::domain::AgentId::nil(),
                    delegate: None,
                    status: TaskStatus::Running,
                    pending_confirmation_id: None,
                    input_summary: "test".to_string(),
                    result_summary: String::new(),
                    priority: 0,
                    created_at: now,
                    updated_at: now,
                    retry_count: 0,
                    max_retries: 3,
                    next_retry_at: None,
                    last_error: None,
                    multi_turn: false,
                    parent_task_id: None,
                    batch_id: None,
                    origin_channel: Some(channel),
                    routing_policy: crate::domain::TaskRoutingPolicy::conversational(ChannelId {
                        frontend: FrontendKind::Tui,
                        user_id: "test".to_string(),
                        thread_id: None,
                    }),
                    last_evaluated_turn: None,
                },
                stm,
                PreviousTaskStatus(TaskStatus::Pending),
            ))
            .id();

        app.world_mut()
            .resource_mut::<EntityIndex>()
            .tasks
            .insert(task_id, entity);

        app.world_mut().spawn(ClearTaskMessage { task_id });

        app.update();

        let summarize: Vec<_> = app
            .world_mut()
            .query::<&SummarizationRequestMessage>()
            .iter(app.world())
            .collect();
        assert!(
            summarize.is_empty(),
            "/clear should not spawn SummarizationRequestMessage"
        );
    }

    #[test]
    fn clear_task_system_pushes_task_cleared_event() {
        use std::sync::{Arc, Mutex};

        use crate::contracts::FrontendRegistry;
        use crate::domain::{
            ChannelId, ClearTaskMessage, EngineEvent, EventTarget, Frontend, FrontendKind,
            PreviousTaskStatus, ShortTermMemory, Task, TaskStatus, UserAction,
        };
        use crate::ecs::EntityIndex;

        struct MockFrontend {
            events: Arc<Mutex<Vec<EngineEvent>>>,
        }
        impl Frontend for MockFrontend {
            fn kind(&self) -> FrontendKind {
                FrontendKind::Tui
            }
            fn push_event(&self, event: EngineEvent) {
                self.events.lock().unwrap().push(event);
            }
            fn poll_actions(&self) -> Vec<UserAction> {
                vec![]
            }
        }

        let mut app = App::new();
        let events = Arc::new(Mutex::new(Vec::new()));
        app.insert_resource(FrontendRegistry {
            frontends: vec![Box::new(MockFrontend {
                events: events.clone(),
            })],
        });
        app.init_resource::<EntityIndex>();
        app.insert_resource(crate::domain::MemoryConfig::default());
        app.insert_resource(crate::systems::NativeProcessBackend::default());
        app.insert_resource(ExperienceStore::default());
        app.add_systems(Update, clear_task_system);

        let channel = ChannelId {
            frontend: FrontendKind::Tui,
            user_id: "test".to_string(),
            thread_id: None,
        };
        let now = chrono::Utc::now();
        let task_id = crate::domain::TaskId::new();
        let entity = app
            .world_mut()
            .spawn((
                Task {
                    id: task_id,
                    content: "to clear".to_string(),
                    creator: crate::domain::AgentId::nil(),
                    delegate: None,
                    status: TaskStatus::Running,
                    pending_confirmation_id: None,
                    input_summary: "test".to_string(),
                    result_summary: String::new(),
                    priority: 0,
                    created_at: now,
                    updated_at: now,
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
                },
                ShortTermMemory::default(),
                PreviousTaskStatus(TaskStatus::Pending),
            ))
            .id();

        app.world_mut()
            .resource_mut::<EntityIndex>()
            .tasks
            .insert(task_id, entity);

        app.world_mut().spawn(ClearTaskMessage { task_id });

        app.update();

        let events = events.lock().unwrap();
        match events
            .iter()
            .find(|e| matches!(e, EngineEvent::TaskCleared { .. }))
        {
            Some(EngineEvent::TaskCleared {
                target,
                task_id: tid,
            }) => {
                assert_eq!(*tid, task_id);
                match target {
                    EventTarget::Directed(v) => {
                        assert_eq!(v.len(), 1);
                        assert_eq!(v[0].frontend, FrontendKind::Tui);
                    }
                    other => panic!("expected Directed target, got {other:?}"),
                }
            }
            other => panic!("expected TaskCleared event, got {other:?}"),
        }
    }

    /// 构造最小 Bevy App，注册 retry_ready_system 及其所需资源。
    fn make_retry_test_app() -> App {
        let mut app = App::new();
        app.insert_resource(Clock::default());
        app.init_resource::<EntityIndex>();
        app.add_systems(Update, retry_ready_system);
        app
    }

    /// 在 app world 中 spawn 一个 Persistent Agent，返回其 AgentId。
    fn spawn_agent_for_retry(app: &mut App, name: &str) -> crate::domain::AgentId {
        use crate::domain::{
            AgentCapabilities, AgentProfile, AgentToolPermissions, ToolPermission,
        };
        use std::collections::HashMap;

        let agent_id = crate::domain::AgentId::new();
        let agent = Agent {
            id: agent_id,
            profile: AgentProfile {
                name: name.to_string(),
                model: "test-model".to_string(),
            },
            capabilities: AgentCapabilities {
                tags: vec![name.to_string()],
                description: format!("{} agent", name),
            },
            kind: crate::domain::AgentKind::Persistent,
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
            .insert(agent_id, entity);
        agent_id
    }

    /// 复现 bug（2026-09-03 brain-decision-leak-on-retry）：任务经 BrainLlm 派发后
    /// `delegate = brain agent` 且携带 `AwaitingBrainDecision`，brain 调用 502 可重试
    /// 失败进入 `schedule_retry`（brain_decision_system 的可重试分支不移除该组件）。
    /// 重试就绪时应重新走 `BrainLlm` 策略重建 BrainDecision 请求（带候选 agent 列表），
    /// 而非 `DirectDelegate`——否则 brain 会吐出路由决策 JSON 被当作最终回复泄漏到前端。
    #[test]
    fn retry_with_awaiting_brain_decision_uses_brain_llm_strategy() {
        use crate::domain::AwaitingBrainDecision;

        let mut app = make_retry_test_app();
        let now = chrono::Utc::now();

        let brain_agent_id = spawn_agent_for_retry(&mut app, "brain");

        // 模拟 brain 派发 + 可重试失败：delegate = brain，状态 Waiting(RetryBackoff)
        let mut task = Task::from_user_input(
            "qq_group_message".to_string(),
            3,
            crate::domain::ChannelId {
                frontend: crate::domain::FrontendKind::Tui,
                user_id: "test".to_string(),
                thread_id: None,
            },
        );
        task.mark_waiting_for_agent(brain_agent_id, now);
        task.schedule_retry(
            &crate::domain::ExecutionError::Transport("502 Bad Gateway".to_string()),
            now,
        );
        let task_id = task.id;
        let task_entity = app
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
            .insert(task_id, task_entity);

        app.world_mut().spawn(RetryReadyMessage { task_id });
        app.update();

        let pending = app
            .world_mut()
            .entity(task_entity)
            .get::<PendingDispatch>()
            .expect("PendingDispatch should be attached on retry");
        assert!(
            matches!(pending.hint.strategy, DispatchStrategy::BrainLlm),
            "retry of a task still awaiting brain decision must re-dispatch via BrainLlm \
             to rebuild the BrainDecision request, not DirectDelegate (got {:?})",
            pending.hint.strategy
        );
        assert_eq!(
            pending.hint.preferred_agent_name, None,
            "BrainLlm retry should not pin a preferred agent"
        );
    }

    /// 回归守卫：无 `AwaitingBrainDecision` 的普通 delegate 重试仍走
    /// `DirectDelegate`（既有行为不受本修复影响）。
    #[test]
    fn retry_with_delegate_without_awaiting_brain_uses_direct_delegate() {
        let mut app = make_retry_test_app();
        let now = chrono::Utc::now();

        let agent_id = spawn_agent_for_retry(&mut app, "worker");

        let mut task = Task::from_user_input(
            "normal task".to_string(),
            3,
            crate::domain::ChannelId {
                frontend: crate::domain::FrontendKind::Tui,
                user_id: "test".to_string(),
                thread_id: None,
            },
        );
        task.mark_waiting_for_agent(agent_id, now);
        task.schedule_retry(
            &crate::domain::ExecutionError::Transport("conn reset".to_string()),
            now,
        );
        let task_id = task.id;
        let task_entity = app.world_mut().spawn(task).id();
        app.world_mut()
            .resource_mut::<EntityIndex>()
            .tasks
            .insert(task_id, task_entity);

        app.world_mut().spawn(RetryReadyMessage { task_id });
        app.update();

        let pending = app
            .world_mut()
            .entity(task_entity)
            .get::<PendingDispatch>()
            .expect("PendingDispatch should be attached on retry");
        assert!(
            matches!(pending.hint.strategy, DispatchStrategy::DirectDelegate),
            "retry of a delegated task without AwaitingBrainDecision should use \
             DirectDelegate (got {:?})",
            pending.hint.strategy
        );
        assert_eq!(
            pending.hint.preferred_agent_name.as_deref(),
            Some("worker"),
            "DirectDelegate retry should pin the delegate agent name"
        );
    }
}
