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

/// Agent loop 控制配置（自动拉起预算、通知合并窗口、后台轮询间隔）。
#[derive(Debug, Clone, PartialEq, Eq, Resource)]
pub struct LoopControlConfig {
    /// 无用户输入时连续自动拉起的轮数上限
    pub max_auto_wakes: u32,
    /// 空闲任务唤醒的通知合并窗口（毫秒）
    pub notification_coalesce_ms: u64,
    /// 后台 shell 会话状态轮询间隔（毫秒）
    pub background_poll_ms: u64,
}

impl Default for LoopControlConfig {
    fn default() -> Self {
        Self {
            max_auto_wakes: 5,
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
}

impl InboxItem {
    pub fn received_at(&self) -> DateTime<Utc> {
        match self {
            Self::UserSteer { received_at, .. } | Self::TaskNotification { received_at, .. } => {
                *received_at
            }
        }
    }

    pub fn is_user_steer(&self) -> bool {
        matches!(self, Self::UserSteer { .. })
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
        }
    }
}

/// 把多条收件箱条目合并渲染为一条 User 消息，避免出现连续多个 User 轮次。
pub fn render_inbox_items(items: &[InboxItem]) -> String {
    let mut out = String::new();
    if items.iter().any(InboxItem::is_user_steer) {
        out.push_str(
            "[以下是用户在你执行过程中追加的输入，请结合当前进度调整后续行动，不要丢弃已完成的工作]\n",
        );
    }
    let blocks: Vec<String> = items.iter().map(InboxItem::render).collect();
    out.push_str(&blocks.join("\n"));
    out
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

    pub fn push(&mut self, item: InboxItem) {
        self.items.push_back(item);
    }

    /// 取出全部积压（take 语义，保证不重复投递）。
    pub fn drain_all(&mut self) -> Vec<InboxItem> {
        self.items.drain(..).collect()
    }

    pub fn has_user_steer(&self) -> bool {
        self.items.iter().any(InboxItem::is_user_steer)
    }

    pub fn last_received_at(&self) -> Option<DateTime<Utc>> {
        self.items.iter().map(InboxItem::received_at).max()
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
/// 由 `agent_execution_system` 登记（仅任务级 `LlmCompletion`），`stop_task_system` 中止并移除；
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
        let Some(handles) = self.by_task.remove(&task_id) else {
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
    /// `shell_start(notify_on_exit = true)` 启动的会话
    Shell { handle_id: SessionHandleId },
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

/// 无用户输入的连续自动拉起轮数，收到真实用户输入时清零。
#[derive(Debug, Clone, Copy, Default, Component)]
pub struct AutoWakeBudget {
    pub consecutive: u32,
    /// 已就"超出上限"提示过用户，避免重复推送
    pub exhausted_notified: bool,
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
        inflight.register(stopped, runtime.spawn(pending()).abort_handle());
        inflight.register(other, runtime.spawn(pending()).abort_handle());

        assert_eq!(inflight.abort_task(stopped), 1);
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
}
