//! Agent loop 控制端到端测试：引导式插入（steer）、`/stop` 中断与后台通知拉起。
//!
//! 设计见 `docs/design/2026-10-28-loop-steering-and-background-notification-design.md`。

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use crossbeam_channel::unbounded;
use harness::prelude::*;
use harness::{
    app::build_harness_app,
    domain::{
        Agent, AgentCapabilities, AgentExecutionOutput, AgentExecutionRequest,
        AgentExecutionRequestMessage, AgentExecutor, AgentId, AgentKind, AgentProfile,
        AgentRequestKind, AgentToolPermissions, ChannelId, ConversationMessage, ExecutorFuture,
        FrontendKind, LlmToolCall, OutputContent, ShortTermMemory, SpaceToolRegistry, Task,
        TaskStatus, ToolDefinition, ToolExecutorKind, ToolPermission, ToolSchema, UserInputMessage,
        WaitingReason,
    },
    llm::ExecutorRegistry,
    systems::HarnessConfig,
};
use tokio::{runtime::Runtime, sync::Notify};

fn channel() -> ChannelId {
    ChannelId {
        frontend: FrontendKind::Tui,
        user_id: "default".to_string(),
        thread_id: None,
    }
}

fn update_with_yield(app: &mut App, n: usize) {
    for _ in 0..n {
        app.update();
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// 脚本化执行器：记录全部任务级 LLM 请求；第一次请求阻塞到 `gate` 放行后返回
/// 指定的工具调用，后续请求返回文本。
struct GatedToolExecutor {
    gate: Arc<Notify>,
    first_call: Mutex<bool>,
    tool_call: LlmToolCall,
    requests: Arc<Mutex<Vec<AgentExecutionRequest>>>,
}

impl AgentExecutor for GatedToolExecutor {
    fn execute(&self, request: AgentExecutionRequest) -> ExecutorFuture {
        if request.request_kind != AgentRequestKind::LlmCompletion || request.work_item_id.is_some()
        {
            return Box::pin(async {
                Ok(AgentExecutionOutput {
                    content: OutputContent::Text("ok".to_string()),
                    reasoning_content: None,
                })
            });
        }
        self.requests.lock().unwrap().push(request);
        let is_first = std::mem::replace(&mut *self.first_call.lock().unwrap(), false);
        let gate = self.gate.clone();
        let tool_call = self.tool_call.clone();
        Box::pin(async move {
            if is_first {
                gate.notified().await;
                Ok(AgentExecutionOutput {
                    content: OutputContent::ToolCalls(vec![tool_call]),
                    reasoning_content: None,
                })
            } else {
                Ok(AgentExecutionOutput {
                    content: OutputContent::Text("final answer".to_string()),
                    reasoning_content: None,
                })
            }
        })
    }
}

struct Harness {
    app: App,
    gate: Arc<Notify>,
    requests: Arc<Mutex<Vec<AgentExecutionRequest>>>,
    task_entity: Entity,
}

fn shell_exec_call(command: &str) -> LlmToolCall {
    LlmToolCall {
        id: "call_1".to_string(),
        name: "shell_exec".to_string(),
        arguments: serde_json::json!({ "command": command }).to_string(),
    }
}

/// 构造一个已发出首个 LLM 请求、执行器阻塞中的多轮顶层任务。
fn start_blocked_task(tool_call: LlmToolCall) -> Harness {
    let gate = Arc::new(Notify::new());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let executor: Arc<dyn AgentExecutor> = Arc::new(GatedToolExecutor {
        gate: gate.clone(),
        first_call: Mutex::new(true),
        tool_call,
        requests: requests.clone(),
    });
    let registry = ExecutorRegistry::from_single_executor(executor, "default");
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(
        HarnessConfig {
            max_tool_iterations: 5,
            ..HarnessConfig::default()
        },
        Arc::new(Runtime::new().unwrap()),
        registry,
        input_rx,
        vec![],
        harness::channels::ChannelManager::empty().0,
    );
    app.update();

    let agent_id = AgentId::new();
    let agent_entity = app
        .world_mut()
        .spawn(Agent {
            id: agent_id,
            profile: AgentProfile {
                name: "test-agent".to_string(),
                model: "test-model".to_string(),
            },
            capabilities: AgentCapabilities {
                tags: vec!["test".to_string()],
                description: "test agent".to_string(),
            },
            kind: AgentKind::Persistent,
            parent_id: None,
            bound_task_id: None,
            tool_permissions: AgentToolPermissions {
                default_permission: ToolPermission::Allow,
                default_permission_explicit: true,
                overrides: HashMap::new(),
            },
            system_prompt: None,
        })
        .id();
    app.world_mut()
        .resource_mut::<harness::ecs::EntityIndex>()
        .agents
        .insert(agent_id, agent_entity);

    let tools: Vec<ToolDefinition> = {
        let mut registry = app.world_mut().resource_mut::<SpaceToolRegistry>();
        if registry.get("shell_exec").is_none() {
            registry.register(ToolDefinition {
                name: "shell_exec".to_string(),
                description: "run a command".to_string(),
                parameters: ToolSchema::default(),
                default_permission: ToolPermission::Allow,
                executor: ToolExecutorKind::Builtin("shell_exec".to_string()),
                required_tag: None,
            });
        }
        registry.iter().cloned().collect()
    };

    let mut task = Task::from_user_input("build the thing", 3, channel());
    task.mark_waiting_for_agent(agent_id, chrono::Utc::now());
    let task_id = task.id;
    let task_entity = app
        .world_mut()
        .spawn((task, ShortTermMemory::default()))
        .id();
    app.world_mut()
        .resource_mut::<harness::ecs::EntityIndex>()
        .tasks
        .insert(task_id, task_entity);

    app.world_mut().spawn(AgentExecutionRequestMessage {
        request: AgentExecutionRequest {
            task_id,
            agent_id,
            request_kind: AgentRequestKind::LlmCompletion,
            prompt: "build the thing".to_string(),
            system_prompt: None,
            tools,
            conversation: None,
            work_item_id: None,
            model_override: None,
            turn_generation: None,
        },
    });
    update_with_yield(&mut app, 3);

    Harness {
        app,
        gate,
        requests,
        task_entity,
    }
}

fn task_count(app: &mut App) -> usize {
    app.world_mut().query::<&Task>().iter(app.world()).count()
}

fn send_input(app: &mut App, content: &str) {
    app.world_mut().spawn(UserInputMessage {
        content: content.to_string(),
        origin_channel: channel(),
    });
}

#[test]
fn steer_during_running_turn_reaches_follow_up_request_without_new_task() {
    let mut h = start_blocked_task(shell_exec_call("echo hi"));
    assert_eq!(
        h.app.world().get::<Task>(h.task_entity).unwrap().status(),
        &TaskStatus::Running
    );

    send_input(&mut h.app, "please also add tests");
    update_with_yield(&mut h.app, 2);
    assert_eq!(
        task_count(&mut h.app),
        1,
        "steer must not create a new task"
    );

    h.gate.notify_one();
    update_with_yield(&mut h.app, 30);

    let requests = h.requests.lock().unwrap().clone();
    assert!(requests.len() >= 2, "expected a follow-up request");
    let follow_up = requests[1]
        .conversation
        .clone()
        .expect("structured follow-up");
    let tool_idx = follow_up
        .iter()
        .position(|m| matches!(m, ConversationMessage::Tool { .. }))
        .expect("tool result present");
    let steer_idx = follow_up
        .iter()
        .position(|m| {
            matches!(m, ConversationMessage::User { content }
                if content.contains("<user_steer>") && content.contains("please also add tests"))
        })
        .expect("steer delivered in follow-up");
    assert!(steer_idx > tool_idx, "steer appended after tool results");

    let stm = h.app.world().get::<ShortTermMemory>(h.task_entity).unwrap();
    assert!(
        stm.entries
            .iter()
            .any(|e| e.content.contains("please also add tests")),
        "steer recorded in short-term memory"
    );
    assert_eq!(
        h.app.world().get::<Task>(h.task_entity).unwrap().status(),
        &TaskStatus::Waiting(WaitingReason::User)
    );
}
