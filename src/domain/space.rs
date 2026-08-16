//! Space 相关 Resource 定义
//!
//! Space 是全局共享的运行时语义容器，承载非任务级的共享资源。

use std::collections::HashMap;

use crate::prelude::{Component, Resource};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{
    AgentId, ChannelId, ExperienceStore, MemoryImportance, OwnedToolContext, SessionHandleId,
    SessionInputRequest, SessionReadRequest, SessionStartRequest, SubTaskDefinition, TaskId,
    ToolEffect, ToolError,
};

/// 共享知识审核状态。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum KnowledgeValidationStatus {
    Candidate,
    Approved,
    Rejected,
    Deprecated,
}

/// 共享知识来源。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum KnowledgeSource {
    UserCommand,
    BrainReview,
    Migration,
}

/// 共享知识条目。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SharedKnowledgeEntry {
    pub content: String,
    pub kind: String,
    pub scope_tags: Vec<String>,
    pub importance: MemoryImportance,
    pub created_at: DateTime<Utc>,
    pub last_accessed_at: Option<DateTime<Utc>>,
    pub reuse_count: u32,
    pub confidence: f32,
    pub validation_status: KnowledgeValidationStatus,
    pub approved_by: Option<String>,
    pub source: KnowledgeSource,
}

impl SharedKnowledgeEntry {
    /// 创建用户显式确认的共享知识条目。
    pub fn approved_from_user_input(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            kind: "fact".to_string(),
            scope_tags: Vec::new(),
            importance: MemoryImportance::High,
            created_at: Utc::now(),
            last_accessed_at: None,
            reuse_count: 0,
            confidence: 1.0,
            validation_status: KnowledgeValidationStatus::Approved,
            approved_by: Some("user:/remember".to_string()),
            source: KnowledgeSource::UserCommand,
        }
    }

    /// 创建待审核的共享知识候选条目。
    pub fn candidate(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            kind: "fact".to_string(),
            scope_tags: Vec::new(),
            importance: MemoryImportance::Medium,
            created_at: Utc::now(),
            last_accessed_at: None,
            reuse_count: 0,
            confidence: 0.6,
            validation_status: KnowledgeValidationStatus::Candidate,
            approved_by: None,
            source: KnowledgeSource::BrainReview,
        }
    }
}

/// 全局共享知识库。
#[derive(Resource, Default)]
pub struct SharedKnowledgeBase {
    pub entries: Vec<SharedKnowledgeEntry>,
}

/// 待派发 `on_shared_knowledge_write` hook 的条目队列。
///
/// 由于 `SharedKnowledgeBase` 是 Resource 而非 Entity，无法附带 Component 标记，
/// 因此使用此 scratch resource 作为写入事件队列。写入系统将条目推入此队列，
/// companion 系统 `on_shared_knowledge_write_hook_system` 逐条派发 hook 后清空。
#[derive(Resource, Default)]
pub struct PendingKnowledgeWriteHooks(pub Vec<SharedKnowledgeEntry>);

/// 全局工具注册表
#[derive(Resource, Default)]
pub struct SpaceToolRegistry {
    tools: HashMap<String, ToolDefinition>,
}

impl SpaceToolRegistry {
    /// 注册新工具。
    pub fn register(&mut self, tool: ToolDefinition) {
        self.tools.insert(tool.name.clone(), tool);
    }

    /// 获取工具定义。
    pub fn get(&self, name: &str) -> Option<&ToolDefinition> {
        self.tools.get(name)
    }

    /// 移除指定名称的工具，返回被移除的定义。
    pub fn remove(&mut self, name: &str) -> Option<ToolDefinition> {
        self.tools.remove(name)
    }

    /// 遍历所有工具定义。
    pub fn iter(&self) -> impl Iterator<Item = &ToolDefinition> {
        self.tools.values()
    }
}

/// Tool 定义
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolDefinition {
    /// 工具名称（唯一标识）
    pub name: String,
    /// 工具描述（供 LLM 理解用途）
    pub description: String,
    /// JSON Schema 参数定义
    pub parameters: ToolSchema,
    /// 默认权限级别
    pub default_permission: ToolPermission,
    /// 执行器类型
    pub executor: ToolExecutorKind,
    /// 执行所需的最小 tag（如 "brain"）
    #[serde(default)]
    pub required_tag: Option<String>,
}

/// Tool 参数 Schema
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolSchema {
    pub schema: serde_json::Value,
}

impl Default for ToolSchema {
    fn default() -> Self {
        Self {
            schema: serde_json::json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        }
    }
}

/// Tool 执行器类型
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ToolExecutorKind {
    /// 内置执行器，由系统内注册函数实现
    Builtin(String),
    /// 外部进程执行（后续扩展）
    External { command: String, args: Vec<String> },
    /// HTTP 调用（后续扩展）
    Http { endpoint: String },
}

/// Tool 权限级别
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolPermission {
    /// 允许直接执行
    Allow,
    /// 需要用户确认
    #[default]
    Confirm,
    /// 禁止执行
    Deny,
}

/// Tool 执行动作
#[derive(Debug, Clone)]
pub enum ToolAction {
    /// 直接返回结果
    Direct(serde_json::Value),
    /// 创建子任务批次
    CreateBatch(Vec<SubTaskDefinition>),
    /// 等待子任务完成
    WaitForTasks {
        task_ids: Vec<TaskId>,
        timeout_secs: u64,
    },
    /// 阻塞执行 shell 命令
    ExecSession(SessionStartRequest),
    /// 启动后台 shell 会话
    StartSession(SessionStartRequest),
    /// 读取 shell 会话状态和最新输出快照
    ReadSession(SessionReadRequest),
    /// 列出活动 shell 会话
    ListSessions,
    /// 发送交互输入到 shell 会话
    InputSession(SessionInputRequest),
    /// 停止 shell 会话
    StopSession(SessionHandleId),
    /// 提交经验候选
    SubmitExperienceCandidate(ExperienceCandidateSubmission),
    /// 向 IM 通道发送消息
    SendChannelMessage {
        channel: String,
        target: Option<String>,
        content: String,
        attachments: Vec<crate::channels::ChannelAttachment>,
    },
    /// 开始或继续 chat_with_agent 对话轮次。
    /// executor 只负责解析参数，真正的子任务创建/更新在 orchestrator 中完成。
    StartChatRound {
        /// 目标 Persistent Agent 名称（第一轮必填，后续可用来校验）
        agent_name: Option<String>,
        /// 目标 Persistent Agent 匹配标签（agent 不存在时的备选）
        agent_tags: Vec<String>,
        /// 本轮要发送给子 Agent 的消息
        message: String,
        /// 仅在第一轮生效的额外系统上下文
        context: Option<String>,
        /// 已有对话的 handle（即子任务 task_id），不传表示开始新对话
        handle: Option<TaskId>,
    },
    /// 提交 profile 更新（孵化场景生成新 profile，更新场景提议新 tags/description）
    ///
    /// 由 profile-designer Agent 调用，实际 profile 提取与 proposal 创建
    /// 在 `profile_generation_completion_system`（任务 6）中完成。
    SubmitProfileUpdate {
        name: String,
        tags: Vec<String>,
        description: String,
    },
    /// 跳过 profile 更新（更新场景下 LLM 认为不需要更新）
    SkipProfileUpdate,
    /// 提交 skill 更新的结构化 diff 操作
    ///
    /// 由 skill-updater Agent 调用，实际的 skill 文件 apply 与 registry 刷新
    /// 在 `skill_update_completion_system` 中完成。
    ///
    /// 仅承载 LLM 能决定的 `operations` 与 `rationale`；
    /// `skill_id` / `base_version` / `new_version` 由 orchestrator 从
    /// `SkillUpdateContext` 服务端权威注入，避免 LLM 臆造 skill_id。
    SubmitSkillUpdate {
        /// 结构化 diff 操作列表
        operations: Vec<crate::domain::SkillUpdateOperation>,
        /// 本次更新的理由说明
        rationale: String,
    },
    /// 提交 skill 创建候选
    ///
    /// 由 skill-creator Agent 调用，实际的 skill 文件写入与 registry 刷新
    /// 在后续任务中完成。
    SubmitSkillCandidate { name: String, description: String },
    /// 向用户提出问题并等待开放文本回复。
    /// executor 只负责解析参数，问题呈现与等待状态由 orchestrator 完成。
    AskUser {
        /// 向用户展示的问题文本
        question: String,
    },
}

/// 经验候选提交数据
#[derive(Debug, Clone)]
pub struct ExperienceCandidateSubmission {
    pub title: String,
    pub kind: crate::domain::ExperienceKindHint,
    pub content: Option<String>,
    pub skill_description: Option<String>,
    pub instructions: Option<String>,
    pub file_refs: Vec<crate::domain::SkillFileRef>,
}

impl ExperienceCandidateSubmission {
    /// 从 JSON 工具输入构造候选提交数据。
    pub fn from_json(
        _task_id: TaskId,
        _agent_id: AgentId,
        title: &str,
        input: &serde_json::Value,
    ) -> Result<Self, ToolError> {
        let kind_str = input
            .get("kind")
            .and_then(|v| v.as_str())
            .unwrap_or("knowledge");
        let kind = match kind_str {
            "skill" => crate::domain::ExperienceKindHint::Skill,
            _ => crate::domain::ExperienceKindHint::Knowledge,
        };

        let content = input
            .get("content")
            .and_then(|v| v.as_str())
            .map(String::from);

        let skill_description = input
            .get("skill_description")
            .and_then(|v| v.as_str())
            .map(String::from);

        let instructions = input
            .get("instructions")
            .and_then(|v| v.as_str())
            .map(String::from);

        let file_refs = input
            .get("file_refs")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|item| {
                        let path = item.get("path")?.as_str()?.to_string();
                        let role_str = item.get("role").and_then(|v| v.as_str()).unwrap_or("");
                        let role = match role_str {
                            "script" => crate::domain::SkillFileRole::Script,
                            "reference" => crate::domain::SkillFileRole::Reference,
                            "asset" => crate::domain::SkillFileRole::Asset,
                            _ => {
                                // 根据扩展名推断
                                if path.ends_with(".sh") || path.ends_with(".py") {
                                    crate::domain::SkillFileRole::Script
                                } else if path.ends_with(".md") || path.ends_with(".txt") {
                                    crate::domain::SkillFileRole::Reference
                                } else {
                                    crate::domain::SkillFileRole::Asset
                                }
                            }
                        };
                        Some(crate::domain::SkillFileRef { path, role })
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok(Self {
            title: title.to_string(),
            kind,
            content,
            skill_description,
            instructions,
            file_refs,
        })
    }
}

/// 经验合并请求消息：触发 LLM 对多个相似候选做去重合并。
#[derive(Debug, Clone, Component)]
pub struct ExperienceConsolidationRequestMessage {
    pub task_id: TaskId,
    pub parent_task_id: TaskId,
    pub governing_agent_id: AgentId,
    pub candidate_kind: crate::domain::ExperienceKindHint,
    pub candidate_ids: Vec<uuid::Uuid>,
}

/// 内置 Tool 执行上下文
pub struct ToolContext<'a> {
    pub knowledge: &'a SharedKnowledgeBase,
    /// 经验候选仓库
    pub experience_store: &'a ExperienceStore,
    /// wait_tasks 工具的默认超时时间（秒）
    pub default_wait_tasks_timeout_secs: u64,
    /// shell 工具默认返回的最新输出行数
    pub shell_default_tail_lines: usize,
    /// shell 工具允许返回的最大输出行数
    pub shell_max_tail_lines: usize,
    /// shell.exec 默认超时时间（秒）
    pub shell_default_exec_timeout_secs: u64,
    /// shell.stop(wait_for_exit=true) 默认超时时间（秒）
    pub shell_default_stop_timeout_secs: u64,
    /// 异步工具桥失联超时（秒）—— sweeper 推导 max_duration 的全局缺省。
    ///
    /// 双轨期 sync 路径暂不用，但保持 ctx 与 `HarnessConfig` 对齐：
    /// 所有调用点统一从 `settings.0` 取值，避免出现「sync 路径不知道全局超时」的
    /// 二义状态。Task 2 引入异步 dispatch 后由 worker 路径真正使用。
    pub tool_inflight_timeout_secs: u64,
    /// 当前 task ID
    pub current_task_id: TaskId,
    /// 当前 agent ID
    pub current_agent_id: AgentId,
    /// 当前任务的 origin_channel，供 schedule_task 等工具继承输出通道
    pub current_origin_channel: Option<ChannelId>,
    /// ADR-006：当前 skill 更新上下文中的 skill 目录路径。
    /// 仅在 skill-updater WorkItem 执行时填充，供 read_skill_file 工具使用。
    pub current_skill_dir: Option<std::path::PathBuf>,
}

/// 工具执行模式：双轨期 dispatch 分流依据。
///
/// - `Sync`：原有同步路径，dispatch 现场直执 `execute()`
/// - `Async`：经异步桥（挂起 → worker → 通道 → ingest）
///
/// 缺省 `Sync`：现有所有 BuiltinTool 实现零改动即可编译。
/// 异步工具显式 override `kind()` 返回 `Async`，由 dispatch system 路由到 `run_async`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ToolActionKind {
    /// 同步执行（原有路径）
    #[default]
    Sync,
    /// 异步执行（经异步桥）
    Async,
}

/// worker 的执行产出：要么是给 LLM 的最终值，要么是声明式写效果（交 commit 系统落账）。
///
/// `run_async` 的成功路径返回本枚举；失败路径返回 `ToolError`。
#[derive(Debug, Clone)]
pub enum ToolWorkerOutput {
    /// 直接结果（纯读/纯计算工具）
    Value(serde_json::Value),
    /// 声明式效果（写路径工具，由 commit_tool_effects_system 应用）
    Effect(ToolEffect),
}

/// 异步工具执行的 Future 形态。
///
/// `Box<dyn Future + Send>` 让 dispatch system 可在 trait object 上泛型路由：
/// 不需要为每个异步工具单声 async fn trait。
pub type ToolFuture = std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<ToolWorkerOutput, ToolError>> + Send>,
>;

/// 内置 Tool trait
pub trait BuiltinTool: Send + Sync + 'static {
    /// 工具名称
    fn name(&self) -> &str;

    /// 执行模式，缺省 `Sync`（现有工具零改动）。
    ///
    /// dispatch system 在 `ToolActionKind::Async` 时走 `run_async`，
    /// 否则走 `execute` 同步路径。本钩子是双轨期 dispatch 分流的唯一依据。
    fn kind(&self) -> ToolActionKind {
        ToolActionKind::Sync
    }

    /// sweeper 超时推导钩子。缺省返回全局配置；shell_exec 等 override 为业务超时 + margin。
    ///
    /// 调用现场是 dispatch 挂起时（主 ECS 线程），不在 worker 内。
    /// 签名直收 `tool_inflight_timeout_secs`（全局缺省秒数）而非 `&ToolContext`：
    /// 异步 dispatch system 不持有 borrowed ctx 所需的全部资源，
    /// 把全局缺省显式传值可让 worker 路径独立推导超时。
    /// 语义不变（缺省走全局配置），只是参数从「ctx 里取」改为「显式传值」。
    fn max_duration(
        &self,
        _input: &serde_json::Value,
        tool_inflight_timeout_secs: u64,
    ) -> std::time::Duration {
        std::time::Duration::from_secs(tool_inflight_timeout_secs)
    }

    /// 异步执行入口。仅 `kind() == Async` 的工具需要 override；
    /// 缺省实现返回 `InternalState` 错误——Sync 工具误入 worker 路径时快速失败，不静默。
    ///
    /// 这是 trait 方法而非独立 trait，让 dispatch system 在 `Box<dyn BuiltinTool>`
    /// 上泛型路由（D9）：不需要为异步工具单独维护一份执行器表。
    fn run_async(&self, _input: serde_json::Value, _ctx: OwnedToolContext) -> ToolFuture {
        Box::pin(async {
            Err(ToolError::InternalState(
                "tool does not implement run_async (not migrated)".to_string(),
            ))
        })
    }

    /// 执行工具并返回动作（双轨期：仅供未迁移 sync 工具使用）。
    ///
    /// `kind() == Async` 的工具走 `run_async`，dispatch 不会调用本方法。
    /// 已上桥工具（`schedule_task` / `list_scheduled_tasks` /
    /// `delete_scheduled_task` / `shell_exec` / `list_experience_candidates` /
    /// rhai_plugin 包裹器 `RhaiPluginAsyncWrapper`）的 `execute` 应返回
    /// `ToolError::InternalState` 防御错误。新工具应实现 `run_async` 而非本方法。
    fn execute(
        &self,
        input: &serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolAction, ToolError>;
}

/// 内置 Tool 执行器注册表
#[derive(Resource, Default)]
pub struct BuiltinToolExecutors {
    executors: HashMap<String, Box<dyn BuiltinTool>>,
}

impl BuiltinToolExecutors {
    pub fn register(&mut self, executor: Box<dyn BuiltinTool>) {
        self.executors.insert(executor.name().to_string(), executor);
    }

    pub fn get(&self, name: &str) -> Option<&dyn BuiltinTool> {
        self.executors.get(name).map(|e| e.as_ref())
    }

    /// 移除指定名称的执行器，返回被移除的实例。
    pub fn remove(&mut self, name: &str) -> Option<Box<dyn BuiltinTool>> {
        self.executors.remove(name)
    }

    /// 遍历所有已注册执行器的名称。
    pub fn iter_names(&self) -> impl Iterator<Item = &str> {
        self.executors.keys().map(|s| s.as_str())
    }
}

/// Agent 的 Tool 配置（来自 agents.toml）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentToolsConfig {
    /// 未显式配置的 Tool 默认权限
    pub default_permission: Option<ToolPermission>,
    /// 针对特定 Tool 的覆盖项
    #[serde(flatten)]
    pub overrides: HashMap<String, ToolPermission>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_knowledge_entry_from_user_is_approved() {
        let entry =
            SharedKnowledgeEntry::approved_from_user_input("Project docs are written in Chinese");

        assert_eq!(entry.validation_status, KnowledgeValidationStatus::Approved);
        assert_eq!(entry.source, KnowledgeSource::UserCommand);
    }

    #[test]
    fn space_tool_registry_add_then_remove() {
        let mut registry = SpaceToolRegistry::default();
        let tool = ToolDefinition {
            name: "test_tool".to_string(),
            description: "a test tool".to_string(),
            parameters: ToolSchema::default(),
            default_permission: ToolPermission::Allow,
            executor: ToolExecutorKind::Builtin("test_tool".to_string()),
            required_tag: None,
        };
        registry.register(tool.clone());
        assert!(registry.get("test_tool").is_some());

        let removed = registry.remove("test_tool");
        assert_eq!(removed.as_ref(), Some(&tool));
        assert!(registry.get("test_tool").is_none());
    }

    #[test]
    fn space_tool_registry_remove_nonexistent_returns_none() {
        let mut registry = SpaceToolRegistry::default();
        assert!(registry.remove("no_such_tool").is_none());
    }

    #[test]
    fn builtin_tool_executors_remove_and_iter_names() {
        struct FakeTool;
        impl BuiltinTool for FakeTool {
            fn name(&self) -> &str {
                "fake"
            }
            fn execute(
                &self,
                _input: &serde_json::Value,
                _ctx: &ToolContext,
            ) -> Result<ToolAction, ToolError> {
                Err(ToolError::ExecutionFailed("not implemented".to_string()))
            }
        }

        let mut execs = BuiltinToolExecutors::default();
        execs.register(Box::new(FakeTool));
        assert!(execs.get("fake").is_some());

        let names: Vec<&str> = execs.iter_names().collect();
        assert!(names.contains(&"fake"));

        let removed = execs.remove("fake");
        assert!(removed.is_some());
        assert!(execs.get("fake").is_none());

        let names: Vec<&str> = execs.iter_names().collect();
        assert!(!names.contains(&"fake"));
    }

    #[test]
    fn builtin_tool_executors_remove_nonexistent_returns_none() {
        let mut execs = BuiltinToolExecutors::default();
        assert!(execs.remove("no_such").is_none());
    }
}
