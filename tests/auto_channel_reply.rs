use std::{sync::Arc, time::Duration};

use crossbeam_channel::unbounded;
use harness::{
    Agent, AgentCapabilities, AgentExecutionOutput, AgentExecutionRequest, AgentExecutor,
    AgentKind, AgentProfile, AgentRequestKind, AgentToolPermissions, BrainConfig, ChannelId,
    EntityIndex, ExecutorFuture, ExternalInput, FrontendKind, HarnessConfig, OutputContent,
    build_harness_app,
    channels::{Channel, ChannelManager, TelegramChannel, TelegramConfig},
    llm::ExecutorRegistry,
};
use tokio::runtime::Runtime;
use uuid::Uuid;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn extract_short_id_from_text(text: &str) -> Option<String> {
    let start = text.find('[')? + 1;
    let end = text[start..].find(']')? + start;
    Some(text[start..end].to_string())
}

/// 一个极简的 Executor：
/// - BrainDecision 请求返回 JSON 决策（选择 default-llm-agent）
/// - 其他请求返回固定文本作为 Agent 回复
struct EchoExecutor;

impl AgentExecutor for EchoExecutor {
    fn execute(&self, request: AgentExecutionRequest) -> ExecutorFuture {
        match request.request_kind {
            AgentRequestKind::BrainDecision => Box::pin(async move {
                Ok(AgentExecutionOutput {
                    content: OutputContent::Text(
                        r#"{"agent_name":"default-llm-agent","skill_name":null}"#.to_string(),
                    ),
                    reasoning_content: None,
                })
            }),
            _ => Box::pin(async move {
                Ok(AgentExecutionOutput {
                    content: OutputContent::Text("echo reply".to_string()),
                    reasoning_content: None,
                })
            }),
        }
    }
}

/// 生成启用 Brain 的测试配置（auto_channel_reply 测试需要走 BrainLlm 派发路径）。
fn brain_enabled_config() -> HarnessConfig {
    HarnessConfig {
        brain: Some(BrainConfig { enabled: true }),
        ..Default::default()
    }
}

/// Spawn Brain Agent 与 default-llm-agent（auto_channel_reply 测试需要两者）。
///
/// 同时写入 `EntityIndex.agents`，确保 `dispatch_system` 能经 index O(1) 解析
/// brain_agent_id → Entity（ADR-005 §3 阶段 2 要求）。
fn spawn_brain_and_default_agent(app: &mut bevy_app::App) {
    let brain_agent = Agent {
        id: Uuid::new_v4(),
        profile: AgentProfile {
            name: "brain".to_string(),
            model: "gpt-4.1-mini".to_string(),
        },
        capabilities: AgentCapabilities {
            tags: vec!["brain".to_string()],
            description: "Brain Agent".to_string(),
        },
        kind: AgentKind::Persistent,
        parent_id: None,
        bound_task_id: None,
        tool_permissions: AgentToolPermissions::default(),
        system_prompt: None,
    };
    let brain_id = brain_agent.id;
    let brain_entity = app
        .world_mut()
        .spawn((brain_agent, harness::LongTermMemory::default()))
        .id();
    app.world_mut()
        .resource_mut::<EntityIndex>()
        .agents
        .insert(brain_id, brain_entity);

    let default_agent = Agent {
        id: Uuid::new_v4(),
        profile: AgentProfile {
            name: "default-llm-agent".to_string(),
            model: "gpt-4.1-mini".to_string(),
        },
        capabilities: AgentCapabilities {
            tags: vec!["llm".to_string(), "default".to_string()],
            description: "默认 Agent".to_string(),
        },
        kind: AgentKind::Persistent,
        parent_id: None,
        bound_task_id: None,
        tool_permissions: AgentToolPermissions::default(),
        system_prompt: None,
    };
    let default_id = default_agent.id;
    let default_entity = app
        .world_mut()
        .spawn((default_agent, harness::LongTermMemory::default()))
        .id();
    app.world_mut()
        .resource_mut::<EntityIndex>()
        .agents
        .insert(default_id, default_entity);
}

/// 验证来自 Telegram 的输入经 Agent 处理后，回复会自动通过 sendMessage 返回。
#[test]
fn auto_channel_reply() {
    let rt = Arc::new(Runtime::new().unwrap());

    rt.block_on(async {
        let mock_server = MockServer::start().await;
        let bot_token = "test-token";

        Mock::given(method("POST"))
            .and(path(format!("/bot{}/sendMessage", bot_token)))
            .and(body_string_contains(r#""chat_id":"123456""#))
            .and(body_string_contains(r#""parse_mode":"HTML""#))
            .and(body_string_contains("助手: echo reply"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": { "message_id": 42 }
            })))
            .expect(1)
            .mount(&mock_server)
            .await;

        let (input_tx, input_rx) = unbounded::<ExternalInput>();

        let cfg = TelegramConfig {
            bot_token: bot_token.to_string(),
            allowed_users: vec!["123456".to_string()],
            pairing_enabled: false,
            pairing_code: None,
        };
        let channel = Arc::new(TelegramChannel::new(cfg).with_base_url(mock_server.uri()))
            as Arc<dyn Channel>;
        let (channel_manager, _channel_handle, channel_frontends) =
            ChannelManager::new(vec![channel], input_tx.clone());

        let config = HarnessConfig {
            agents_config_path: "/nonexistent_agents.toml".to_string(),
            ..brain_enabled_config()
        };
        let executor: Arc<dyn AgentExecutor> = Arc::new(EchoExecutor);
        let executor_registry = ExecutorRegistry::from_single_executor(executor, "openai");

        let mut app = build_harness_app(
            config,
            rt.clone(),
            executor_registry,
            input_rx,
            channel_frontends,
            channel_manager,
        );

        // 初始化 Startup 系统
        app.update();

        // 手动 spawn Brain + default-llm-agent（因为 agents.toml 不存在）
        spawn_brain_and_default_agent(&mut app);

        // 注入一条来自 Telegram 的入向消息
        let origin = ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "123456".to_string(),
            thread_id: None,
        };
        input_tx
            .send(ExternalInput::TextWithChannel {
                channel: origin,
                content: "hello bot".to_string(),
            })
            .expect("send input");

        // 驱动 ECS 若干帧，让 Agent 执行、结果回传、出向消息发送到 mock
        for _ in 0..100 {
            app.update();
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        mock_server.verify().await;
    });
}

/// 验证多个并行任务发送到同一通道时，出站消息带有不同的短 ID 前缀。
#[test]
fn multi_task_channel_reply_has_different_short_ids() {
    let rt = Arc::new(Runtime::new().unwrap());

    rt.block_on(async {
        let mock_server = MockServer::start().await;
        let bot_token = "test-token";

        Mock::given(method("POST"))
            .and(path(format!("/bot{}/sendMessage", bot_token)))
            .and(body_string_contains(r#""chat_id":"123456""#))
            .and(body_string_contains("助手: echo reply"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": { "message_id": 42 }
            })))
            .expect(2)
            .mount(&mock_server)
            .await;

        let (input_tx, input_rx) = unbounded::<ExternalInput>();

        let cfg = TelegramConfig {
            bot_token: bot_token.to_string(),
            allowed_users: vec!["123456".to_string()],
            pairing_enabled: false,
            pairing_code: None,
        };
        let channel = Arc::new(TelegramChannel::new(cfg).with_base_url(mock_server.uri()))
            as Arc<dyn Channel>;
        let (channel_manager, _channel_handle, channel_frontends) =
            ChannelManager::new(vec![channel], input_tx.clone());

        let config = HarnessConfig {
            agents_config_path: "/nonexistent_agents.toml".to_string(),
            ..brain_enabled_config()
        };
        let executor: Arc<dyn AgentExecutor> = Arc::new(EchoExecutor);
        let executor_registry = ExecutorRegistry::from_single_executor(executor, "openai");

        let mut app = build_harness_app(
            config,
            rt.clone(),
            executor_registry,
            input_rx,
            channel_frontends,
            channel_manager,
        );

        app.update();

        // 手动 spawn Brain + default-llm-agent（因为 agents.toml 不存在）
        spawn_brain_and_default_agent(&mut app);

        let origin = ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "123456".to_string(),
            thread_id: None,
        };

        input_tx
            .send(ExternalInput::TextWithChannel {
                channel: origin.clone(),
                content: "first task".to_string(),
            })
            .expect("send first input");
        input_tx
            .send(ExternalInput::TextWithChannel {
                channel: origin,
                content: "second task".to_string(),
            })
            .expect("send second input");

        for _ in 0..200 {
            app.update();
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        mock_server.verify().await;

        let requests = mock_server
            .received_requests()
            .await
            .expect("received requests");
        let text_requests: Vec<_> = requests
            .into_iter()
            .filter(|req| {
                if req.body.is_empty() {
                    return false;
                }
                serde_json::from_slice::<serde_json::Value>(&req.body)
                    .ok()
                    .and_then(|body| body["text"].as_str().map(|t| t.to_string()))
                    .map(|t| t.contains("助手: echo reply"))
                    .unwrap_or(false)
            })
            .collect();
        assert_eq!(
            text_requests.len(),
            2,
            "expected two agent text outbound messages"
        );

        let ids: Vec<String> = text_requests
            .iter()
            .map(|req| {
                let body: serde_json::Value =
                    serde_json::from_slice(&req.body).expect("valid json");
                let text = body["text"].as_str().expect("text field");
                extract_short_id_from_text(text).expect("short id prefix")
            })
            .collect();

        assert_ne!(ids[0], ids[1], "two tasks should have different short ids");
    });
}
