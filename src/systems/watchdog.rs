//! Watchdog：定时驱动的外部 Control Loop。
//!
//! - `watchdog_tick_system`：每隔 `watchdog_interval_secs` 巡检一次。运行超过该时长的
//!   后台工具调用与子任务进入候选，程序侧收集进展证据（输出尾部、最近对话、todo）并与上次
//!   巡检比对，再合并为一次 watchdog Agent 判断（单个 WorkItem，无工具）。
//! - `watchdog_response_system`：落地决策。continue 不打扰任何人；adjust 把建议注入子任务；
//!   cancel / retry 经后台作业控制入口执行；notify_owner 才唤醒所属任务的 Agent。
//!
//! 主 Agent 不再需要轮询子任务或后台命令：监督过程的上下文只存在于 watchdog 的单轮请求中。
//! 未配置带 `watchdog` 标签的 Agent 时不巡检。

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use tracing::{debug, info, warn};

use crate::contracts::Clock;
use crate::domain::{
    Agent, AgentExecutionOutput, AgentExecutionResultMessage, AgentKind, BackgroundJob,
    BackgroundJobControl, BackgroundedToolCall, ConversationMessage, DispatchHint, DispatchKind,
    FailureReason, InFlightToolCall, InboxDeliveryMessage, InboxItem, JobControlAction,
    JobControlOrigin, JobSource, LoopControlConfig, OutputContent, PendingDispatch, SessionBackend,
    SessionReadRequest, SessionStatus, SystemOutputMessage, Task, TaskId, TaskStatus, TaskTodoList,
    ToolCallingState, ToolRequestPending, WATCHDOG_TAG, WatchdogAction, WatchdogReviewOrigin,
    WatchdogSubject, WatchdogSubjectKey, WatchdogSubjectKind, WorkItem, WorkItemType,
    build_watchdog_prompt, evidence_fingerprint, parse_watchdog_decisions, render_todos,
    truncate_chars,
};
use crate::ecs::EntityIndex;
use crate::prelude::*;
use crate::systems::NativeProcessBackend;
use crate::systems::demand::{DeadlineKey, DemandSchedule};

/// 证据中保留的后台命令输出行数。
const OUTPUT_TAIL_LINES: usize = 20;
/// 证据中保留的子任务最近对话条数。
const RECENT_MESSAGES: usize = 6;
/// 每个对象保留的历史决策条数。
const HISTORY_LIMIT: usize = 4;
/// 同一后台调用自动重试的上限；超过后改为通知所属任务。
const MAX_RETRIES: u32 = 2;

#[derive(Debug, Default)]
struct LedgerEntry {
    last_review_at: Option<DateTime<Utc>>,
    fingerprint: Option<u64>,
    history: Vec<String>,
    /// 巡检请求在飞：同一对象不并发判断。
    reviewing_since: Option<DateTime<Utc>>,
    retries: u32,
}

/// watchdog 的进程内账本：上次巡检时间、证据指纹与历史决策。
#[derive(Resource, Default)]
pub struct WatchdogLedger {
    entries: HashMap<WatchdogSubjectKey, LedgerEntry>,
    /// 无按需调度时（独立 system 测试）的节流时刻。
    last_tick: Option<DateTime<Utc>>,
    missing_agent_logged: bool,
}

fn watchdog_available(agents: &Query<&Agent>) -> bool {
    agents.iter().any(|agent| {
        agent.kind == AgentKind::Persistent
            && agent.capabilities.tags.iter().any(|t| t == WATCHDOG_TAG)
    })
}

fn goal_of(index: &EntityIndex, tasks: &Query<&Task>, task_id: TaskId) -> String {
    index
        .get_task(&task_id)
        .and_then(|e| tasks.get(e).ok())
        .map(|t| truncate_chars(&t.content, 400))
        .unwrap_or_default()
}

/// 后台 shell_exec 的实时输出尾部（按所属任务与命令匹配运行中的会话）。
fn shell_output_tail(
    backend: Option<&NativeProcessBackend>,
    owner: TaskId,
    input: &serde_json::Value,
) -> Option<String> {
    let backend = backend?;
    let command = input.get("command")?.as_str()?;
    let session = backend
        .list_task_sessions(owner)
        .ok()?
        .into_iter()
        .find(|s| {
            s.command == command
                && matches!(
                    s.status,
                    SessionStatus::Starting
                        | SessionStatus::Running
                        | SessionStatus::WaitingForInput
                )
        })?;
    let summary = backend
        .read_session(SessionReadRequest {
            handle_id: session.handle_id,
            tail_lines: OUTPUT_TAIL_LINES,
        })
        .ok()?;
    Some(summary.output.output)
}

fn render_message(message: &ConversationMessage) -> Option<String> {
    match message {
        ConversationMessage::System { .. } => None,
        ConversationMessage::User { content } => {
            Some(format!("[user] {}", truncate_chars(content, 300)))
        }
        ConversationMessage::Assistant {
            content,
            tool_calls,
            ..
        } => {
            let mut line = String::from("[assistant]");
            if let Some(text) = content.as_deref().filter(|t| !t.trim().is_empty()) {
                line.push(' ');
                line.push_str(&truncate_chars(text, 300));
            }
            for call in tool_calls {
                line.push_str(&format!(
                    " → {}({})",
                    call.name,
                    truncate_chars(&call.arguments, 200)
                ));
            }
            Some(line)
        }
        ConversationMessage::Tool { content, .. } => {
            Some(format!("[tool] {}", truncate_chars(content, 300)))
        }
    }
}

fn subtask_evidence(
    task: &Task,
    calling: Option<&ToolCallingState>,
    todos: Option<&TaskTodoList>,
) -> String {
    let mut evidence = format!(
        "任务内容：{}\n状态：{:?}\n",
        truncate_chars(&task.content, 600),
        task.status
    );
    if let Some(state) = calling {
        evidence.push_str(&format!(
            "工具循环：第 {}/{} 轮\n",
            state.iteration, state.max_iterations
        ));
    }
    if let Some(todos) = todos.filter(|t| !t.items.is_empty()) {
        evidence.push_str("进度清单：\n");
        evidence.push_str(&render_todos(&todos.items));
        evidence.push('\n');
    }
    if let Some(error) = task.last_error.as_deref() {
        evidence.push_str(&format!("最近错误：{}\n", truncate_chars(error, 300)));
    }
    if let Some(state) = calling {
        let recent: Vec<String> = state
            .conversation
            .iter()
            .filter_map(render_message)
            .collect();
        let skip = recent.len().saturating_sub(RECENT_MESSAGES);
        evidence.push_str(&format!("最近对话（共 {} 条）：\n", recent.len()));
        for line in &recent[skip..] {
            evidence.push_str(line);
            evidence.push('\n');
        }
    }
    evidence
}

/// 定时巡检：收集候选对象与证据，合并为一次 watchdog 判断。
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn watchdog_tick_system(
    mut commands: Commands,
    clock: Res<Clock>,
    config: Res<LoopControlConfig>,
    mut ledger: ResMut<WatchdogLedger>,
    mut schedule: Option<ResMut<DemandSchedule>>,
    agents: Query<&Agent>,
    index: Res<EntityIndex>,
    tasks: Query<&Task>,
    jobs: Query<&BackgroundJob>,
    pendings: Query<(
        &ToolRequestPending,
        &BackgroundedToolCall,
        Option<&InFlightToolCall>,
    )>,
    calling: Query<&ToolCallingState>,
    todos: Query<&TaskTodoList>,
    backend: Option<Res<NativeProcessBackend>>,
) {
    let now = clock.0;
    let supervised = jobs.iter().any(|job| {
        matches!(
            job.source,
            JobSource::ToolCall { .. } | JobSource::SubTaskBatch { .. }
        )
    });
    if config.watchdog_interval_secs == 0 || !supervised {
        ledger.entries.clear();
        if let Some(schedule) = schedule.as_deref_mut() {
            schedule.clear(DeadlineKey::Watchdog);
        }
        return;
    }
    let interval =
        chrono::Duration::seconds(i64::try_from(config.watchdog_interval_secs).unwrap_or(i64::MAX));
    match schedule.as_deref_mut() {
        Some(schedule) => {
            if !schedule.periodic_ready(DeadlineKey::Watchdog, now, interval) {
                return;
            }
        }
        None => {
            if ledger.last_tick.is_some_and(|last| now - last < interval) {
                return;
            }
            ledger.last_tick = Some(now);
        }
    }
    if !watchdog_available(&agents) {
        if !ledger.missing_agent_logged {
            ledger.missing_agent_logged = true;
            info!(
                event = "WatchdogDisabledNoAgent",
                tag = WATCHDOG_TAG,
                "no persistent agent tagged watchdog; background supervision is off"
            );
        }
        return;
    }

    let mut subjects: Vec<WatchdogSubject> = Vec::new();
    for job in &jobs {
        match &job.source {
            JobSource::ToolCall {
                tool_call_id,
                tool_name,
            } => {
                let Some((pending, _, in_flight)) = pendings
                    .iter()
                    .find(|(p, b, _)| &p.tool_call_id == tool_call_id && !b.silent)
                else {
                    continue;
                };
                let mut progress = format!(
                    "工具：{tool_name}\n入参：{}\n",
                    truncate_chars(&pending.tool_input.to_string(), 600)
                );
                match shell_output_tail(backend.as_deref(), job.owner_task_id, &pending.tool_input)
                {
                    Some(tail) if !tail.trim().is_empty() => {
                        progress.push_str(&format!(
                            "最新输出（末 {OUTPUT_TAIL_LINES} 行）：\n{tail}\n"
                        ));
                    }
                    Some(_) => progress.push_str("最新输出：（尚无输出）\n"),
                    None => progress.push_str("最新输出：（无法读取实时输出）\n"),
                }
                let mut evidence = progress.clone();
                if let Some(in_flight) = in_flight {
                    let remaining = in_flight.started_at + in_flight.timeout - now;
                    evidence.push_str(&format!(
                        "距失联超时还剩：{}s\n",
                        remaining.num_seconds().max(0)
                    ));
                }
                subjects.push(WatchdogSubject {
                    key: WatchdogSubjectKey::ToolJob(job.id),
                    kind: WatchdogSubjectKind::ToolCall {
                        tool_name: tool_name.clone(),
                    },
                    owner_task_id: job.owner_task_id,
                    goal: goal_of(&index, &tasks, job.owner_task_id),
                    elapsed_secs: (now - job.started_at).num_seconds(),
                    evidence,
                    fingerprint: evidence_fingerprint(&progress),
                    unchanged_since_last_review: false,
                    history: Vec::new(),
                });
            }
            JobSource::SubTaskBatch { task_ids, .. } => {
                for task_id in task_ids {
                    let Some(child_entity) = index.get_task(task_id) else {
                        continue;
                    };
                    let Ok(child) = tasks.get(child_entity) else {
                        continue;
                    };
                    // 未启动（等待依赖 / 派发）或已终态的子任务不在监督范围。
                    if child.status.is_terminal()
                        || matches!(child.status, TaskStatus::Pending | TaskStatus::Ready)
                    {
                        continue;
                    }
                    let state = calling
                        .iter()
                        .find(|s| s.task_id == child.id && s.work_item_id.is_none());
                    let evidence = subtask_evidence(child, state, todos.get(child_entity).ok());
                    let name = if child.input_summary.is_empty() {
                        child.id.to_string()
                    } else {
                        child.input_summary.clone()
                    };
                    subjects.push(WatchdogSubject {
                        key: WatchdogSubjectKey::SubTask(child.id),
                        kind: WatchdogSubjectKind::SubTask { name },
                        owner_task_id: job.owner_task_id,
                        goal: goal_of(&index, &tasks, job.owner_task_id),
                        elapsed_secs: (now - child.created_at).num_seconds(),
                        fingerprint: evidence_fingerprint(&evidence),
                        evidence,
                        unchanged_since_last_review: false,
                        history: Vec::new(),
                    });
                }
            }
            JobSource::Shell { .. } => {}
        }
    }

    // 账本只保留仍存在的对象。
    ledger
        .entries
        .retain(|key, _| subjects.iter().any(|s| s.key == *key));

    let stale_review = interval * 3;
    let mut selected: Vec<(String, WatchdogSubject)> = Vec::new();
    for mut subject in subjects {
        if subject.elapsed_secs < interval.num_seconds() {
            continue;
        }
        let entry = ledger.entries.entry(subject.key).or_default();
        if entry
            .reviewing_since
            .is_some_and(|since| now - since < stale_review)
        {
            continue;
        }
        // 节拍抖动容差：上次巡检距今不足半个周期则跳过本轮。
        if entry
            .last_review_at
            .is_some_and(|last| now - last < interval / 2)
        {
            continue;
        }
        subject.unchanged_since_last_review = entry.fingerprint == Some(subject.fingerprint);
        subject.history = entry.history.clone();
        entry.reviewing_since = Some(now);
        let id = format!("S{}", selected.len() + 1);
        selected.push((id, subject));
    }
    if selected.is_empty() {
        return;
    }

    let prompt = build_watchdog_prompt(&selected);
    let work_item = WorkItem::watchdog_review(TaskId::new(), prompt);
    let work_item_id = work_item.id;
    debug!(
        event = "WatchdogReviewDispatched",
        work_item_id = %work_item_id,
        subject_count = selected.len(),
        unchanged = selected.iter().filter(|(_, s)| s.unchanged_since_last_review).count(),
        "watchdog review dispatched"
    );
    commands.spawn((
        work_item,
        WatchdogReviewOrigin { subjects: selected },
        PendingDispatch {
            kind: DispatchKind::WorkItem(WorkItemType::WatchdogReview),
            hint: DispatchHint::default(),
        },
    ));
}

/// 落地 watchdog 回复。在 `llm_response_system` 之后运行；后者对巡检 WorkItem 直接跳过。
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn watchdog_response_system(
    mut commands: Commands,
    clock: Res<Clock>,
    index: Res<EntityIndex>,
    mut ledger: ResMut<WatchdogLedger>,
    results: Query<(Entity, &AgentExecutionResultMessage)>,
    work_items: Query<(Entity, &WorkItem, Option<&WatchdogReviewOrigin>)>,
    mut tasks: Query<&mut Task>,
) {
    let now = clock.0;
    for (result_entity, message) in &results {
        let result = &message.result;
        let Some(work_item_id) = result.work_item_id else {
            continue;
        };
        let Some((work_item_entity, _, origin)) = work_items.iter().find(|(_, wi, _)| {
            wi.id == work_item_id && wi.work_type == WorkItemType::WatchdogReview
        }) else {
            continue;
        };
        commands.entity(work_item_entity).despawn();
        commands.entity(result_entity).despawn();
        let Some(origin) = origin else {
            continue;
        };
        for (_, subject) in &origin.subjects {
            let entry = ledger.entries.entry(subject.key).or_default();
            entry.reviewing_since = None;
            entry.last_review_at = Some(now);
            entry.fingerprint = Some(subject.fingerprint);
        }
        let decisions = match &result.result {
            Ok(AgentExecutionOutput {
                content: OutputContent::Text(text),
                ..
            }) => parse_watchdog_decisions(text, &origin.subjects),
            Ok(_) => Err("watchdog returned tool calls instead of decisions".to_string()),
            Err(error) => Err(error.to_string()),
        };
        let decisions = match decisions {
            Ok(decisions) => decisions,
            Err(error) => {
                warn!(
                    event = "WatchdogReviewFailed",
                    work_item_id = %work_item_id,
                    error_type = "WatchdogReply",
                    error = %error,
                    "watchdog review failed; subjects will be reviewed again next interval"
                );
                continue;
            }
        };

        for (id, subject) in &origin.subjects {
            let decision = decisions.iter().find(|d| &d.subject_id == id);
            let entry = ledger.entries.entry(subject.key).or_default();
            let Some(decision) = decision else {
                push_history(entry, now, "continue", "未给出决策");
                continue;
            };
            let mut action = decision.action;
            let mut guidance = decision.guidance.clone();
            if action == WatchdogAction::Retry && entry.retries >= MAX_RETRIES {
                action = WatchdogAction::NotifyOwner;
                guidance = Some(format!(
                    "已自动重试 {} 次仍无进展，请决定是否换方案或停止。",
                    entry.retries
                ));
            }
            if action == WatchdogAction::Retry {
                entry.retries += 1;
            }
            push_history(
                entry,
                now,
                &format!("{}/{}", decision.progress, action.as_str()),
                &decision.reason,
            );
            info!(
                event = "WatchdogDecision",
                subject = ?subject.key,
                owner_task_id = %subject.owner_task_id,
                progress = %decision.progress,
                action = action.as_str(),
                reason = %decision.reason,
                "watchdog decision"
            );
            if action == WatchdogAction::Continue {
                continue;
            }
            apply_decision(
                &mut commands,
                &index,
                &mut tasks,
                now,
                subject,
                action,
                guidance.as_deref(),
                &decision.reason,
                &decision.progress,
            );
        }
    }
}

fn push_history(entry: &mut LedgerEntry, now: DateTime<Utc>, verdict: &str, reason: &str) {
    entry
        .history
        .push(format!("{} {verdict}：{reason}", now.format("%H:%M:%S")));
    if entry.history.len() > HISTORY_LIMIT {
        entry.history.remove(0);
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_decision(
    commands: &mut Commands,
    index: &EntityIndex,
    tasks: &mut Query<&mut Task>,
    now: DateTime<Utc>,
    subject: &WatchdogSubject,
    action: WatchdogAction,
    guidance: Option<&str>,
    reason: &str,
    progress: &str,
) {
    let label = subject.kind.label();
    let notice = match (action, subject.key) {
        (WatchdogAction::Cancel, WatchdogSubjectKey::ToolJob(job_id)) => {
            commands.spawn(BackgroundJobControl {
                job_id,
                action: JobControlAction::Stop,
                origin: JobControlOrigin::Watchdog {
                    reason: reason.to_string(),
                },
            });
            format!("已取消{label}：{reason}")
        }
        (WatchdogAction::Retry, WatchdogSubjectKey::ToolJob(job_id)) => {
            commands.spawn(BackgroundJobControl {
                job_id,
                action: JobControlAction::Retry,
                origin: JobControlOrigin::Watchdog {
                    reason: reason.to_string(),
                },
            });
            format!("已重试{label}：{reason}")
        }
        (WatchdogAction::NotifyOwner, WatchdogSubjectKey::ToolJob(job_id)) => {
            let guidance = guidance.unwrap_or(reason);
            commands.spawn(InboxDeliveryMessage {
                task_id: subject.owner_task_id,
                item: InboxItem::TaskNotification {
                    job_id,
                    source: "watchdog".to_string(),
                    status: progress.to_string(),
                    summary: format!(
                        "{label} 仍在后台运行，watchdog 判断需要你介入。\n建议：{guidance}\n依据：{reason}\n可调用 stop_background_job 停止该作业（job_id 同上），或继续等待结果。"
                    ),
                    received_at: now,
                },
            });
            format!("提醒处理{label}：{reason}")
        }
        (WatchdogAction::Adjust, WatchdogSubjectKey::SubTask(task_id)) => {
            let guidance = guidance.unwrap_or(reason);
            commands.spawn(InboxDeliveryMessage {
                task_id,
                item: InboxItem::WatchdogGuidance {
                    content: format!("{guidance}\n依据：{reason}"),
                    received_at: now,
                },
            });
            format!("已调整{label}的执行策略：{guidance}")
        }
        (WatchdogAction::Cancel, WatchdogSubjectKey::SubTask(task_id)) => {
            if let Some(mut child) = index.get_task(&task_id).and_then(|e| tasks.get_mut(e).ok())
                && !child.status.is_terminal()
            {
                child.mark_failed_reason(
                    FailureReason::WatchdogCancelled,
                    format!("watchdog 取消：{reason}"),
                    now,
                );
            }
            format!("已取消{label}：{reason}")
        }
        (action, key) => {
            warn!(
                event = "WatchdogActionUnsupported",
                subject = ?key,
                action = action.as_str(),
                "watchdog action not applicable to subject; ignored"
            );
            return;
        }
    };
    commands.spawn(SystemOutputMessage {
        task_id: subject.owner_task_id,
        content: format!("🐕 Watchdog {notice}"),
    });
}
