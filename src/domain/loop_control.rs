//! Agent loop 控制领域类型
//!
//! 承载"不打断式"外部输入（用户 steer、后台任务通知）、回合代次（`/stop` 作废过期结果）、
//! 后台任务登记与任务进度清单。设计见
//! `docs/design/2026-10-28-loop-steering-and-background-notification-design.md`。
//!
//! 注意：`TaskInbox` 与经验治理的 `ExperienceInbox` 无关，前者只暂存尚未送达 LLM 的回合输入。

use std::collections::VecDeque;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::prelude::{Component, Entity, Resource};

use super::{SessionHandleId, TaskId};

/// 纯后台通知触发的汇报轮次；附于续轮消息及当前 Task，不进入持久化。
#[derive(Component, Debug, Clone, Copy)]
pub struct BackgroundReportTurn;

/// Agent loop 控制配置（自动拉起预算、通知合并窗口、后台轮询间隔）。
#[derive(Debug, Clone, PartialEq, Eq, Resource)]
pub struct LoopControlConfig {
    /// 滚动窗口内后台通知自动拉起的轮数上限
    pub max_auto_wakes: u32,
    /// 后台自动拉起额度的滚动窗口（秒）
    pub auto_wake_window_secs: u64,
    /// 空闲任务唤醒的通知合并窗口（毫秒）
    pub notification_coalesce_ms: u64,
    /// 后台 shell 会话状态轮询间隔（毫秒）
    pub background_poll_ms: u64,
}

impl Default for LoopControlConfig {
    fn default() -> Self {
        Self {
            max_auto_wakes: 5,
            auto_wake_window_secs: 10,
            notification_coalesce_ms: 300,
            background_poll_ms: 500,
        }
    }
}

/// 尚未送达 LLM 的外部输入（Durable：送达后成为对话历史的一部分）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum InboxItem {
    /// 任务运行中用户追加的输入
    UserSteer {
        content: String,
        received_at: DateTime<Utc>,
    },
    /// 后台任务完成通知
    TaskNotification {
        job_id: Uuid,
        source: String,
        status: String,
        summary: String,
        received_at: DateTime<Utc>,
    },
    /// 经验管理员开局预查命中的历史经验；随下一次请求送达，不单独唤醒任务。
    ExperienceHint {
        content: String,
        received_at: DateTime<Utc>,
    },
}

impl InboxItem {
    pub fn received_at(&self) -> DateTime<Utc> {
        match self {
            Self::UserSteer { received_at, .. }
            | Self::TaskNotification { received_at, .. }
            | Self::ExperienceHint { received_at, .. } => *received_at,
        }
    }

    pub fn is_user_steer(&self) -> bool {
        matches!(self, Self::UserSteer { .. })
    }

    /// 是否只随后续请求附带、不应单独唤醒空闲任务。
    pub fn is_passive(&self) -> bool {
        matches!(self, Self::ExperienceHint { .. })
    }

    /// 渲染为送达 LLM 的标签块。
    pub fn render(&self) -> String {
        match self {
            Self::UserSteer { content, .. } => {
                format!("<user_steer>\n{content}\n</user_steer>")
            }
            Self::TaskNotification {
                job_id,
                source,
                status,
                summary,
                ..
            } => format!(
                "<task-notification source=\"{source}\" job=\"{job_id}\" status=\"{status}\">\n{summary}\n</task-notification>"
            ),
            Self::ExperienceHint { content, .. } => {
                format!("<experience_hint>\n{content}\n</experience_hint>")
            }
        }
    }
}

/// 把多条收件箱条目合并渲染为一条 User 消息，避免出现连续多个 User 轮次。
pub fn render_inbox_items(items: &[InboxItem]) -> String {
    let mut out = String::new();
    if items.iter().any(InboxItem::is_user_steer) {
        out.push_str(
            "[以下是用户在你执行过程中追加的输入。请先用简短文字回应补充或追问，再结合当前进度继续行动；文字可以与工具调用同时返回。不要丢弃已完成的工作，也不要把本次回应当成结束原任务。]\n",
        );
    }
    let blocks: Vec<String> = items.iter().map(InboxItem::render).collect();
    out.push_str(&blocks.join("\n"));
    out
}

/// 仅限制 Agent 未消费的监控上下文；平台告警独立投递，不受此合并影响。
const MAX_MONITOR_CONTEXT_SESSIONS: usize = 32;
const MAX_MONITOR_CONTEXT_BYTES: usize = 2 * 1024;
const MONITOR_MERGED: &str = "[同会话较早通知已合并；完整告警以独立投递与回执为准]\n";
const MONITOR_EVICTED: &str = "[较早会话摘要已淘汰；完整告警以独立投递与回执为准]\n";
const MONITOR_PRIOR_FAILURE: &str = "[先前监控发送失败：";

/// 按 UTF-8 边界保留摘要头部，并把截断事实算入总字节上限。
fn bounded_monitor_text(text: &str, max_bytes: usize) -> String {
    const TRUNCATED: &str = "…[摘要已截断]";
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let mut end = max_bytes.saturating_sub(TRUNCATED.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{TRUNCATED}", &text[..end])
}

/// Task 实体上的回合收件箱。
///
/// push 只发生在 `inbox_ingest_system`（单一写入点）；drain 只有两个出口：
/// `context_attachment_system`（任务在循环中）与 `inbox_wake_system`（任务空闲）。
#[derive(Debug, Clone, Default, Component)]
pub struct TaskInbox {
    pub items: VecDeque<InboxItem>,
}

impl TaskInbox {
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn push(&mut self, mut item: InboxItem) {
        if let InboxItem::TaskNotification {
            job_id,
            source,
            summary,
            received_at,
            ..
        } = &mut item
            && source == "shell_output"
        {
            let mut context_note = String::new();
            let existing = self.items.iter().position(|previous| {
                matches!(previous,
                InboxItem::TaskNotification { job_id: previous_id, source, .. }
                    if previous_id == job_id && source == "shell_output")
            });
            if let Some(index) = existing
                && let Some(InboxItem::TaskNotification {
                    summary: previous_summary,
                    status: previous_status,
                    received_at: first_at,
                    ..
                }) = self.items.remove(index)
            {
                // 只替换尚未消费的同会话上下文，平台消息与交付记录保持独立。
                *received_at = (*received_at).min(first_at);
                if previous_summary.starts_with(MONITOR_PRIOR_FAILURE) {
                    if let Some(failure) = previous_summary.lines().next() {
                        context_note.push_str(&bounded_monitor_text(failure, 512));
                        context_note.push('\n');
                    }
                } else if previous_status == "failed" || previous_status.starts_with("failed:") {
                    let reason = bounded_monitor_text(&previous_summary.replace('\n', " "), 512);
                    context_note.push_str(&format!("{MONITOR_PRIOR_FAILURE}{reason}]\n"));
                }
                if previous_summary.contains(MONITOR_EVICTED) {
                    context_note.push_str(MONITOR_EVICTED);
                }
                context_note.push_str(MONITOR_MERGED);
            }
            let monitor_count = self
                .items
                .iter()
                .filter(|previous| {
                    matches!(previous,
                InboxItem::TaskNotification { source, .. } if source == "shell_output")
                })
                .count();
            if monitor_count >= MAX_MONITOR_CONTEXT_SESSIONS
                && let Some(index) = self.items.iter().position(|previous| {
                    matches!(previous,
                    InboxItem::TaskNotification { source, .. } if source == "shell_output")
                })
            {
                if let Some(previous) = self.items.remove(index) {
                    *received_at = (*received_at).min(previous.received_at());
                }
                if !context_note.contains(MONITOR_EVICTED) {
                    context_note.push_str(MONITOR_EVICTED);
                }
            }
            let available = MAX_MONITOR_CONTEXT_BYTES.saturating_sub(context_note.len());
            context_note.push_str(&bounded_monitor_text(summary, available));
            *summary = context_note;
            // 最新状态保留在 status；先前发送失败在受限摘要中单独保留。
        }
        self.items.push_back(item);
    }

    /// 取出全部积压（take 语义，保证不重复投递）。
    pub fn drain_all(&mut self) -> Vec<InboxItem> {
        self.items.drain(..).collect()
    }

    pub fn has_user_steer(&self) -> bool {
        self.items.iter().any(InboxItem::is_user_steer)
    }

    /// 需要 Agent 处理的积压条数（不含被动条目）。
    pub fn actionable_len(&self) -> usize {
        self.items.iter().filter(|item| !item.is_passive()).count()
    }

    /// 积压是否全部为被动条目（不触发空闲唤醒）。
    pub fn only_passive(&self) -> bool {
        self.items.iter().all(InboxItem::is_passive)
    }

    /// 首条积压确定合并截止时间；后续通知不能无限延后唤醒。
    pub fn first_received_at(&self) -> Option<DateTime<Utc>> {
        self.items.iter().map(InboxItem::received_at).min()
    }
}

/// 投递事件：其他系统一律 spawn 本消息，由 `inbox_ingest_system` 写入 `TaskInbox`。
#[derive(Debug, Clone, Component)]
pub struct InboxDeliveryMessage {
    pub task_id: TaskId,
    pub item: InboxItem,
}

/// 回合代次：`/stop` 时递增，过期代次的 LLM 结果在 ingest 处丢弃。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Component)]
pub struct TurnGeneration(pub u64);

/// 在飞的任务级 LLM 请求句柄（`/stop` 时中止 provider 侧 HTTP 请求）。
///
/// 由 `agent_execution_system` 登记任务级请求；中止后保留句柄到实际完成，以供关闭屏障检查。
/// 已结束的句柄在登记新请求时顺带清理。
#[derive(Debug, Default, Resource)]
pub struct InflightLlmRequests {
    by_task: std::collections::HashMap<TaskId, Vec<tokio::task::AbortHandle>>,
}

impl InflightLlmRequests {
    pub fn register(&mut self, task_id: TaskId, handle: tokio::task::AbortHandle) {
        let handles = self.by_task.entry(task_id).or_default();
        handles.retain(|h| !h.is_finished());
        handles.push(handle);
        self.by_task
            .retain(|_, hs| hs.iter().any(|h| !h.is_finished()));
    }

    /// 中止该任务全部在飞请求，返回实际中止的数量。
    pub fn abort_task(&mut self, task_id: TaskId) -> usize {
        let Some(handles) = self.by_task.get_mut(&task_id) else {
            return 0;
        };
        let mut aborted = 0;
        for handle in handles {
            if !handle.is_finished() {
                handle.abort();
                aborted += 1;
            }
        }
        aborted
    }

    pub fn in_flight(&self, task_id: TaskId) -> usize {
        self.by_task
            .get(&task_id)
            .map(|hs| hs.iter().filter(|h| !h.is_finished()).count())
            .unwrap_or(0)
    }
}

/// `/stop` 请求消息。
#[derive(Debug, Clone, Component)]
pub struct StopTaskMessage {
    pub task_id: TaskId,
}

/// 标记：任务刚被用户中断，`cancel_monitor_system` 据此取消其在飞异步工具调用。
#[derive(Debug, Clone, Copy, Default, Component)]
pub struct StopRequested;

/// 后台任务来源。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum JobSource {
    /// shell_start 的受管后台会话；静默退出也必须保留生命周期归属。
    Shell {
        handle_id: SessionHandleId,
        notify_on_exit: bool,
    },
    /// `create_tasks` 派出的子任务批次（未被 `wait_tasks` 认领时完成后通知）
    SubTaskBatch {
        batch_id: Uuid,
        task_ids: Vec<TaskId>,
    },
}

impl JobSource {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Shell { .. } => "shell",
            Self::SubTaskBatch { .. } => "subtasks",
        }
    }
}

/// 后台任务登记（独立实体，经 `JobOwner` 关系指向所属 Task 实体）。
#[derive(Debug, Clone, Component)]
pub struct BackgroundJob {
    pub id: Uuid,
    pub owner_task_id: TaskId,
    pub source: JobSource,
    pub started_at: DateTime<Utc>,
    /// 被 `wait_tasks` 显式等待时置位，完成后不再单独发通知
    pub claimed: bool,
}

/// 后台任务 → 所属 Task 实体的关系。
#[derive(Debug, Component)]
#[relationship(relationship_target = OwnedJobs)]
pub struct JobOwner(pub Entity);

/// Task 侧由 Bevy 自动维护的后台任务集合。
#[derive(Debug, Component)]
#[relationship_target(relationship = JobOwner)]
pub struct OwnedJobs(Vec<Entity>);

impl OwnedJobs {
    pub fn jobs(&self) -> &[Entity] {
        &self.0
    }
}

/// 后台自动拉起的滚动窗口；用户 steer 不占用或清空窗口额度。
#[derive(Debug, Clone, Default, Component)]
pub struct AutoWakeBudget {
    pub recent_wakes: VecDeque<DateTime<Utc>>,
    /// 每次限流期间只记录一次延期日志。
    pub exhausted_notified: bool,
}

impl AutoWakeBudget {
    /// 仅在实际自动唤醒前调用；到期额度会自行释放，不依赖下一次用户输入。
    pub fn try_record_wake(&mut self, now: DateTime<Utc>, config: &LoopControlConfig) -> bool {
        let seconds = i64::try_from(config.auto_wake_window_secs.max(1)).unwrap_or(i64::MAX);
        let window = chrono::Duration::try_seconds(seconds).unwrap_or(chrono::Duration::MAX);
        while self
            .recent_wakes
            .front()
            .is_some_and(|at| now.signed_duration_since(*at) >= window)
        {
            self.recent_wakes.pop_front();
        }
        if self.recent_wakes.len() >= config.max_auto_wakes as usize {
            return false;
        }
        self.recent_wakes.push_back(now);
        self.exhausted_notified = false;
        true
    }
}

/// 任务自然完成但仍有活跃后台任务时暂存的候选结果。
#[derive(Debug, Clone, Component)]
pub struct DeferredResult(pub String);

/// 进度清单条目状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

impl TodoStatus {
    pub fn marker(&self) -> &'static str {
        match self {
            Self::Pending => "○",
            Self::InProgress => "▶",
            Self::Completed => "✓",
        }
    }
}

/// 进度清单条目。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoItem {
    pub content: String,
    pub status: TodoStatus,
}

/// 进度清单最大条目数。
pub const MAX_TODO_ITEMS: usize = 30;

/// 校验进度清单：条目上限、内容非空、最多一项 in_progress。
pub fn validate_todos(items: &[TodoItem]) -> Result<(), String> {
    if items.len() > MAX_TODO_ITEMS {
        return Err(format!("at most {MAX_TODO_ITEMS} todos allowed"));
    }
    if items.iter().any(|t| t.content.trim().is_empty()) {
        return Err("todo content must not be empty".to_string());
    }
    let in_progress = items
        .iter()
        .filter(|t| t.status == TodoStatus::InProgress)
        .count();
    if in_progress > 1 {
        return Err("at most one todo can be in_progress".to_string());
    }
    Ok(())
}

/// 渲染清单为多行文本（前端展示与上下文回填共用）。
pub fn render_todos(items: &[TodoItem]) -> String {
    items
        .iter()
        .map(|t| format!("{} {}", t.status.marker(), t.content))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Agent 自报的进度清单（纯展示数据，见 ADR-007：任何调度系统不得读取）。
#[derive(Debug, Clone, Default, Component)]
pub struct TaskTodoList {
    pub items: Vec<TodoItem>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn steer(content: &str) -> InboxItem {
        InboxItem::UserSteer {
            content: content.to_string(),
            received_at: Utc::now(),
        }
    }

    #[test]
    fn inflight_requests_abort_only_target_task() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let mut inflight = InflightLlmRequests::default();
        let stopped = TaskId::new();
        let other = TaskId::new();
        let pending = || std::future::pending::<()>();
        let stopped_handle = runtime.spawn(pending());
        inflight.register(stopped, stopped_handle.abort_handle());
        inflight.register(other, runtime.spawn(pending()).abort_handle());

        assert_eq!(inflight.abort_task(stopped), 1);
        assert!(runtime.block_on(stopped_handle).unwrap_err().is_cancelled());
        assert_eq!(inflight.in_flight(stopped), 0);
        assert_eq!(inflight.in_flight(other), 1);
        assert_eq!(inflight.abort_task(stopped), 0, "second abort is a no-op");
    }

    #[test]
    fn drain_all_takes_items_once() {
        let mut inbox = TaskInbox::default();
        inbox.push(steer("a"));
        inbox.push(steer("b"));
        assert_eq!(inbox.drain_all().len(), 2);
        assert!(inbox.is_empty());
        assert!(inbox.drain_all().is_empty());
    }

    #[test]
    fn render_inbox_items_merges_in_order() {
        let notification = InboxItem::TaskNotification {
            job_id: Uuid::nil(),
            source: "shell".to_string(),
            status: "exited(0)".to_string(),
            summary: "done".to_string(),
            received_at: Utc::now(),
        };
        let text = render_inbox_items(&[steer("first"), notification]);
        let first = text.find("<user_steer>").unwrap();
        let second = text.find("<task-notification").unwrap();
        assert!(first < second);
        assert!(text.contains("status=\"exited(0)\""));
    }

    #[test]
    fn notification_only_render_has_no_steer_hint() {
        let notification = InboxItem::TaskNotification {
            job_id: Uuid::nil(),
            source: "shell".to_string(),
            status: "exited(0)".to_string(),
            summary: "done".to_string(),
            received_at: Utc::now(),
        };
        assert!(!render_inbox_items(&[notification]).contains("用户在你执行过程中"));
    }

    #[test]
    fn validate_todos_rejects_multiple_in_progress() {
        let items = vec![
            TodoItem {
                content: "a".to_string(),
                status: TodoStatus::InProgress,
            },
            TodoItem {
                content: "b".to_string(),
                status: TodoStatus::InProgress,
            },
        ];
        assert!(validate_todos(&items).is_err());
    }

    #[test]
    fn validate_todos_rejects_empty_content_and_overflow() {
        let empty = vec![TodoItem {
            content: "  ".to_string(),
            status: TodoStatus::Pending,
        }];
        assert!(validate_todos(&empty).is_err());

        let many: Vec<TodoItem> = (0..=MAX_TODO_ITEMS)
            .map(|i| TodoItem {
                content: format!("t{i}"),
                status: TodoStatus::Pending,
            })
            .collect();
        assert!(validate_todos(&many).is_err());
    }

    #[test]
    fn render_todos_uses_status_markers() {
        let items = vec![
            TodoItem {
                content: "done".to_string(),
                status: TodoStatus::Completed,
            },
            TodoItem {
                content: "doing".to_string(),
                status: TodoStatus::InProgress,
            },
        ];
        assert_eq!(render_todos(&items), "✓ done\n▶ doing");
    }

    fn monitor(
        job_id: Uuid,
        status: &str,
        summary: String,
        received_at: DateTime<Utc>,
    ) -> InboxItem {
        InboxItem::TaskNotification {
            job_id,
            source: "shell_output".into(),
            status: status.into(),
            summary,
            received_at,
        }
    }

    #[test]
    fn monitor_context_pressure_is_bounded_without_dropping_steer_or_other_notifications() {
        let mut inbox = TaskInbox::default();
        let user_content = "必须保留的用户约束".repeat(512);
        inbox.push(steer(&user_content));
        let other = InboxItem::TaskNotification {
            job_id: Uuid::new_v4(),
            source: "subtasks".into(),
            status: "completed".into(),
            summary: "完整子任务结果".repeat(512),
            received_at: Utc::now(),
        };
        inbox.push(other.clone());
        let first_monitor_at = Utc::now();
        for index in 1..=1000 {
            inbox.push(monitor(
                Uuid::from_u128(index),
                "delivered",
                "中文告警数据".repeat(512),
                first_monitor_at + chrono::Duration::milliseconds(index as i64),
            ));
        }
        let first_retained_at = inbox
            .items
            .iter()
            .filter_map(|item| match item {
                InboxItem::TaskNotification {
                    source,
                    received_at,
                    ..
                } if source == "shell_output" => Some(*received_at),
                _ => None,
            })
            .min()
            .unwrap();
        assert_eq!(
            first_retained_at,
            first_monitor_at + chrono::Duration::milliseconds(1),
            "eviction must not allow a continuous stream to postpone the first pending deadline"
        );
        let monitors: Vec<_> = inbox
            .items
            .iter()
            .filter_map(|item| match item {
                InboxItem::TaskNotification {
                    source, summary, ..
                } if source == "shell_output" => Some(summary),
                _ => None,
            })
            .collect();
        assert!(
            monitors.len() <= 32,
            "unconsumed monitor sessions must have a fixed context bound: {}",
            monitors.len()
        );
        assert!(
            monitors
                .iter()
                .all(|summary| summary.len() <= 2048 && summary.is_char_boundary(summary.len()))
        );
        assert!(
            monitors
                .iter()
                .any(|summary| summary.contains("较早会话摘要已淘汰")),
            "context loss must be explicit"
        );
        assert!(inbox.items.iter().any(
            |item| matches!(item, InboxItem::UserSteer { content, .. } if content == &user_content)
        ));
        assert!(inbox.items.contains(&other));
    }

    #[test]
    fn monitor_context_coalesces_same_session_without_forgetting_failure_or_first_deadline() {
        let mut inbox = TaskInbox::default();
        let now = Utc::now();
        let job = Uuid::new_v4();
        inbox.push(monitor(
            job,
            "failed: channel API error 400",
            "platform-down-reason".into(),
            now,
        ));
        inbox.push(steer("keep user input"));
        for index in 1..=1000 {
            inbox.push(monitor(
                job,
                "delivered",
                format!("latest-{index}"),
                now + chrono::Duration::milliseconds(index),
            ));
        }
        let items: Vec<_> = inbox
            .items
            .iter()
            .filter_map(|item| match item {
                InboxItem::TaskNotification {
                    job_id,
                    source,
                    status,
                    summary,
                    received_at,
                } if source == "shell_output" => Some((*job_id, status, summary, *received_at)),
                _ => None,
            })
            .collect();
        assert_eq!(
            items.len(),
            1,
            "one unconsumed summary per monitoring session"
        );
        let (id, status, summary, at) = items[0];
        assert_eq!(id, job);
        assert_eq!(status, "delivered");
        assert!(summary.contains("latest-1000"));
        assert!(
            summary.contains("platform-down-reason"),
            "a later success cannot erase prior failure evidence"
        );
        assert!(summary.contains("同会话较早通知已合并"));
        assert!(summary.len() <= 2048);
        assert_eq!(
            at, now,
            "coalescing updates cannot postpone the first pending deadline"
        );
        assert!(inbox.has_user_steer());
    }
}
