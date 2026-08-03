//! Task 相关类型定义
//!
//! 定义任务实体、状态、等待原因等。

use crate::prelude::Component;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};
use tracing::debug;
use uuid::Uuid;

use super::{AgentId, ChannelId, ExecutionError, FailureReason, TaskId, WaitingReason};

/// 任务状态
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TaskStatus {
    Pending,
    Ready,
    Running,
    Waiting(WaitingReason),
    Done,
    Failed(FailureReason),
}

impl TaskStatus {
    /// 判断任务是否已经到达终态。
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Done | Self::Failed(_))
    }
}

/// 定义任务的普通输出与审批输出去向。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskRoutingPolicy {
    pub output_channel: Option<ChannelId>,
    pub approval_channel: Option<ChannelId>,
    pub approval_context: Option<String>,
}

impl TaskRoutingPolicy {
    /// 构造普通聊天任务的路由策略。
    pub fn conversational(channel: ChannelId) -> Self {
        Self {
            output_channel: Some(channel.clone()),
            approval_channel: Some(channel),
            approval_context: None,
        }
    }

    /// 构造事件任务的路由策略。
    pub fn event(approval_channel: Option<ChannelId>, approval_context: Option<String>) -> Self {
        Self {
            output_channel: None,
            approval_channel,
            approval_context,
        }
    }

    /// 构造 schedule_task 动态任务的路由策略：output_channel 同时作为审批通道。
    pub fn scheduled_task(output_channel: Option<ChannelId>, approval_context: &str) -> Self {
        let approval_channel = output_channel.clone();
        Self {
            output_channel,
            approval_channel,
            approval_context: Some(approval_context.to_string()),
        }
    }
}

/// 任务实体
#[derive(Debug, Clone, Component, Serialize, Deserialize)]
pub struct Task {
    pub id: TaskId,
    pub content: String,
    pub creator: AgentId,
    pub delegate: Option<AgentId>,
    pub status: TaskStatus,
    /// 当前正在等待用户确认的工具请求 ID（仅当 status == Waiting(User) 且等待工具确认时存在）
    pub pending_confirmation_id: Option<Uuid>,
    pub input_summary: String,
    pub result_summary: String,
    pub priority: u32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub retry_count: u32,
    pub max_retries: u32,
    pub next_retry_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    /// 是否支持多轮对话
    pub multi_turn: bool,
    /// 父 Task ID（子任务回传用）
    pub parent_task_id: Option<TaskId>,
    /// 批次 ID（同一批 create_tasks 调用共享）
    pub batch_id: Option<Uuid>,
    /// 任务来源的前端通道，事件任务为 None
    pub origin_channel: Option<ChannelId>,
    /// 任务输出与审批路由策略
    pub routing_policy: TaskRoutingPolicy,
    /// 最近一次由 TurnLimitReached 触发评估时的轮数（用于同进度去重）
    pub last_evaluated_turn: Option<u32>,
}

/// 标记刚创建、尚未派发 `on_task_created` hook 的 Task entity。
///
/// 由 `user_message_to_task_system` 在创建 Task 时附带，由 companion 系统
/// `on_task_created_hook_system` 派发 hook 后移除。用户插件 hook 在创建后即可执行，
/// hook 内可通过 `get_task_ids()` / `get_task(id)` 查询刚创建的 Task。
#[derive(Component, Debug, Clone, Default)]
pub struct NewlyCreatedTask;

/// 上次观察到 Task 的状态，用于状态转换检测。
///
/// 由 `init_previous_task_status_system`（companion）在 Task 首次进入 ECS 时
/// 自动插入初值 `TaskStatus::Pending`，由 `task_termination_system` 在每次
/// `Changed<Task>` 触发后同步为当前 `Task.status`。
///
/// 用途：让 `task_termination_system` 区分"非终态→终态"的真正转换与终态内
/// 字段更新（如 `result_summary`、`updated_at` 刷新），避免重复 spawn
/// `TaskTerminatedMessage`。这是 `mark_done` 幂等化的纵深防御层。
#[derive(Component, Debug, Clone, Serialize, Deserialize)]
pub struct PreviousTaskStatus(pub TaskStatus);

/// 标记刚生成、尚未派发 `on_tool_called` 前置 hook 的 `ToolExecutionRequestMessage`。
///
/// 由 `ToolExecutionRequestMessage` 的所有 spawn 点附带，由 companion 系统
/// `on_tool_called_hook_system` 派发 hook 后移除。若插件调用 `tool_deny` 拒绝调用，
/// companion 系统会替换请求为 `PermissionDenied` 错误结果并销毁请求 entity，
/// 不流转到 `tool_dispatch_system`。`task_input` / `pending_*` 等字段决策不受标记影响。
#[derive(Component, Debug, Clone, Default)]
pub struct ToolCalledHookPending;

/// 标记刚生成、尚未派发 `on_tool_returned` 观察 hook 的 `ToolExecutionResultMessage`。
///
/// 由 `ToolExecutionResultMessage` 的所有 spawn 点附带，由 companion 系统
/// `on_tool_returned_hook_system` 派发 hook 后移除。若插件调用 `tool_set_result`，
/// companion 系统会用插件提供的值替换 `tool_output`，原始输出保留在
/// `original_tool_output` 审计字段中。`deny` 在后 hook 上无语义，若调用仅记录警告。
#[derive(Component, Debug, Clone, Default)]
pub struct ToolReturnedHookPending;

/// Task 等待其他任务完成的状态信息
/// 此组件添加到发起等待的 Task Entity 上
#[derive(Component, Debug, Clone)]
pub struct WaitingForTasksInfo {
    /// 等待的目标任务 ID 列表
    pub target_task_ids: Vec<TaskId>,
    /// 超时时刻
    pub timeout_at: DateTime<Utc>,
    /// Tool call ID（用于返回结果给 LLM）
    pub tool_call_id: String,
    /// 发起等待的 Agent ID
    pub agent_id: AgentId,
}

/// Task 等待 shell 会话完成的状态信息
/// 此组件添加到发起等待的 Task Entity 上
#[derive(Component, Debug, Clone)]
pub struct WaitingForSessionInfo {
    /// 等待的会话句柄 ID
    pub handle_id: super::SessionHandleId,
    /// 超时时刻
    pub timeout_at: DateTime<Utc>,
    /// Tool call ID（用于返回结果给 LLM）
    pub tool_call_id: String,
    /// 发起等待的 Agent ID
    pub agent_id: AgentId,
    /// 返回的输出行数
    pub return_tail_lines: usize,
}

/// Task 等待用户回复 ask_user 问题的状态信息
/// 此组件添加到发起 ask_user 的 Task Entity 上
#[derive(Component, Debug, Clone)]
pub struct AskUserPending {
    /// Tool call ID（用于返回结果给 LLM）
    pub tool_call_id: String,
    /// 发起问询的 Agent ID
    pub agent_id: AgentId,
}

impl Task {
    /// 任务结果应送达的通道：优先路由策略的 `output_channel`，回退到发起来源 `origin_channel`。
    ///
    /// 对话任务经 `from_user_input` 同时设置 `origin_channel` 与
    /// `routing_policy.output_channel`；scheduled 任务仅有 `routing_policy.output_channel`
    /// （`origin_channel` 为 None）。统一从该方法读取可保证两类任务行为一致。
    pub fn delivery_channel(&self) -> Option<&ChannelId> {
        self.routing_policy
            .output_channel
            .as_ref()
            .or(self.origin_channel.as_ref())
    }

    /// 基于用户输入创建一个处于 Pending 状态的新任务（支持多轮对话）。
    pub fn from_user_input(
        content: impl Into<String>,
        max_retries: u32,
        channel: ChannelId,
    ) -> Self {
        let content = content.into();
        let now = Utc::now();

        Self {
            id: Uuid::new_v4(),
            content: content.clone(),
            creator: Uuid::nil(),
            delegate: None,
            status: TaskStatus::Pending,
            pending_confirmation_id: None,
            input_summary: String::new(),
            result_summary: String::new(),
            priority: 0,
            created_at: now,
            updated_at: now,
            retry_count: 0,
            max_retries,
            next_retry_at: None,
            last_error: None,
            multi_turn: true,
            parent_task_id: None,
            batch_id: None,
            origin_channel: Some(channel.clone()),
            routing_policy: TaskRoutingPolicy::conversational(channel),
            last_evaluated_turn: None,
        }
    }

    /// 基于用户输入创建一个处于 Ready 状态的新任务（用于测试或单轮场景）。
    pub fn from_user_input_ready(
        content: impl Into<String>,
        max_retries: u32,
        channel: ChannelId,
    ) -> Self {
        let content = content.into();
        let now = Utc::now();

        Self {
            id: Uuid::new_v4(),
            content: content.clone(),
            creator: Uuid::nil(),
            delegate: None,
            status: TaskStatus::Ready,
            pending_confirmation_id: None,
            input_summary: content.clone(),
            result_summary: String::new(),
            priority: 0,
            created_at: now,
            updated_at: now,
            retry_count: 0,
            max_retries,
            next_retry_at: None,
            last_error: None,
            multi_turn: false,
            parent_task_id: None,
            batch_id: None,
            origin_channel: Some(channel.clone()),
            routing_policy: TaskRoutingPolicy::conversational(channel),
            last_evaluated_turn: None,
        }
    }

    /// 基于外部事件创建一个处于 Pending 状态的新任务。
    pub fn from_trigger(
        content: impl Into<String>,
        max_retries: u32,
        routing_policy: TaskRoutingPolicy,
    ) -> Self {
        let content = content.into();
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            content: content.clone(),
            creator: Uuid::nil(),
            delegate: None,
            status: TaskStatus::Pending,
            pending_confirmation_id: None,
            input_summary: String::new(),
            result_summary: String::new(),
            priority: 0,
            created_at: now,
            updated_at: now,
            retry_count: 0,
            max_retries,
            next_retry_at: None,
            last_error: None,
            multi_turn: false,
            parent_task_id: None,
            batch_id: None,
            origin_channel: None,
            routing_policy,
            last_evaluated_turn: None,
        }
    }

    /// 将任务标记为分发等待状态。
    pub fn mark_waiting_for_agent(&mut self, agent_id: AgentId, now: DateTime<Utc>) {
        let old_status = self.status.clone();
        self.delegate = Some(agent_id);
        self.status = TaskStatus::Waiting(WaitingReason::Agent);
        self.updated_at = now;
        debug!(
            event = "TaskStatusTransition",
            task_id = %self.id,
            from_status = ?old_status,
            to_status = ?self.status,
            agent_id = %agent_id,
            reason = "mark_waiting_for_agent",
            "task waiting for agent"
        );
    }

    /// 将任务标记为运行中。
    pub fn mark_running(&mut self, now: DateTime<Utc>) {
        let old_status = self.status.clone();
        self.status = TaskStatus::Running;
        self.updated_at = now;
        debug!(
            event = "TaskStatusTransition",
            task_id = %self.id,
            from_status = ?old_status,
            to_status = ?self.status,
            delegate = ?self.delegate,
            reason = "mark_running",
            "task now running"
        );
    }

    /// 在成功完成后写回结果并清理重试状态。
    ///
    /// 幂等：若 Task 已是 `Done` 状态，再次调用为 no-op，不覆盖 `result_summary`、
    /// 不更新 `updated_at`、不触发 `Changed<Task>`。这避免下游系统（如
    /// `task_termination_system`）因重复 mark_done 而误触发状态转换副作用。
    pub fn mark_done(&mut self, result: impl Into<String>, now: DateTime<Utc>) {
        if matches!(self.status, TaskStatus::Done) {
            debug!(
                event = "TaskAlreadyDone",
                task_id = %self.id,
                reason = "mark_done_noop",
                "mark_done called on already-done task, no-op"
            );
            return;
        }
        let old_status = self.status.clone();
        let result_str = result.into();
        self.result_summary = result_str.clone();
        self.status = TaskStatus::Done;
        self.updated_at = now;
        self.next_retry_at = None;
        self.last_error = None;
        debug!(
            event = "TaskStatusTransition",
            task_id = %self.id,
            from_status = ?old_status,
            to_status = ?self.status,
            result = %result_str,
            result_len = result_str.len(),
            reason = "mark_done",
            "task completed successfully"
        );
    }

    /// 根据可重试错误更新任务回退信息。
    pub fn schedule_retry(&mut self, error: &ExecutionError, now: DateTime<Utc>) {
        let old_status = self.status.clone();
        self.retry_count += 1;
        let delay = error.retry_delay(self.retry_count);
        self.next_retry_at = Some(
            now + ChronoDuration::from_std(delay).unwrap_or_else(|_| ChronoDuration::seconds(1)),
        );
        let error_msg = error.message().to_string();
        self.last_error = Some(error_msg.clone());
        self.status = TaskStatus::Waiting(WaitingReason::RetryBackoff);
        self.updated_at = now;
        debug!(
            event = "TaskStatusTransition",
            task_id = %self.id,
            from_status = ?old_status,
            to_status = ?self.status,
            retry_count = self.retry_count,
            max_retries = self.max_retries,
            error = %error_msg,
            error_type = std::any::type_name_of_val(error),
            retry_delay_secs = delay.as_secs(),
            reason = "schedule_retry",
            "task scheduled for retry"
        );
    }

    /// 将任务标记为最终失败。
    pub fn mark_failed(&mut self, error: &ExecutionError, now: DateTime<Utc>) {
        let old_status = self.status.clone();
        let error_msg = error.message().to_string();
        let failure_reason = error.to_failure_reason();
        self.last_error = Some(error_msg.clone());
        self.status = TaskStatus::Failed(failure_reason.clone());
        self.updated_at = now;
        debug!(
            event = "TaskStatusTransition",
            task_id = %self.id,
            from_status = ?old_status,
            to_status = ?self.status,
            retry_count = self.retry_count,
            max_retries = self.max_retries,
            error = %error_msg,
            error_type = std::any::type_name_of_val(error),
            failure_reason = ?failure_reason,
            reason = "mark_failed",
            "task marked as failed"
        );
    }

    /// 将任务重新置回 Ready 以进入下一次调度。
    pub fn mark_ready_for_retry(&mut self, now: DateTime<Utc>) {
        let old_status = self.status.clone();
        self.status = TaskStatus::Ready;
        self.next_retry_at = None;
        self.updated_at = now;
        debug!(
            event = "TaskStatusTransition",
            task_id = %self.id,
            from_status = ?old_status,
            to_status = ?self.status,
            retry_count = self.retry_count,
            max_retries = self.max_retries,
            reason = "mark_ready_for_retry",
            "task ready for retry"
        );
    }

    /// 记录最近一次评估对应的轮数，用于 TurnLimitReached 去重。
    pub fn record_evaluation_at_turn(&mut self, turn: u32) {
        self.last_evaluated_turn = Some(turn);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_evaluation_at_turn_sets_last_evaluated_turn() {
        let mut task = Task::from_user_input(
            "test",
            3,
            ChannelId {
                frontend: crate::domain::FrontendKind::Tui,
                user_id: "test".to_string(),
                thread_id: None,
            },
        );
        assert!(task.last_evaluated_turn.is_none());
        task.record_evaluation_at_turn(5);
        assert_eq!(task.last_evaluated_turn, Some(5));
        // 再次调用应覆盖
        task.record_evaluation_at_turn(10);
        assert_eq!(task.last_evaluated_turn, Some(10));
    }

    #[test]
    fn task_constructors_initialize_last_evaluated_turn_to_none() {
        let ch = ChannelId {
            frontend: crate::domain::FrontendKind::Tui,
            user_id: "test".to_string(),
            thread_id: None,
        };
        let t1 = Task::from_user_input("a", 0, ch.clone());
        assert!(t1.last_evaluated_turn.is_none());
        let t2 = Task::from_user_input_ready("b", 0, ch);
        assert!(t2.last_evaluated_turn.is_none());
    }

    #[test]
    fn conversational_routing_policy_targets_same_channel() {
        let channel = ChannelId {
            frontend: crate::domain::FrontendKind::Telegram,
            user_id: "u1".to_string(),
            thread_id: Some("t1".to_string()),
        };
        let policy = TaskRoutingPolicy::conversational(channel.clone());
        assert_eq!(policy.output_channel, Some(channel.clone()));
        assert_eq!(policy.approval_channel, Some(channel));
        assert!(policy.approval_context.is_none());
    }

    #[test]
    fn trigger_task_has_no_origin_channel_and_keeps_approval_route() {
        let approval_channel = ChannelId {
            frontend: crate::domain::FrontendKind::QQ,
            user_id: "reviewer".to_string(),
            thread_id: None,
        };
        let task = Task::from_trigger(
            "analyze webhook",
            3,
            TaskRoutingPolicy::event(
                Some(approval_channel.clone()),
                Some("GitHub issue opened".to_string()),
            ),
        );
        assert_eq!(task.origin_channel, None);
        assert_eq!(task.routing_policy.output_channel, None);
        assert_eq!(task.routing_policy.approval_channel, Some(approval_channel));
    }

    #[test]
    fn scheduled_task_routing_policy_approval_equals_output() {
        let channel = ChannelId {
            frontend: crate::domain::FrontendKind::Telegram,
            user_id: "chat".to_string(),
            thread_id: None,
        };
        let policy = TaskRoutingPolicy::scheduled_task(Some(channel.clone()), "scheduled task");
        assert_eq!(policy.output_channel, Some(channel.clone()));
        assert_eq!(policy.approval_channel, Some(channel));
        assert_eq!(policy.approval_context.as_deref(), Some("scheduled task"));
    }

    #[test]
    fn delivery_channel_prefers_routing_output_channel() {
        let base = ChannelId {
            frontend: crate::domain::FrontendKind::Tui,
            user_id: "base".to_string(),
            thread_id: None,
        };
        let output = ChannelId {
            frontend: crate::domain::FrontendKind::QQ,
            user_id: "group:xxx".to_string(),
            thread_id: None,
        };
        // 模拟 scheduled 任务：origin 有值但路由策略显式指定了 output_channel。
        let task = Task {
            routing_policy: TaskRoutingPolicy {
                output_channel: Some(output.clone()),
                approval_channel: None,
                approval_context: None,
            },
            ..Task::from_user_input("test", 3, base)
        };
        assert_eq!(task.delivery_channel(), Some(&output));
    }

    #[test]
    fn delivery_channel_falls_back_to_origin_channel() {
        let origin = ChannelId {
            frontend: crate::domain::FrontendKind::Tui,
            user_id: "origin".to_string(),
            thread_id: None,
        };
        // 事件任务：无 output_channel，应回退到发起来源。
        let task = Task {
            routing_policy: TaskRoutingPolicy::event(None, None),
            ..Task::from_user_input("test", 3, origin.clone())
        };
        assert_eq!(task.delivery_channel(), Some(&origin));
    }

    #[test]
    fn delivery_channel_none_when_no_channel_configured() {
        // trigger 任务：既无 output_channel 也无来源会话。
        let task = Task::from_trigger("webhook", 3, TaskRoutingPolicy::event(None, None));
        assert_eq!(task.delivery_channel(), None);
    }

    #[test]
    fn mark_done_is_idempotent_when_already_done() {
        let mut task = Task::from_user_input(
            "test",
            3,
            ChannelId {
                frontend: crate::domain::FrontendKind::Tui,
                user_id: "test".to_string(),
                thread_id: None,
            },
        );
        let t0 = chrono::Utc::now();
        task.mark_done("first result", t0);

        // 记录首次 mark_done 后的字段快照
        let snapshot_status = task.status.clone();
        let snapshot_result = task.result_summary.clone();
        let snapshot_updated_at = task.updated_at;
        let snapshot_last_error = task.last_error.clone();
        let snapshot_next_retry_at = task.next_retry_at;

        // 对已 Done 的 Task 再次调用 mark_done：应是 no-op
        let t1 = t0 + chrono::Duration::seconds(60);
        task.mark_done("second result", t1);

        assert_eq!(task.status, snapshot_status, "status must not change");
        assert_eq!(
            task.result_summary, snapshot_result,
            "result_summary must not be overwritten on already-done task"
        );
        assert_eq!(
            task.updated_at, snapshot_updated_at,
            "updated_at must not change on idempotent mark_done"
        );
        assert_eq!(
            task.last_error, snapshot_last_error,
            "last_error must not change on idempotent mark_done"
        );
        assert_eq!(
            task.next_retry_at, snapshot_next_retry_at,
            "next_retry_at must not change on idempotent mark_done"
        );
    }
}
