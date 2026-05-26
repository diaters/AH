//! Tool 执行相关 System
//!
//! 实现 Tool 的分发、执行和结果处理。

use bevy::prelude::*;
use tracing::{debug, warn};
use uuid::Uuid;

use crate::domain::{
    Agent, AgentExecutionOutput, AgentExecutionResult, AgentSpawnRequestMessage, ApprovalDecision,
    ApprovalRequestMessage, ApprovalResultMessage, BatchTaskState, BuiltinTool,
    BuiltinToolExecutors, ChannelId, ConfirmationOption, ConfirmationSource, ExecutionError,
    FrontendKind, GrantMode, ShortTermMemory, SpaceKnowledge, SpaceToolRegistry,
    SubTaskBatchCreatedMessage, SubTaskBatchState, SubTaskConfig, SubTaskDefinition, Task,
    TaskStatus, ToolAction, ToolCallingState, ToolConfirmationRequestMessage,
    ToolConfirmationResponseMessage, ToolContext, ToolDefinition, ToolError,
    ToolExecutionRequestMessage, ToolExecutionResultMessage, ToolPermission, WaitingReason,
};

// ========== Builtin Tool Implementations ==========

struct KnowledgeSearchTool;

impl BuiltinTool for KnowledgeSearchTool {
    fn name(&self) -> &str {
        "knowledge_search"
    }

    fn execute(
        &self,
        input: &serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolAction, ToolError> {
        let query = input
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing 'query' parameter".to_string()))?;

        let limit = input.get("limit").and_then(|v| v.as_u64()).unwrap_or(3) as usize;

        let results: Vec<&str> = ctx
            .knowledge
            .entries
            .iter()
            .filter(|entry| entry.content.to_lowercase().contains(&query.to_lowercase()))
            .take(limit)
            .map(|entry| entry.content.as_str())
            .collect();

        Ok(ToolAction::Direct(serde_json::json!({
            "query": query,
            "results": results,
            "count": results.len()
        })))
    }
}

struct SpawnAgentTool;

impl BuiltinTool for SpawnAgentTool {
    fn name(&self) -> &str {
        "spawn_agent"
    }

    fn execute(
        &self,
        input: &serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolAction, ToolError> {
        let (name, model, description, tools) = parse_spawn_agent_params(input);
        Ok(ToolAction::SpawnAgent {
            name,
            model,
            description,
            tools,
        })
    }
}

struct CreateTasksTool;

impl BuiltinTool for CreateTasksTool {
    fn name(&self) -> &str {
        "create_tasks"
    }

    fn execute(
        &self,
        input: &serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolAction, ToolError> {
        let definitions = parse_create_tasks_params(input).map_err(ToolError::InvalidInput)?;
        Ok(ToolAction::CreateBatch(definitions))
    }
}

// ========== Registration ==========

/// 注册内置 Tool
pub fn register_builtin_tools(
    registry: &mut SpaceToolRegistry,
    executors: &mut BuiltinToolExecutors,
) {
    use crate::domain::{ToolExecutorKind, ToolSchema};

    registry.register(ToolDefinition {
        name: "knowledge_search".to_string(),
        description: "Search for relevant information in the shared knowledge base. Use this when you need to access global knowledge, user preferences, or context that is not in your personal memory.".to_string(),
        parameters: ToolSchema {
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "The search query or keywords to look for"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of results to return (default: 3)",
                        "default": 3
                    }
                },
                "required": ["query"]
            }),
        },
        default_permission: ToolPermission::Allow,
        executor: ToolExecutorKind::Builtin("knowledge_search".to_string()),
        required_tag: None,
    });
    executors.register(Box::new(KnowledgeSearchTool));

    registry.register(ToolDefinition {
        name: "spawn_agent".to_string(),
        description: "Create a child agent with specified tools and capabilities. The child agent will be bound to the current task and automatically terminated when the task completes.".to_string(),
        parameters: ToolSchema {
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Name for the child agent"
                    },
                    "model": {
                        "type": "string",
                        "description": "Optional model to use. Defaults to parent agent's model."
                    },
                    "description": {
                        "type": "string",
                        "description": "Description of the child agent's capabilities"
                    },
                    "tools": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "List of tool names the child agent can use"
                    }
                },
                "required": ["name", "description", "tools"]
            }),
        },
        default_permission: ToolPermission::Allow,
        executor: ToolExecutorKind::Builtin("spawn_agent".to_string()),
        required_tag: Some("brain".to_string()),
    });
    executors.register(Box::new(SpawnAgentTool));

    registry.register(ToolDefinition {
        name: "create_tasks".to_string(),
        description: "Create sub-tasks to delegate work to specialized child agents. Supports creating multiple tasks with dependency ordering. Tasks without dependencies will run in parallel; tasks with dependencies will wait for them to complete.".to_string(),
        parameters: ToolSchema {
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "tasks": {
                        "type": "array",
                        "description": "List of sub-tasks to create",
                        "items": {
                            "type": "object",
                            "properties": {
                                "name": {
                                    "type": "string",
                                    "description": "Name for the sub-task/child agent"
                                },
                                "content": {
                                    "type": "string",
                                    "description": "Task description/prompt for the child agent"
                                },
                                "tools": {
                                    "type": "array",
                                    "items": { "type": "string" },
                                    "description": "List of tool names the child agent can use"
                                },
                                "depends_on": {
                                    "type": "array",
                                    "items": { "type": "string" },
                                    "description": "Names of other sub-tasks in this batch that must complete before this one starts"
                                },
                                "model": {
                                    "type": "string",
                                    "description": "Optional model override for the child agent"
                                }
                            },
                            "required": ["name", "content", "tools"]
                        }
                    }
                },
                "required": ["tasks"]
            }),
        },
        default_permission: ToolPermission::Allow,
        executor: ToolExecutorKind::Builtin("create_tasks".to_string()),
        required_tag: None,
    });
    executors.register(Box::new(CreateTasksTool));
}

// ========== Helpers ==========

/// 解析 spawn_agent tool 输入参数
fn parse_spawn_agent_params(
    input: &serde_json::Value,
) -> (String, Option<String>, String, Vec<String>) {
    let name = input
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("child-agent")
        .to_string();

    let model = input
        .get("model")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let description = input
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let tools: Vec<String> = input
        .get("tools")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|t| t.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    (name, model, description, tools)
}

/// 为 spawn_agent 生成 AgentSpawnRequestMessage 和 ToolExecutionResultMessage，并清理请求 entity
fn spawn_spawn_agent_messages(
    commands: &mut Commands,
    request_entity: Entity,
    agent_id: crate::domain::AgentId,
    task_id: crate::domain::TaskId,
    request_kind: crate::domain::AgentRequestKind,
    params: (String, Option<String>, String, Vec<String>),
    tool_call_id: Option<String>,
) {
    let (name, model, description, tools) = params;
    debug!(
        event = "SpawnAgentRequestCreated",
        %agent_id,
        %task_id,
        %name,
        ?model,
        %description,
        ?tools,
        ?tool_call_id,
        "spawn_agent request submitted"
    );

    commands.spawn(AgentSpawnRequestMessage {
        parent_agent_id: agent_id,
        task_id,
        name,
        model,
        description,
        tools,
        task_prompt: String::new(),
        task_system_prompt: None,
    });

    commands.spawn(ToolExecutionResultMessage {
        result: AgentExecutionResult {
            task_id,
            agent_id,
            request_kind,
            result: Ok(AgentExecutionOutput {
                content: crate::domain::OutputContent::Text(
                    "spawn_agent request submitted".to_string(),
                ),
                reasoning_content: None,
            }),
            prompt: String::new(),
            system_prompt: None,
            tools: vec![],
            reasoning_content: None,
        },
        tool_name: "spawn_agent".to_string(),
        tool_output: Ok(serde_json::json!({
            "status": "spawn_request_created"
        })),
        tool_call_id,
        processed: false,
    });

    commands.entity(request_entity).despawn();
}

/// 解析 create_tasks tool 输入参数，包含循环依赖检测
fn parse_create_tasks_params(input: &serde_json::Value) -> Result<Vec<SubTaskDefinition>, String> {
    let tasks_array = input
        .get("tasks")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "missing or invalid 'tasks' array".to_string())?;

    if tasks_array.is_empty() {
        return Err("tasks array must not be empty".to_string());
    }

    let mut definitions = Vec::new();
    let mut names = std::collections::HashSet::new();

    for task_val in tasks_array {
        let name = task_val
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "each task must have a 'name' field".to_string())?
            .to_string();

        if !names.insert(name.clone()) {
            return Err(format!("duplicate task name: '{}'", name));
        }

        let content = task_val
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("task '{}' missing 'content' field", name))?
            .to_string();

        let tools: Vec<String> = task_val
            .get("tools")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|t| t.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        let depends_on: Vec<String> = task_val
            .get("depends_on")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|d| d.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        let model = task_val
            .get("model")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        definitions.push(SubTaskDefinition {
            name,
            content,
            tools,
            depends_on,
            model,
        });
    }

    // 验证 depends_on 引用的 name 在 tasks 中存在
    for def in &definitions {
        for dep in &def.depends_on {
            if !names.contains(dep.as_str()) {
                return Err(format!(
                    "task '{}' depends_on '{}' which does not exist in this batch",
                    def.name, dep
                ));
            }
        }
    }

    // 检测循环依赖（DFS）
    detect_cycle(&definitions)?;

    Ok(definitions)
}

/// DFS 循环依赖检测
fn detect_cycle(definitions: &[SubTaskDefinition]) -> Result<(), String> {
    let name_to_idx: std::collections::HashMap<&str, usize> = definitions
        .iter()
        .enumerate()
        .map(|(i, d)| (d.name.as_str(), i))
        .collect();

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum VisitState {
        Unvisited,
        Visiting,
        Visited,
    }

    let mut states = vec![VisitState::Unvisited; definitions.len()];

    fn dfs(
        node: usize,
        states: &mut [VisitState],
        name_to_idx: &std::collections::HashMap<&str, usize>,
        definitions: &[SubTaskDefinition],
    ) -> Result<(), String> {
        states[node] = VisitState::Visiting;
        for dep in &definitions[node].depends_on {
            if let Some(&dep_idx) = name_to_idx.get(dep.as_str()) {
                match states[dep_idx] {
                    VisitState::Visiting => {
                        return Err(format!(
                            "circular dependency detected involving '{}'",
                            definitions[node].name
                        ));
                    }
                    VisitState::Unvisited => {
                        dfs(dep_idx, states, name_to_idx, definitions)?;
                    }
                    VisitState::Visited => {}
                }
            }
        }
        states[node] = VisitState::Visited;
        Ok(())
    }

    for i in 0..definitions.len() {
        if states[i] == VisitState::Unvisited {
            dfs(i, &mut states, &name_to_idx, definitions)?;
        }
    }

    Ok(())
}

/// 为 create_tasks 生成子 Task 实体、SubTaskBatchState 和消息
fn spawn_create_tasks_messages(
    commands: &mut Commands,
    request_entity: Entity,
    agent_id: crate::domain::AgentId,
    task_id: crate::domain::TaskId,
    request_kind: crate::domain::AgentRequestKind,
    definitions: Vec<SubTaskDefinition>,
    tool_call_id: Option<String>,
) {
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
        let child_task_id = Uuid::new_v4();
        let child_task = Task {
            id: child_task_id,
            content: def.content.clone(),
            creator: agent_id,
            delegate: None,
            status: TaskStatus::Pending,
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
            origin_channel: ChannelId { frontend: FrontendKind::Tui, user_id: "default".to_string() },
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

        commands.spawn((child_task, sub_task_config, ShortTermMemory::default()));

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

    // 产出 SubTaskBatchCreatedMessage（触发父 Task 阻塞 + Brain 分发）
    commands.spawn(SubTaskBatchCreatedMessage {
        parent_task_id: task_id,
        batch_id,
        parent_tool_call_id: tool_call_id.clone().unwrap_or_default(),
        tasks: definitions,
    });

    // 产出 ToolExecutionResultMessage（让 tool calling loop 收到结果）
    let task_names: Vec<String> = batch_tasks.keys().cloned().collect();
    commands.spawn(ToolExecutionResultMessage {
        result: AgentExecutionResult {
            task_id,
            agent_id,
            request_kind,
            result: Ok(AgentExecutionOutput {
                content: crate::domain::OutputContent::Text(format!(
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
        },
        tool_name: "create_tasks".to_string(),
        tool_output: Ok(serde_json::json!({
            "status": "batch_created",
            "batch_id": batch_id.to_string(),
            "task_count": total_count,
            "tasks": task_names,
        })),
        tool_call_id,
        processed: false,
    });

    commands.entity(request_entity).despawn();
}

/// 统一处理 Tool 执行动作
fn handle_tool_action(
    commands: &mut Commands,
    request_entity: Entity,
    request: &ToolExecutionRequestMessage,
    action: Result<ToolAction, ToolError>,
) {
    match action {
        Ok(ToolAction::Direct(value)) => {
            let execution_result = AgentExecutionResult {
                task_id: request.request.task_id,
                agent_id: request.request.agent_id,
                request_kind: request.request.request_kind.clone(),
                result: Ok(AgentExecutionOutput {
                    content: crate::domain::OutputContent::Text("tool executed".to_string()),
                    reasoning_content: None,
                }),
                prompt: String::new(),
                system_prompt: None,
                tools: vec![],
                reasoning_content: None,
            };

            commands.spawn(ToolExecutionResultMessage {
                result: execution_result,
                tool_name: request.tool_name.clone(),
                tool_output: Ok(value),
                tool_call_id: request.tool_call_id.clone(),
                processed: false,
            });

            commands.entity(request_entity).despawn();
        }
        Ok(ToolAction::SpawnAgent {
            name,
            model,
            description,
            tools,
        }) => {
            spawn_spawn_agent_messages(
                commands,
                request_entity,
                request.request.agent_id,
                request.request.task_id,
                request.request.request_kind.clone(),
                (name, model, description, tools),
                request.tool_call_id.clone(),
            );
        }
        Ok(ToolAction::CreateBatch(definitions)) => {
            spawn_create_tasks_messages(
                commands,
                request_entity,
                request.request.agent_id,
                request.request.task_id,
                request.request.request_kind.clone(),
                definitions,
                request.tool_call_id.clone(),
            );
        }
        Err(e) => {
            spawn_tool_error(commands, request_entity, request, e);
        }
    }
}

/// 恢复 Task 状态（从 Waiting 恢复到 Ready 或 Waiting(ToolExecution)）
fn restore_task_after_tool(
    tasks: &mut Query<&mut Task>,
    calling_states: &Query<&ToolCallingState>,
    task_id: crate::domain::TaskId,
) {
    if let Some(mut task) = tasks.iter_mut().find(|t| t.id == task_id) {
        if !matches!(task.status, TaskStatus::Waiting(_)) {
            return;
        }
        let has_calling_state = calling_states.iter().any(|cs| cs.task_id == task.id);
        task.status = if has_calling_state {
            TaskStatus::Waiting(WaitingReason::ToolExecution)
        } else {
            TaskStatus::Ready
        };
    }
}

/// 生成 Tool 错误结果
fn spawn_tool_error(
    commands: &mut Commands,
    request_entity: Entity,
    request: &ToolExecutionRequestMessage,
    error: ToolError,
) {
    let execution_result = AgentExecutionResult {
        task_id: request.request.task_id,
        agent_id: request.request.agent_id,
        request_kind: request.request.request_kind.clone(),
        result: Err(ExecutionError::Unknown(error.to_string())),
        prompt: String::new(),
        system_prompt: None,
        tools: vec![],
        reasoning_content: None,
    };

    commands.spawn(ToolExecutionResultMessage {
        result: execution_result,
        tool_name: request.tool_name.clone(),
        tool_output: Err(error),
        tool_call_id: request.tool_call_id.clone(),
        processed: false,
    });

    commands.entity(request_entity).despawn();
}

// ========== Systems ==========

/// Tool 分发 System
///
/// 检查 Tool 权限并决定直接执行、用户确认或父 Agent 审批
pub(crate) fn tool_dispatch_system(
    mut commands: Commands,
    mut tasks: Query<&mut Task>,
    registry: Res<SpaceToolRegistry>,
    executors: Res<BuiltinToolExecutors>,
    knowledge: Res<SpaceKnowledge>,
    agents: Query<&Agent>,
    mut requests: Query<(Entity, &mut ToolExecutionRequestMessage)>,
) {
    for (entity, mut request) in &mut requests {
        // 跳过已经在等待确认的请求
        if request.pending_confirmation_id.is_some() {
            continue;
        }

        let tool_name = request.tool_name.clone();

        // 查找 Tool 定义
        let Some(tool_def) = registry.get(&tool_name) else {
            warn!(
                event = "ToolNotFound",
                tool_name = %tool_name,
                task_id = %request.request.task_id,
                agent_id = %request.request.agent_id,
                "tool not found in registry"
            );
            spawn_tool_error(
                &mut commands,
                entity,
                &request,
                ToolError::NotFound(tool_name.clone()),
            );
            continue;
        };

        // 获取 Agent 权限
        let Some(agent) = agents.iter().find(|a| a.id == request.request.agent_id) else {
            warn!(
                event = "AgentNotFound",
                agent_id = %request.request.agent_id,
                tool_name = %tool_name,
                "agent not found for tool execution"
            );
            spawn_tool_error(
                &mut commands,
                entity,
                &request,
                ToolError::NotFound(format!("agent {}", request.request.agent_id)),
            );
            continue;
        };

        // 检查 required_tag
        if let Some(required_tag) = &tool_def.required_tag
            && !agent.capabilities.tags.iter().any(|t| t == required_tag)
        {
            warn!(
                event = "ToolTagDenied",
                tool_name = %tool_name,
                agent_id = %agent.id,
                agent_name = %agent.profile.name,
                required_tag = %required_tag,
                "agent lacks required tag for tool"
            );
            spawn_tool_error(
                &mut commands,
                entity,
                &request,
                ToolError::PermissionDenied(format!(
                    "tool '{}' requires tag '{}'",
                    tool_name, required_tag
                )),
            );
            continue;
        }

        let permission = agent.tool_permissions.get_permission(&tool_name);

        debug!(
            event = "ToolDispatch",
            tool_name = %tool_name,
            agent_id = %agent.id,
            agent_name = %agent.profile.name,
            permission = ?permission,
            tool_input = ?request.tool_input,
            task_id = %request.request.task_id,
            "tool execution decision"
        );

        match permission {
            ToolPermission::Allow => {
                // 直接执行
                let Some(executor) = executors.get(&tool_name) else {
                    warn!(
                        event = "ToolExecutorNotFound",
                        tool_name = %tool_name,
                        "no executor registered for tool"
                    );
                    spawn_tool_error(
                        &mut commands,
                        entity,
                        &request,
                        ToolError::NotFound(format!("executor for {}", tool_name)),
                    );
                    continue;
                };

                debug!(
                    event = "ToolExecutionAllowed",
                    tool_name = %tool_name,
                    agent_id = %agent.id,
                    "tool execution allowed"
                );

                let ctx = ToolContext {
                    knowledge: &knowledge,
                };
                let action = executor.execute(&request.tool_input, &ctx);
                handle_tool_action(&mut commands, entity, &request, action);
            }
            ToolPermission::Confirm => {
                // 检查 Agent 是否有父 Agent，且父 Agent 有该工具的 Allow 权限
                if let Some(parent_id) = agent.parent_id
                    && let Some(parent) = agents.iter().find(|a| a.id == parent_id)
                    && parent.has_permission(&tool_name)
                {
                    debug!(
                        event = "ToolRequiresParentApproval",
                        tool_name = %tool_name,
                        agent_id = %agent.id,
                        parent_agent_id = %parent.id,
                        reason = "parent agent has permission",
                        "tool requires parent agent approval"
                    );

                    // 将 Task 设置为等待父 Agent 审批状态
                    if let Some(mut task) =
                        tasks.iter_mut().find(|t| t.id == request.request.task_id)
                    {
                        task.status = TaskStatus::Waiting(WaitingReason::Approval);
                    }

                    // 生成父 Agent 审批请求消息
                    let request_id = Uuid::new_v4();
                    commands.spawn(ApprovalRequestMessage {
                        request_id,
                        tool_name: tool_name.clone(),
                        source_task_id: request.request.task_id,
                        parent_agent_id: parent.id,
                        child_agent_id: agent.id,
                        tool_input: request.tool_input.clone(),
                        approval_task_id: Uuid::new_v4(),
                        context: String::new(),
                    });

                    request.pending_confirmation_id = Some(request_id);
                    continue;
                }

                // 无父 Agent 或父 Agent 无权限 → 用户确认
                debug!(
                    event = "ToolRequiresUserConfirmation",
                    tool_name = %tool_name,
                    agent_id = %agent.id,
                    reason = "no parent agent or parent lacks permission",
                    "tool requires user confirmation"
                );

                // 将 Task 设置为等待用户确认状态
                if let Some(mut task) = tasks.iter_mut().find(|t| t.id == request.request.task_id) {
                    task.status = TaskStatus::Waiting(WaitingReason::User);
                }

                // 生成用户确认请求消息
                let request_id = Uuid::new_v4();
                let options = ConfirmationOption::default_options();
                commands.spawn(ToolConfirmationRequestMessage {
                    request_id,
                    task_id: request.request.task_id,
                    agent_id: agent.id,
                    tool_name: tool_name.clone(),
                    tool_input: request.tool_input.clone(),
                    options: options.clone(),
                    source: ConfirmationSource::User,
                    parent_agent_id: None,
                });

                request.pending_confirmation_id = Some(request_id);
                request.pending_confirmation_options = Some(options);
            }
            ToolPermission::Deny => {
                // 拒绝执行
                warn!(
                    event = "ToolExecutionDenied",
                    tool_name = %tool_name,
                    agent_id = %agent.id,
                    "tool execution denied"
                );
                spawn_tool_error(
                    &mut commands,
                    entity,
                    &request,
                    ToolError::PermissionDenied(tool_name.clone()),
                );
            }
        }
    }
}

/// Tool 结果处理 System
///
/// 处理 Tool 执行结果，记录 ToolCall，恢复原 Task。
/// 当 ToolCallingState 存在时保留 ToolExecutionResultMessage，由 orchestrator 清理。
pub(crate) fn tool_result_system(
    mut commands: Commands,
    clock: Res<crate::app::Clock>,
    mut results: Query<(Entity, &mut ToolExecutionResultMessage)>,
    mut tasks: Query<(&Task, Option<&mut ShortTermMemory>)>,
    calling_states: Query<&ToolCallingState>,
) {
    for (entity, mut result) in &mut results {
        if result.processed {
            continue;
        }

        // 查找对应的 Task 及其 ShortTermMemory
        let mut found_task = false;
        for (task, short_term_memory) in &mut tasks {
            if task.id != result.result.task_id {
                continue;
            }
            found_task = true;

            match &result.tool_output {
                Ok(output) => {
                    let output_str =
                        serde_json::to_string(output).unwrap_or_else(|_| output.to_string());
                    debug!(
                        event = "ToolExecuted",
                        tool_name = %result.tool_name,
                        task_id = %task.id,
                        agent_id = %result.result.agent_id,
                        success = true,
                        output = %output_str,
                        output_len = output_str.len(),
                        "tool execution completed"
                    );

                    // 记录 ToolCall 到 ShortTermMemory
                    if let Some(mut stm) = short_term_memory {
                        stm.record_tool_call(
                            result.tool_call_id.clone(),
                            result.tool_name.clone(),
                            serde_json::to_string(output).unwrap_or_default(),
                            output_str,
                            clock.0,
                        );
                    }
                }
                Err(e) => {
                    warn!(
                        event = "ToolExecutionFailed",
                        tool_name = %result.tool_name,
                        task_id = %task.id,
                        agent_id = %result.result.agent_id,
                        success = false,
                        error = %e,
                        "tool execution failed"
                    );
                }
            }
            break;
        }

        if !found_task {
            warn!(
                event = "ToolResultTaskNotFound",
                task_id = %result.result.task_id,
                tool_name = %result.tool_name,
                "tool result has no matching task"
            );
        }

        // Mark as processed to prevent re-handling on subsequent frames
        result.processed = true;

        // Only despawn if no ToolCallingState is tracking this result
        let should_keep = result.tool_call_id.as_ref().is_some_and(|call_id| {
            calling_states
                .iter()
                .any(|s| s.pending_tool_call_ids.contains(call_id))
        });
        if !should_keep {
            commands.entity(entity).despawn();
        }
    }
}

/// 审批分发 System
///
/// 为需要父 Agent 决策的请求创建审批任务。
/// MVP 阶段：父 Agent 审批默认自动通过。
pub(crate) fn approval_dispatch_system(
    mut commands: Commands,
    tasks: Query<&Task>,
    approval_requests: Query<(Entity, &ApprovalRequestMessage)>,
) {
    for (entity, request) in &approval_requests {
        debug!(
            event = "ApprovalRequestReceived",
            request_id = %request.request_id,
            tool_name = %request.tool_name,
            source_task_id = %request.source_task_id,
            parent_agent_id = %request.parent_agent_id,
            child_agent_id = %request.child_agent_id,
            tool_input = ?request.tool_input,
            "approval request received - auto-approving in MVP"
        );

        // 记录原 Task 状态
        if let Some(task) = tasks.iter().find(|t| t.id == request.source_task_id)
            && task.status == TaskStatus::Waiting(WaitingReason::Approval)
        {
            debug!(
                event = "SourceTaskWaiting",
                task_id = %task.id,
                "source task is waiting for approval"
            );
        }

        // 生成自动批准结果
        commands.spawn(ApprovalResultMessage {
            request_id: request.request_id,
            source_task_id: request.source_task_id,
            approval_task_id: request.approval_task_id,
            decision: ApprovalDecision::Approved,
            reasoning: "MVP auto-approve: parent agent approval".to_string(),
            grant_mode: GrantMode::Once,
        });

        commands.entity(entity).despawn();
    }
}

/// 审批结果处理 System
///
/// 处理父 Agent 审批结果，更新权限，恢复任务
#[allow(clippy::too_many_arguments)]
pub(crate) fn approval_result_system(
    mut commands: Commands,
    mut agents: Query<&mut Agent>,
    mut tasks: Query<&mut Task>,
    executors: Res<BuiltinToolExecutors>,
    knowledge: Res<SpaceKnowledge>,
    approval_results: Query<(Entity, &ApprovalResultMessage)>,
    tool_requests: Query<(Entity, &ToolExecutionRequestMessage)>,
    calling_states: Query<&ToolCallingState>,
) {
    for (entity, result) in &approval_results {
        // 查找对应的 Tool 执行请求
        let Some((request_entity, tool_request)) = tool_requests
            .iter()
            .find(|(_, r)| r.pending_confirmation_id == Some(result.request_id))
        else {
            debug!(
                event = "ApprovalResultNoMatch",
                request_id = %result.request_id,
                "no matching tool request found, may have been processed"
            );
            commands.entity(entity).despawn();
            continue;
        };

        match result.decision {
            ApprovalDecision::Rejected => {
                warn!(
                    event = "ToolApprovalRejected",
                    tool_name = %tool_request.tool_name,
                    task_id = %tool_request.request.task_id,
                    agent_id = %tool_request.request.agent_id,
                    reasoning = %result.reasoning,
                    "tool execution rejected by parent agent"
                );

                let execution_result = AgentExecutionResult {
                    task_id: tool_request.request.task_id,
                    agent_id: tool_request.request.agent_id,
                    request_kind: tool_request.request.request_kind.clone(),
                    result: Err(ExecutionError::UserCancelled(format!(
                        "parent agent rejected: {}",
                        result.reasoning
                    ))),
                    prompt: String::new(),
                    system_prompt: None,
                    tools: vec![],
                    reasoning_content: None,
                };

                commands.spawn(ToolExecutionResultMessage {
                    result: execution_result,
                    tool_name: tool_request.tool_name.clone(),
                    tool_output: Err(ToolError::PermissionDenied(format!(
                        "parent agent rejected: {}",
                        result.reasoning
                    ))),
                    tool_call_id: tool_request.tool_call_id.clone(),
                    processed: false,
                });

                restore_task_after_tool(&mut tasks, &calling_states, result.source_task_id);
                commands.entity(request_entity).despawn();
            }
            ApprovalDecision::Approved => {
                debug!(
                    event = "ToolApprovalGranted",
                    tool_name = %tool_request.tool_name,
                    task_id = %tool_request.request.task_id,
                    agent_id = %tool_request.request.agent_id,
                    grant_mode = ?result.grant_mode,
                    "tool execution approved by parent agent"
                );

                // Permanent 模式：更新 Agent 权限
                if result.grant_mode == GrantMode::Permanent
                    && let Some(mut agent) = agents
                        .iter_mut()
                        .find(|a| a.id == tool_request.request.agent_id)
                {
                    agent.grant_permission(tool_request.tool_name.clone());
                    debug!(
                        event = "AgentPermissionUpdated",
                        agent_id = %agent.id,
                        tool_name = %tool_request.tool_name,
                        "agent permission updated to Allow permanently"
                    );
                }

                // 执行 Tool
                let Some(executor) = executors.get(&tool_request.tool_name) else {
                    warn!(
                        event = "ToolExecutorNotFound",
                        tool_name = %tool_request.tool_name,
                        "no executor registered for tool after approval"
                    );
                    spawn_tool_error(
                        &mut commands,
                        request_entity,
                        tool_request,
                        ToolError::NotFound(format!("executor for {}", tool_request.tool_name)),
                    );
                    restore_task_after_tool(&mut tasks, &calling_states, result.source_task_id);
                    commands.entity(entity).despawn();
                    continue;
                };

                let ctx = ToolContext {
                    knowledge: &knowledge,
                };
                let action = executor.execute(&tool_request.tool_input, &ctx);
                handle_tool_action(&mut commands, request_entity, tool_request, action);

                restore_task_after_tool(&mut tasks, &calling_states, result.source_task_id);
            }
        }

        commands.entity(entity).despawn();
    }
}

/// Agent 演化 System
///
/// 将批准后的长期权限修正或经验写回 Agent
#[allow(dead_code)]
pub(crate) fn agent_evolution_system(agents: Query<&Agent>) {
    // MVP 阶段暂不实现具体演化逻辑
    // 后续扩展：
    // - 从 Tool 执行结果中提取经验
    // - 更新 Agent.experience
    // - 根据 Permanent 确认更新 Agent.tool_permissions
    let _ = agents;
}

/// Tool 确认请求输出 System
///
/// 将确认请求通过 frontend_output_system 推送给前端（ToolConfirmationRequestMessage 已被该 system 捕获）
pub(crate) fn tool_confirmation_request_system(
    _agents: Query<&Agent>,
    _requests: Query<(Entity, &ToolConfirmationRequestMessage)>,
) {
    // frontend_output_system 负责监听 Added<ToolConfirmationRequestMessage> 并推送给前端，
    // 此 system 保留为占位，后续可在此添加额外逻辑（如日志增强）
}

/// Tool 确认响应处理 System
///
/// 处理用户的确认响应
#[allow(clippy::too_many_arguments)]
pub(crate) fn tool_confirmation_result_system(
    mut commands: Commands,
    mut agents: Query<&mut Agent>,
    mut tasks: Query<&mut Task>,
    executors: Res<BuiltinToolExecutors>,
    knowledge: Res<SpaceKnowledge>,
    tool_requests: Query<(Entity, &ToolExecutionRequestMessage)>,
    responses: Query<(Entity, &ToolConfirmationResponseMessage)>,
    calling_states: Query<&ToolCallingState>,
) {
    for (entity, response) in &responses {
        // 查找对应的 Tool 执行请求（通过 pending_confirmation_id 关联）
        let Some((request_entity, tool_request)) = tool_requests
            .iter()
            .find(|(_, r)| r.pending_confirmation_id == Some(response.request_id))
        else {
            warn!(
                event = "ToolConfirmationNoMatch",
                request_id = %response.request_id,
                "no matching tool request found"
            );
            commands.entity(entity).despawn();
            continue;
        };

        // 从 ToolExecutionRequestMessage 保存的选项中查找
        let options = tool_request
            .pending_confirmation_options
            .clone()
            .unwrap_or_else(ConfirmationOption::default_options);
        let selected_option = options
            .iter()
            .find(|opt| opt.id == response.selected_option);

        match selected_option {
            Some(option) if option.is_deny() => {
                // 用户拒绝
                warn!(
                    event = "ToolConfirmationDenied",
                    tool_name = %tool_request.tool_name,
                    task_id = %tool_request.request.task_id,
                    agent_id = %tool_request.request.agent_id,
                    "tool execution denied by user"
                );

                // 生成错误结果
                let execution_result = AgentExecutionResult {
                    task_id: tool_request.request.task_id,
                    agent_id: tool_request.request.agent_id,
                    request_kind: tool_request.request.request_kind.clone(),
                    result: Err(ExecutionError::UserCancelled(
                        "user denied tool execution".to_string(),
                    )),
                    prompt: String::new(),
                    system_prompt: None,
                    tools: vec![],
                    reasoning_content: None,
                };

                commands.spawn(ToolExecutionResultMessage {
                    result: execution_result,
                    tool_name: tool_request.tool_name.clone(),
                    tool_output: Err(ToolError::PermissionDenied("user denied".to_string())),
                    tool_call_id: tool_request.tool_call_id.clone(),
                    processed: false,
                });

                restore_task_after_tool(&mut tasks, &calling_states, tool_request.request.task_id);
                commands.entity(request_entity).despawn();
            }
            Some(option) => {
                // 用户确认
                debug!(
                    event = "ToolConfirmationApproved",
                    tool_name = %tool_request.tool_name,
                    task_id = %tool_request.request.task_id,
                    agent_id = %tool_request.request.agent_id,
                    mode = ?option.mode,
                    "tool execution confirmed by user"
                );

                // Permanent 模式：更新 Agent 权限
                if option.mode == crate::domain::GrantMode::Permanent
                    && let Some(mut agent) = agents
                        .iter_mut()
                        .find(|a| a.id == tool_request.request.agent_id)
                {
                    agent
                        .tool_permissions
                        .overrides
                        .insert(tool_request.tool_name.clone(), ToolPermission::Allow);
                    debug!(
                        event = "AgentPermissionUpdated",
                        agent_id = %agent.id,
                        tool_name = %tool_request.tool_name,
                        new_permission = ?ToolPermission::Allow,
                        "agent permission updated to Allow permanently"
                    );
                }

                // 执行 Tool
                let Some(executor) = executors.get(&tool_request.tool_name) else {
                    warn!(
                        event = "ToolExecutorNotFound",
                        tool_name = %tool_request.tool_name,
                        "no executor registered for tool after confirmation"
                    );
                    spawn_tool_error(
                        &mut commands,
                        request_entity,
                        tool_request,
                        ToolError::NotFound(format!("executor for {}", tool_request.tool_name)),
                    );
                    restore_task_after_tool(
                        &mut tasks,
                        &calling_states,
                        tool_request.request.task_id,
                    );
                    commands.entity(entity).despawn();
                    continue;
                };

                let ctx = ToolContext {
                    knowledge: &knowledge,
                };
                let action = executor.execute(&tool_request.tool_input, &ctx);
                handle_tool_action(&mut commands, request_entity, tool_request, action);

                restore_task_after_tool(&mut tasks, &calling_states, tool_request.request.task_id);
            }
            None => {
                warn!(
                    event = "ToolConfirmationUnknownOption",
                    request_id = %response.request_id,
                    selected_option = %response.selected_option,
                    "unknown option selected"
                );
                // 清理残留的请求 entity，避免永久泄漏
                commands.entity(request_entity).despawn();
            }
        }

        commands.entity(entity).despawn();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        AgentCapabilities, AgentExperience, AgentKind, AgentProfile, AgentToolPermissions,
        EntryRole, MemoryEntry,
    };

    #[allow(dead_code)]
    fn test_agent() -> Agent {
        Agent {
            id: uuid::Uuid::nil(),
            profile: AgentProfile {
                name: "test".to_string(),
                model: "test-model".to_string(),
            },
            capabilities: AgentCapabilities {
                tags: vec![],
                description: "test agent".to_string(),
            },
            kind: AgentKind::Persistent,
            parent_id: None,
            bound_task_id: None,
            tool_permissions: AgentToolPermissions::default(),
            experience: AgentExperience::default(),
        }
    }

    #[test]
    fn executor_knowledge_search() {
        let mut knowledge = SpaceKnowledge::default();
        knowledge.entries.push(MemoryEntry::new(
            EntryRole::User,
            "The project uses Rust and Bevy framework",
        ));
        knowledge.entries.push(MemoryEntry::new(
            EntryRole::User,
            "The system follows ECS architecture",
        ));

        let ctx = ToolContext {
            knowledge: &knowledge,
        };
        let executor = KnowledgeSearchTool;

        // Search for "rust"
        let input = serde_json::json!({"query": "rust"});
        let result = executor.execute(&input, &ctx);
        assert!(result.is_ok());
        match result.unwrap() {
            ToolAction::Direct(value) => {
                assert_eq!(value["count"], 1);
            }
            other => panic!("expected Direct action, got {:?}", other),
        }

        // Search for "bevy"
        let input = serde_json::json!({"query": "bevy"});
        let result = executor.execute(&input, &ctx);
        assert!(result.is_ok());

        // Search for non-existent
        let input = serde_json::json!({"query": "python"});
        let result = executor.execute(&input, &ctx);
        assert!(result.is_ok());
        match result.unwrap() {
            ToolAction::Direct(value) => {
                assert_eq!(value["count"], 0);
            }
            other => panic!("expected Direct action, got {:?}", other),
        }
    }

    #[test]
    fn executor_knowledge_search_missing_query() {
        let knowledge = SpaceKnowledge::default();
        let ctx = ToolContext {
            knowledge: &knowledge,
        };
        let executor = KnowledgeSearchTool;
        let input = serde_json::json!({"limit": 5});
        let result = executor.execute(&input, &ctx);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ToolError::InvalidInput(_)));
    }

    #[test]
    fn executor_spawn_agent() {
        let knowledge = SpaceKnowledge::default();
        let ctx = ToolContext {
            knowledge: &knowledge,
        };
        let executor = SpawnAgentTool;
        let input = serde_json::json!({
            "name": "child",
            "model": "gpt-4",
            "description": "A child agent",
            "tools": ["knowledge_search"]
        });
        let result = executor.execute(&input, &ctx);
        assert!(result.is_ok());
        match result.unwrap() {
            ToolAction::SpawnAgent {
                name,
                model,
                description,
                tools,
            } => {
                assert_eq!(name, "child");
                assert_eq!(model, Some("gpt-4".to_string()));
                assert_eq!(description, "A child agent");
                assert_eq!(tools, vec!["knowledge_search"]);
            }
            other => panic!("expected SpawnAgent action, got {:?}", other),
        }
    }

    #[test]
    fn executor_create_tasks() {
        let knowledge = SpaceKnowledge::default();
        let ctx = ToolContext {
            knowledge: &knowledge,
        };
        let executor = CreateTasksTool;
        let input = serde_json::json!({
            "tasks": [
                {
                    "name": "task-a",
                    "content": "do something",
                    "tools": ["knowledge_search"]
                },
                {
                    "name": "task-b",
                    "content": "do something else",
                    "tools": ["knowledge_search"],
                    "depends_on": ["task-a"]
                }
            ]
        });
        let result = executor.execute(&input, &ctx);
        assert!(result.is_ok());
        match result.unwrap() {
            ToolAction::CreateBatch(defs) => {
                assert_eq!(defs.len(), 2);
                assert_eq!(defs[0].name, "task-a");
                assert!(defs[0].depends_on.is_empty());
                assert_eq!(defs[1].name, "task-b");
                assert_eq!(defs[1].depends_on, vec!["task-a"]);
            }
            other => panic!("expected CreateBatch action, got {:?}", other),
        }
    }

    #[test]
    fn agent_tool_permissions_default_is_confirm() {
        let perms = AgentToolPermissions::default();
        assert_eq!(
            perms.get_permission("unknown_tool"),
            ToolPermission::Confirm
        );
    }

    #[test]
    fn agent_tool_permissions_override() {
        let mut perms = AgentToolPermissions {
            default_permission: ToolPermission::Deny,
            ..Default::default()
        };
        perms
            .overrides
            .insert("knowledge_search".to_string(), ToolPermission::Allow);

        assert_eq!(
            perms.get_permission("knowledge_search"),
            ToolPermission::Allow
        );
        assert_eq!(perms.get_permission("other"), ToolPermission::Deny);
    }

    #[test]
    fn parse_create_tasks_params_basic() {
        let input = serde_json::json!({
            "tasks": [
                {
                    "name": "task-a",
                    "content": "do something",
                    "tools": ["knowledge_search"]
                },
                {
                    "name": "task-b",
                    "content": "do something else",
                    "tools": ["knowledge_search"],
                    "depends_on": ["task-a"]
                }
            ]
        });

        let result = parse_create_tasks_params(&input);
        assert!(
            result.is_ok(),
            "should parse valid tasks: {:?}",
            result.err()
        );
        let defs = result.unwrap();
        assert_eq!(defs.len(), 2);
        assert_eq!(defs[0].name, "task-a");
        assert!(defs[0].depends_on.is_empty());
        assert_eq!(defs[1].name, "task-b");
        assert_eq!(defs[1].depends_on, vec!["task-a"]);
    }

    #[test]
    fn parse_create_tasks_params_empty_tasks() {
        let input = serde_json::json!({"tasks": []});
        let result = parse_create_tasks_params(&input);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("must not be empty"));
    }

    #[test]
    fn parse_create_tasks_params_duplicate_name() {
        let input = serde_json::json!({
            "tasks": [
                {"name": "dup", "content": "first", "tools": []},
                {"name": "dup", "content": "second", "tools": []}
            ]
        });
        let result = parse_create_tasks_params(&input);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("duplicate"));
    }

    #[test]
    fn parse_create_tasks_params_missing_dependency() {
        let input = serde_json::json!({
            "tasks": [
                {
                    "name": "only-task",
                    "content": "do something",
                    "tools": ["knowledge_search"],
                    "depends_on": ["nonexistent"]
                }
            ]
        });
        let result = parse_create_tasks_params(&input);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("does not exist"));
    }

    #[test]
    fn parse_create_tasks_params_cycle_detection() {
        let input = serde_json::json!({
            "tasks": [
                {
                    "name": "task-a",
                    "content": "first",
                    "tools": ["knowledge_search"],
                    "depends_on": ["task-b"]
                },
                {
                    "name": "task-b",
                    "content": "second",
                    "tools": ["knowledge_search"],
                    "depends_on": ["task-a"]
                }
            ]
        });
        let result = parse_create_tasks_params(&input);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("circular dependency"));
    }

    #[test]
    fn parse_create_tasks_params_self_cycle() {
        let input = serde_json::json!({
            "tasks": [
                {
                    "name": "self-ref",
                    "content": "bad",
                    "tools": ["knowledge_search"],
                    "depends_on": ["self-ref"]
                }
            ]
        });
        let result = parse_create_tasks_params(&input);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("circular dependency"));
    }

    #[test]
    fn parse_create_tasks_params_optional_fields() {
        let input = serde_json::json!({
            "tasks": [
                {
                    "name": "minimal",
                    "content": "just content",
                    "tools": ["knowledge_search"]
                }
            ]
        });
        let result = parse_create_tasks_params(&input);
        assert!(result.is_ok());
        let defs = result.unwrap();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "minimal");
        assert!(defs[0].depends_on.is_empty());
        assert!(defs[0].model.is_none());
    }
}
