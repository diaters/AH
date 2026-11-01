//! 异步 ReAct 端到端：工具调用超过前台预算自动转后台，LLM 收到 `running_in_background`
//! 后继续推理或结束本轮；结果出来后以 `<task-notification>` 注入并开启新的一轮。

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
        AgentRequestKind, AgentToolPermissions, BackgroundJob, ChannelId, ConversationMessage,
        ExecutorFuture, FrontendKind, LlmToolCall, OutputContent, ShortTermMemory,
        SpaceToolRegistry, StopTaskMessage, Task, TaskStatus, ToolPermission, WaitingReason,
    },
    llm::ExecutorRegistry,
    systems::HarnessConfig,
};
use tokio::runtime::Runtime;

type Script = Box<dyn Fn(&AgentExecutionRequest, usize) -> OutputContent + Send + Sync>;

/// 记录任务级请求，按脚本决定每一轮的输出。
struct Scripted {
    script: Script,
    requests: Arc<Mutex<Vec<AgentExecutionRequest>>>,
}

impl AgentExecutor for Scripted {
    fn execute(&self, request: AgentExecutionRequest) -> ExecutorFuture {
        if request.request_kind != AgentRequestKind::LlmCompletion || request.work_item_id.is_some()
        {
            return Box::pin(async {
                Ok(AgentExecutionOutput {
                    content: OutputContent::Text("ok".into()),
                    reasoning_content: None,
                })
            });
        }
        let turn = {
            let mut requests = self.requests.lock().unwrap();
            requests.push(request.clone());
            requests.len()
        };
        let content = (self.script)(&request, turn);
        Box::pin(async move {
            Ok(AgentExecutionOutput {
                content,
                reasoning_content: None,
            })
        })
    }
}

fn channel() -> ChannelId {
    ChannelId {
        frontend: FrontendKind::Tui,
        user_id: "default".into(),
        thread_id: None,
    }
}

struct Harness {
    app: App,
    requests: Arc<Mutex<Vec<AgentExecutionRequest>>>,
    task_entity: Entity,
    task_id: harness::domain::TaskId,
}

fn start(script: Script, multi_turn: bool) -> Harness {
    start_with(script, multi_turn, vec![], channel())
}

fn start_with(
    script: Script,
    multi_turn: bool,
    frontends: Vec<Box<dyn harness::domain::Frontend>>,
    channel: ChannelId,
) -> Harness {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let executor: Arc<dyn AgentExecutor> = Arc::new(Scripted {
        script,
        requests: requests.clone(),
    });
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(
        HarnessConfig {
            agents_config_path: "/nonexistent_agents.toml".into(),
            max_tool_iterations: 8,
            tool_background_after_secs: 1,
            ..HarnessConfig::default()
        },
        Arc::new(Runtime::new().unwrap()),
        ExecutorRegistry::from_single_executor(executor, "default"),
        input_rx,
        frontends,
        harness::channels::ChannelManager::empty().0,
    );
    app.update();

    let agent_id = AgentId::new();
    let agent_entity = app
        .world_mut()
        .spawn(Agent {
            id: agent_id,
            profile: AgentProfile {
                name: "main".into(),
                model: "test-model".into(),
            },
            capabilities: AgentCapabilities {
                tags: vec!["general".into()],
                description: "main".into(),
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
    let tools: Vec<_> = app
        .world()
        .resource::<SpaceToolRegistry>()
        .iter()
        .filter(|t| ["shell_exec", "stop_background_job"].contains(&t.name.as_str()))
        .cloned()
        .collect();
    assert_eq!(tools.len(), 2);

    let mut task = if multi_turn {
        Task::from_user_input("build it", 3, channel.clone())
    } else {
        Task::from_user_input_ready("build it", 3, channel.clone())
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
            prompt: "build it".into(),
            system_prompt: None,
            tools,
            conversation: None,
            work_item_id: None,
            model_override: None,
            turn_generation: None,
            vision: false,
            trusted_images: Vec::new(),
        },
    });
    Harness {
        app,
        requests,
        task_entity,
        task_id,
    }
}

/// 模拟主循环一帧：先取出到期的截止时间（转后台 / watchdog 等定时工作依赖它）。
fn tick(app: &mut App) {
    app.world_mut()
        .resource_mut::<harness::systems::demand::DemandSchedule>()
        .take_due(std::time::Instant::now());
    app.update();
}

fn update_until(app: &mut App, max: usize, mut done: impl FnMut(&mut App) -> bool) -> bool {
    for _ in 0..max {
        tick(app);
        if done(app) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

fn tool_results(request: &AgentExecutionRequest) -> Vec<String> {
    request
        .conversation
        .iter()
        .flatten()
        .filter_map(|m| match m {
            ConversationMessage::Tool { content, .. } => Some(content.clone()),
            _ => None,
        })
        .collect()
}

fn mentions(request: &AgentExecutionRequest, needle: &str) -> bool {
    request.prompt.contains(needle)
        || request.conversation.iter().flatten().any(|m| match m {
            ConversationMessage::User { content } => content.contains(needle),
            _ => false,
        })
}

fn shell_call(id: &str, command: &str) -> OutputContent {
    OutputContent::ToolCalls(vec![LlmToolCall {
        id: id.into(),
        name: "shell_exec".into(),
        arguments: serde_json::json!({ "command": command }).to_string(),
    }])
}

fn job_count(app: &mut App) -> usize {
    app.world_mut()
        .query::<&BackgroundJob>()
        .iter(app.world())
        .count()
}

/// 依赖结果时结束本轮，结果出来后自动注入并开启新的一轮。
#[test]
fn slow_tool_moves_to_background_and_result_starts_new_turn() {
    let mut h = start(
        Box::new(|request, _| {
            if mentions(request, "<task-notification") {
                OutputContent::Text("构建完成".into())
            } else if tool_results(request)
                .iter()
                .any(|r| r.contains("running_in_background"))
            {
                OutputContent::Text("构建在后台进行，结果出来后继续".into())
            } else {
                shell_call("call_build", "sleep 2.5; echo build-finished")
            }
        }),
        true,
    );

    let entity = h.task_entity;
    let requests = h.requests.clone();
    // 前台预算 1s 到期：工具循环拿到合成结果，LLM 结束本轮，任务回到等待用户。
    let ended = update_until(&mut h.app, 150, |app| {
        requests.lock().unwrap().len() >= 2
            && app.world().get::<Task>(entity).unwrap().status()
                == &TaskStatus::Waiting(WaitingReason::User)
    });
    let snapshot: Vec<Vec<String>> = h
        .requests
        .lock()
        .unwrap()
        .iter()
        .map(tool_results)
        .collect();
    assert!(
        ended,
        "turn should end while the command keeps running: status={:?} results={snapshot:?}",
        h.app.world().get::<Task>(entity).unwrap().status(),
    );
    {
        let requests = h.requests.lock().unwrap();
        let results = tool_results(&requests[1]);
        assert!(
            results.iter().any(|r| r.contains("running_in_background")
                && r.contains("job_id")
                && !r.contains("build-finished")),
            "follow-up must carry the background notice, got {results:?}"
        );
    }
    assert_eq!(job_count(&mut h.app), 1, "backgrounded call is a job");

    // 命令结束：结果作为通知注入，空闲任务被拉起新一轮。
    let requests = h.requests.clone();
    assert!(
        update_until(&mut h.app, 300, |_| {
            requests
                .lock()
                .unwrap()
                .iter()
                .any(|r| mentions(r, "<task-notification source=\"tool\""))
        }),
        "result should wake the idle task"
    );
    let all = h.requests.lock().unwrap().clone();
    let woken = all
        .iter()
        .find(|r| mentions(r, "<task-notification source=\"tool\""))
        .unwrap();
    assert!(
        mentions(woken, "build-finished"),
        "notification carries result"
    );
    assert!(
        all.iter().all(|r| tool_results(r)
            .iter()
            .all(|content| !content.contains("build-finished"))),
        "late result must not be injected as a tool result"
    );
    assert!(update_until(&mut h.app, 50, |app| job_count(app) == 0));
}

/// 单轮任务：结果未送达前不进入终态，送达后完成。
#[test]
fn single_turn_task_completion_waits_for_background_tool() {
    let mut h = start(
        Box::new(|request, _| {
            if mentions(request, "<task-notification") {
                OutputContent::Text("最终结果：done".into())
            } else if !tool_results(request).is_empty() {
                OutputContent::Text("等待后台构建".into())
            } else {
                shell_call("call_build", "sleep 2; echo late")
            }
        }),
        false,
    );
    let entity = h.task_entity;
    assert!(
        update_until(&mut h.app, 150, |app| {
            app.world().get::<Task>(entity).unwrap().status()
                == &TaskStatus::Waiting(WaitingReason::BackgroundJobs)
        }),
        "single-turn task must wait for its background tool call"
    );
    assert!(
        update_until(&mut h.app, 300, |app| {
            app.world().get::<Task>(entity).unwrap().status() == &TaskStatus::Done
        }),
        "task completes once the result arrives"
    );
}

/// stop_background_job 停止后台调用：进程被取消，结果不再通知。
#[test]
fn stop_background_job_cancels_and_suppresses_notification() {
    let mut h = start(
        Box::new(|request, _| {
            let results = tool_results(request);
            if results.iter().any(|r| r.contains("\"stopped\"")) {
                return OutputContent::Text("已放弃该命令".into());
            }
            if let Some(notice) = results.iter().find(|r| r.contains("running_in_background")) {
                let value: serde_json::Value = serde_json::from_str(notice).unwrap();
                let job_id = value["job_id"].as_str().unwrap().to_string();
                return OutputContent::ToolCalls(vec![LlmToolCall {
                    id: "call_stop".into(),
                    name: "stop_background_job".into(),
                    arguments: serde_json::json!({ "job_id": job_id }).to_string(),
                }]);
            }
            shell_call("call_hang", "sleep 30; echo never")
        }),
        true,
    );
    let entity = h.task_entity;
    let requests = h.requests.clone();
    assert!(
        update_until(&mut h.app, 200, |app| {
            requests.lock().unwrap().len() >= 3
                && app.world().get::<Task>(entity).unwrap().status()
                    == &TaskStatus::Waiting(WaitingReason::User)
        }),
        "agent stops the job and ends its turn"
    );
    assert!(
        update_until(&mut h.app, 150, |app| job_count(app) == 0),
        "stopped job retires once the cancelled worker returns"
    );
    for _ in 0..20 {
        tick(&mut h.app);
        std::thread::sleep(Duration::from_millis(20));
    }
    let all = h.requests.lock().unwrap();
    assert_eq!(all.len(), 3, "no extra turn after stopping");
    assert!(!all.iter().any(|r| mentions(r, "<task-notification")));
}

/// /stop 中断当前回合但不取消已转后台的调用，结果仍会送达。
#[test]
fn stop_command_keeps_backgrounded_call_running() {
    let mut h = start(
        Box::new(|request, _| {
            if mentions(request, "<task-notification") {
                OutputContent::Text("收到结果".into())
            } else if tool_results(request)
                .iter()
                .any(|r| r.contains("running_in_background"))
            {
                // 继续做别的事：再发起一个会被 /stop 中断的长命令。
                shell_call("call_other", "sleep 30")
            } else {
                shell_call("call_build", "sleep 2; echo survived")
            }
        }),
        true,
    );
    let requests = h.requests.clone();
    assert!(update_until(&mut h.app, 150, |_| {
        requests.lock().unwrap().len() >= 2
    }));
    let task_id = h.task_id;
    h.app.world_mut().spawn(StopTaskMessage { task_id });
    let requests = h.requests.clone();
    assert!(
        update_until(&mut h.app, 300, |_| {
            requests
                .lock()
                .unwrap()
                .iter()
                .any(|r| mentions(r, "survived"))
        }),
        "backgrounded call must survive /stop and deliver its result"
    );
}

struct ClientRecorder(Arc<Mutex<Vec<harness::domain::EngineEvent>>>);

impl harness::domain::Frontend for ClientRecorder {
    fn kind(&self) -> FrontendKind {
        FrontendKind::Client
    }
    fn push_event(&self, event: harness::domain::EngineEvent) {
        self.0.lock().unwrap().push(event);
    }
    fn poll_actions(&self) -> Vec<harness::domain::UserAction> {
        vec![]
    }
}

/// 客户端执行记录：转后台时收到 started，后台结束时收到同一调用的真实结果 finished。
#[test]
fn client_observes_background_start_and_final_result() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let client_channel = ChannelId {
        frontend: FrontendKind::Client,
        user_id: "alice".into(),
        thread_id: Some("session".into()),
    };
    let mut h = start_with(
        Box::new(|request, _| {
            if mentions(request, "<task-notification") {
                OutputContent::Text("完成".into())
            } else if !tool_results(request).is_empty() {
                OutputContent::Text("等待后台命令".into())
            } else {
                shell_call("call_bg", "sleep 2; echo client-visible")
            }
        }),
        true,
        vec![Box::new(ClientRecorder(events.clone()))],
        client_channel,
    );
    let recorded = events.clone();
    let phases = move || -> Vec<(Option<&'static str>, String)> {
        recorded
            .lock()
            .unwrap()
            .iter()
            .filter_map(|e| match e {
                harness::domain::EngineEvent::ToolCallCompleted {
                    call_id,
                    background,
                    output,
                    ..
                } if call_id.as_deref() == Some("call_bg") => Some((*background, output.clone())),
                _ => None,
            })
            .collect()
    };
    let check = phases.clone();
    assert!(
        update_until(&mut h.app, 300, |_| check().len() >= 2),
        "client should see both background phases, got {:?}",
        phases()
    );
    let seen = phases();
    assert_eq!(seen[0].0, Some("started"));
    assert!(seen[0].1.contains("running_in_background"));
    assert_eq!(seen[1].0, Some("finished"));
    assert!(seen[1].1.contains("client-visible"));
}
