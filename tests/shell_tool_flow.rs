//! shell 工具集成测试

use std::sync::Arc;

use crossbeam_channel::unbounded;
use harness::{
    Agent, AgentCapabilities, AgentExecutionOutput, AgentExecutionRequest, AgentExecutor,
    AgentExperience, AgentKind, AgentProfile, AgentRequestKind, AgentToolPermissions, ChannelId,
    ExecutorFuture, FrontendKind, HarnessConfig, ShortTermMemory, Task,
    ToolExecutionRequestMessage, build_harness_app, SessionBackend,
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
    // Check that we got the tail (last 2 lines: b\nc)
    let combined_tail = output_json["output"]["combined_tail"].as_str().unwrap();
    assert!(combined_tail.contains("b"), "tail should contain 'b'");
    assert!(combined_tail.contains("c"), "tail should contain 'c'");
    assert_eq!(output_json["output"]["combined_truncated"], true);
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
    assert!(output_json["handle_id"].is_string());
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
            "command": "sleep 5",
            "session_name": "stop-test"
        }),
        pending_confirmation_id: None,
        tool_call_id: Some("call_shell_start_for_stop".to_string()),
        pending_confirmation_options: None,
    });

    app.update();

    let handle_id = {
        let world = app.world_mut();
        let mut query = world.query::<&harness::ToolExecutionResultMessage>();
        let results = query.iter(world).cloned().collect::<Vec<_>>();
        results[0].tool_output.clone().unwrap()["handle_id"]
            .as_str()
            .unwrap()
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
            "handle_id": handle_id,
            "wait_for_exit": false
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
fn shell_wait_returns_completed_when_process_exits() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, vec![]);

    app.update();

    let agent_id = spawn_agent(app.world_mut());
    let task_entity = app
        .world_mut()
        .spawn((
            Task::from_user_input_ready("shell wait", 3, default_channel()),
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
            "command": "sleep 0.1"
        }),
        pending_confirmation_id: None,
        tool_call_id: Some("call_shell_start_for_wait".to_string()),
        pending_confirmation_options: None,
    });
    app.update();

    let handle_id = {
        let world = app.world_mut();
        let mut query = world.query::<&harness::ToolExecutionResultMessage>();
        let results = query.iter(world).cloned().collect::<Vec<_>>();
        results[0].tool_output.clone().unwrap()["handle_id"]
            .as_str()
            .unwrap()
            .to_string()
    };

    let wait_request = AgentExecutionRequest {
        task_id,
        agent_id,
        request_kind: AgentRequestKind::ToolExecution {
            tool_name: "shell_wait".to_string(),
        },
        prompt: String::new(),
        system_prompt: None,
        tools: vec![],
        conversation: None,
        work_item_id: None,
    };

    app.world_mut().spawn(ToolExecutionRequestMessage {
        request: wait_request,
        tool_name: "shell_wait".to_string(),
        tool_input: serde_json::json!({
            "handle_id": handle_id,
            "timeout_secs": 2
        }),
        pending_confirmation_id: None,
        tool_call_id: Some("call_shell_wait".to_string()),
        pending_confirmation_options: None,
    });

    let mut wait_result = None;
    for _ in 0..20 {
        app.update();
        std::thread::sleep(std::time::Duration::from_millis(20));

        // Check for results immediately after each update (before tool_result_system despawns them)
        {
            let app = app.world_mut();
            let mut result_query = app.query::<&harness::ToolExecutionResultMessage>();
            let results: Vec<_> = result_query.iter(app).cloned().collect();
            if let Some(result) = results.iter().find(|r| r.tool_name == "shell_wait") {
                wait_result = Some(result.clone());
                break;
            }
        }
    }

    let wait_result = wait_result.expect("shell_wait result should be present");
    let output_json = wait_result.tool_output.clone().unwrap();
    assert_eq!(output_json["status"], "completed");
}

#[test]
fn shell_send_input_returns_backend_backed_status() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, vec![]);

    app.update();

    let agent_id = spawn_agent(app.world_mut());
    let task_entity = app
        .world_mut()
        .spawn((
            Task::from_user_input_ready("shell input", 3, default_channel()),
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
            "command": "cat"
        }),
        pending_confirmation_id: None,
        tool_call_id: Some("call_shell_start_for_input".to_string()),
        pending_confirmation_options: None,
    });
    app.update();

    let handle_id = {
        let world = app.world_mut();
        let mut query = world.query::<&harness::ToolExecutionResultMessage>();
        let results = query.iter(world).cloned().collect::<Vec<_>>();
        results[0].tool_output.clone().unwrap()["handle_id"]
            .as_str()
            .unwrap()
            .to_string()
    };

    let input_request = AgentExecutionRequest {
        task_id,
        agent_id,
        request_kind: AgentRequestKind::ToolExecution {
            tool_name: "shell_send_input".to_string(),
        },
        prompt: String::new(),
        system_prompt: None,
        tools: vec![],
        conversation: None,
        work_item_id: None,
    };

    app.world_mut().spawn(ToolExecutionRequestMessage {
        request: input_request,
        tool_name: "shell_send_input".to_string(),
        tool_input: serde_json::json!({
            "handle_id": handle_id,
            "input": "hello",
            "append_newline": true
        }),
        pending_confirmation_id: None,
        tool_call_id: Some("call_shell_send_input".to_string()),
        pending_confirmation_options: None,
    });

    // Check result after first update (tool_result_system despawns results after processing)
    app.update();

    let results = {
        let world = app.world_mut();
        let mut query = world.query::<&harness::ToolExecutionResultMessage>();
        query.iter(world).cloned().collect::<Vec<_>>()
    };

    let last = results.last().unwrap().tool_output.clone().unwrap();
    assert!(last["status"].is_string());
}

#[test]
fn shell_read_output_supports_cursor_progression() {
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
        .read_output(harness::SessionOutputRequest {
            handle_id: handle.handle_id,
            cursor: None,
            tail_lines: 2,
        })
        .expect("read_output should succeed");

    assert!(first.output.next_cursor.is_some());
}
