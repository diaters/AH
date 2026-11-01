//! 流量监控日志回归：混合响应、运行中追问、受管采样、平台回执与结束清理。
//! 采样值来自临时文件；真实 shell 和生产 ECS/ChannelManager 路径不依赖网卡或账号。

use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use async_trait::async_trait;
use harness::{
    app::build_harness_app,
    channels::{
        Channel, ChannelError, ChannelInboundMessage, ChannelManager, ChannelOutboundMessage,
    },
    domain::{
        Agent, AgentCapabilities, AgentExecutionOutput, AgentExecutionRequest,
        AgentExecutionRequestMessage, AgentExecutor, AgentId, AgentKind, AgentProfile,
        AgentRequestKind, AgentToolPermissions, ChannelId, ConversationMessage, ExecutionError,
        ExecutorFuture, FrontendKind, InFlightToolCall, LlmToolCall, LoopControlConfig,
        OutputContent, SessionBackend, ShortTermMemory, SpaceToolRegistry, Task, TaskStatus,
        ToolCallingState, ToolPermission, UserInputMessage, WaitingReason,
    },
    ecs::EntityIndex,
    llm::ExecutorRegistry,
    prelude::*,
    systems::{HarnessConfig, MonitorDeliveries, tools::NativeProcessBackend},
};
use tokio::runtime::Runtime;

const PROGRESS: &str = "正在验证采样，再启动受管监控。";
const STEER_REPLY: &str = "正在启动监控；采样检查已经通过。";
const PRIVATE_REASONING: &str = "private-provider-reasoning";
const EXTRA_TOOL_ROUNDS: usize = 34;

struct ScriptedMonitor {
    responses: Mutex<VecDeque<AgentExecutionOutput>>,
    requests: Arc<Mutex<Vec<AgentExecutionRequest>>>,
    reject_reports: AtomicBool,
}

impl AgentExecutor for ScriptedMonitor {
    fn execute(&self, request: AgentExecutionRequest) -> ExecutorFuture {
        if request.request_kind != AgentRequestKind::LlmCompletion || request.work_item_id.is_some()
        {
            return Box::pin(async { Ok(text("internal")) });
        }
        self.requests.lock().unwrap().push(request);
        if self.requests.lock().unwrap().len() > 3 + EXTRA_TOOL_ROUNDS
            && self.reject_reports.load(Ordering::SeqCst)
        {
            return Box::pin(async {
                Err(ExecutionError::InvalidRequest(
                    "test report rejected".into(),
                ))
            });
        }
        let output = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| text("已收到监控状态更新。"));
        Box::pin(async move { Ok(output) })
    }
}

fn text(value: &str) -> AgentExecutionOutput {
    AgentExecutionOutput {
        content: OutputContent::Text(value.into()),
        reasoning_content: None,
    }
}

fn tool_with_text(text: &str, name: &str, input: serde_json::Value) -> AgentExecutionOutput {
    AgentExecutionOutput {
        content: OutputContent::ToolCallsWithText {
            text: text.into(),
            calls: vec![LlmToolCall {
                id: format!("call-{name}"),
                name: name.into(),
                arguments: input.to_string(),
            }],
        },
        reasoning_content: Some(PRIVATE_REASONING.into()),
    }
}

struct TestChannel(Arc<Mutex<Vec<ChannelOutboundMessage>>>);

#[async_trait]
impl Channel for TestChannel {
    fn name(&self) -> &str {
        "qq"
    }

    async fn send(&self, message: &ChannelOutboundMessage) -> Result<Option<String>, ChannelError> {
        self.0.lock().unwrap().push(message.clone());
        Ok(Some("test-platform-receipt".into()))
    }

    async fn listen(
        &self,
        _: crossbeam_channel::Sender<ChannelInboundMessage>,
    ) -> Result<(), ChannelError> {
        std::future::pending().await
    }
}

struct Cleanup(NativeProcessBackend, ChannelManager);
impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = self.0.shutdown();
        self.1.shutdown();
    }
}

#[track_caller]
fn pump_until(app: &mut App, mut done: impl FnMut(&mut App) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        app.update();
        if done(app) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "monitor scenario did not reach the expected state: tasks={:?}, tool loops={:?}",
            app.world_mut()
                .query::<&Task>()
                .iter(app.world())
                .map(|t| (t.status().clone(), t.last_error().map(str::to_owned)))
                .collect::<Vec<_>>(),
            app.world_mut()
                .query::<&ToolCallingState>()
                .iter(app.world())
                .map(|s| (s.iteration, s.pending_tool_call_ids.clone()))
                .collect::<Vec<_>>()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn traffic_monitor_keeps_feedback_delivery_and_process_ownership_in_one_task() {
    let dir = tempfile::tempdir().unwrap();
    let agents_config = dir.path().join("agents.toml");
    std::fs::write(&agents_config, "agent = []\n").unwrap();
    std::fs::write(dir.path().join("rate"), "400000\n").unwrap();
    let cwd = dir.path().to_str().unwrap();
    let sampler = r#"
        printf 'diagnostic-only\n' >&2
        active=0
        while :; do
            rate=$(cat rate)
            if [ "$rate" -gt 1000000 ]; then
                if [ "$active" -eq 0 ]; then
                    printf '流量告警：%s B/s，阈值 1000000 B/s\n' "$rate"
                    active=1
                fi
            elif [ "$active" -eq 1 ]; then
                printf '流量已回落\n'
                active=0
            fi
            printf '%s' "$rate" > sampled
            sleep 0.02
        done
    "#;
    let requests = Arc::new(Mutex::new(Vec::new()));
    let executor = Arc::new(ScriptedMonitor {
        requests: requests.clone(),
        reject_reports: AtomicBool::new(true),
        responses: Mutex::new({
            let mut responses = VecDeque::from([
                tool_with_text(
                    PROGRESS,
                    "shell_exec",
                    serde_json::json!({
                        "command": "while [ ! -f release ]; do sleep 0.01; done; printf SAMPLE_OK",
                        "cwd": cwd, "timeout_secs": 20,
                    }),
                ),
                tool_with_text(
                    STEER_REPLY,
                    "shell_start",
                    serde_json::json!({
                        "command": sampler, "cwd": cwd, "notify_on_output": true,
                    }),
                ),
                text("已启动；按 1 MB/s 监控，告警由 Harness 直接投递。"),
            ]);
            let final_text = responses.pop_back().unwrap();
            for round in 0..EXTRA_TOOL_ROUNDS {
                responses.push_back(AgentExecutionOutput {
                    content: OutputContent::ToolCalls(vec![LlmToolCall {
                        id: format!("list-{round}"),
                        name: "shell_list".into(),
                        arguments: "{}".into(),
                    }]),
                    reasoning_content: Some(PRIVATE_REASONING.into()),
                });
            }
            responses.push_back(final_text);
            responses
        }),
    });
    let runtime = Arc::new(Runtime::new().unwrap());
    let sent = Arc::new(Mutex::new(Vec::<ChannelOutboundMessage>::new()));
    let (input_tx, input_rx) = crossbeam_channel::unbounded();
    let (manager, manager_handle, frontends) = {
        let _entered = runtime.enter();
        ChannelManager::new(vec![Arc::new(TestChannel(sent.clone()))], input_tx).unwrap()
    };
    let mut app = build_harness_app(
        HarnessConfig {
            agents_config_path: agents_config.to_string_lossy().into_owned(),
            max_tool_iterations: 50,
            subtask_checkpoint_iterations: harness::systems::DEFAULT_SUBTASK_CHECKPOINT_ITERATIONS,
            subtask_max_runtime_secs: harness::systems::DEFAULT_SUBTASK_MAX_RUNTIME_SECS,
            memory: harness::domain::MemoryConfig {
                compression_threshold_tokens: 100_000,
                ..Default::default()
            },
            ..Default::default()
        },
        runtime.clone(),
        ExecutorRegistry::from_single_executor(executor.clone(), "default"),
        input_rx,
        frontends,
        manager.clone(),
    );
    app.insert_resource(LoopControlConfig {
        background_poll_ms: 1,
        notification_coalesce_ms: 1,
        ..Default::default()
    });
    app.update();
    let backend = app.world().resource::<NativeProcessBackend>().clone();
    let _cleanup = Cleanup(backend.clone(), manager.clone());
    let agent_id = AgentId::new();
    let agent_entity = app
        .world_mut()
        .spawn(Agent {
            id: agent_id,
            profile: AgentProfile {
                name: "monitor-test".into(),
                model: "test".into(),
            },
            capabilities: AgentCapabilities {
                tags: vec![],
                description: "test".into(),
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
    let channel = ChannelId {
        frontend: FrontendKind::QQ,
        user_id: "test-user".into(),
        thread_id: Some("monitor-thread".into()),
    };
    let mut task = Task::from_user_input(
        "帮我监控系统的流量，当有超过1m/s的速度时，请通知我",
        3,
        channel.clone(),
    );
    task.delegate = Some(agent_id);
    task.mark_waiting(WaitingReason::Agent, chrono::Utc::now());
    let task_id = task.id;
    let task_entity = app
        .world_mut()
        .spawn((task, ShortTermMemory::default()))
        .id();
    let tools = app
        .world()
        .resource::<SpaceToolRegistry>()
        .iter()
        .cloned()
        .collect();
    {
        let mut index = app.world_mut().resource_mut::<EntityIndex>();
        index.tasks.insert(task_id, task_entity);
        index.agents.insert(agent_id, agent_entity);
    }
    app.world_mut().spawn(AgentExecutionRequestMessage {
        request: AgentExecutionRequest {
            task_id,
            agent_id,
            request_kind: AgentRequestKind::LlmCompletion,
            prompt: "监控流量并在超过 1 MB/s 时通知".into(),
            system_prompt: None,
            tools,
            conversation: None,
            work_item_id: None,
            model_override: None,
            turn_generation: None,
            vision: false,
            trusted_images: vec![],
        },
    });
    pump_until(&mut app, |app| {
        app.world_mut()
            .query::<&InFlightToolCall>()
            .iter(app.world())
            .count()
            == 1
            && backend.list_task_sessions(task_id).unwrap().len() == 1
            && sent
                .lock()
                .unwrap()
                .iter()
                .any(|m| m.content.contains(PROGRESS))
    });
    assert_eq!(
        app.world().get::<Task>(task_entity).unwrap().status(),
        &TaskStatus::Waiting(WaitingReason::ToolExecution)
    );
    assert_eq!(backend.list_task_sessions(task_id).unwrap().len(), 1);
    app.world_mut().spawn(UserInputMessage {
        content: "当前在干什么？".into(),
        origin_channel: channel.clone(),
        sender: None,
    });
    pump_until(&mut app, |_| {
        sent.lock()
            .unwrap()
            .iter()
            .any(|m| m.content.contains("已收到补充") && m.content.contains("shell_exec（已运行"))
    });
    assert_eq!(
        requests.lock().unwrap().len(),
        1,
        "steer must not start a concurrent LLM request"
    );
    assert_eq!(
        app.world_mut().query::<&Task>().iter(app.world()).count(),
        1
    );
    std::fs::write(dir.path().join("release"), "go").unwrap();
    pump_until(&mut app, |app| {
        backend.list_task_sessions(task_id).unwrap().len() == 1
            && dir.path().join("sampled").exists()
            && app
                .world_mut()
                .query::<&ToolCallingState>()
                .iter(app.world())
                .count()
                == 0
    });
    {
        let requests = requests.lock().unwrap();
        let follow_up = requests[1].conversation.as_ref().unwrap();
        assert!(follow_up.iter().any(|m| matches!(m, ConversationMessage::Assistant { content: Some(s), tool_calls, .. } if s == PROGRESS && tool_calls.len() == 1)));
        assert!(
            matches!(follow_up.last(), Some(ConversationMessage::User { content }) if content.contains("当前在干什么？") && content.contains("先用简短文字回应"))
        );
        assert!(requests[2].conversation.as_ref().unwrap().iter().any(|m| matches!(m, ConversationMessage::Assistant { content: Some(s), tool_calls, .. } if s == STEER_REPLY && tool_calls.len() == 1)));
    }
    assert!(
        sent.lock()
            .unwrap()
            .iter()
            .all(|m| !m.content.contains("流量告警：")
                && !m.content.contains("diagnostic-only")
                && !m.content.contains(PRIVATE_REASONING))
    );
    std::fs::write(dir.path().join("rate.tmp"), "1500000\n").unwrap();
    std::fs::rename(dir.path().join("rate.tmp"), dir.path().join("rate")).unwrap();
    pump_until(&mut app, |app| {
        app.world()
            .resource::<MonitorDeliveries>()
            .recent_records()
            .any(|r| {
                r.task_id == task_id
                    && r.status == "delivered"
                    && r.content.contains("流量告警：1500000")
            })
    });
    pump_until(&mut app, |_| {
        sent.lock()
            .unwrap()
            .iter()
            .any(|m| m.content.contains("后台汇报失败") && m.content.contains("仍在运行"))
    });
    {
        let requests = requests.lock().unwrap();
        let restored = requests[3 + EXTRA_TOOL_ROUNDS]
            .conversation
            .as_ref()
            .expect("resume actual protocol");
        let calls: Vec<_> = restored
            .iter()
            .filter_map(|m| match m {
                ConversationMessage::Assistant {
                    tool_calls,
                    reasoning_content,
                    ..
                } if !tool_calls.is_empty() => {
                    assert_eq!(reasoning_content.as_deref(), Some(PRIVATE_REASONING));
                    Some(tool_calls)
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            calls.len(),
            2 + EXTRA_TOOL_ROUNDS,
            "keep separate provider rounds"
        );
        assert!(calls[0][0].arguments.contains("timeout_secs"));
        assert!(calls[1][0].arguments.contains("notify_on_output"));
        assert!(calls[2..].iter().all(|round| round[0].arguments == "{}"));
    }
    assert_eq!(
        backend.list_task_sessions(task_id).unwrap().len(),
        1,
        "notification must not stop the monitor"
    );
    assert_eq!(
        app.world().get::<Task>(task_entity).unwrap().content,
        "帮我监控系统的流量，当有超过1m/s的速度时，请通知我",
        "automatic reports must not replace the user's goal"
    );
    executor.reject_reports.store(false, Ordering::SeqCst);
    app.world_mut().spawn(UserInputMessage {
        content: "查询本次受管监控的状态".into(),
        origin_channel: channel.clone(),
        sender: None,
    });
    pump_until(&mut app, |_| {
        sent.lock()
            .unwrap()
            .iter()
            .any(|m| m.content.contains("已收到监控状态更新"))
    });
    assert!(
        requests
            .lock()
            .unwrap()
            .iter()
            .all(|r| r.task_id == task_id)
    );
    assert_eq!(
        app.world_mut().query::<&Task>().iter(app.world()).count(),
        1
    );
    assert!(
        app.world()
            .get::<harness::domain::BackgroundReportTurn>(task_entity)
            .is_none()
    );
    std::fs::write(dir.path().join("rate.tmp"), "200000\n").unwrap();
    std::fs::rename(dir.path().join("rate.tmp"), dir.path().join("rate")).unwrap();
    pump_until(&mut app, |app| {
        app.world()
            .resource::<MonitorDeliveries>()
            .recent_records()
            .any(|r| r.status == "delivered" && r.content.contains("流量已回落"))
    });
    app.world_mut().spawn(UserInputMessage {
        content: "/finish".into(),
        origin_channel: channel,
        sender: None,
    });
    pump_until(&mut app, |app| {
        app.world()
            .get::<Task>(task_entity)
            .is_some_and(|task| task.status().is_terminal())
            && backend.list_task_sessions(task_id).unwrap().is_empty()
    });
    {
        let sent = sent.lock().unwrap();
        for text in [PROGRESS, STEER_REPLY, "流量告警：1500000", "流量已回落"] {
            assert_eq!(
                sent.iter().filter(|m| m.content.contains(text)).count(),
                1,
                "duplicated or missing text: {text}"
            );
        }
        assert!(sent.iter().all(
            |m| m.recipient == "test-user" && m.thread_id.as_deref() == Some("monitor-thread")
        ));
    }
    manager.shutdown();
    runtime.block_on(manager_handle).unwrap();
}
