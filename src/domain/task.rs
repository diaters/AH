//! Task 相关类型定义
//!
//! 定义任务实体、状态、等待原因等。

use bevy::prelude::Component;
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

/// 任务实体
#[derive(Debug, Clone, Component, Serialize, Deserialize)]
pub struct Task {
    pub id: TaskId,
    pub content: String,
    pub creator: AgentId,
    pub delegate: Option<AgentId>,
    pub status: TaskStatus,
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
    /// 任务来源的前端通道
    pub origin_channel: ChannelId,
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

/// 标记刚生成、尚未派发 `on_tool_called` 前置 hook 的 `ToolExecutionRequestMessage`。
///
/// 由 `ToolExecutionRequestMessage` 的所有 spawn 点附带，由 companion 系统
/// `on_tool_called_hook_system` 派发 hook 后移除。若插件调用 `tool_deny` 拒绝调用，
/// companion 系统会替换请求为 `PermissionDenied` 错误结果并销毁请求 entity，
/// 不流转到 `tool_dispatch_system`。`task_input` / `pending_*` 等字段决策不受标记影响。
#[derive(Component, Debug, Clone, Default)]
pub struct ToolCalledHookPending;

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

impl Task {
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
            origin_channel: channel,
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
            origin_channel: channel,
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
    pub fn mark_done(&mut self, result: impl Into<String>, now: DateTime<Utc>) {
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
        };
        let t1 = Task::from_user_input("a", 0, ch.clone());
        assert!(t1.last_evaluated_turn.is_none());
        let t2 = Task::from_user_input_ready("b", 0, ch);
        assert!(t2.last_evaluated_turn.is_none());
    }
}
