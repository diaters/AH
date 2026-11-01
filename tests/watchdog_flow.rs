//! Watchdog 端到端：定时巡检后台工具调用与子任务，按 watchdog Agent 的判断
//! 继续 / 调整 / 取消 / 重试，主 Agent 不参与监督过程。

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
        ExecutorFuture, FailureReason, FrontendKind, LlmToolCall, LoopControlConfig, OutputContent,
        ShortTermMemory, SpaceToolRegistry, Task, TaskStatus, ToolPermission,
    },
    llm::ExecutorRegistry,
    systems::HarnessConfig,
};
use tokio::runtime::Runtime;

/// 一轮脚本输出：内容 + 模拟推理耗时。
struct Turn {
    content: OutputContent,
    delay: Duration,
}

impl Turn {
    fn now(content: OutputContent) -> Self {
        Self {
            content,
            delay: Duration::ZERO,
        }
    }
}

type Script = Box<dyn Fn(&AgentExecutionRequest) -> Turn + Send + Sync>;

struct Scripted {
    main: Script,
    watchdog: Script,
    requests: Arc<Mutex<Vec<AgentExecutionRequest>>>,
    watchdog_prompts: Arc<Mutex<Vec<String>>>,
}

impl AgentExecutor for Scripted {
    fn execute(&self, request: AgentExecutionRequest) -> ExecutorFuture {
        let turn = match request.request_kind {
            AgentRequestKind::Watchdog => {
                self.watchdog_prompts
                    .lock()
                    .unwrap()
                    .push(request.prompt.clone());
                (self.watchdog)(&request)
            }
            AgentRequestKind::LlmCompletion if request.work_item_id.is_none() => {
                self.requests.lock().unwrap().push(request.clone());
                (self.main)(&request)
            }
            _ => Turn::now(OutputContent::Text("ok".into())),
        };
        Box::pin(async move {
            if !turn.delay.is_zero() {
                tokio::time::sleep(turn.delay).await;
            }
            Ok(AgentExecutionOutput {
                content: turn.content,
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

fn spawn_agent(app: &mut App, name: &str, tags: &[&str]) -> AgentId {
    let id = AgentId::new();
    let entity = app
        .world_mut()
        .spawn(Agent {
            id,
            profile: AgentProfile {
                name: name.into(),
                model: "test-model".into(),
            },
            capabilities: AgentCapabilities {
                tags: tags.iter().map(|t| t.to_string()).collect(),
                description: name.into(),
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
        .insert(id, entity);
    id
}

struct Harness {
    app: App,
    requests: Arc<Mutex<Vec<AgentExecutionRequest>>>,
    watchdog_prompts: Arc<Mutex<Vec<String>>>,
    task_entity: Entity,
}

fn start(main: Script, watchdog: Script) -> Harness {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let watchdog_prompts = Arc::new(Mutex::new(Vec::new()));
    let executor: Arc<dyn AgentExecutor> = Arc::new(Scripted {
        main,
        watchdog,
        requests: requests.clone(),
        watchdog_prompts: watchdog_prompts.clone(),
    });
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(
        HarnessConfig {
            agents_config_path: "/nonexistent_agents.toml".into(),
            max_tool_iterations: 8,
            tool_background_after_secs: 1,
            loop_control: LoopControlConfig {
                watchdog_interval_secs: 1,
                ..LoopControlConfig::default()
            },
            ..HarnessConfig::default()
        },
        Arc::new(Runtime::new().unwrap()),
        ExecutorRegistry::from_single_executor(executor, "default"),
        input_rx,
        vec![],
        harness::channels::ChannelManager::empty().0,
    );
    app.update();
    // 子任务未指定 Agent 时交给主 Agent（默认名 default-llm-agent）。
    let main_agent = spawn_agent(&mut app, "default-llm-agent", &["general"]);
    spawn_agent(&mut app, "watchdog", &["watchdog"]);

    let tools: Vec<_> = app
        .world()
        .resource::<SpaceToolRegistry>()
        .iter()
        .filter(|t| {
            [
                "shell_exec",
                "create_tasks",
                "wait_tasks",
                "stop_background_job",
            ]
            .contains(&t.name.as_str())
        })
        .cloned()
        .collect();
    let mut task = Task::from_user_input("PARENT-GOAL build it", 3, channel());
    task.mark_waiting_for_agent(main_agent, chrono::Utc::now());
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
            agent_id: main_agent,
            request_kind: AgentRequestKind::LlmCompletion,
            prompt: "PARENT-GOAL build it".into(),
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
        watchdog_prompts,
        task_entity,
    }
}

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

fn mentions(request: &AgentExecutionRequest, needle: &str) -> bool {
    request.prompt.contains(needle)
        || request.conversation.iter().flatten().any(|m| match m {
            ConversationMessage::User { content } => content.contains(needle),
            _ => false,
        })
}

fn has_tool_result(request: &AgentExecutionRequest) -> bool {
    request
        .conversation
        .iter()
        .flatten()
        .any(|m| matches!(m, ConversationMessage::Tool { .. }))
}

fn decisions(action: &str, extra: &str) -> Turn {
    Turn::now(OutputContent::Text(format!(
        r#"{{"decisions":[{{"id":"S1","progress":"stalled","action":"{action}",{extra}"reason":"长时间没有任何输出"}}]}}"#
    )))
}

fn shell_call(id: &str, command: &str) -> OutputContent {
    OutputContent::ToolCalls(vec![LlmToolCall {
        id: id.into(),
        name: "shell_exec".into(),
        arguments: serde_json::json!({ "command": command }).to_string(),
    }])
}

fn create_child(content: &str) -> OutputContent {
    OutputContent::ToolCalls(vec![LlmToolCall {
        id: "call_tasks".into(),
        name: "create_tasks".into(),
        arguments: serde_json::json!({
            "tasks": [{ "name": "child", "content": content, "tools": ["shell_exec"] }]
        })
        .to_string(),
    }])
}

fn job_count(app: &mut App) -> usize {
    app.world_mut()
        .query::<&BackgroundJob>()
        .iter(app.world())
        .count()
}

/// 后台命令长时间无输出：watchdog 取消，所属任务收到带原因的通知并开启新一轮。
#[test]
fn watchdog_cancels_stalled_background_tool_call() {
    let mut h = start(
        Box::new(|request| {
            if mentions(request, "<task-notification") {
                Turn::now(OutputContent::Text("命令被取消，改用其他方案".into()))
            } else if has_tool_result(request) {
                Turn::now(OutputContent::Text("等待后台命令".into()))
            } else {
                Turn::now(shell_call("call_hang", "sleep 30"))
            }
        }),
        Box::new(|_| decisions("cancel", "")),
    );
    let requests = h.requests.clone();
    assert!(
        update_until(&mut h.app, 400, |_| {
            requests
                .lock()
                .unwrap()
                .iter()
                .any(|r| mentions(r, "<task-notification source=\"tool\""))
        }),
        "watchdog cancel should deliver a notification to the owner"
    );
    let prompts = h.watchdog_prompts.lock().unwrap().clone();
    assert!(!prompts.is_empty());
    assert!(prompts[0].contains("后台工具调用 shell_exec"));
    assert!(
        prompts[0].contains("PARENT-GOAL"),
        "goal is part of the evidence"
    );
    assert!(prompts[0].contains("sleep 30"));
    let all = h.requests.lock().unwrap().clone();
    let woken = all
        .iter()
        .find(|r| mentions(r, "<task-notification source=\"tool\""))
        .unwrap();
    assert!(mentions(woken, "已被 watchdog 取消"), "reason is attached");
    assert!(update_until(&mut h.app, 100, |app| job_count(app) == 0));
}

/// 后台命令卡住：watchdog 以相同参数重试，新调用的结果照常通知。
#[test]
fn watchdog_retries_background_tool_call_with_same_input() {
    let marker = std::env::temp_dir().join(format!("watchdog-retry-{}", uuid::Uuid::new_v4()));
    let marker_path = marker.display().to_string();
    // 第一次运行创建标记后卡住；重试时标记已存在，立即输出并结束。
    let command = format!(
        "if [ -f {marker_path} ]; then echo retried-ok; else touch {marker_path}; sleep 30; fi"
    );
    let mut h = start(
        Box::new(move |request| {
            if mentions(request, "<task-notification") {
                Turn::now(OutputContent::Text("完成".into()))
            } else if has_tool_result(request) {
                Turn::now(OutputContent::Text("等待后台命令".into()))
            } else {
                Turn::now(shell_call("call_flaky", &command))
            }
        }),
        Box::new(|_| decisions("retry", "")),
    );
    let requests = h.requests.clone();
    assert!(
        update_until(&mut h.app, 400, |_| {
            requests
                .lock()
                .unwrap()
                .iter()
                .any(|r| mentions(r, "retried-ok"))
        }),
        "retried call should deliver its result"
    );
    let all = h.requests.lock().unwrap().clone();
    let woken = all.iter().find(|r| mentions(r, "retried-ok")).unwrap();
    assert!(mentions(woken, "watchdog 已用相同参数重试"));
    let _ = std::fs::remove_file(&marker);
}

/// 子任务缺乏进展：watchdog 把调整建议注入子任务的下一次请求。
#[test]
fn watchdog_adjust_injects_guidance_into_sub_task() {
    let mut h = start(
        Box::new(|request| {
            if mentions(request, "CHILD-WORK") {
                if mentions(request, "<watchdog_guidance>") {
                    return Turn::now(OutputContent::Text(
                        "<<<RESULT>>>按建议完成<<</RESULT>>>".into(),
                    ));
                }
                if has_tool_result(request) {
                    // 继续在原路线上兜圈子，直到收到建议。
                    return Turn {
                        content: shell_call("call_loop", "echo still-trying"),
                        delay: Duration::from_millis(300),
                    };
                }
                // 首次推理较慢，使子任务运行超过巡检周期。
                return Turn {
                    content: shell_call("call_loop", "echo still-trying"),
                    delay: Duration::from_millis(1200),
                };
            }
            if mentions(request, "<task-notification") {
                Turn::now(OutputContent::Text("子任务完成".into()))
            } else if has_tool_result(request) {
                Turn::now(OutputContent::Text("子任务在后台执行".into()))
            } else {
                Turn::now(create_child("CHILD-WORK do the work"))
            }
        }),
        Box::new(|_| decisions("adjust", r#""guidance":"换用 --offline 参数","#)),
    );
    let requests = h.requests.clone();
    assert!(
        update_until(&mut h.app, 500, |_| {
            requests
                .lock()
                .unwrap()
                .iter()
                .any(|r| mentions(r, "<task-notification source=\"subtasks\""))
        }),
        "batch notification should arrive after the child follows the guidance"
    );
    let all = h.requests.lock().unwrap().clone();
    let guided = all
        .iter()
        .find(|r| mentions(r, "<watchdog_guidance>"))
        .expect("guidance reaches the child");
    assert!(mentions(guided, "换用 --offline 参数"));
    assert!(
        !all.iter()
            .filter(|r| !mentions(r, "CHILD-WORK"))
            .any(|r| mentions(r, "<watchdog_guidance>")),
        "guidance must not leak into the parent context"
    );
    let prompts = h.watchdog_prompts.lock().unwrap();
    assert!(prompts[0].contains("子任务「child」"));
}

/// 子任务无望：watchdog 取消，父任务收到带原因的批次通知。
#[test]
fn watchdog_cancels_hopeless_sub_task() {
    let mut h = start(
        Box::new(|request| {
            if mentions(request, "CHILD-WORK") {
                return Turn {
                    content: shell_call("call_loop", "echo again"),
                    delay: Duration::from_millis(400),
                };
            }
            if mentions(request, "<task-notification") {
                Turn::now(OutputContent::Text("子任务被取消".into()))
            } else if has_tool_result(request) {
                Turn::now(OutputContent::Text("子任务在后台执行".into()))
            } else {
                Turn::now(create_child("CHILD-WORK loop forever"))
            }
        }),
        Box::new(|_| decisions("cancel", "")),
    );
    let requests = h.requests.clone();
    assert!(
        update_until(&mut h.app, 500, |_| {
            requests
                .lock()
                .unwrap()
                .iter()
                .any(|r| mentions(r, "<task-notification source=\"subtasks\""))
        }),
        "parent should be notified once the child is cancelled"
    );
    let all = h.requests.lock().unwrap().clone();
    let woken = all
        .iter()
        .find(|r| mentions(r, "<task-notification source=\"subtasks\""))
        .unwrap();
    assert!(mentions(woken, "watchdog 取消"));
    let child_failed = h
        .app
        .world_mut()
        .query::<&Task>()
        .iter(h.app.world())
        .any(|t| t.status() == &TaskStatus::Failed(FailureReason::WatchdogCancelled));
    assert!(child_failed);
    assert!(
        !h.app
            .world()
            .get::<Task>(h.task_entity)
            .unwrap()
            .status()
            .is_terminal(),
        "parent keeps running"
    );
}

/// wait_tasks 超过前台预算：返回 running_in_background，批次完成后仍以通知送达
/// （此前被 wait_tasks 认领过的批次不会因此静默退役）。
#[test]
fn wait_tasks_moves_to_background_and_batch_still_notifies() {
    let mut h = start(
        Box::new(|request| {
            if mentions(request, "CHILD-WORK") {
                return Turn {
                    content: OutputContent::Text("<<<RESULT>>>child-result<<</RESULT>>>".into()),
                    delay: Duration::from_millis(2500),
                };
            }
            if mentions(request, "<task-notification") {
                return Turn::now(OutputContent::Text("汇总完成".into()));
            }
            let results: Vec<String> = request
                .conversation
                .iter()
                .flatten()
                .filter_map(|m| match m {
                    ConversationMessage::Tool { content, .. } => Some(content.clone()),
                    _ => None,
                })
                .collect();
            if results.iter().any(|r| r.contains("running_in_background")) {
                return Turn::now(OutputContent::Text("子任务仍在运行，稍后汇总".into()));
            }
            if let Some(created) = results.iter().find(|r| r.contains("batch_created")) {
                let value: serde_json::Value = serde_json::from_str(created).unwrap();
                let ids: Vec<String> = value["tasks"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|t| t["task_id"].as_str().unwrap().to_string())
                    .collect();
                assert!(value["job_id"].is_string(), "create_tasks exposes job_id");
                return Turn::now(OutputContent::ToolCalls(vec![LlmToolCall {
                    id: "call_wait".into(),
                    name: "wait_tasks".into(),
                    arguments: serde_json::json!({ "task_ids": ids, "timeout_secs": 300 })
                        .to_string(),
                }]));
            }
            Turn::now(create_child("CHILD-WORK slow"))
        }),
        Box::new(|_| decisions("continue", "")),
    );
    let requests = h.requests.clone();
    assert!(
        update_until(&mut h.app, 500, |_| {
            requests
                .lock()
                .unwrap()
                .iter()
                .any(|r| mentions(r, "<task-notification source=\"subtasks\""))
        }),
        "batch completion must still notify after wait_tasks moved to background"
    );
    let all = h.requests.lock().unwrap().clone();
    let woken = all
        .iter()
        .find(|r| mentions(r, "<task-notification source=\"subtasks\""))
        .unwrap();
    assert!(mentions(woken, "child-result"));
    assert!(
        all.iter().any(|r| r
            .conversation
            .iter()
            .flatten()
            .any(|m| matches!(m, ConversationMessage::Tool { content, .. }
                if content.contains("running_in_background") && content.contains("timed_out")))),
        "wait_tasks returned the background notice within the foreground budget"
    );
}
