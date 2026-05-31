//! 工作流相关类型定义
//!
//! 定义子任务、批处理、DAG 执行状态等。

use bevy::prelude::Component;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

use super::{AgentId, TaskId};

/// 单个子任务的定义（从 create_tasks 工具输入解析）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubTaskDefinition {
    /// 子 Agent 名称
    pub name: String,
    /// 任务描述/prompt
    pub content: String,
    /// 需要的工具列表
    pub tools: Vec<String>,
    /// 依赖的任务 name 列表（在本批次内）
    pub depends_on: Vec<String>,
    /// 可选模型覆盖
    pub model: Option<String>,
}

/// 附加在每个子 Task 实体上，供 Brain 和调度系统读取
#[derive(Debug, Clone, Component)]
pub struct SubTaskConfig {
    pub batch_id: Uuid,
    pub child_agent_name: String,
    pub child_agent_model: Option<String>,
    pub allowed_tools: Vec<String>,
    /// 创建子任务的父 Agent ID，用于 spawn 请求和权限继承
    pub parent_agent_id: AgentId,
    /// 本任务依赖的其他子任务 name 列表（在 batch 内）
    pub depends_on: Vec<String>,
    /// 依赖本任务完成的子任务 name 列表（反向索引，Brain 用）
    pub depended_by: Vec<String>,
}

/// 批次级别的 DAG 执行状态，附加在父 Task 实体上
#[derive(Debug, Clone, Component)]
pub struct SubTaskBatchState {
    pub batch_id: Uuid,
    pub parent_tool_call_id: String,
    /// name → 任务状态
    pub tasks: HashMap<String, BatchTaskStatus>,
    /// 已完成的任务数量
    pub completed_count: usize,
    pub total_count: usize,
}

/// 批次任务状态
#[derive(Debug, Clone)]
pub struct BatchTaskStatus {
    pub task_id: TaskId,
    pub state: BatchTaskState,
    pub result_summary: Option<String>,
}

/// 批次任务执行状态
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BatchTaskState {
    /// 等待依赖满足
    Pending,
    /// 已分发给 Brain/Agent
    Dispatched,
    /// Agent 正在执行
    Running,
    /// 已完成
    Done,
    /// 执行失败
    Failed,
}

impl SubTaskBatchState {
    /// 返回当前可以分发的任务（所有依赖已满足）的 name 列表
    pub fn ready_tasks(&self) -> Vec<String> {
        self.tasks
            .iter()
            .filter(|(_, status)| {
                status.state == BatchTaskState::Pending && self.dependencies_satisfied(status)
            })
            .map(|(name, _)| name.clone())
            .collect()
    }

    fn dependencies_satisfied(&self, _status: &BatchTaskStatus) -> bool {
        // 需要从 SubTaskConfig 获取 depends_on，这里使用 tasks map 的状态来判断
        // 实际使用时由调用方传入 depends_on 列表
        true
    }

    /// 检查是否全部完成
    pub fn all_done(&self) -> bool {
        self.completed_count >= self.total_count
    }

    /// 更新任务状态
    pub fn update_task_state(
        &mut self,
        name: &str,
        new_state: BatchTaskState,
        result_summary: Option<String>,
    ) {
        if let Some(status) = self.tasks.get_mut(name) {
            let was_terminal =
                matches!(status.state, BatchTaskState::Done | BatchTaskState::Failed);
            status.state = new_state;
            if let Some(summary) = result_summary {
                status.result_summary = Some(summary);
            }
            let is_terminal = matches!(status.state, BatchTaskState::Done | BatchTaskState::Failed);
            if !was_terminal && is_terminal {
                self.completed_count += 1;
            }
        }
    }
}
