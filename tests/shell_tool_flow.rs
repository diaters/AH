//! shell 工具集成测试

use std::sync::Arc;

use crossbeam_channel::unbounded;
use harness::{
    Agent, AgentCapabilities, AgentExecutionOutput, AgentExecutionRequest, AgentExecutor,
    AgentExperience, AgentKind, AgentProfile, AgentRequestKind, AgentToolPermissions, ChannelId,
    ExecutorFuture, FrontendKind, HarnessConfig, SessionBackend, ShortTermMemory, Task,
    ToolExecutionRequestMessage, build_harness_app,
};
use tokio::runtime::Runtime;
use uuid::Uuid;

fn default_channel() -> ChannelId {
    ChannelId {
        frontend: FrontendKind::Tui,
        user_id: "default".to_string(),
    }
}

struct MockExecutor;

impl AgentExecutor for MockExecutor {
    fn execute(&self, _request: AgentExecutionRequest) -> ExecutorFuture {
        Box::pin(async move {
            Ok(AgentExecutionOutput {
                content: harness::OutputContent::Text("mock response".to_string()),
                reasoning_content: None,
            })
        })
    }
}

fn test_config() -> HarnessConfig {
    HarnessConfig::default()
}

fn spawn_agent(world: &mut bevy::prelude::World) -> Uuid {
    let id = Uuid::new_v4();
    world.spawn(Agent {
        id,
        profile: AgentProfile {
            name: "shell-agent".to_string(),
            model: "test-model".to_string(),
        },
        capabilities: AgentCapabilities {
            tags: vec!["test".to_string()],
            description: "shell test agent".to_string(),
        },
        kind: AgentKind::Persistent,
        parent_id: None,
        bound_task_id: None,
        tool_permissions: AgentToolPermissions {
            default_permission: harness::ToolPermission::Allow,
            overrides: Default::default(),
        },
        experience: AgentExperience::default(),
    });
    id
}

/// 验证 shell 工具注册表已经收敛为六个简化后的高层工具。
#[test]
fn shell_registry_only_exposes_six_simplified_tools() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockExecutor);
    let (_input_tx, input_rx) = unbounded();
    let app = build_harness_app(test_config(), runtime, executor, input_rx, vec![]);

    let registry = app.world().resource::<harness::SpaceToolRegistry>();

    for name in [
        "shell_exec",
        "shell_start",
        "shell_read",
        "shell_list",
        "shell_input",
        "shell_stop",
    ] {
        assert!(registry.get(name).is_some(), "missing {name}");
    }

    for name in [
        "shell_status",
        "shell_read_output",
        "shell_send_input",
        "shell_send_signal",
        "shell_wait",
    ] {
        assert!(
            registry.get(name).is_none(),
            "legacy tool still exposed: {name}"
        );
    }
}

/// 验证 `shell_read` 返回简化后的状态字段与最新输出快照。
#[test]
fn shell_read_returns_status_and_latest_snapshot() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, vec![]);

    app.update();
    let agent_id = spawn_agent(app.world_mut());
    let task_entity = app
        .world_mut()
        .spawn((
            Task::from_user_input_ready("shell read", 3, default_channel()),
            ShortTermMemory::default(),
        ))
        .id();
    let task_id = app.world().get::<Task>(task_entity).unwrap().id;

    app.world_mut().spawn(ToolExecutionRequestMessage {
        request: AgentExecutionRequest {
            task_id,
            agent_id,
            request_kind: AgentRequestKind::ToolExecution {
                tool_name: "shell_start".to_string(),
            },
            prompt: String::new(),
            system_prompt: None,
            tools: vec![],
            conversation: None,
            work_item_id: None,
        },
        tool_name: "shell_start".to_string(),
        tool_input: serde_json::json!({ "command": "printf 'hello\\n'; sleep 1" }),
        pending_confirmation_id: None,
        tool_call_id: Some("call_start_read_case".to_string()),
        pending_confirmation_options: None,
    });
    app.update();

    let session_id = {
        let backend = app.world().resource::<harness::NativeProcessBackend>();
        backend
            .list_task_sessions(task_id)
            .unwrap()
            .first()
            .unwrap()
            .handle_id
            .to_string()
    };

    app.world_mut().spawn(ToolExecutionRequestMessage {
        request: AgentExecutionRequest {
            task_id,
            agent_id,
            request_kind: AgentRequestKind::ToolExecution {
                tool_name: "shell_read".to_string(),
            },
            prompt: String::new(),
            system_prompt: None,
            tools: vec![],
            conversation: None,
            work_item_id: None,
        },
        tool_name: "shell_read".to_string(),
        tool_input: serde_json::json!({ "session_id": session_id, "tail_lines": 20 }),
        pending_confirmation_id: None,
        tool_call_id: Some("call_read_case".to_string()),
        pending_confirmation_options: None,
    });

    app.update();

    let world = app.world_mut();
    let mut query = world.query::<&harness::ToolExecutionResultMessage>();
    let results = query.iter(world).cloned().collect::<Vec<_>>();
    let output = results.last().unwrap().tool_output.clone().unwrap();

    assert!(output["status"].is_string());
    assert!(output["running"].is_boolean());
    assert!(output["output"].is_string());
}

/// 验证 `shell_list` 只返回活动会话列表。
#[test]
fn shell_list_returns_only_active_sessions() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, vec![]);

    app.update();
    let agent_id = spawn_agent(app.world_mut());
    let task_entity = app
        .world_mut()
        .spawn((
            Task::from_user_input_ready("shell list", 3, default_channel()),
            ShortTermMemory::default(),
        ))
        .id();
    let task_id = app.world().get::<Task>(task_entity).unwrap().id;

    app.world_mut().spawn(ToolExecutionRequestMessage {
        request: AgentExecutionRequest {
            task_id,
            agent_id,
            request_kind: AgentRequestKind::ToolExecution {
                tool_name: "shell_start".to_string(),
            },
            prompt: String::new(),
            system_prompt: None,
            tools: vec![],
            conversation: None,
            work_item_id: None,
        },
        tool_name: "shell_start".to_string(),
        tool_input: serde_json::json!({ "command": "sleep 30" }),
        pending_confirmation_id: None,
        tool_call_id: Some("call_start_list_case".to_string()),
        pending_confirmation_options: None,
    });
    app.update();

    app.world_mut().spawn(ToolExecutionRequestMessage {
        request: AgentExecutionRequest {
            task_id,
            agent_id,
            request_kind: AgentRequestKind::ToolExecution {
                tool_name: "shell_list".to_string(),
            },
            prompt: String::new(),
            system_prompt: None,
            tools: vec![],
            conversation: None,
            work_item_id: None,
        },
        tool_name: "shell_list".to_string(),
        tool_input: serde_json::json!({}),
        pending_confirmation_id: None,
        tool_call_id: Some("call_list_case".to_string()),
        pending_confirmation_options: None,
    });

    app.update();

    let world = app.world_mut();
    let mut query = world.query::<&harness::ToolExecutionResultMessage>();
    let results = query.iter(world).cloned().collect::<Vec<_>>();
    let output = results.last().unwrap().tool_output.clone().unwrap();

    assert!(output.is_array());
    if let Some(first) = output.as_array().unwrap().first() {
        assert!(first["session_id"].is_string());
        assert!(first["status"].is_string());
    }
}

#[test]
fn shell_exec_returns_result_message() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, vec![]);

    app.update();

    let agent_id = spawn_agent(app.world_mut());
    let task_entity = app
        .world_mut()
        .spawn((
            Task::from_user_input_ready("shell test", 3, default_channel()),
            ShortTermMemory::default(),
        ))
        .id();
    let task_id = app.world().get::<Task>(task_entity).unwrap().id;

    let request = AgentExecutionRequest {
        task_id,
        agent_id,
        request_kind: AgentRequestKind::ToolExecution {
            tool_name: "shell_exec".to_string(),
        },
        prompt: String::new(),
        system_prompt: None,
        tools: vec![],
        conversation: None,
        work_item_id: None,
    };

    app.world_mut().spawn(ToolExecutionRequestMessage {
        request,
        tool_name: "shell_exec".to_string(),
        tool_input: serde_json::json!({
            "command": "printf 'a\\nb\\nc\\n'",
            "tail_lines": 2
        }),
        pending_confirmation_id: None,
        tool_call_id: Some("call_shell_exec".to_string()),
        pending_confirmation_options: None,
    });

    // Check result after first update (tool_result_system despawns results after processing)
    app.update();
    let results = {
        let world = app.world_mut();
        let mut query = world.query::<&harness::ToolExecutionResultMessage>();
        query.iter(world).cloned().collect::<Vec<_>>()
    };

    assert!(
        !results.is_empty(),
        "shell_exec should produce a ToolExecutionResultMessage"
    );
    let output_json = results[0]
        .tool_output
        .clone()
        .expect("shell_exec should succeed");
    assert_eq!(output_json["status"], "completed");
    let output = output_json["output"].as_str().unwrap();
    assert!(output.contains("b"), "tail should contain 'b'");
    assert!(output.contains("c"), "tail should contain 'c'");
    assert_eq!(output_json["truncated"], true);
}

#[test]
fn shell_start_returns_running_handle() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, vec![]);

    app.update();

    let agent_id = spawn_agent(app.world_mut());
    let task_entity = app
        .world_mut()
        .spawn((
            Task::from_user_input_ready("shell start", 3, default_channel()),
            ShortTermMemory::default(),
        ))
        .id();
    let task_id = app.world().get::<Task>(task_entity).unwrap().id;

    let request = AgentExecutionRequest {
        task_id,
        agent_id,
        request_kind: AgentRequestKind::ToolExecution {
            tool_name: "shell_start".to_string(),
        },
        prompt: String::new(),
        system_prompt: None,
        tools: vec![],
        conversation: None,
        work_item_id: None,
    };

    app.world_mut().spawn(ToolExecutionRequestMessage {
        request,
        tool_name: "shell_start".to_string(),
        tool_input: serde_json::json!({
            "command": "sleep 1"
        }),
        pending_confirmation_id: None,
        tool_call_id: Some("call_shell_start".to_string()),
        pending_confirmation_options: None,
    });

    // Check result after first update
    app.update();
    let results = {
        let world = app.world_mut();
        let mut query = world.query::<&harness::ToolExecutionResultMessage>();
        query.iter(world).cloned().collect::<Vec<_>>()
    };

    assert!(!results.is_empty(), "shell_start should return immediately");
    let output_json = results[0]
        .tool_output
        .clone()
        .expect("shell_start should succeed");
    assert_eq!(output_json["status"], "running");
    assert!(output_json["session_id"].is_string());
}

#[test]
fn shell_exec_with_exit_code_error() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, vec![]);

    app.update();

    let agent_id = spawn_agent(app.world_mut());
    let task_entity = app
        .world_mut()
        .spawn((
            Task::from_user_input_ready("shell error test", 3, default_channel()),
            ShortTermMemory::default(),
        ))
        .id();
    let task_id = app.world().get::<Task>(task_entity).unwrap().id;

    let request = AgentExecutionRequest {
        task_id,
        agent_id,
        request_kind: AgentRequestKind::ToolExecution {
            tool_name: "shell_exec".to_string(),
        },
        prompt: String::new(),
        system_prompt: None,
        tools: vec![],
        conversation: None,
        work_item_id: None,
    };

    app.world_mut().spawn(ToolExecutionRequestMessage {
        request,
        tool_name: "shell_exec".to_string(),
        tool_input: serde_json::json!({
            "command": "exit 1"
        }),
        pending_confirmation_id: None,
        tool_call_id: Some("call_shell_exec_error".to_string()),
        pending_confirmation_options: None,
    });

    // Check result after first update
    app.update();
    let results = {
        let world = app.world_mut();
        let mut query = world.query::<&harness::ToolExecutionResultMessage>();
        query.iter(world).cloned().collect::<Vec<_>>()
    };

    assert!(
        !results.is_empty(),
        "shell_exec should produce a result even on non-zero exit"
    );
    let output_json = results[0]
        .tool_output
        .clone()
        .expect("shell_exec should succeed");
    // Non-zero exit code should be "exited_with_error", not a tool error
    assert_eq!(output_json["status"], "exited_with_error");
    assert_eq!(output_json["exit_code"], 1);
}

#[test]
fn shell_stop_transitions_a_running_session_to_stopped() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, vec![]);

    app.update();

    let agent_id = spawn_agent(app.world_mut());
    let task_entity = app
        .world_mut()
        .spawn((
            Task::from_user_input_ready("shell stop", 3, default_channel()),
            ShortTermMemory::default(),
        ))
        .id();
    let task_id = app.world().get::<Task>(task_entity).unwrap().id;

    let start_request = AgentExecutionRequest {
        task_id,
        agent_id,
        request_kind: AgentRequestKind::ToolExecution {
            tool_name: "shell_start".to_string(),
        },
        prompt: String::new(),
        system_prompt: None,
        tools: vec![],
        conversation: None,
        work_item_id: None,
    };

    app.world_mut().spawn(ToolExecutionRequestMessage {
        request: start_request,
        tool_name: "shell_start".to_string(),
        tool_input: serde_json::json!({
            "command": "sleep 30"
        }),
        pending_confirmation_id: None,
        tool_call_id: Some("call_shell_start_for_stop".to_string()),
        pending_confirmation_options: None,
    });

    app.update();

    let session_id = {
        let backend = app.world().resource::<harness::NativeProcessBackend>();
        backend
            .list_task_sessions(task_id)
            .unwrap()
            .first()
            .unwrap()
            .handle_id
            .to_string()
    };

    let stop_request = AgentExecutionRequest {
        task_id,
        agent_id,
        request_kind: AgentRequestKind::ToolExecution {
            tool_name: "shell_stop".to_string(),
        },
        prompt: String::new(),
        system_prompt: None,
        tools: vec![],
        conversation: None,
        work_item_id: None,
    };

    app.world_mut().spawn(ToolExecutionRequestMessage {
        request: stop_request,
        tool_name: "shell_stop".to_string(),
        tool_input: serde_json::json!({
            "session_id": session_id
        }),
        pending_confirmation_id: None,
        tool_call_id: Some("call_shell_stop".to_string()),
        pending_confirmation_options: None,
    });

    app.update();

    let results = {
        let world = app.world_mut();
        let mut query = world.query::<&harness::ToolExecutionResultMessage>();
        query.iter(world).cloned().collect::<Vec<_>>()
    };

    let last = results.last().unwrap().tool_output.clone().unwrap();
    assert_eq!(last["status"], "stopped");
}

#[test]
fn shell_input_returns_backend_backed_status() {
    let backend = harness::NativeProcessBackend::default();
    let task_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();

    let handle = backend
        .start_session(harness::SessionStartRequest {
            command: "cat".to_string(),
            session_name: None,
            cwd: None,
            env: std::collections::HashMap::new(),
            timeout_secs: None,
            tail_lines: 20,
            owner_task_id: task_id,
            owner_agent_id: agent_id,
        })
        .expect("start_session should succeed");

    let updated = backend
        .input_session(harness::SessionInputRequest {
            handle_id: handle.handle_id,
            input: "hello".to_string(),
            append_newline: true,
        })
        .expect("input_session should succeed when stdin is available");
    let accepted = harness::ShellSessionResult::accepted_input(&updated);

    assert_eq!(updated.handle_id, handle.handle_id);
    assert_eq!(updated.status, harness::SessionStatus::Running);
    assert_eq!(accepted.session_id, handle.handle_id.to_string());
    assert_eq!(accepted.status, harness::SessionStatus::Running);
    assert!(accepted.accepted);

    let _ = backend.stop_session(handle.handle_id);
}

/// 验证 `shell_input` 在 stdin 句柄缺失时返回执行错误，而不是伪装成 accepted。
#[test]
fn shell_input_returns_error_when_stdin_is_unavailable() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, vec![]);

    app.update();

    let agent_id = spawn_agent(app.world_mut());
    let mut task = Task::from_user_input_ready("shell input missing stdin", 3, default_channel());
    task.status = harness::TaskStatus::Waiting(harness::WaitingReason::ToolExecution);
    let task_entity = app
        .world_mut()
        .spawn((task, ShortTermMemory::default()))
        .id();
    let task_id = app.world().get::<Task>(task_entity).unwrap().id;

    app.world_mut().spawn(ToolExecutionRequestMessage {
        request: AgentExecutionRequest {
            task_id,
            agent_id,
            request_kind: AgentRequestKind::ToolExecution {
                tool_name: "shell_start".to_string(),
            },
            prompt: String::new(),
            system_prompt: None,
            tools: vec![],
            conversation: None,
            work_item_id: None,
        },
        tool_name: "shell_start".to_string(),
        tool_input: serde_json::json!({
            "command": "cat"
        }),
        pending_confirmation_id: None,
        tool_call_id: Some("call_shell_start_missing_stdin".to_string()),
        pending_confirmation_options: None,
    });
    app.update();

    let session_id = {
        let backend = app.world().resource::<harness::NativeProcessBackend>();
        let session_id = backend
            .list_task_sessions(task_id)
            .unwrap()
            .first()
            .unwrap()
            .handle_id;
        backend.stdins.lock().unwrap().remove(&session_id);
        session_id.to_string()
    };

    app.world_mut().spawn(ToolExecutionRequestMessage {
        request: AgentExecutionRequest {
            task_id,
            agent_id,
            request_kind: AgentRequestKind::ToolExecution {
                tool_name: "shell_input".to_string(),
            },
            prompt: String::new(),
            system_prompt: None,
            tools: vec![],
            conversation: None,
            work_item_id: None,
        },
        tool_name: "shell_input".to_string(),
        tool_input: serde_json::json!({
            "session_id": session_id,
            "input": "hello",
            "append_newline": true
        }),
        pending_confirmation_id: None,
        tool_call_id: Some("call_shell_input_missing_stdin".to_string()),
        pending_confirmation_options: None,
    });
    app.update();

    let results = {
        let world = app.world_mut();
        let mut query = world.query::<&harness::ToolExecutionResultMessage>();
        query
            .iter(world)
            .filter(|result| {
                result.tool_name == "shell_input"
                    && result.tool_call_id.as_deref() == Some("call_shell_input_missing_stdin")
            })
            .cloned()
            .collect::<Vec<_>>()
    };

    assert_eq!(results.len(), 1, "shell_input should produce one result");
    let error = results[0]
        .tool_output
        .clone()
        .expect_err("missing stdin should fail instead of reporting accepted");
    assert!(
        matches!(error, harness::ToolError::ExecutionFailed(_)),
        "expected ExecutionFailed, got {:?}",
        error
    );
}

#[test]
fn shell_read_backend_returns_latest_snapshot() {
    let backend = harness::NativeProcessBackend::default();
    let handle = backend
        .exec_blocking(harness::SessionStartRequest {
            command: "printf 'line1\\nline2\\nline3\\n'".to_string(),
            session_name: Some("cursor-test".to_string()),
            cwd: None,
            env: std::collections::HashMap::new(),
            timeout_secs: None,
            tail_lines: 2,
            owner_task_id: Uuid::new_v4(),
            owner_agent_id: Uuid::new_v4(),
        })
        .expect("exec_blocking should succeed");

    let first = backend
        .read_session(harness::SessionReadRequest {
            handle_id: handle.handle_id,
            tail_lines: 2,
        })
        .expect("read_session should succeed");

    assert!(first.output.output.contains("line2"));
    assert!(first.output.output.contains("line3"));
}

#[test]
fn shell_exec_and_shell_start_share_core_result_fields() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, vec![]);

    app.update();

    let agent_id = spawn_agent(app.world_mut());
    let task_entity = app
        .world_mut()
        .spawn((
            Task::from_user_input_ready("shell shape", 3, default_channel()),
            ShortTermMemory::default(),
        ))
        .id();
    let task_id = app.world().get::<Task>(task_entity).unwrap().id;

    let make_request = |tool_name: &str| AgentExecutionRequest {
        task_id,
        agent_id,
        request_kind: AgentRequestKind::ToolExecution {
            tool_name: tool_name.to_string(),
        },
        prompt: String::new(),
        system_prompt: None,
        tools: vec![],
        conversation: None,
        work_item_id: None,
    };

    app.world_mut().spawn(ToolExecutionRequestMessage {
        request: make_request("shell_exec"),
        tool_name: "shell_exec".to_string(),
        tool_input: serde_json::json!({ "command": "printf 'ok\\n'" }),
        pending_confirmation_id: None,
        tool_call_id: Some("call_shape_exec".to_string()),
        pending_confirmation_options: None,
    });
    app.update();

    app.world_mut().spawn(ToolExecutionRequestMessage {
        request: make_request("shell_start"),
        tool_name: "shell_start".to_string(),
        tool_input: serde_json::json!({ "command": "sleep 1" }),
        pending_confirmation_id: None,
        tool_call_id: Some("call_shape_start".to_string()),
        pending_confirmation_options: None,
    });
    app.update();

    let outputs = {
        let world = app.world_mut();
        let mut query = world.query::<&harness::ToolExecutionResultMessage>();
        query
            .iter(world)
            .map(|result| result.tool_output.clone().unwrap())
            .collect::<Vec<_>>()
    };

    for output in outputs {
        assert!(output.get("status").is_some());
        assert!(output.get("session_id").is_some() || output.get("timed_out").is_some());
    }
}

#[test]
fn shell_exec_timeout_returns_stopped_and_timed_out() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, vec![]);

    app.update();

    let agent_id = spawn_agent(app.world_mut());
    let task_entity = app
        .world_mut()
        .spawn((
            Task::from_user_input_ready("shell timeout", 3, default_channel()),
            ShortTermMemory::default(),
        ))
        .id();
    let task_id = app.world().get::<Task>(task_entity).unwrap().id;

    let request = AgentExecutionRequest {
        task_id,
        agent_id,
        request_kind: AgentRequestKind::ToolExecution {
            tool_name: "shell_exec".to_string(),
        },
        prompt: String::new(),
        system_prompt: None,
        tools: vec![],
        conversation: None,
        work_item_id: None,
    };

    app.world_mut().spawn(ToolExecutionRequestMessage {
        request,
        tool_name: "shell_exec".to_string(),
        tool_input: serde_json::json!({
            "command": "sleep 2",
            "timeout_secs": 0
        }),
        pending_confirmation_id: None,
        tool_call_id: Some("call_shell_exec_timeout".to_string()),
        pending_confirmation_options: None,
    });

    app.update();

    let results = {
        let world = app.world_mut();
        let mut query = world.query::<&harness::ToolExecutionResultMessage>();
        query.iter(world).cloned().collect::<Vec<_>>()
    };

    let output_json = results[0].tool_output.clone().unwrap();
    assert_eq!(output_json["status"], "stopped");
    assert_eq!(output_json["timed_out"], true);
}

#[test]
fn shell_read_returns_output_text() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, vec![]);

    app.update();

    let agent_id = spawn_agent(app.world_mut());
    let task_entity = app
        .world_mut()
        .spawn((
            Task::from_user_input_ready("shell read", 3, default_channel()),
            ShortTermMemory::default(),
        ))
        .id();
    let task_id = app.world().get::<Task>(task_entity).unwrap().id;

    let start_request = AgentExecutionRequest {
        task_id,
        agent_id,
        request_kind: AgentRequestKind::ToolExecution {
            tool_name: "shell_start".to_string(),
        },
        prompt: String::new(),
        system_prompt: None,
        tools: vec![],
        conversation: None,
        work_item_id: None,
    };

    app.world_mut().spawn(ToolExecutionRequestMessage {
        request: start_request,
        tool_name: "shell_start".to_string(),
        tool_input: serde_json::json!({
            "command": "printf 'x\\ny\\n'; sleep 1"
        }),
        pending_confirmation_id: None,
        tool_call_id: Some("call_shell_start_for_read".to_string()),
        pending_confirmation_options: None,
    });
    app.update();

    let session_id = {
        let world = app.world_mut();
        let mut query = world.query::<&harness::ToolExecutionResultMessage>();
        let results = query.iter(world).cloned().collect::<Vec<_>>();
        results
            .iter()
            .find(|result| result.tool_name == "shell_start")
            .unwrap()
            .tool_output
            .clone()
            .unwrap()["session_id"]
            .as_str()
            .unwrap()
            .to_string()
    };

    let read_request = AgentExecutionRequest {
        task_id,
        agent_id,
        request_kind: AgentRequestKind::ToolExecution {
            tool_name: "shell_read".to_string(),
        },
        prompt: String::new(),
        system_prompt: None,
        tools: vec![],
        conversation: None,
        work_item_id: None,
    };

    app.world_mut().spawn(ToolExecutionRequestMessage {
        request: read_request,
        tool_name: "shell_read".to_string(),
        tool_input: serde_json::json!({
            "session_id": session_id,
            "tail_lines": 1
        }),
        pending_confirmation_id: None,
        tool_call_id: Some("call_shell_read_text".to_string()),
        pending_confirmation_options: None,
    });
    app.update();

    let outputs = {
        let world = app.world_mut();
        let mut query = world.query::<&harness::ToolExecutionResultMessage>();
        query.iter(world).cloned().collect::<Vec<_>>()
    };

    let output_json = outputs.last().unwrap().tool_output.clone().unwrap();
    assert!(output_json["output"].is_string());
}

/// shell_exec 在超时时应返回 stopped + timed_out=true，并尽可能带上已产生的 tail 输出。
#[test]
fn shell_exec_times_out_returns_stopped_with_tail_output() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, vec![]);

    app.update();

    let agent_id = spawn_agent(app.world_mut());
    let task_entity = app
        .world_mut()
        .spawn((
            Task::from_user_input_ready("shell exec timeout", 3, default_channel()),
            ShortTermMemory::default(),
        ))
        .id();
    let task_id = app.world().get::<Task>(task_entity).unwrap().id;

    let request = AgentExecutionRequest {
        task_id,
        agent_id,
        request_kind: AgentRequestKind::ToolExecution {
            tool_name: "shell_exec".to_string(),
        },
        prompt: String::new(),
        system_prompt: None,
        tools: vec![],
        conversation: None,
        work_item_id: None,
    };

    app.world_mut().spawn(ToolExecutionRequestMessage {
        request,
        tool_name: "shell_exec".to_string(),
        tool_input: serde_json::json!({
            "command": "i=0; while true; do echo tick-$i; i=$((i+1)); sleep 0.02; done",
            "timeout_secs": 1,
            "tail_lines": 20
        }),
        pending_confirmation_id: None,
        tool_call_id: Some("call_shell_exec_timeout_with_tail".to_string()),
        pending_confirmation_options: None,
    });

    app.update();

    let results = {
        let world = app.world_mut();
        let mut query = world.query::<&harness::ToolExecutionResultMessage>();
        query.iter(world).cloned().collect::<Vec<_>>()
    };

    assert!(
        !results.is_empty(),
        "shell_exec should produce a ToolExecutionResultMessage"
    );
    let output_json = results[0]
        .tool_output
        .clone()
        .expect("shell_exec should succeed");

    assert_eq!(output_json["status"], "stopped");
    assert_eq!(output_json["timed_out"], true);

    let output = output_json["output"].as_str().unwrap_or_default();
    assert!(
        output.contains("tick-"),
        "timeout result should carry partial output tail"
    );
}

/// 验证省略 `timeout_secs` 时，`shell_exec` 会使用配置中的默认超时。
#[test]
fn shell_exec_uses_default_timeout_when_omitted() {
    let mut config = test_config();
    config.shell_default_exec_timeout_secs = 1;

    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(config, runtime, executor, input_rx, vec![]);

    app.update();
    let agent_id = spawn_agent(app.world_mut());
    let task_entity = app
        .world_mut()
        .spawn((
            Task::from_user_input_ready("shell exec timeout", 3, default_channel()),
            ShortTermMemory::default(),
        ))
        .id();
    let task_id = app.world().get::<Task>(task_entity).unwrap().id;

    app.world_mut().spawn(ToolExecutionRequestMessage {
        request: AgentExecutionRequest {
            task_id,
            agent_id,
            request_kind: AgentRequestKind::ToolExecution {
                tool_name: "shell_exec".to_string(),
            },
            prompt: String::new(),
            system_prompt: None,
            tools: vec![],
            conversation: None,
            work_item_id: None,
        },
        tool_name: "shell_exec".to_string(),
        tool_input: serde_json::json!({ "command": "sleep 2" }),
        pending_confirmation_id: None,
        tool_call_id: Some("call_exec_timeout_default".to_string()),
        pending_confirmation_options: None,
    });

    app.update();

    let world = app.world_mut();
    let mut query = world.query::<&harness::ToolExecutionResultMessage>();
    let results = query.iter(world).cloned().collect::<Vec<_>>();
    let output = results.last().unwrap().tool_output.clone().unwrap();

    assert_eq!(output["timed_out"], true);
}

/// 验证 `shell_input` 与 `shell_stop` 返回精简契约，不再暴露旧运行时冗余字段。
#[test]
fn shell_input_and_stop_follow_simplified_contract() {
    let backend = harness::NativeProcessBackend::default();
    let handle = backend
        .start_session(harness::SessionStartRequest {
            command: "cat".to_string(),
            session_name: None,
            cwd: None,
            env: std::collections::HashMap::new(),
            timeout_secs: None,
            tail_lines: 20,
            owner_task_id: Uuid::new_v4(),
            owner_agent_id: Uuid::new_v4(),
        })
        .expect("start_session should succeed");
    let session_id = handle.handle_id.to_string();

    let input_handle = backend
        .input_session(harness::SessionInputRequest {
            handle_id: handle.handle_id,
            input: "hello".to_string(),
            append_newline: true,
        })
        .expect("input_session should succeed");
    let input_output =
        serde_json::to_value(harness::ShellSessionResult::accepted_input(&input_handle))
            .expect("input result should serialize");
    assert_eq!(input_output["accepted"], true);
    assert_eq!(
        input_output["session_id"].as_str(),
        Some(session_id.as_str())
    );
    assert!(input_output["status"].is_string());
    let input_keys = input_output
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    let expected_input_keys = ["accepted", "session_id", "status"]
        .into_iter()
        .map(str::to_string)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(input_keys, expected_input_keys);

    let stopped_handle = backend
        .stop_session(handle.handle_id)
        .expect("stop_session should succeed");
    let stop_output = serde_json::to_value(harness::ShellSessionResult::stopped(&stopped_handle))
        .expect("stop result should serialize");
    assert_eq!(
        stop_output["session_id"].as_str(),
        Some(session_id.as_str())
    );
    assert_eq!(stop_output["status"], "stopped");
    let stop_keys = stop_output
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    let expected_stop_keys = ["session_id", "status"]
        .into_iter()
        .map(str::to_string)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(stop_keys, expected_stop_keys);
}

/// 验证 `shell_list` 只返回当前 Task 拥有的活动会话，不暴露其他 Task 的 session。
#[test]
fn shell_list_only_returns_sessions_for_current_task() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, vec![]);

    app.update();

    let agent_id = spawn_agent(app.world_mut());

    // Task A creates a session
    let task_a_entity = app
        .world_mut()
        .spawn((
            Task::from_user_input_ready("task A", 3, default_channel()),
            ShortTermMemory::default(),
        ))
        .id();
    let task_a_id = app.world().get::<Task>(task_a_entity).unwrap().id;

    app.world_mut().spawn(ToolExecutionRequestMessage {
        request: AgentExecutionRequest {
            task_id: task_a_id,
            agent_id,
            request_kind: AgentRequestKind::ToolExecution {
                tool_name: "shell_start".to_string(),
            },
            prompt: String::new(),
            system_prompt: None,
            tools: vec![],
            conversation: None,
            work_item_id: None,
        },
        tool_name: "shell_start".to_string(),
        tool_input: serde_json::json!({ "command": "sleep 5" }),
        pending_confirmation_id: None,
        tool_call_id: Some("call_start_task_a".to_string()),
        pending_confirmation_options: None,
    });
    app.update();

    // Task B calls shell_list - should NOT see Task A's session
    let task_b_entity = app
        .world_mut()
        .spawn((
            Task::from_user_input_ready("task B", 3, default_channel()),
            ShortTermMemory::default(),
        ))
        .id();
    let task_b_id = app.world().get::<Task>(task_b_entity).unwrap().id;

    app.world_mut().spawn(ToolExecutionRequestMessage {
        request: AgentExecutionRequest {
            task_id: task_b_id,
            agent_id,
            request_kind: AgentRequestKind::ToolExecution {
                tool_name: "shell_list".to_string(),
            },
            prompt: String::new(),
            system_prompt: None,
            tools: vec![],
            conversation: None,
            work_item_id: None,
        },
        tool_name: "shell_list".to_string(),
        tool_input: serde_json::json!({}),
        pending_confirmation_id: None,
        tool_call_id: Some("call_list_task_b".to_string()),
        pending_confirmation_options: None,
    });
    app.update();

    let world = app.world_mut();
    let mut query = world.query::<&harness::ToolExecutionResultMessage>();
    let list_result = query
        .iter(world)
        .filter(|m| m.tool_name == "shell_list")
        .cloned()
        .collect::<Vec<_>>();
    let list_output = list_result[0].tool_output.clone().unwrap();

    // Task B should see an empty list since it has no sessions
    assert!(list_output.is_array());
    assert!(
        list_output.as_array().unwrap().is_empty(),
        "shell_list should not expose sessions from other tasks"
    );
}

/// 验证 `shell_read` 拒绝访问其他 Task 创建的 session。
#[test]
fn shell_read_rejects_session_owned_by_another_task() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, vec![]);

    app.update();

    let agent_id = spawn_agent(app.world_mut());

    // Task A creates a session
    let task_a_entity = app
        .world_mut()
        .spawn((
            Task::from_user_input_ready("task A read", 3, default_channel()),
            ShortTermMemory::default(),
        ))
        .id();
    let task_a_id = app.world().get::<Task>(task_a_entity).unwrap().id;

    app.world_mut().spawn(ToolExecutionRequestMessage {
        request: AgentExecutionRequest {
            task_id: task_a_id,
            agent_id,
            request_kind: AgentRequestKind::ToolExecution {
                tool_name: "shell_start".to_string(),
            },
            prompt: String::new(),
            system_prompt: None,
            tools: vec![],
            conversation: None,
            work_item_id: None,
        },
        tool_name: "shell_start".to_string(),
        tool_input: serde_json::json!({ "command": "sleep 5" }),
        pending_confirmation_id: None,
        tool_call_id: Some("call_start_read_reject".to_string()),
        pending_confirmation_options: None,
    });
    app.update();

    let session_id = {
        let world = app.world_mut();
        let mut query = world.query::<&harness::ToolExecutionResultMessage>();
        let results = query.iter(world).cloned().collect::<Vec<_>>();
        results
            .iter()
            .find(|result| result.tool_name == "shell_start")
            .unwrap()
            .tool_output
            .clone()
            .unwrap()["session_id"]
            .as_str()
            .unwrap()
            .to_string()
    };

    // Task B tries to read Task A's session
    let task_b_entity = app
        .world_mut()
        .spawn((
            Task::from_user_input_ready("task B read", 3, default_channel()),
            ShortTermMemory::default(),
        ))
        .id();
    let task_b_id = app.world().get::<Task>(task_b_entity).unwrap().id;

    app.world_mut().spawn(ToolExecutionRequestMessage {
        request: AgentExecutionRequest {
            task_id: task_b_id,
            agent_id,
            request_kind: AgentRequestKind::ToolExecution {
                tool_name: "shell_read".to_string(),
            },
            prompt: String::new(),
            system_prompt: None,
            tools: vec![],
            conversation: None,
            work_item_id: None,
        },
        tool_name: "shell_read".to_string(),
        tool_input: serde_json::json!({ "session_id": session_id }),
        pending_confirmation_id: None,
        tool_call_id: Some("call_read_cross_task".to_string()),
        pending_confirmation_options: None,
    });
    app.update();

    let world = app.world_mut();
    let mut query = world.query::<&harness::ToolExecutionResultMessage>();
    let results = query
        .iter(world)
        .filter(|m| m.tool_name == "shell_read")
        .cloned()
        .collect::<Vec<_>>();
    let result = &results[0];

    // Should be a permission denied error
    assert!(
        result.tool_output.is_err(),
        "shell_read should reject cross-task access"
    );
    let error = result.tool_output.clone().unwrap_err();
    assert!(
        matches!(error, harness::ToolError::PermissionDenied(_)),
        "expected PermissionDenied, got {:?}",
        error
    );
}

/// 验证 `shell_input` 拒绝访问其他 Task 创建的 session。
#[test]
fn shell_input_rejects_session_owned_by_another_task() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, vec![]);

    app.update();

    let agent_id = spawn_agent(app.world_mut());

    // Task A creates a session
    let task_a_entity = app
        .world_mut()
        .spawn((
            Task::from_user_input_ready("task A input", 3, default_channel()),
            ShortTermMemory::default(),
        ))
        .id();
    let task_a_id = app.world().get::<Task>(task_a_entity).unwrap().id;

    app.world_mut().spawn(ToolExecutionRequestMessage {
        request: AgentExecutionRequest {
            task_id: task_a_id,
            agent_id,
            request_kind: AgentRequestKind::ToolExecution {
                tool_name: "shell_start".to_string(),
            },
            prompt: String::new(),
            system_prompt: None,
            tools: vec![],
            conversation: None,
            work_item_id: None,
        },
        tool_name: "shell_start".to_string(),
        tool_input: serde_json::json!({ "command": "cat" }),
        pending_confirmation_id: None,
        tool_call_id: Some("call_start_input_reject".to_string()),
        pending_confirmation_options: None,
    });
    app.update();

    let session_id = {
        let world = app.world_mut();
        let mut query = world.query::<&harness::ToolExecutionResultMessage>();
        let results = query.iter(world).cloned().collect::<Vec<_>>();
        results
            .iter()
            .find(|result| result.tool_name == "shell_start")
            .unwrap()
            .tool_output
            .clone()
            .unwrap()["session_id"]
            .as_str()
            .unwrap()
            .to_string()
    };

    // Task B tries to input to Task A's session
    let task_b_entity = app
        .world_mut()
        .spawn((
            Task::from_user_input_ready("task B input", 3, default_channel()),
            ShortTermMemory::default(),
        ))
        .id();
    let task_b_id = app.world().get::<Task>(task_b_entity).unwrap().id;

    app.world_mut().spawn(ToolExecutionRequestMessage {
        request: AgentExecutionRequest {
            task_id: task_b_id,
            agent_id,
            request_kind: AgentRequestKind::ToolExecution {
                tool_name: "shell_input".to_string(),
            },
            prompt: String::new(),
            system_prompt: None,
            tools: vec![],
            conversation: None,
            work_item_id: None,
        },
        tool_name: "shell_input".to_string(),
        tool_input: serde_json::json!({ "session_id": session_id, "input": "hello" }),
        pending_confirmation_id: None,
        tool_call_id: Some("call_input_cross_task".to_string()),
        pending_confirmation_options: None,
    });
    app.update();

    let world = app.world_mut();
    let mut query = world.query::<&harness::ToolExecutionResultMessage>();
    let results = query
        .iter(world)
        .filter(|m| m.tool_name == "shell_input")
        .cloned()
        .collect::<Vec<_>>();
    let result = &results[0];

    assert!(
        result.tool_output.is_err(),
        "shell_input should reject cross-task access"
    );
    let error = result.tool_output.clone().unwrap_err();
    assert!(
        matches!(error, harness::ToolError::PermissionDenied(_)),
        "expected PermissionDenied, got {:?}",
        error
    );
}

/// 验证 `shell_stop` 拒绝访问其他 Task 创建的 session。
#[test]
fn shell_stop_rejects_session_owned_by_another_task() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, vec![]);

    app.update();

    let agent_id = spawn_agent(app.world_mut());

    // Task A creates a session
    let task_a_entity = app
        .world_mut()
        .spawn((
            Task::from_user_input_ready("task A stop", 3, default_channel()),
            ShortTermMemory::default(),
        ))
        .id();
    let task_a_id = app.world().get::<Task>(task_a_entity).unwrap().id;

    app.world_mut().spawn(ToolExecutionRequestMessage {
        request: AgentExecutionRequest {
            task_id: task_a_id,
            agent_id,
            request_kind: AgentRequestKind::ToolExecution {
                tool_name: "shell_start".to_string(),
            },
            prompt: String::new(),
            system_prompt: None,
            tools: vec![],
            conversation: None,
            work_item_id: None,
        },
        tool_name: "shell_start".to_string(),
        tool_input: serde_json::json!({ "command": "sleep 5" }),
        pending_confirmation_id: None,
        tool_call_id: Some("call_start_stop_reject".to_string()),
        pending_confirmation_options: None,
    });
    app.update();

    let session_id = {
        let world = app.world_mut();
        let mut query = world.query::<&harness::ToolExecutionResultMessage>();
        let results = query.iter(world).cloned().collect::<Vec<_>>();
        results
            .iter()
            .find(|result| result.tool_name == "shell_start")
            .unwrap()
            .tool_output
            .clone()
            .unwrap()["session_id"]
            .as_str()
            .unwrap()
            .to_string()
    };

    // Task B tries to stop Task A's session
    let task_b_entity = app
        .world_mut()
        .spawn((
            Task::from_user_input_ready("task B stop", 3, default_channel()),
            ShortTermMemory::default(),
        ))
        .id();
    let task_b_id = app.world().get::<Task>(task_b_entity).unwrap().id;

    app.world_mut().spawn(ToolExecutionRequestMessage {
        request: AgentExecutionRequest {
            task_id: task_b_id,
            agent_id,
            request_kind: AgentRequestKind::ToolExecution {
                tool_name: "shell_stop".to_string(),
            },
            prompt: String::new(),
            system_prompt: None,
            tools: vec![],
            conversation: None,
            work_item_id: None,
        },
        tool_name: "shell_stop".to_string(),
        tool_input: serde_json::json!({ "session_id": session_id }),
        pending_confirmation_id: None,
        tool_call_id: Some("call_stop_cross_task".to_string()),
        pending_confirmation_options: None,
    });
    app.update();

    let world = app.world_mut();
    let mut query = world.query::<&harness::ToolExecutionResultMessage>();
    let results = query
        .iter(world)
        .filter(|m| m.tool_name == "shell_stop")
        .cloned()
        .collect::<Vec<_>>();
    let result = &results[0];

    assert!(
        result.tool_output.is_err(),
        "shell_stop should reject cross-task access"
    );
    let error = result.tool_output.clone().unwrap_err();
    assert!(
        matches!(error, harness::ToolError::PermissionDenied(_)),
        "expected PermissionDenied, got {:?}",
        error
    );
}

/// 验证 Task 进入 Done 终态时，关联的活动 shell session 会被自动关闭。
#[test]
fn task_termination_stops_owned_shell_sessions() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, vec![]);

    app.update();

    let agent_id = spawn_agent(app.world_mut());

    // Create a task and start a long-running session
    let task_entity = app
        .world_mut()
        .spawn((
            Task::from_user_input_ready("task termination test", 3, default_channel()),
            ShortTermMemory::default(),
        ))
        .id();
    let task_id = app.world().get::<Task>(task_entity).unwrap().id;

    app.world_mut().spawn(ToolExecutionRequestMessage {
        request: AgentExecutionRequest {
            task_id,
            agent_id,
            request_kind: AgentRequestKind::ToolExecution {
                tool_name: "shell_start".to_string(),
            },
            prompt: String::new(),
            system_prompt: None,
            tools: vec![],
            conversation: None,
            work_item_id: None,
        },
        tool_name: "shell_start".to_string(),
        tool_input: serde_json::json!({ "command": "sleep 30" }),
        pending_confirmation_id: None,
        tool_call_id: Some("call_start_termination".to_string()),
        pending_confirmation_options: None,
    });
    app.update();

    // Verify session is running
    let backend = app.world().resource::<harness::NativeProcessBackend>();
    let task_sessions = backend.list_task_sessions(task_id).unwrap();
    assert_eq!(
        task_sessions.len(),
        1,
        "task should have 1 active session before termination"
    );

    // Mark task as Done
    {
        let mut task = app.world_mut().get_mut::<Task>(task_entity).unwrap();
        task.status = harness::TaskStatus::Done;
    }

    // Drive app update to trigger task_termination_system
    app.update();

    // Verify session has been stopped
    let backend = app.world().resource::<harness::NativeProcessBackend>();
    let task_sessions = backend.list_task_sessions(task_id).unwrap();
    assert!(
        task_sessions.is_empty(),
        "task's sessions should be stopped after task termination"
    );
}

/// 验证 Task 进入 Failed 终态时，关联的活动 shell session 也会被自动关闭。
#[test]
fn failed_task_also_stops_owned_shell_sessions() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, vec![]);

    app.update();

    let agent_id = spawn_agent(app.world_mut());

    // Create a task and start a long-running session
    let task_entity = app
        .world_mut()
        .spawn((
            Task::from_user_input_ready("failed task test", 3, default_channel()),
            ShortTermMemory::default(),
        ))
        .id();
    let task_id = app.world().get::<Task>(task_entity).unwrap().id;

    app.world_mut().spawn(ToolExecutionRequestMessage {
        request: AgentExecutionRequest {
            task_id,
            agent_id,
            request_kind: AgentRequestKind::ToolExecution {
                tool_name: "shell_start".to_string(),
            },
            prompt: String::new(),
            system_prompt: None,
            tools: vec![],
            conversation: None,
            work_item_id: None,
        },
        tool_name: "shell_start".to_string(),
        tool_input: serde_json::json!({ "command": "sleep 30" }),
        pending_confirmation_id: None,
        tool_call_id: Some("call_start_failed_task".to_string()),
        pending_confirmation_options: None,
    });
    app.update();

    // Verify session is running
    let backend = app.world().resource::<harness::NativeProcessBackend>();
    let task_sessions = backend.list_task_sessions(task_id).unwrap();
    assert_eq!(
        task_sessions.len(),
        1,
        "task should have 1 active session before failure"
    );

    // Mark task as Failed
    {
        let mut task = app.world_mut().get_mut::<Task>(task_entity).unwrap();
        task.status = harness::TaskStatus::Failed(harness::FailureReason::AgentError);
    }

    // Drive app update to trigger task_termination_system
    app.update();

    // Verify session has been stopped
    let backend = app.world().resource::<harness::NativeProcessBackend>();
    let task_sessions = backend.list_task_sessions(task_id).unwrap();
    assert!(
        task_sessions.is_empty(),
        "task's sessions should be stopped after task failure"
    );
}
