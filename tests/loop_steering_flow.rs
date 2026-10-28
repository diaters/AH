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
/// 被 drop 时若尚未完成则置位：用于断言 `/stop` 真正中止了在飞请求。
struct DropProbe {
    aborted: Arc<std::sync::atomic::AtomicBool>,
    completed: bool,
}

impl Drop for DropProbe {
    fn drop(&mut self) {
        if !self.completed {
            self.aborted
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }
}

struct GatedToolExecutor {
    gate: Arc<Notify>,
    aborted: Arc<std::sync::atomic::AtomicBool>,
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
        let aborted = self.aborted.clone();
        Box::pin(async move {
            if is_first {
                let mut probe = DropProbe {
                    aborted,
                    completed: false,
                };
                gate.notified().await;
                probe.completed = true;
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
    aborted: Arc<std::sync::atomic::AtomicBool>,
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
    start_blocked_task_with(tool_call, true)
}

fn start_blocked_task_with(tool_call: LlmToolCall, multi_turn: bool) -> Harness {
    let gate = Arc::new(Notify::new());
    let aborted = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let executor: Arc<dyn AgentExecutor> = Arc::new(GatedToolExecutor {
        gate: gate.clone(),
        aborted: aborted.clone(),
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
        for name in ["shell_exec", "shell_start"] {
            if registry.get(name).is_none() {
                registry.register(ToolDefinition {
                    name: name.to_string(),
                    description: "run a command".to_string(),
                    parameters: ToolSchema::default(),
                    default_permission: ToolPermission::Allow,
                    executor: ToolExecutorKind::Builtin(name.to_string()),
                    required_tag: None,
                });
            }
        }
        registry.iter().cloned().collect()
    };

    let mut task = if multi_turn {
        Task::from_user_input("build the thing", 3, channel())
    } else {
        Task::from_user_input_ready("build the thing", 3, channel())
    };
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
            vision: false,
        },
    });
    update_with_yield(&mut app, 3);

    Harness {
        app,
        gate,
        aborted,
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

#[test]
fn stop_discards_in_flight_result_and_keeps_context() {
    let mut h = start_blocked_task(shell_exec_call("echo should-not-run"));
    send_input(&mut h.app, "/stop");
    update_with_yield(&mut h.app, 2);

    let task = h.app.world().get::<Task>(h.task_entity).unwrap().clone();
    assert_eq!(task.status(), &TaskStatus::Waiting(WaitingReason::User));
    assert_eq!(
        h.app
            .world()
            .get::<harness::domain::TurnGeneration>(h.task_entity)
            .map(|g| g.0),
        Some(1)
    );

    // 在飞 LLM 请求被真正中止（provider 侧 HTTP 随 future drop 取消）
    assert!(
        h.aborted.load(std::sync::atomic::Ordering::SeqCst),
        "in-flight llm future must be aborted by /stop"
    );

    // 即便放行，被中止的请求也不会再回送结果，不会触发工具调用
    h.gate.notify_one();
    update_with_yield(&mut h.app, 20);

    assert_eq!(h.requests.lock().unwrap().len(), 1, "no follow-up request");
    let task = h.app.world().get::<Task>(h.task_entity).unwrap();
    assert_eq!(task.status(), &TaskStatus::Waiting(WaitingReason::User));
    let stm = h.app.world().get::<ShortTermMemory>(h.task_entity).unwrap();
    assert!(stm.entries.iter().any(|e| e.content.contains("用户中断")));
    assert!(
        !stm.entries.iter().any(|e| e
            .metadata
            .tool_calls
            .iter()
            .any(|c| c.tool_name == "shell_exec")),
        "stale tool call must not execute"
    );
    assert_eq!(task_count(&mut h.app), 1);
}

fn shell_start_call(command: &str) -> LlmToolCall {
    LlmToolCall {
        id: "call_bg".to_string(),
        name: "shell_start".to_string(),
        arguments: serde_json::json!({ "command": command }).to_string(),
    }
}

/// 轮询推进直到条件满足（后台进程退出 + 节流轮询 + 唤醒需要真实时间）。
fn update_until(app: &mut App, max_iters: usize, mut done: impl FnMut(&mut App) -> bool) -> bool {
    for _ in 0..max_iters {
        app.update();
        if done(app) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    false
}

fn request_mentions(requests: &[AgentExecutionRequest], needle: &str) -> bool {
    requests.iter().any(|r| {
        r.prompt.contains(needle)
            || r.conversation.as_ref().is_some_and(|c| {
                c.iter().any(|m| match m {
                    ConversationMessage::User { content } => content.contains(needle),
                    _ => false,
                })
            })
    })
}

#[test]
fn background_shell_exit_wakes_idle_task_with_notification() {
    let mut h = start_blocked_task(shell_start_call("sleep 0.3; echo bg-done"));
    h.gate.notify_one();

    // 首轮：shell_start → follow-up → 文本回复，任务空闲
    let requests = h.requests.clone();
    assert!(update_until(&mut h.app, 200, |_| {
        requests.lock().unwrap().len() >= 2
    }));
    // 后台进程退出 → 通知入箱 → 空闲任务被拉起
    let requests = h.requests.clone();
    let woke = update_until(&mut h.app, 400, |_| {
        request_mentions(&requests.lock().unwrap(), "<task-notification")
    });
    assert!(woke, "idle task should be woken by background notification");

    let all = h.requests.lock().unwrap().clone();
    assert!(
        request_mentions(&all, "bg-done"),
        "notification carries output tail"
    );
    assert!(request_mentions(&all, "exited(0)"));
    assert_eq!(task_count(&mut h.app), 1);
    let jobs = h
        .app
        .world_mut()
        .query::<&harness::domain::BackgroundJob>()
        .iter(h.app.world())
        .count();
    assert_eq!(jobs, 0, "finished job is retired");
}

#[test]
fn natural_completion_waits_for_background_jobs() {
    let mut h = start_blocked_task_with(shell_start_call("sleep 1.5; echo late"), false);
    h.gate.notify_one();

    let entity = h.task_entity;
    assert!(
        update_until(&mut h.app, 200, |app| {
            app.world().get::<Task>(entity).unwrap().status()
                == &TaskStatus::Waiting(WaitingReason::BackgroundJobs)
        }),
        "single-turn task must not reach Done while its shell job is alive"
    );

    assert!(
        update_until(&mut h.app, 400, |app| {
            app.world().get::<Task>(entity).unwrap().status() == &TaskStatus::Done
        }),
        "task completes after the background job finishes"
    );

    let all = h.requests.lock().unwrap().clone();
    assert!(
        request_mentions(&all, "候选结果"),
        "deferred result is replayed to the LLM"
    );
    assert!(request_mentions(&all, "late"));
}

#[test]
fn stopping_owner_task_with_finish_force_stops_background_shell() {
    let mut h = start_blocked_task(shell_start_call("sleep 30"));
    h.gate.notify_one();
    let requests = h.requests.clone();
    assert!(update_until(&mut h.app, 200, |_| {
        requests.lock().unwrap().len() >= 2
    }));
    let jobs_before = h
        .app
        .world_mut()
        .query::<&harness::domain::BackgroundJob>()
        .iter(h.app.world())
        .count();
    assert_eq!(jobs_before, 1);

    send_input(&mut h.app, "/finish");
    let ok = update_until(&mut h.app, 200, |app| {
        app.world_mut()
            .query::<&harness::domain::BackgroundJob>()
            .iter(app.world())
            .count()
            == 0
    });
    assert!(ok, "terminal task must not keep background jobs");
    let backend = h
        .app
        .world()
        .resource::<harness::NativeProcessBackend>()
        .clone();
    use harness::domain::SessionBackend;
    assert!(
        backend.list_active_sessions().unwrap().is_empty(),
        "shell session stopped"
    );
}

#[test]
fn update_todos_is_stored_and_replayed_at_next_turn_start() {
    let call = LlmToolCall {
        id: "call_todo".to_string(),
        name: "update_todos".to_string(),
        arguments: serde_json::json!({
            "todos": [
                { "content": "investigate loop", "status": "completed" },
                { "content": "implement steer", "status": "in_progress" }
            ]
        })
        .to_string(),
    };
    let mut h = start_blocked_task(call);
    h.gate.notify_one();
    let entity = h.task_entity;
    assert!(update_until(&mut h.app, 200, |app| {
        app.world().get::<Task>(entity).unwrap().status()
            == &TaskStatus::Waiting(WaitingReason::User)
    }));

    let todos = h
        .app
        .world()
        .get::<harness::domain::TaskTodoList>(entity)
        .expect("todo list stored on task")
        .items
        .clone();
    assert_eq!(todos.len(), 2);
    assert_eq!(todos[1].status, harness::domain::TodoStatus::InProgress);

    // 下一轮首个请求回填进度清单快照
    send_input(&mut h.app, "继续");
    let requests = h.requests.clone();
    assert!(update_until(&mut h.app, 200, |_| {
        requests.lock().unwrap().len() >= 3
    }));
    let all = h.requests.lock().unwrap().clone();
    assert!(request_mentions(&all[2..], "<todo_snapshot>"));
    assert!(request_mentions(&all[2..], "▶ implement steer"));
}
