//! 经验库：经验管理员检索、开局预查与客户端起草。
//!
//! 管理员只做判断（选哪几条 / 怎么写草稿），不持有工具；
//! 检索结果由 harness 按管理员选择逐字拼装原文后回填，主 Agent 不接触检索过程。

use serde_json::json;
use tracing::{debug, info, warn};

use crate::contracts::{FrontendRegistry, SharedExperienceLibrary};
use crate::domain::{
    Agent, AgentExecutionOutput, AgentExecutionResult, AgentExecutionResultMessage, AgentId,
    AgentKind, AgentRequestKind, DispatchHint, DispatchKind, EngineEvent, EventTarget,
    ExperienceDraftOrigin, ExperienceDraftReadyMessage, ExperienceDraftRequest, ExperienceEntry,
    ExperienceLookupOrigin, ExperienceLookupRequest, FrontendKind, InboxDeliveryMessage, InboxItem,
    NO_MATCHING_EXPERIENCE, OutputContent, PendingDispatch, ScheduledRun, SystemOutputMessage,
    Task, TaskId, TaskStatus, ToolError, ToolExecutionResultMessage, ToolReturnedHookPending,
    WaitingReason, WorkItem, WorkItemType, build_draft_prompt, build_lookup_prompt,
    librarian_index, parse_draft, parse_lookup_decision, rank_by_overlap, render_lookup_result,
};
use crate::ecs::EntityIndex;
use crate::prelude::*;

/// 经验管理员 Agent 的能力标签。
pub const LIBRARIAN_TAG: &str = "experience-librarian";
pub const SEARCH_EXPERIENCE_TOOL: &str = "search_experience";
/// 起草时附给管理员参考的相似经验条数。
const DRAFT_SIMILAR_LIMIT: usize = 3;

/// ECS 侧的经验库句柄；与客户端通道共享同一实现。
#[derive(Resource, Clone)]
pub struct ExperienceLibraryHandle(pub SharedExperienceLibrary);

fn librarian_available(agents: &Query<&Agent>) -> bool {
    agents.iter().any(|agent| {
        agent.kind == AgentKind::Persistent
            && agent
                .capabilities
                .tags
                .iter()
                .any(|tag| tag == LIBRARIAN_TAG)
    })
}

fn spawn_librarian_work_item(
    commands: &mut Commands,
    task_id: TaskId,
    work_type: WorkItemType,
    prompt: String,
    origin: impl Bundle,
) -> uuid::Uuid {
    let work_item = WorkItem::experience_librarian(task_id, work_type, prompt);
    let id = work_item.id;
    commands.spawn((
        work_item,
        origin,
        PendingDispatch {
            kind: DispatchKind::WorkItem(work_type),
            hint: DispatchHint::default(),
        },
    ));
    id
}

/// 回填 `search_experience` 工具结果。
fn spawn_search_result(
    commands: &mut Commands,
    task_id: TaskId,
    agent_id: AgentId,
    request_kind: AgentRequestKind,
    tool_call_id: &str,
    output: Result<String, String>,
) {
    let result = match &output {
        Ok(text) => Ok(AgentExecutionOutput {
            content: OutputContent::Text(text.clone()),
            reasoning_content: None,
        }),
        Err(error) => Err(crate::domain::ExecutionError::Unknown(error.clone())),
    };
    commands.spawn((
        ToolExecutionResultMessage {
            result: AgentExecutionResult {
                task_id,
                agent_id,
                request_kind,
                result,
                prompt: String::new(),
                system_prompt: None,
                tools: vec![],
                reasoning_content: None,
                work_item_id: None,
                conversation: None,
                turn_generation: None,
            },
            tool_name: SEARCH_EXPERIENCE_TOOL.to_string(),
            tool_output: output
                .map(|text| json!({ "result": text }))
                .map_err(ToolError::ExecutionFailed),
            tool_call_id: Some(tool_call_id.to_string()),
            processed: false,
            original_tool_output: None,
        },
        ToolReturnedHookPending,
    ));
}

fn reply_lookup(
    commands: &mut Commands,
    request: &ExperienceLookupRequest,
    output: Result<String, String>,
) {
    if let ExperienceLookupOrigin::ToolCall {
        agent_id,
        request_kind,
        tool_call_id,
    } = &request.origin
    {
        spawn_search_result(
            commands,
            request.task_id,
            *agent_id,
            request_kind.clone(),
            tool_call_id,
            output,
        );
    }
}

/// 开局预查：用户会话新建的顶层任务，并行请管理员按用户原话检索一次。
pub(crate) fn experience_preflight_trigger_system(
    mut commands: Commands,
    library: Option<Res<ExperienceLibraryHandle>>,
    agents: Query<&Agent>,
    tasks: Query<&Task, (Added<Task>, Without<ScheduledRun>)>,
) {
    if library.is_none() || tasks.is_empty() || !librarian_available(&agents) {
        return;
    }
    for task in &tasks {
        if task.parent_task_id.is_some()
            || task.origin_channel.is_none()
            || task.content.trim().is_empty()
        {
            continue;
        }
        debug!(
            event = "ExperiencePreflightRequested",
            task_id = %task.id,
            "requesting experience preflight lookup for new task"
        );
        commands.spawn(ExperienceLookupRequest {
            task_id: task.id,
            situation: task.content.clone(),
            origin: ExperienceLookupOrigin::Preflight,
        });
    }
}

/// 检索请求：读库、构造目录并派发管理员 WorkItem；空库直接回复，不调用模型。
pub(crate) fn experience_lookup_request_system(
    mut commands: Commands,
    library: Option<Res<ExperienceLibraryHandle>>,
    agents: Query<&Agent>,
    requests: Query<(Entity, &ExperienceLookupRequest)>,
) {
    for (entity, request) in &requests {
        commands.entity(entity).despawn();
        let Some(library) = library.as_deref().filter(|_| librarian_available(&agents)) else {
            warn!(
                event = "ExperienceLookupUnavailable",
                task_id = %request.task_id,
                error_type = "LibrarianMissing",
                error = "no persistent agent tagged experience-librarian",
                "experience lookup skipped"
            );
            reply_lookup(
                &mut commands,
                request,
                Err(format!(
                    "经验库检索不可用：未配置带 `{LIBRARIAN_TAG}` 标签的经验管理员"
                )),
            );
            continue;
        };
        let entries = match library.0.list() {
            Ok(entries) => entries,
            Err(error) => {
                warn!(
                    event = "ExperienceLibraryReadFailed",
                    task_id = %request.task_id,
                    error_type = "ExperienceLibraryIo",
                    error = %error,
                    "failed to read experience library"
                );
                reply_lookup(
                    &mut commands,
                    request,
                    Err(format!("读取经验库失败：{error}")),
                );
                continue;
            }
        };
        if entries.is_empty() {
            reply_lookup(
                &mut commands,
                request,
                Ok(NO_MATCHING_EXPERIENCE.to_string()),
            );
            continue;
        }
        let index = librarian_index(&request.situation, &entries);
        let prompt = build_lookup_prompt(&request.situation, &index);
        let work_item_id = spawn_librarian_work_item(
            &mut commands,
            request.task_id,
            WorkItemType::ExperienceLookup,
            prompt,
            request.origin.clone(),
        );
        debug!(
            event = "ExperienceLookupDispatched",
            task_id = %request.task_id,
            work_item_id = %work_item_id,
            preflight = matches!(request.origin, ExperienceLookupOrigin::Preflight),
            library_size = entries.len(),
            index_size = index.len(),
            "experience lookup work item created"
        );
    }
}

/// 起草请求：附上相似的已有经验，派发管理员 WorkItem。
pub(crate) fn experience_draft_request_system(
    mut commands: Commands,
    library: Option<Res<ExperienceLibraryHandle>>,
    agents: Query<&Agent>,
    requests: Query<(Entity, &ExperienceDraftRequest)>,
) {
    for (entity, request) in &requests {
        commands.entity(entity).despawn();
        let fail = |commands: &mut Commands, error: String| {
            commands.spawn(ExperienceDraftReadyMessage {
                channel: request.channel.clone(),
                draft_id: request.draft_id,
                outcome: Err(error),
            });
        };
        let Some(library) = library.as_deref().filter(|_| librarian_available(&agents)) else {
            fail(
                &mut commands,
                format!("未配置带 `{LIBRARIAN_TAG}` 标签的经验管理员"),
            );
            continue;
        };
        let entries = match library.0.list() {
            Ok(entries) => entries,
            Err(error) => {
                fail(&mut commands, format!("读取经验库失败：{error}"));
                continue;
            }
        };
        let query = format!(
            "{}\n{}",
            request.note.as_deref().unwrap_or_default(),
            request.transcript
        );
        let similar = rank_by_overlap(&query, &entries, DRAFT_SIMILAR_LIMIT);
        let prompt = build_draft_prompt(&request.transcript, request.note.as_deref(), &similar);
        // 起草不依附运行中的任务：会话可能早已结束或重启过。
        let work_item_id = spawn_librarian_work_item(
            &mut commands,
            TaskId::new(),
            WorkItemType::ExperienceDraft,
            prompt,
            ExperienceDraftOrigin {
                channel: request.channel.clone(),
                draft_id: request.draft_id,
            },
        );
        info!(
            event = "ExperienceDraftDispatched",
            draft_id = %request.draft_id,
            work_item_id = %work_item_id,
            transcript_chars = request.transcript.chars().count(),
            similar_count = similar.len(),
            "经验起草已派发给经验管理员"
        );
    }
}

/// 预查结果只在任务仍在推进时投递；任务已回到等待用户或终态时丢弃，避免附到无关的下一轮。
fn accepts_preflight_hint(task: &Task) -> bool {
    !task.status.is_terminal() && !matches!(task.status, TaskStatus::Waiting(WaitingReason::User))
}

fn matched_titles(
    matches: &[crate::domain::ExperienceMatch],
    entries: &[ExperienceEntry],
) -> Vec<String> {
    matches
        .iter()
        .filter_map(|m| entries.iter().find(|e| e.id == m.id))
        .map(|e| e.title.clone())
        .collect()
}

/// 管理员回复：检索结果回填工具调用或投递收件箱，起草结果交给推送系统。
///
/// 在 `llm_response_system` 之后运行；后者对经验管理员 WorkItem 直接跳过。
pub(crate) fn experience_librarian_response_system(
    mut commands: Commands,
    index: Res<EntityIndex>,
    library: Option<Res<ExperienceLibraryHandle>>,
    results: Query<(Entity, &AgentExecutionResultMessage)>,
    mut work_items: Query<(
        Entity,
        &mut WorkItem,
        Option<&ExperienceLookupOrigin>,
        Option<&ExperienceDraftOrigin>,
    )>,
    tasks: Query<&Task>,
) {
    for (result_entity, message) in &results {
        let result = &message.result;
        let Some(work_item_id) = result.work_item_id else {
            continue;
        };
        let Some((work_item_entity, mut work_item, lookup_origin, draft_origin)) = work_items
            .iter_mut()
            .find(|(_, wi, _, _)| wi.id == work_item_id)
        else {
            continue;
        };
        if !matches!(
            work_item.work_type,
            WorkItemType::ExperienceLookup | WorkItemType::ExperienceDraft
        ) {
            continue;
        }

        let reply: Result<String, String> = match &result.result {
            Ok(AgentExecutionOutput {
                content: OutputContent::Text(text),
                ..
            }) => Ok(text.clone()),
            Ok(_) => Err("经验管理员返回了工具调用而非结论".to_string()),
            Err(error) => Err(error.to_string()),
        };
        if reply.is_ok() {
            work_item.complete();
        } else {
            work_item.fail();
        }

        match (work_item.work_type, lookup_origin, draft_origin) {
            (WorkItemType::ExperienceLookup, Some(origin), _) => {
                let entries = library
                    .as_deref()
                    .map(|l| l.0.list().unwrap_or_default())
                    .unwrap_or_default();
                let decision = reply.and_then(|text| parse_lookup_decision(&text));
                let request = ExperienceLookupRequest {
                    task_id: work_item.task_id,
                    situation: String::new(),
                    origin: origin.clone(),
                };
                match &decision {
                    Ok(matches) => info!(
                        event = "ExperienceLookupCompleted",
                        task_id = %work_item.task_id,
                        preflight = matches!(origin, ExperienceLookupOrigin::Preflight),
                        matched = ?matched_titles(matches, &entries),
                        "经验检索完成"
                    ),
                    Err(error) => warn!(
                        event = "ExperienceLookupFailed",
                        task_id = %work_item.task_id,
                        error_type = "LibrarianReply",
                        error = %error,
                        "experience lookup failed"
                    ),
                }
                match origin {
                    ExperienceLookupOrigin::ToolCall { .. } => {
                        let output = decision.map(|matches| {
                            render_lookup_result(&matches, &entries)
                                .unwrap_or_else(|| NO_MATCHING_EXPERIENCE.to_string())
                        });
                        reply_lookup(&mut commands, &request, output);
                    }
                    ExperienceLookupOrigin::Preflight => {
                        let Ok(matches) = decision else {
                            commands.entity(work_item_entity).despawn();
                            commands.entity(result_entity).despawn();
                            continue;
                        };
                        let active = index
                            .get_task(&work_item.task_id)
                            .and_then(|e| tasks.get(e).ok())
                            .is_some_and(accepts_preflight_hint);
                        if let Some(content) = render_lookup_result(&matches, &entries)
                            && active
                        {
                            let titles = matched_titles(&matches, &entries);
                            commands.spawn(InboxDeliveryMessage {
                                task_id: work_item.task_id,
                                item: InboxItem::ExperienceHint {
                                    content,
                                    received_at: chrono::Utc::now(),
                                },
                            });
                            commands.spawn(SystemOutputMessage {
                                task_id: work_item.task_id,
                                content: format!("📚 已参考经验库：{}", titles.join("；")),
                            });
                        }
                    }
                }
            }
            (WorkItemType::ExperienceDraft, _, Some(origin)) => {
                let outcome = reply.and_then(|text| parse_draft(&text));
                commands.spawn(ExperienceDraftReadyMessage {
                    channel: origin.channel.clone(),
                    draft_id: origin.draft_id,
                    outcome,
                });
            }
            _ => warn!(
                event = "ExperienceLibrarianOriginMissing",
                work_item_id = %work_item.id,
                error_type = "MissingOrigin",
                error = "librarian work item has no origin component",
                "dropping librarian reply"
            ),
        }
        commands.entity(work_item_entity).despawn();
        commands.entity(result_entity).despawn();
    }
}

/// 起草结果推送到客户端会话。
pub(crate) fn experience_draft_notify_system(
    mut commands: Commands,
    registry: Option<Res<FrontendRegistry>>,
    messages: Query<(Entity, &ExperienceDraftReadyMessage)>,
) {
    for (entity, message) in &messages {
        commands.entity(entity).despawn();
        match &message.outcome {
            Ok(draft) => info!(
                event = "ExperienceDraftReady",
                draft_id = %message.draft_id,
                title = %draft.title,
                merge_target = ?draft.merge_target,
                "经验草稿已生成"
            ),
            Err(error) => warn!(
                event = "ExperienceDraftFailed",
                draft_id = %message.draft_id,
                error_type = "ExperienceDraft",
                error = %error,
                "experience draft failed"
            ),
        }
        let Some(registry) = registry.as_deref() else {
            continue;
        };
        let event = EngineEvent::ExperienceDraftUpdated {
            target: EventTarget::Directed(vec![message.channel.clone()]),
            draft_id: message.draft_id,
            outcome: message.outcome.clone(),
        };
        for frontend in registry
            .frontends
            .iter()
            .filter(|f| f.kind() == FrontendKind::Client)
        {
            frontend.push_event(event.clone());
        }
    }
}
