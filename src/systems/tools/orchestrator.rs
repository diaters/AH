//! Tool 执行协调器
//!
//! 处理 Tool 执行动作和消息生成。
//!
//! # 流水线协议约定（单一权威）
//!
//! - **sync 路径边界**：`handle_tool_action`（sync 路径）只处理会话型/任务编排/
//!   提交类 `ToolAction`。`shell_exec` 等已上桥
//!   async worker（kind==Async）；`Direct` / `ExecSession` 是退役动作，sync 路径
//!   误用即报错（见 [`handle_retired_action`]），防止静默构造结果消息绕过异步桥
//!   的 hook 与通道单点落地。
//! - **InFlightToolCall claim 语义**：摘除 `InFlightToolCall` 后，cancel sweeper
//!   不再扫该实体（防重复发 error）。
//! - **工具结果落地 + 请求 despawn 协议**：结果消息统一经 `spawn_tool_error` /
//!   `spawn_shell_result` 构造并 spawn（`ToolExecutionResultMessage` +
//!   `ToolReturnedHookPending`），二者内部已含 `request_entity` despawn。

use crate::prelude::*;
use serde::Serialize;
use tracing::{debug, warn};
use uuid::Uuid;

use crate::contracts::{Clock, FrontendRegistry};
use crate::domain::JobSource;
use crate::domain::SessionBackend;
use crate::domain::{
    Agent, AgentExecutionOutput, AgentExecutionResult, AgentId, AgentKind, AskUserPending,
    BatchTaskState, ChannelId, ChatRoundStartedMessage, ChatSession, DispatchHint, DispatchKind,
    EngineEvent, EntryRole, EventTarget, FrontendKind, MessageRole, OutputContent, PendingDispatch,
    SessionHandleId, SessionInputRequest, SessionReadRequest, SessionStartRequest, SessionSummary,
    ShellSessionResult, ShortTermMemory, SubTaskBatchCreatedMessage, SubTaskBatchState,
    SubTaskConfig, SubTaskDefinition, Task, TaskId, TaskStatus, ToolAction, ToolCallingState,
    ToolError, ToolExecutionRequestMessage, ToolExecutionResultMessage, ToolReturnedHookPending,
    WaitingForTasksInfo, WaitingReason,
};
use crate::ecs::EntityIndex;

/// 清除任务上正在等待的工具确认 ID。
pub fn clear_task_pending_confirmation_id(
    tasks: &mut Query<(Entity, &mut Task)>,
    index: &EntityIndex,
    task_id: TaskId,
) {
    // 经 EntityIndex O(1) 解析 TaskId → Entity（替代全量线性扫描）
    if let Some((_, mut task)) = index.get_task(&task_id).and_then(|e| tasks.get_mut(e).ok()) {
        task.pending_confirmation_id = None;
    }
}

/// 等待任务结果
#[derive(Debug, Clone, Serialize)]
pub struct TaskWaitResult {
    pub task_id: String,
    pub status: TaskStatus,
    pub result: Option<String>,
    pub error: Option<String>,
}

/// 为 create_tasks 生成子 Task 实体、SubTaskBatchState 和消息
///
/// 返回 `(batch_id, 子任务 ID 列表)`，供调用方登记后台任务。
#[allow(clippy::too_many_arguments)]
pub fn spawn_create_tasks_messages(
    commands: &mut Commands,
    request_entity: Entity,
    agent_id: AgentId,
    task_id: TaskId,
    request_kind: crate::domain::AgentRequestKind,
    definitions: Vec<SubTaskDefinition>,
    tool_call_id: Option<String>,
    parent_origin_channel: Option<ChannelId>,
    job_id: Option<Uuid>,
    index: &mut EntityIndex,
) -> (Uuid, Vec<TaskId>) {
    let batch_id = Uuid::new_v4();
    let total_count = definitions.len();

    // 计算反向依赖：对每个任务，找出哪些任务依赖它
    let mut depended_by_map: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    for def in &definitions {
        for dep in &def.depends_on {
            depended_by_map
                .entry(dep.clone())
                .or_default()
                .push(def.name.clone());
        }
    }

    let mut batch_tasks = std::collections::HashMap::new();

    for def in &definitions {
        let child_task_id = crate::domain::TaskId::new();
        let child_task = Task {
            id: child_task_id,
            content: def.content.clone(),
            creator: agent_id,
            delegate: None,
            status: TaskStatus::Pending,
            pending_confirmation_id: None,
            input_summary: def.name.clone(),
            result_summary: String::new(),
            priority: 0,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            retry_count: 0,
            max_retries: 3,
            next_retry_at: None,
            last_error: None,
            multi_turn: false,
            parent_task_id: Some(task_id),
            batch_id: Some(batch_id),
            origin_channel: parent_origin_channel.clone(),
            routing_policy: parent_origin_channel.as_ref().map_or_else(
                || crate::domain::TaskRoutingPolicy::event(None, None),
                |ch| crate::domain::TaskRoutingPolicy::conversational(ch.clone()),
            ),
            last_evaluated_turn: None,
        };

        let depended_by = depended_by_map.get(&def.name).cloned().unwrap_or_default();

        let sub_task_config = SubTaskConfig {
            batch_id,
            child_agent_name: def.name.clone(),
            child_agent_model: def.model.clone(),
            allowed_tools: def.tools.clone(),
            parent_agent_id: agent_id,
            depends_on: def.depends_on.clone(),
            depended_by,
        };

        // 同步注册 EntityIndex：各 system 经索引 O(1) 查找任务，
        // 与 spawn_task 封装保持同样的索引一致性。
        let child_entity = commands
            .spawn((child_task, sub_task_config, ShortTermMemory::default()))
            .id();
        index.tasks.insert(child_task_id, child_entity);

        batch_tasks.insert(
            def.name.clone(),
            crate::domain::BatchTaskStatus {
                task_id: child_task_id,
                state: BatchTaskState::Pending,
                result_summary: None,
            },
        );
    }

    debug!(
        event = "CreateTasksBatchCreated",
        %batch_id,
        parent_task_id = %task_id,
        parent_agent_id = %agent_id,
        task_count = total_count,
        task_names = ?definitions.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(),
        ?tool_call_id,
        "sub-task batch created"
    );

    // 产出 SubTaskBatchState（附加到父 Task 实体以便后续查询）
    commands.spawn(SubTaskBatchState {
        batch_id,
        parent_tool_call_id: tool_call_id.clone().unwrap_or_default(),
        tasks: batch_tasks.clone(),
        completed_count: 0,
        total_count,
    });

    // 产出 SubTaskBatchCreatedMessage（触发父 Task 阻塞 + 子任务派发）
    commands.spawn(SubTaskBatchCreatedMessage {
        parent_task_id: task_id,
        batch_id,
        parent_tool_call_id: tool_call_id.clone().unwrap_or_default(),
        tasks: definitions,
    });

    // 产出 ToolExecutionResultMessage（让 tool calling loop 收到结果）
    // 构建包含 name 和 task_id 映射的任务列表
    let task_names: Vec<String> = batch_tasks.keys().cloned().collect();
    let tasks_with_ids: Vec<serde_json::Value> = batch_tasks
        .iter()
        .map(|(name, status)| {
            serde_json::json!({
                "name": name,
                "task_id": status.task_id.to_string()
            })
        })
        .collect();

    commands.spawn((
        ToolExecutionResultMessage {
            result: AgentExecutionResult {
                task_id,
                agent_id,
                request_kind,
                result: Ok(AgentExecutionOutput {
                    content: OutputContent::Text(format!(
                        "created {} sub-tasks (batch {}): {}",
                        total_count,
                        batch_id,
                        task_names.join(", ")
                    )),
                    reasoning_content: None,
                }),
                prompt: String::new(),
                system_prompt: None,
                tools: vec![],
                reasoning_content: None,
                work_item_id: None,
                conversation: None,
                turn_generation: None,
            },
            tool_name: "create_tasks".to_string(),
            tool_output: Ok({
                let mut output = serde_json::json!({
                    "status": "batch_created",
                    "batch_id": batch_id.to_string(),
                    "task_count": total_count,
                    "tasks": tasks_with_ids,
                    "note": "Sub-tasks run in the background and are supervised by a watchdog. Continue independent work, or end your turn if you depend on their results: a <task-notification> with all results is delivered automatically when the batch finishes. Do not poll.",
                });
                if let Some(job_id) = job_id {
                    output["job_id"] = serde_json::json!(job_id.to_string());
                }
                output
            }),
            tool_call_id,
            processed: false,
            original_tool_output: None,
        },
        ToolReturnedHookPending,
    ));

    commands.entity(request_entity).despawn();
    let child_ids = batch_tasks.values().map(|status| status.task_id).collect();
    (batch_id, child_ids)
}

/// 生成等待任务的消息和状态
pub fn spawn_wait_for_tasks(
    commands: &mut Commands,
    request_entity: Entity,
    task_entity: Entity,
    agent_id: AgentId,
    tool_call_id: String,
    task_ids: Vec<TaskId>,
    timeout_secs: u64,
) {
    debug!(
        event = "WaitForTasksInitiated",
        task_ids = ?task_ids,
        timeout_secs = timeout_secs,
        "task entering wait state for child tasks"
    );

    // 在 Task Entity 上添加等待信息组件
    commands.entity(task_entity).insert(WaitingForTasksInfo {
        target_task_ids: task_ids,
        timeout_at: chrono::Utc::now() + chrono::Duration::seconds(timeout_secs as i64),
        tool_call_id,
        agent_id,
    });

    // 清理请求实体
    commands.entity(request_entity).despawn();
}

/// 验证目标任务是否为当前任务的子任务
pub fn validate_task_ownership(
    current_task_id: TaskId,
    target_task_ids: &[TaskId],
    tasks: &Query<(Entity, &mut Task)>,
) -> Result<(), ToolError> {
    let _current_task = tasks
        .iter()
        .find(|(_, t)| t.id == current_task_id)
        .map(|(_, t)| t)
        .ok_or_else(|| ToolError::NotFound(format!("current task {}", current_task_id)))?;

    for target_id in target_task_ids {
        let target = tasks
            .iter()
            .find(|(_, t)| t.id == *target_id)
            .map(|(_, t)| t)
            .ok_or_else(|| ToolError::NotFound(format!("task {}", target_id)))?;

        // 目标任务必须是当前任务的子任务（parent_task_id 匹配）
        if target.parent_task_id != Some(current_task_id) {
            return Err(ToolError::PermissionDenied(format!(
                "task {} is not a child of current task",
                target_id
            )));
        }
    }

    Ok(())
}

/// 收集目标任务的结果
pub fn collect_task_results(
    task_ids: &[TaskId],
    tasks: &Query<&Task>,
    index: &EntityIndex,
) -> Vec<TaskWaitResult> {
    task_ids
        .iter()
        .map(|id| {
            // 经 EntityIndex O(1) 解析 TaskId → Entity（替代全量线性扫描）
            let task = index.get_task(id).and_then(|e| tasks.get(e).ok());
            TaskWaitResult {
                task_id: id.to_string(),
                status: task
                    .map(|t| t.status.clone())
                    .unwrap_or(TaskStatus::Pending),
                result: task.and_then(|t| {
                    if t.status == TaskStatus::Done {
                        Some(t.result_summary.clone())
                    } else {
                        None
                    }
                }),
                error: task.and_then(|t| {
                    if matches!(t.status, TaskStatus::Failed(_)) {
                        t.last_error.clone()
                    } else {
                        None
                    }
                }),
            }
        })
        .collect()
}

/// 生成等待结果消息
pub fn spawn_wait_result_message(
    commands: &mut Commands,
    task_id: TaskId,
    info: &WaitingForTasksInfo,
    results: Vec<TaskWaitResult>,
    timed_out: bool,
) {
    // 超时即转后台：批次继续执行，完成后由后台作业通知送达，不再需要重复等待。
    let still_running = results.iter().any(|r| !r.status.is_terminal());
    let output = if timed_out && still_running {
        serde_json::json!({
            "status": "running_in_background",
            "results": results,
            "timed_out": true,
            "message": "子任务仍在后台执行。全部结束后结果会以 <task-notification source=\"subtasks\"> 送达并自动开启新的一轮；无需再次调用 wait_tasks 或轮询进度，进展由 watchdog 监督。不依赖这些结果的工作可以继续，否则请结束本轮回复并说明在等待什么。",
        })
    } else {
        serde_json::json!({
            "results": results,
            "timed_out": timed_out,
        })
    };

    debug!(
        event = "WaitForTasksCompleted",
        task_id = %task_id,
        results_count = results.len(),
        timed_out = timed_out,
        "wait_tasks completed, resuming task"
    );

    // 生成工具执行结果消息
    commands.spawn((
        ToolExecutionResultMessage {
            result: AgentExecutionResult {
                task_id,
                agent_id: info.agent_id,
                request_kind: crate::domain::AgentRequestKind::LlmCompletion,
                result: Ok(AgentExecutionOutput {
                    content: OutputContent::Text("wait_tasks completed".to_string()),
                    reasoning_content: None,
                }),
                prompt: String::new(),
                system_prompt: None,
                tools: vec![],
                reasoning_content: None,
                work_item_id: None,
                conversation: None,
                turn_generation: None,
            },
            tool_name: "wait_tasks".to_string(),
            tool_output: Ok(output),
            tool_call_id: Some(info.tool_call_id.clone()),
            processed: false,
            original_tool_output: None,
        },
        ToolReturnedHookPending,
    ));
}

/// `handle_tool_action` 的 ECS 查询组：各工具分支共用的 World 检索句柄。
///
/// 生命周期约定：`Query` 对 `'world`/`'state`/`D` 均 invariant，无法靠子类型
/// 换生命周期；Bevy system 中各 Query 参数（乃至 QueryData 元组内各组件引用）
/// 的生命周期相互独立，因此全部独立声明、由调用处推断各自匹配：
/// - `'a`：本 struct 对各 Query 的借用期，覆盖一次 `handle_tool_action` 调用即可；
/// - `('wt,'st,'dt)` / `('wa,'sa,'da)` / ...：tasks / agents / ... 各自的三元组；
pub(crate) struct ToolWorldQueries<'a, 'wt, 'st, 'dt, 'wa, 'sa, 'da, 'wc, 'sc, 'dc, 'wm, 'sm, 'dm> {
    pub(crate) tasks: &'a mut Query<'wt, 'st, (Entity, &'dt mut Task)>,
    /// 调用方 system 持有 `Query<&mut Agent>`（approval 路径需 `get_mut`），
    /// 此处按可变引用透传，各分支内仅只读使用。
    pub(crate) agents: &'a Query<'wa, 'sa, &'da mut Agent>,
    pub(crate) chat_sessions: &'a Query<'wc, 'sc, &'dc ChatSession>,
    pub(crate) short_term_memories: &'a mut Query<'wm, 'sm, &'dm mut ShortTermMemory>,
}

/// `handle_tool_action` 的资源句柄组：工具落地所需的全局资源。
pub(crate) struct ToolResources<'a, B: SessionBackend> {
    pub(crate) backend: &'a B,
    /// ask_user 需要把问题推送到 task 的 output_channel 对应前端。
    pub(crate) frontend_registry: &'a FrontendRegistry,
    pub(crate) clock: &'a Clock,
}

/// 已退役的 sync 路径动作，仅用于区分误用报错文案。
enum RetiredAction {
    Direct,
    ExecSession,
}

/// 统一处理 Tool 执行动作：只做分派，各 ToolAction 分支逻辑见对应 `handle_*` 函数。
///
/// 流水线协议约定见模块级 rustdoc。
#[allow(clippy::too_many_arguments)]
pub fn handle_tool_action<B: SessionBackend>(
    commands: &mut Commands,
    request_entity: Entity,
    task_entity: Entity,
    request: &ToolExecutionRequestMessage,
    action: Result<ToolAction, ToolError>,
    world: &mut ToolWorldQueries<'_, '_, '_, '_, '_, '_, '_, '_, '_, '_, '_, '_, '_>,
    resources: &mut ToolResources<'_, B>,
    index: &mut EntityIndex,
) {
    match action {
        Ok(ToolAction::Direct(_value)) => {
            handle_retired_action(commands, request_entity, request, RetiredAction::Direct);
        }
        Ok(ToolAction::CreateBatch(definitions)) => {
            handle_create_batch(
                commands,
                request_entity,
                task_entity,
                request,
                world.tasks,
                definitions,
                index,
            );
        }
        Ok(ToolAction::WaitForTasks {
            task_ids,
            timeout_secs,
        }) => {
            handle_wait_for_tasks(
                commands,
                request_entity,
                task_entity,
                request,
                world.tasks,
                task_ids,
                timeout_secs,
            );
        }
        Ok(ToolAction::ExecSession(_session_request)) => {
            handle_retired_action(
                commands,
                request_entity,
                request,
                RetiredAction::ExecSession,
            );
        }
        Ok(
            action @ (ToolAction::StartSession(_)
            | ToolAction::ReadSession(_)
            | ToolAction::ListSessions
            | ToolAction::InputSession(_)
            | ToolAction::StopSession(_)),
        ) => {
            dispatch_session_action(
                commands,
                request_entity,
                task_entity,
                request,
                resources.backend,
                action,
            );
        }
        Ok(ToolAction::SearchExperience { situation }) => {
            // 检索交给经验管理员；结果由 experience_librarian_response_system 回填本次调用。
            let Some(tool_call_id) = request.tool_call_id.clone() else {
                spawn_tool_error(
                    commands,
                    request_entity,
                    request,
                    ToolError::InvalidInput("search_experience requires a tool call id".into()),
                );
                return;
            };
            commands.spawn(crate::domain::ExperienceLookupRequest {
                task_id: request.request.task_id,
                situation,
                origin: crate::domain::ExperienceLookupOrigin::ToolCall {
                    agent_id: request.request.agent_id,
                    request_kind: request.request.request_kind.clone(),
                    tool_call_id,
                },
            });
            commands.entity(request_entity).despawn();
        }
        Ok(ToolAction::StopBackgroundJob { job_id }) => {
            // 交给后台作业控制系统统一执行（与 watchdog 同一入口），结果由其回填本次调用。
            let Some(tool_call_id) = request.tool_call_id.clone() else {
                spawn_tool_error(
                    commands,
                    request_entity,
                    request,
                    ToolError::InvalidInput("stop_background_job requires a tool call id".into()),
                );
                return;
            };
            commands.spawn(crate::domain::BackgroundJobControl {
                job_id,
                action: crate::domain::JobControlAction::Stop,
                origin: crate::domain::JobControlOrigin::Agent {
                    task_id: request.request.task_id,
                    agent_id: request.request.agent_id,
                    tool_call_id,
                },
            });
            commands.entity(request_entity).despawn();
        }
        Ok(ToolAction::SendChannelMessage {
            channel,
            target,
            content,
            attachments,
        }) => {
            handle_send_channel_message(
                commands,
                request_entity,
                request,
                channel,
                target,
                content,
                attachments,
            );
        }
        Ok(ToolAction::StartChatRound {
            agent_name,
            agent_tags,
            message,
            context,
            handle,
        }) => {
            handle_start_chat_round(
                commands,
                request_entity,
                task_entity,
                request,
                world,
                resources.clock,
                agent_name,
                agent_tags,
                message,
                context,
                handle,
            );
        }
        Ok(ToolAction::AskUser { question }) => {
            handle_ask_user(
                commands,
                request_entity,
                task_entity,
                request,
                world.tasks,
                resources.frontend_registry,
                resources.clock,
                question,
            );
        }
        Err(e) => {
            spawn_tool_error(commands, request_entity, request, e);
        }
    }
}

/// Session 类动作子分派（Start/Read/List/Input/Stop）：
/// 均只依赖 backend + 请求元数据，收敛为单入口以压缩主分派体积。
fn dispatch_session_action<B: SessionBackend>(
    commands: &mut Commands,
    request_entity: Entity,
    task_entity: Entity,
    request: &ToolExecutionRequestMessage,
    backend: &B,
    action: ToolAction,
) {
    match action {
        ToolAction::StartSession(session_request) => {
            handle_start_session(
                commands,
                request_entity,
                task_entity,
                request,
                backend,
                session_request,
            );
        }
        ToolAction::ReadSession(read_request) => {
            handle_read_session(commands, request_entity, request, backend, read_request);
        }
        ToolAction::ListSessions => {
            handle_list_sessions(commands, request_entity, request, backend);
        }
        ToolAction::InputSession(input_request) => {
            handle_input_session(commands, request_entity, request, backend, input_request);
        }
        ToolAction::StopSession(handle_id) => {
            handle_stop_session(commands, request_entity, request, backend, handle_id);
        }
        // 主分派经 or-pattern 保证只传入上述 5 类动作。
        _ => unreachable!("dispatch_session_action received non-session action"),
    }
}

/// 退役动作兜底：Direct / ExecSession 已上桥 async worker，sync 路径误用即报错。
///
/// 保留分支防止未来误用——若 Sync 工具误返回退役动作，立即报错而非
/// 静默构造结果消息绕过异步桥的 hook / 通道单点落地（Direct）、
/// 或静默调阻塞 backend 拉长帧（ExecSession）。
fn handle_retired_action(
    commands: &mut Commands,
    request_entity: Entity,
    request: &ToolExecutionRequestMessage,
    kind: RetiredAction,
) {
    let error = match kind {
        // list_experience_candidates 已上桥到 async worker（kind==Async），
        // 不再走 sync 路径产生 `ToolAction::Direct`。
        RetiredAction::Direct => ToolError::InternalState(
            "Direct action is retired (list_experience_candidates is async-only); \
             BuiltinTool must not return Direct on sync path"
                .to_string(),
        ),
        // shell_exec 已上桥到 async worker（kind==Async），不再走 sync 路径
        // 产生 `ToolAction::ExecSession`。
        RetiredAction::ExecSession => ToolError::InternalState(
            "ExecSession action is retired (shell_exec is async-only); \
             BuiltinTool must not return ExecSession on sync path"
                .to_string(),
        ),
    };
    spawn_tool_error(commands, request_entity, request, error);
}

/// CreateBatch 分支：读取父任务 origin_channel 后生成子任务批次消息。
fn handle_create_batch(
    commands: &mut Commands,
    request_entity: Entity,
    task_entity: Entity,
    request: &ToolExecutionRequestMessage,
    tasks: &Query<(Entity, &mut Task)>,
    definitions: Vec<SubTaskDefinition>,
    index: &mut EntityIndex,
) {
    let parent_origin_channel = tasks
        .get(task_entity)
        .map(|(_, t)| t.origin_channel.clone())
        .unwrap_or_else(|_| {
            warn!(
                event = "ParentTaskNotFoundForSubTaskChannel",
                task_entity = ?task_entity,
                task_id = %request.request.task_id,
                "parent task entity not found, falling back to Tui/default for sub-task origin_channel"
            );
            Some(ChannelId {
                frontend: FrontendKind::Tui,
                user_id: "default".to_string(),
                thread_id: None,
            })
        });
    // 登记后台任务：未被 wait_tasks 认领时，批次完成后以通知回流父任务。
    // WorkItem 内部调用不登记（其原任务可能已终态）。
    let job_id = request
        .request
        .work_item_id
        .is_none()
        .then(uuid::Uuid::new_v4);
    let (batch_id, task_ids) = spawn_create_tasks_messages(
        commands,
        request_entity,
        request.request.agent_id,
        request.request.task_id,
        request.request.request_kind.clone(),
        definitions,
        request.tool_call_id.clone(),
        parent_origin_channel,
        job_id,
        index,
    );
    if let Some(job_id) = job_id {
        crate::systems::loop_control::register_background_job(
            commands,
            job_id,
            task_entity,
            request.request.task_id,
            JobSource::SubTaskBatch { batch_id, task_ids },
            chrono::Utc::now(),
        );
    }
}

/// WaitForTasks 分支：验证任务归属后挂起等待子任务完成。
fn handle_wait_for_tasks(
    commands: &mut Commands,
    request_entity: Entity,
    task_entity: Entity,
    request: &ToolExecutionRequestMessage,
    tasks: &Query<(Entity, &mut Task)>,
    task_ids: Vec<TaskId>,
    timeout_secs: u64,
) {
    // 验证任务归属
    match validate_task_ownership(request.request.task_id, &task_ids, tasks) {
        Ok(()) => {
            spawn_wait_for_tasks(
                commands,
                request_entity,
                task_entity,
                request.request.agent_id,
                request.tool_call_id.clone().unwrap_or_default(),
                task_ids,
                timeout_secs,
            );
        }
        Err(e) => {
            spawn_tool_error(commands, request_entity, request, e);
        }
    }
}

/// StartSession 分支：受管监控保持后台归属，退出通知单独按 notify_on_exit 控制。
fn handle_start_session<B: SessionBackend>(
    commands: &mut Commands,
    request_entity: Entity,
    task_entity: Entity,
    request: &ToolExecutionRequestMessage,
    backend: &B,
    session_request: SessionStartRequest,
) {
    let notify_on_exit = session_request.notify_on_exit;
    let notify_on_output = session_request.notify_on_output;
    match backend.start_session(session_request) {
        Ok(handle) => {
            if (notify_on_exit || notify_on_output) && request.request.work_item_id.is_none() {
                crate::systems::loop_control::spawn_background_job(
                    commands,
                    task_entity,
                    request.request.task_id,
                    JobSource::Shell {
                        handle_id: handle.handle_id,
                        notify_on_exit,
                    },
                    chrono::Utc::now(),
                );
            }
            let summary = SessionSummary::from_handle(&handle);
            spawn_shell_result(
                commands,
                request_entity,
                request,
                "shell_start",
                serde_json::json!(ShellSessionResult::from_summary(&summary)),
            );
        }
        Err(error) => {
            spawn_tool_error(
                commands,
                request_entity,
                request,
                ToolError::ExecutionFailed(error),
            );
        }
    }
}

/// ReadSession 分支：先断言会话归属，再读取会话状态和最新输出快照。
fn handle_read_session<B: SessionBackend>(
    commands: &mut Commands,
    request_entity: Entity,
    request: &ToolExecutionRequestMessage,
    backend: &B,
    read_request: SessionReadRequest,
) {
    if let Err(e) =
        backend.assert_task_owns_session(request.request.task_id, read_request.handle_id)
    {
        spawn_tool_error(
            commands,
            request_entity,
            request,
            ToolError::PermissionDenied(e),
        );
    } else {
        match backend.read_session(read_request) {
            Ok(summary) => {
                spawn_shell_result(
                    commands,
                    request_entity,
                    request,
                    "shell_read",
                    serde_json::json!(ShellSessionResult::from_summary(&summary)),
                );
            }
            Err(error) => {
                spawn_tool_error(
                    commands,
                    request_entity,
                    request,
                    ToolError::ExecutionFailed(error),
                );
            }
        }
    }
}

/// ListSessions 分支：列出当前任务的全部活动 shell 会话。
fn handle_list_sessions<B: SessionBackend>(
    commands: &mut Commands,
    request_entity: Entity,
    request: &ToolExecutionRequestMessage,
    backend: &B,
) {
    match backend.list_task_sessions(request.request.task_id) {
        Ok(sessions) => {
            let payload = sessions
                .iter()
                .map(ShellSessionResult::from_summary)
                .collect::<Vec<_>>();
            spawn_shell_result(
                commands,
                request_entity,
                request,
                "shell_list",
                serde_json::json!(payload),
            );
        }
        Err(error) => {
            spawn_tool_error(
                commands,
                request_entity,
                request,
                ToolError::ExecutionFailed(error),
            );
        }
    }
}

/// InputSession 分支：先断言会话归属，再发送交互输入。
fn handle_input_session<B: SessionBackend>(
    commands: &mut Commands,
    request_entity: Entity,
    request: &ToolExecutionRequestMessage,
    backend: &B,
    input_request: SessionInputRequest,
) {
    if let Err(e) =
        backend.assert_task_owns_session(request.request.task_id, input_request.handle_id)
    {
        spawn_tool_error(
            commands,
            request_entity,
            request,
            ToolError::PermissionDenied(e),
        );
    } else {
        match backend.input_session(input_request) {
            Ok(handle) => {
                spawn_shell_result(
                    commands,
                    request_entity,
                    request,
                    "shell_input",
                    serde_json::json!(ShellSessionResult::accepted_input(&handle)),
                );
            }
            Err(error) => {
                spawn_tool_error(
                    commands,
                    request_entity,
                    request,
                    ToolError::ExecutionFailed(error),
                );
            }
        }
    }
}

/// StopSession 分支：先断言会话归属，再停止会话。
fn handle_stop_session<B: SessionBackend>(
    commands: &mut Commands,
    request_entity: Entity,
    request: &ToolExecutionRequestMessage,
    backend: &B,
    handle_id: SessionHandleId,
) {
    if let Err(e) = backend.assert_task_owns_session(request.request.task_id, handle_id) {
        spawn_tool_error(
            commands,
            request_entity,
            request,
            ToolError::PermissionDenied(e),
        );
    } else {
        match backend.stop_session(handle_id) {
            Ok(handle) => {
                spawn_shell_result(
                    commands,
                    request_entity,
                    request,
                    "shell_stop",
                    serde_json::json!(ShellSessionResult::stopped(&handle)),
                );
            }
            Err(error) => {
                spawn_tool_error(
                    commands,
                    request_entity,
                    request,
                    ToolError::ExecutionFailed(error),
                );
            }
        }
    }
}

/// SendChannelMessage 分支：只 spawn PendingChannelSend，由通道系统异步落地。
fn handle_send_channel_message(
    commands: &mut Commands,
    request_entity: Entity,
    request: &ToolExecutionRequestMessage,
    channel: String,
    target: Option<String>,
    content: String,
    attachments: Vec<crate::domain::ChannelAttachment>,
) {
    commands.spawn(crate::domain::PendingChannelSend {
        channel,
        recipient: target,
        content,
        attachments,
        tool_call_id: request.tool_call_id.clone(),
        task_id: request.request.task_id,
        agent_id: request.request.agent_id,
        request_entity,
    });
}

/// StartChatRound 分支：继续已有对话或开新对话，并 spawn ChatRoundStartedMessage。
///
/// 内部按 `handle` 是否存在分派到 [`continue_chat_round`] / [`start_new_chat`]。
#[allow(clippy::too_many_arguments)]
fn handle_start_chat_round(
    commands: &mut Commands,
    request_entity: Entity,
    task_entity: Entity,
    request: &ToolExecutionRequestMessage,
    world: &mut ToolWorldQueries<'_, '_, '_, '_, '_, '_, '_, '_, '_, '_, '_, '_, '_>,
    clock: &Clock,
    agent_name: Option<String>,
    agent_tags: Vec<String>,
    message: String,
    context: Option<String>,
    handle: Option<TaskId>,
) {
    let parent_task_id = request.request.task_id;
    let parent_tool_call_id = request.tool_call_id.clone().unwrap_or_default();

    // 一次性从父任务 clone 出所需信息，避免 Query 借用冲突
    let (parent_origin_channel, _parent_delegate) = world
        .tasks
        .get(task_entity)
        .map(|(_, t)| (t.origin_channel.clone(), t.delegate))
        .unwrap_or_else(|_| {
            warn!(
                event = "ParentTaskNotFoundForChatChannel",
                task_id = %parent_task_id,
                "parent task entity not found, falling back to Tui/default for chat subtask origin_channel"
            );
            (
                Some(ChannelId {
                    frontend: FrontendKind::Tui,
                    user_id: "default".to_string(),
                    thread_id: None,
                }),
                None,
            )
        });

    let round = if let Some(handle) = handle {
        continue_chat_round(
            commands,
            request_entity,
            request,
            world,
            clock,
            handle,
            &message,
            parent_task_id,
            &parent_tool_call_id,
        )
    } else {
        start_new_chat(
            commands,
            request_entity,
            request,
            world,
            agent_name,
            agent_tags,
            message,
            context,
            parent_task_id,
            &parent_tool_call_id,
            parent_origin_channel,
        )
    };

    let Some((child_task_id, batch_id)) = round else {
        return;
    };

    commands.spawn(ChatRoundStartedMessage {
        parent_task_id,
        child_task_id,
        batch_id,
        parent_tool_call_id,
    });

    commands.entity(request_entity).despawn();
}

/// 继续已有 chat 对话轮次（handle 分支）。
///
/// 返回 `Some((child_task_id, batch_id))` 表示轮次已就绪；
/// `None` 表示校验失败，已通过 `spawn_tool_error` 落地错误（含 request despawn）。
#[allow(clippy::too_many_arguments)]
fn continue_chat_round(
    commands: &mut Commands,
    request_entity: Entity,
    request: &ToolExecutionRequestMessage,
    world: &mut ToolWorldQueries<'_, '_, '_, '_, '_, '_, '_, '_, '_, '_, '_, '_, '_>,
    clock: &Clock,
    handle: TaskId,
    message: &str,
    parent_task_id: TaskId,
    parent_tool_call_id: &str,
) -> Option<(TaskId, Uuid)> {
    // 继续已有对话：先只读收集信息，再单独修改
    let Some((child_entity, child_task)) = world
        .tasks
        .iter()
        .find(|(_, t)| t.id == handle)
        .map(|(e, t)| (e, t.clone()))
    else {
        spawn_tool_error(
            commands,
            request_entity,
            request,
            ToolError::NotFound(format!("chat handle {}", handle)),
        );
        return None;
    };

    if child_task.parent_task_id != Some(parent_task_id) {
        spawn_tool_error(
            commands,
            request_entity,
            request,
            ToolError::PermissionDenied("chat handle does not belong to current task".to_string()),
        );
        return None;
    }

    if !matches!(
        child_task.status,
        TaskStatus::Waiting(WaitingReason::ChatAgent)
    ) {
        spawn_tool_error(
            commands,
            request_entity,
            request,
            ToolError::InvalidInput("chat handle is not in waiting state".to_string()),
        );
        return None;
    }

    let new_batch_id = Uuid::new_v4();
    let child_task_id = child_task.id;

    // 追加本轮用户消息到子任务 STM
    if let Ok(mut stm) = world.short_term_memories.get_mut(child_entity) {
        stm.add_entry(EntryRole::User, message, Default::default());
    }

    // 更新 ChatSession（保留 child_agent_name）
    let child_agent_name = world
        .chat_sessions
        .get(child_entity)
        .map(|s| s.child_agent_name.clone())
        .unwrap_or_default();
    commands.entity(child_entity).insert(ChatSession {
        child_agent_name: child_agent_name.clone(),
        parent_tool_call_id: parent_tool_call_id.to_string(),
        current_batch_id: new_batch_id,
    });

    // 附加 PendingDispatch，由 dispatch_system 处理 DirectDelegate 派发
    commands.entity(child_entity).insert(PendingDispatch {
        kind: DispatchKind::Task,
        hint: DispatchHint::agent(child_agent_name),
    });

    // 唤醒子任务
    if let Ok((_, mut task)) = world.tasks.get_mut(child_entity) {
        task.mark_ready(clock.0);
    }

    Some((child_task_id, new_batch_id))
}

/// 开始新的 chat 对话（无 handle 分支）。
///
/// 返回 `Some((child_task_id, batch_id))` 表示子任务已创建；
/// `None` 表示未找到匹配 Agent，已通过 `spawn_tool_error` 落地错误（含 request despawn）。
#[allow(clippy::too_many_arguments)]
fn start_new_chat(
    commands: &mut Commands,
    request_entity: Entity,
    request: &ToolExecutionRequestMessage,
    world: &ToolWorldQueries<'_, '_, '_, '_, '_, '_, '_, '_, '_, '_, '_, '_, '_>,
    agent_name: Option<String>,
    agent_tags: Vec<String>,
    message: String,
    context: Option<String>,
    parent_task_id: TaskId,
    parent_tool_call_id: &str,
    parent_origin_channel: Option<ChannelId>,
) -> Option<(TaskId, Uuid)> {
    // 开始新对话
    let agent = {
        let name = agent_name.as_deref();
        let by_name = name.and_then(|n| {
            world
                .agents
                .iter()
                .find(|a| a.kind == AgentKind::Persistent && a.profile.name == n)
        });
        if let Some(a) = by_name {
            Some(a)
        } else if !agent_tags.is_empty() {
            world.agents.iter().find(|a| {
                a.kind == AgentKind::Persistent
                    && agent_tags
                        .iter()
                        .all(|tag| a.capabilities.tags.contains(tag))
            })
        } else {
            None
        }
    };
    let Some(agent) = agent else {
        spawn_tool_error(
            commands,
            request_entity,
            request,
            ToolError::NotFound("no matching persistent agent found".to_string()),
        );
        return None;
    };

    let child_task_id = crate::domain::TaskId::new();
    let batch_id = Uuid::new_v4();

    let mut initial_stm = ShortTermMemory::default();
    if let Some(ref ctx) = context {
        initial_stm.add_entry(
            EntryRole::User,
            format!("[System context]\n{}\n\n{}", ctx, message),
            Default::default(),
        );
    } else {
        initial_stm.add_entry(EntryRole::User, &message, Default::default());
    }

    let mut child_task = Task::from_user_input_ready(
        &message,
        0,
        parent_origin_channel.clone().unwrap_or_else(|| ChannelId {
            frontend: FrontendKind::Tui,
            user_id: "default".to_string(),
            thread_id: None,
        }),
    );
    child_task.id = child_task_id;
    child_task.parent_task_id = Some(parent_task_id);
    child_task.delegate = Some(agent.id);
    child_task.creator = request.request.agent_id;
    child_task.multi_turn = true;

    commands.spawn((
        child_task,
        initial_stm,
        ChatSession {
            child_agent_name: agent.profile.name.clone(),
            parent_tool_call_id: parent_tool_call_id.to_string(),
            current_batch_id: batch_id,
        },
        PendingDispatch {
            kind: DispatchKind::Task,
            hint: DispatchHint::agent(agent.profile.name.clone()),
        },
    ));

    Some((child_task_id, batch_id))
}

/// AskUser 分支：把问题推送到 task 的 output_channel 前端并挂起等待用户回复。
#[allow(clippy::too_many_arguments)]
fn handle_ask_user(
    commands: &mut Commands,
    request_entity: Entity,
    task_entity: Entity,
    request: &ToolExecutionRequestMessage,
    tasks: &mut Query<(Entity, &mut Task)>,
    frontend_registry: &FrontendRegistry,
    clock: &Clock,
    question: String,
) {
    let task_id = request.request.task_id;
    let agent_id = request.request.agent_id;
    // request.tool_call_id 是 Option<String>，AskUserPending.tool_call_id 是 String。
    // ask_user 由 LLM 发起，正常情况 tool_call_id 为 Some；None 时用空串兜底（不影响 LLM loop 续跑）。
    let tool_call_id = request.tool_call_id.clone().unwrap_or_default();

    // 1. 读取 task 的 output_channel
    let output_channel = tasks
        .get(task_entity)
        .map(|(_, t)| t.routing_policy.output_channel().cloned())
        .ok()
        .flatten();

    // 2. 无 output_channel 时返回错误（避免 task 永远卡在 Waiting(AskUser)）
    let Some(channel) = output_channel else {
        spawn_tool_error(
            commands,
            request_entity,
            request,
            ToolError::InvalidInput("ask_user requires task with output_channel".to_string()),
        );
        return;
    };

    let question_event = EngineEvent::QuestionPresented {
        target: EventTarget::Directed(vec![channel.clone()]),
        task_id,
        question_id: tool_call_id.clone(),
        question: question.clone(),
    };
    for frontend in frontend_registry.frontends.iter().filter(|frontend| {
        channel.frontend == FrontendKind::Client && frontend.kind() == FrontendKind::Client
    }) {
        frontend.push_event(question_event.clone());
    }
    // 3. 通过 EngineEvent::Text 把问题推送到 output_channel
    let event = EngineEvent::Text {
        target: EventTarget::Directed(vec![channel]),
        role: MessageRole::Agent,
        content: question.clone(),
        task_id: Some(task_id),
    };
    for frontend in &frontend_registry.frontends {
        frontend.push_event(event.clone());
    }

    // 4. 在 task entity 上挂 AskUserPending（先 insert，再切 status，保证不变量）
    commands.entity(task_entity).insert(AskUserPending {
        tool_call_id,
        agent_id,
    });

    // 5. task.status = Waiting(AskUser)
    if let Ok((_, mut task)) = tasks.get_mut(task_entity) {
        task.mark_waiting(WaitingReason::AskUser, clock.0);
    }

    // 6. despawn ToolExecutionRequestMessage
    commands.entity(request_entity).despawn();
}

/// 恢复 Task 状态（从 Waiting(ToolExecution) 恢复到 Ready 或保持 Waiting(ToolExecution)）
///
/// 仅处理 Waiting(ToolExecution) 状态。其他 Waiting 变体（AskUser、ChatAgent、
/// SubTaskBatch 等）由各自的发起系统主动挂起，不应被本函数覆盖。
pub fn restore_task_after_tool(
    tasks: &mut Query<(Entity, &mut Task)>,
    calling_states: &Query<(Entity, &ToolCallingState)>,
    index: &EntityIndex,
    task_id: TaskId,
) {
    // 经 EntityIndex O(1) 解析 TaskId → Entity（替代全量线性扫描）
    if let Some((_, mut task)) = index.get_task(&task_id).and_then(|e| tasks.get_mut(e).ok()) {
        // 仅恢复 Waiting(ToolExecution)；其他 Waiting 变体由各自系统管理，不在此处覆盖
        if !matches!(
            task.status,
            TaskStatus::Waiting(WaitingReason::ToolExecution)
        ) {
            return;
        }
        let has_calling_state = calling_states.iter().any(|(_, cs)| cs.task_id == task.id);
        task.status = if has_calling_state {
            TaskStatus::Waiting(WaitingReason::ToolExecution)
        } else {
            TaskStatus::Ready
        };
    }
}

/// 生成 Tool 错误结果
pub fn spawn_tool_error(
    commands: &mut Commands,
    request_entity: Entity,
    request: &ToolExecutionRequestMessage,
    error: ToolError,
) {
    let execution_result = AgentExecutionResult {
        task_id: request.request.task_id,
        agent_id: request.request.agent_id,
        request_kind: request.request.request_kind.clone(),
        result: Err(crate::domain::ExecutionError::Unknown(error.to_string())),
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
            result: execution_result,
            tool_name: request.tool_name.clone(),
            tool_output: Err(error),
            tool_call_id: request.tool_call_id.clone(),
            processed: false,
            original_tool_output: None,
        },
        ToolReturnedHookPending,
    ));

    commands.entity(request_entity).despawn();
}

/// 生成 Shell 工具执行结果
pub fn spawn_shell_result(
    commands: &mut Commands,
    request_entity: Entity,
    request: &ToolExecutionRequestMessage,
    tool_name: &str,
    tool_output: serde_json::Value,
) {
    let execution_result = AgentExecutionResult {
        task_id: request.request.task_id,
        agent_id: request.request.agent_id,
        request_kind: request.request.request_kind.clone(),
        result: Ok(AgentExecutionOutput {
            content: OutputContent::Text(format!("{} completed", tool_name)),
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
            result: execution_result,
            tool_name: tool_name.to_string(),
            tool_output: Ok(tool_output),
            tool_call_id: request.tool_call_id.clone(),
            processed: false,
            original_tool_output: None,
        },
        ToolReturnedHookPending,
    ));

    commands.entity(request_entity).despawn();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::AgentRequestKind;

    /// 测试系统：从世界中的父 Task 读取 origin_channel，调用 spawn_create_tasks_messages。
    ///
    /// 通过系统而非直接调用 `world.commands()`，确保 `app.update()` 能正确刷新 Commands。
    fn spawn_subtasks_for_inheritance_test(
        mut commands: Commands,
        tasks: Query<&Task>,
        mut index: ResMut<EntityIndex>,
    ) {
        let parent_task = tasks
            .iter()
            .find(|t| t.content == "parent")
            .expect("parent task should exist");
        let parent_task_id = parent_task.id;
        let parent_origin_channel = parent_task.origin_channel.clone();

        // spawn_create_tasks_messages 会在结束时 despawn request_entity，
        // 因此需要一个真实存在的 entity。
        let request_entity = commands.spawn(()).id();

        spawn_create_tasks_messages(
            &mut commands,
            request_entity,
            crate::domain::AgentId::nil(),
            parent_task_id,
            AgentRequestKind::LlmCompletion,
            vec![SubTaskDefinition {
                name: "child-agent".to_string(),
                content: "do something".to_string(),
                tools: vec![],
                depends_on: vec![],
                model: None,
            }],
            None,
            parent_origin_channel,
            None,
            &mut index,
        );
    }

    /// 验证 `spawn_create_tasks_messages` 生成的子 Task 继承父 Task 的非默认 origin_channel。
    ///
    /// 回归保护：曾出现过子任务硬编码 `Tui/default` 的回归。所有现有集成测试的父任务均使用
    /// `default_channel()` (Tui/default)，因此继承与硬编码无法区分。本测试使用 Telegram 通道
    /// 作为父通道，确保子任务的 `origin_channel == telegram_channel`，而非 `Tui/default`。
    #[test]
    fn create_tasks_subtask_inherits_parent_origin_channel() {
        let mut app = App::new();
        app.init_resource::<EntityIndex>();

        let telegram_channel = ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "tg-user".to_string(),
            thread_id: None,
        };

        // 生成父 Task，使用非默认的 Telegram 通道
        let parent_task_id = crate::domain::TaskId::new();
        let now = chrono::Utc::now();
        let parent_task = Task {
            id: parent_task_id,
            content: "parent".to_string(),
            creator: crate::domain::AgentId::nil(),
            delegate: None,
            status: TaskStatus::Pending,
            pending_confirmation_id: None,
            input_summary: String::new(),
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
            origin_channel: Some(telegram_channel.clone()),
            routing_policy: crate::domain::TaskRoutingPolicy::conversational(
                telegram_channel.clone(),
            ),
            last_evaluated_turn: None,
        };
        app.world_mut()
            .spawn((parent_task, ShortTermMemory::default()));

        app.add_systems(Update, spawn_subtasks_for_inheritance_test);
        app.update();

        // 查询生成的子 Task（通过 parent_task_id 过滤）
        let child_tasks: Vec<_> = app
            .world_mut()
            .query::<&Task>()
            .iter(app.world())
            .filter(|t| t.parent_task_id == Some(parent_task_id))
            .collect();

        assert_eq!(
            child_tasks.len(),
            1,
            "exactly one child task should be spawned"
        );
        assert_eq!(
            child_tasks[0].origin_channel,
            Some(telegram_channel),
            "subtask should inherit parent's Telegram channel, not Tui/default"
        );
        // 显式断言：不得回退到硬编码的 Tui/default
        assert_ne!(
            child_tasks[0].origin_channel,
            Some(ChannelId {
                frontend: FrontendKind::Tui,
                user_id: "default".to_string(),
                thread_id: None,
            }),
            "subtask channel must NOT be the hardcoded Tui/default"
        );
        // 回归保护：子任务必须注册进 EntityIndex。
        // 此前 create_tasks 裸 spawn 未注册索引，经索引查找的 system 会查不到子任务。
        let index = app.world().resource::<EntityIndex>();
        assert!(
            index.get_task(&child_tasks[0].id).is_some(),
            "subtask should be registered in EntityIndex.tasks"
        );
    }

    // ============ AskUser arm 测试 ============
    //
    // 通过 test system wrapper 调用 `handle_tool_action`，验证：
    // 1. task 切到 Waiting(AskUser)
    // 2. AskUserPending 组件挂到 task entity（先 insert 再切 status 的不变量）
    // 3. EngineEvent::Text 推送到 frontend
    // 4. request_entity 被 despawn
    // 5. 无 output_channel 时返回错误（task 不切 Waiting）

    use crate::domain::{Frontend, UserAction};
    use std::sync::{Arc, Mutex};

    /// 捕获推送事件的 mock frontend
    struct MockFrontend {
        kind: FrontendKind,
        events: Arc<Mutex<Vec<EngineEvent>>>,
    }

    impl Frontend for MockFrontend {
        fn kind(&self) -> FrontendKind {
            self.kind.clone()
        }
        fn push_event(&self, event: EngineEvent) {
            self.events.lock().unwrap().push(event);
        }
        fn poll_actions(&self) -> Vec<UserAction> {
            vec![]
        }
    }

    /// 测试 system：从 world 中取第一个 ToolExecutionRequestMessage，
    /// 调用 handle_tool_action 处理 AskUser action。
    #[allow(clippy::too_many_arguments, clippy::type_complexity)]
    fn ask_user_test_system(
        mut commands: Commands,
        mut tasks: Query<(Entity, &mut Task)>,
        agents: Query<&mut Agent>,
        chat_sessions: Query<&ChatSession>,
        mut short_term_memories: Query<&mut ShortTermMemory>,
        backend: Res<crate::systems::NativeProcessBackend>,
        clock: Res<Clock>,
        frontend_registry: Res<FrontendRegistry>,
        mut index: ResMut<EntityIndex>,
        requests: Query<(Entity, &ToolExecutionRequestMessage)>,
    ) {
        let Some((request_entity, request)) = requests.iter().next() else {
            return;
        };
        let task_entity = tasks
            .iter()
            .find(|(_, t)| t.id == request.request.task_id)
            .map(|(e, _)| e)
            .expect("task entity should exist for request");

        handle_tool_action(
            &mut commands,
            request_entity,
            task_entity,
            request,
            Ok(ToolAction::AskUser {
                question: "what is your name?".to_string(),
            }),
            &mut ToolWorldQueries {
                tasks: &mut tasks,
                agents: &agents,
                chat_sessions: &chat_sessions,
                short_term_memories: &mut short_term_memories,
            },
            &mut ToolResources {
                backend: &*backend,

                frontend_registry: &frontend_registry,
                clock: &clock,
            },
            &mut index,
        );
    }

    /// 构造一个带 output_channel 的 Task
    fn make_ask_user_task(channel: ChannelId) -> Task {
        let now = chrono::Utc::now();
        Task {
            id: crate::domain::TaskId::new(),
            content: "ask".to_string(),
            creator: crate::domain::AgentId::nil(),
            delegate: None,
            status: TaskStatus::Pending,
            pending_confirmation_id: None,
            input_summary: String::new(),
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
        }
    }

    /// 构造一个 ToolExecutionRequestMessage，关联到指定 task
    fn make_ask_user_request(
        task_id: crate::domain::TaskId,
        agent_id: crate::domain::AgentId,
    ) -> ToolExecutionRequestMessage {
        ToolExecutionRequestMessage {
            request: crate::domain::AgentExecutionRequest {
                task_id,
                agent_id,
                request_kind: crate::domain::AgentRequestKind::ToolExecution {
                    tool_name: "ask_user".to_string(),
                },
                prompt: String::new(),
                system_prompt: None,
                tools: vec![],
                conversation: None,
                work_item_id: None,
                model_override: None,
                turn_generation: None,
                vision: false,
                trusted_images: Vec::new(),
            },
            tool_name: "ask_user".to_string(),
            tool_input: serde_json::json!({"question": "what is your name?"}),
            pending_confirmation_id: None,
            tool_call_id: Some("call-1".to_string()),
            pending_confirmation_options: None,
            confirmed_once: false,
        }
    }

    /// 初始化测试所需资源（不含 frontend，由各测试单独注入）
    fn init_ask_user_world(world: &mut World) {
        world.init_resource::<EntityIndex>();
        world.insert_resource(crate::systems::NativeProcessBackend::default());
        world.insert_resource(Clock::default());
    }

    /// ask_user 成功路径：task 切到 Waiting(AskUser)，AskUserPending 挂载，
    /// 问题推送到 frontend，request 被 despawn
    #[test]
    fn ask_user_action_sets_task_to_waiting_ask_user() {
        let mut world = World::new();
        init_ask_user_world(&mut world);
        world.insert_resource(FrontendRegistry { frontends: vec![] });

        let channel = ChannelId {
            frontend: FrontendKind::Tui,
            user_id: "test".to_string(),
            thread_id: None,
        };
        let task = make_ask_user_task(channel);
        let task_id = task.id;
        let task_entity = world.spawn(task).id();

        let agent_id = crate::domain::AgentId::new();
        world.spawn(ask_user_test_agent(agent_id));

        let request = make_ask_user_request(task_id, agent_id);
        world.spawn(request);

        let mut schedule = Schedule::default();
        schedule.add_systems(ask_user_test_system);
        schedule.run(&mut world);

        let task = world
            .query::<&Task>()
            .get(&world, task_entity)
            .expect("task should exist");
        assert_eq!(
            task.status,
            TaskStatus::Waiting(WaitingReason::AskUser),
            "task should be Waiting(AskUser)"
        );

        let pending = world
            .query::<&AskUserPending>()
            .get(&world, task_entity)
            .expect("AskUserPending should be inserted on task entity");
        assert_eq!(pending.tool_call_id, "call-1");
        assert_eq!(pending.agent_id, agent_id);

        let remaining_requests = world
            .query::<&ToolExecutionRequestMessage>()
            .iter(&world)
            .count();
        assert_eq!(remaining_requests, 0, "request should be despawned");
    }

    /// ask_user 成功路径：EngineEvent::Text 推送到 frontend，内容与 target 正确
    #[test]
    fn ask_user_action_pushes_text_event_to_output_channel() {
        let mut world = World::new();
        init_ask_user_world(&mut world);

        let channel = ChannelId {
            frontend: FrontendKind::Tui,
            user_id: "alice".to_string(),
            thread_id: None,
        };
        let events = Arc::new(Mutex::new(Vec::new()));
        let mock = MockFrontend {
            kind: FrontendKind::Tui,
            events: events.clone(),
        };
        world.insert_resource(FrontendRegistry {
            frontends: vec![Box::new(mock)],
        });

        let task = make_ask_user_task(channel.clone());
        let task_id = task.id;
        let task_entity = world.spawn(task).id();

        let agent_id = crate::domain::AgentId::new();
        world.spawn(ask_user_test_agent(agent_id));

        world.spawn(make_ask_user_request(task_id, agent_id));

        let mut schedule = Schedule::default();
        schedule.add_systems(ask_user_test_system);
        schedule.run(&mut world);

        let captured = events.lock().unwrap().clone();
        assert_eq!(captured.len(), 1, "exactly one event should be pushed");
        match &captured[0] {
            EngineEvent::Text {
                target,
                role,
                content,
                task_id: evt_task_id,
            } => {
                assert_eq!(*role, MessageRole::Agent, "event role should be Agent");
                assert_eq!(content, "what is your name?");
                assert_eq!(*evt_task_id, Some(task_id));
                match target {
                    EventTarget::Directed(channels) => {
                        assert_eq!(channels.len(), 1);
                        assert_eq!(channels[0], channel);
                    }
                    other => panic!("expected Directed target, got {other:?}"),
                }
            }
            other => panic!("expected EngineEvent::Text, got {other:?}"),
        }

        // 同时验证 task 状态也切了（不变量：先 insert 再切 status）
        let task = world.query::<&Task>().get(&world, task_entity).unwrap();
        assert_eq!(task.status, TaskStatus::Waiting(WaitingReason::AskUser));
    }

    #[test]
    fn native_ask_user_keeps_original_request_id_and_target() {
        let mut world = World::new();
        init_ask_user_world(&mut world);
        let channel = ChannelId {
            frontend: FrontendKind::Client,
            user_id: "alice".into(),
            thread_id: Some("session".into()),
        };
        let events = Arc::new(Mutex::new(Vec::new()));
        world.insert_resource(FrontendRegistry {
            frontends: vec![Box::new(MockFrontend {
                kind: FrontendKind::Client,
                events: events.clone(),
            })],
        });
        let task = make_ask_user_task(channel.clone());
        let task_id = task.id;
        world.spawn(task);
        let agent_id = crate::domain::AgentId::new();
        world.spawn(ask_user_test_agent(agent_id));
        world.spawn(make_ask_user_request(task_id, agent_id));
        let mut schedule = Schedule::default();
        schedule.add_systems(ask_user_test_system);
        schedule.run(&mut world);
        let captured = events.lock().unwrap();
        assert_eq!(captured.len(), 2, "native card plus conversation text");
        let EngineEvent::QuestionPresented {
            target,
            task_id: id,
            question_id,
            question,
        } = &captured[0]
        else {
            panic!("missing structured question")
        };
        assert_eq!(*id, task_id);
        assert_eq!(question_id, "call-1");
        assert_eq!(question, "what is your name?");
        assert!(matches!(target,EventTarget::Directed(channels) if channels==&vec![channel]));
    }

    /// ask_user 无 output_channel：返回错误，task 不切 Waiting，request 不 despawn
    #[test]
    fn ask_user_action_without_output_channel_returns_error() {
        let mut world = World::new();
        init_ask_user_world(&mut world);
        world.insert_resource(FrontendRegistry { frontends: vec![] });

        // 构造一个无 output_channel 的 task（用 scheduled_task 也不行，它可能带 channel）
        // 直接用 event policy（output_channel = None）
        let now = chrono::Utc::now();
        let task_id = crate::domain::TaskId::new();
        let task = Task {
            id: task_id,
            content: "ask-no-channel".to_string(),
            creator: crate::domain::AgentId::nil(),
            delegate: None,
            status: TaskStatus::Pending,
            pending_confirmation_id: None,
            input_summary: String::new(),
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
            origin_channel: None,
            routing_policy: crate::domain::TaskRoutingPolicy::event(None, None),
            last_evaluated_turn: None,
        };
        let task_entity = world.spawn(task).id();

        let agent_id = crate::domain::AgentId::new();
        world.spawn(ask_user_test_agent(agent_id));

        world.spawn(make_ask_user_request(task_id, agent_id));

        let mut schedule = Schedule::default();
        schedule.add_systems(ask_user_test_system);
        schedule.run(&mut world);

        // task 不应切到 Waiting(AskUser)
        let task = world.query::<&Task>().get(&world, task_entity).unwrap();
        assert_ne!(
            task.status,
            TaskStatus::Waiting(WaitingReason::AskUser),
            "task must NOT be Waiting(AskUser) when no output_channel"
        );
        assert_eq!(task.status, TaskStatus::Pending, "task should stay Pending");

        // AskUserPending 不应挂载
        let pending_exists = world
            .query::<&AskUserPending>()
            .get(&world, task_entity)
            .is_ok();
        assert!(
            !pending_exists,
            "AskUserPending must NOT be inserted when no output_channel"
        );

        // 应该生成错误结果消息（spawn_tool_error 会 spawn ToolExecutionResultMessage）
        let error_results = world
            .query::<&ToolExecutionResultMessage>()
            .iter(&world)
            .count();
        assert_eq!(
            error_results, 1,
            "spawn_tool_error should produce one ToolExecutionResultMessage"
        );
    }

    /// 构造一个最小 Agent 用于 ask_user 测试
    fn ask_user_test_agent(agent_id: crate::domain::AgentId) -> Agent {
        Agent {
            id: agent_id,
            profile: crate::domain::AgentProfile {
                name: "ask-user-agent".to_string(),
                model: "test".to_string(),
            },
            capabilities: crate::domain::AgentCapabilities {
                tags: vec![],
                description: "test".to_string(),
            },
            kind: AgentKind::TaskScoped,
            parent_id: None,
            bound_task_id: None,
            tool_permissions: crate::domain::AgentToolPermissions {
                default_permission: crate::domain::ToolPermission::Allow,
                default_permission_explicit: true,
                overrides: std::collections::HashMap::new(),
            },
            system_prompt: None,
        }
    }
}
