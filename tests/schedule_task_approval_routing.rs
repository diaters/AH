//! schedule_task 动态任务审批路由集成测试
//!
//! 验证由 schedule_task 创建的一次性动态任务触发后，执行 Agent 调用需要确认的
//! shell_exec 工具时，审批请求被正确路由到任务 output_channel 指定的 QQ 用户。

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use crossbeam_channel::unbounded;
use harness::triggers::{ScheduledTaskInfo, ScheduledTaskRegistry};
use harness::{
    AgentExecutionOutput, AgentExecutionRequest, AgentExecutor, AgentRequestKind, ChannelId,
    EngineEvent, EventTarget, ExecutorFuture, Frontend, FrontendKind, HarnessConfig, LlmToolCall,
    OutputContent, Task, TaskStatus, TaskTrigger, build_harness_app, llm::ExecutorRegistry,
};
use tokio::runtime::Runtime;
use uuid::Uuid;

fn qq_channel(user_id: &str) -> ChannelId {
    ChannelId {
        frontend: FrontendKind::QQ,
        user_id: user_id.to_string(),
        thread_id: None,
    }
}

fn text_output(text: &str) -> AgentExecutionOutput {
    AgentExecutionOutput {
        content: OutputContent::Text(text.to_string()),
        reasoning_content: None,
    }
}

fn tool_calls_output(calls: Vec<LlmToolCall>) -> AgentExecutionOutput {
    AgentExecutionOutput {
        content: OutputContent::ToolCalls(calls),
        reasoning_content: None,
    }
}

fn shell_exec_call(id: &str, command: &str) -> LlmToolCall {
    LlmToolCall {
        id: id.to_string(),
        name: "shell_exec".to_string(),
        arguments: serde_json::json!({ "command": command }).to_string(),
    }
}

/// 按顺序返回预设 LLM 输出的执行器。
struct CannedExecutor {
    responses: Mutex<VecDeque<AgentExecutionOutput>>,
}

impl CannedExecutor {
    fn new(responses: Vec<AgentExecutionOutput>) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
        }
    }
}

impl AgentExecutor for CannedExecutor {
    fn execute(&self, request: AgentExecutionRequest) -> ExecutorFuture {
        // 治理型 WorkItem / 非普通 LLM 请求直接返回占位文本，避免干扰主流程。
        if request.work_item_id.is_some() || request.request_kind != AgentRequestKind::LlmCompletion
        {
            return Box::pin(async move { Ok(text_output("ok")) });
        }

        let response = self.responses.lock().unwrap().pop_front();
        Box::pin(async move { Ok(response.unwrap_or_else(|| text_output("done"))) })
    }
}

/// 捕获所有前端事件的 QQ MockFrontend。
struct CapturingQQFrontend {
    events: Arc<Mutex<Vec<EngineEvent>>>,
}

impl Frontend for CapturingQQFrontend {
    fn kind(&self) -> FrontendKind {
        FrontendKind::QQ
    }

    fn push_event(&self, event: EngineEvent) {
        self.events.lock().unwrap().push(event);
    }

    fn poll_actions(&self) -> Vec<harness::UserAction> {
        vec![]
    }
}

#[test]
fn scheduled_task_approval_request_routes_to_output_channel() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let (_input_tx, input_rx) = unbounded();
    let runtime = Arc::new(Runtime::new().expect("tokio runtime should be created"));
    let executor: Arc<dyn AgentExecutor> =
        Arc::new(CannedExecutor::new(vec![tool_calls_output(vec![
            shell_exec_call("call_1", "echo scheduled"),
        ])]));
    let executor_registry = ExecutorRegistry::from_single_executor(executor, "default");
    let (channel_manager, _) = harness::channels::ChannelManager::empty();

    let mut app = build_harness_app(
        HarnessConfig::default(),
        runtime,
        executor_registry,
        input_rx,
        vec![Box::new(CapturingQQFrontend {
            events: events.clone(),
        })],
        channel_manager,
    );

    let task_id = Uuid::new_v4();
    let kind = format!("scheduled:{}", task_id);
    let output_channel = qq_channel("qq-test-group");

    {
        let mut registry = app.world_mut().resource_mut::<ScheduledTaskRegistry>();
        registry.insert(
            kind.clone(),
            ScheduledTaskInfo {
                content: "execute scheduled command".to_string(),
                output_channel: Some(output_channel.clone()),
                is_once: true,
            },
        );
    }

    app.world_mut().spawn(harness::domain::TriggerTaskMessage {
        source: harness::domain::SignalSource("timer".to_string()),
        trigger: TaskTrigger::Timer { kind: kind.clone() },
    });

    // 运行足够帧数，让任务创建、Agent 执行、工具确认请求生成并推送到前端。
    for _ in 0..10 {
        app.update();
    }

    let approval_requests: Vec<_> = {
        let events = events.lock().unwrap();
        events
            .iter()
            .filter_map(|event| match event {
                EngineEvent::ApprovalRequest { target, .. } => Some(target.clone()),
                _ => None,
            })
            .collect()
    };

    assert!(
        !approval_requests.is_empty(),
        "QQ MockFrontend 应收到审批请求"
    );

    let directed = approval_requests
        .iter()
        .find_map(|target| match target {
            EventTarget::Directed(channels) => Some(channels.clone()),
            EventTarget::Broadcast => None,
        })
        .expect("审批请求应为 Directed 目标");

    assert_eq!(
        directed,
        vec![output_channel.clone()],
        "审批请求目标应与 scheduled task 的 output_channel 一致"
    );

    let task = app
        .world_mut()
        .query::<&Task>()
        .iter(app.world())
        .find(|task| task.routing_policy.output_channel == Some(output_channel.clone()))
        .expect("scheduled task 应已创建");

    assert!(
        !matches!(task.status, TaskStatus::Failed(_)),
        "任务不应进入 Failed 状态，当前状态: {:?}",
        task.status
    );
}
