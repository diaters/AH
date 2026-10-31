mod common;

use std::{sync::Arc, time::Duration};

use common::mock_executor::FixedTextExecutor;
use crossbeam_channel::unbounded;
use harness::{
    app::build_harness_app,
    channels::{Channel, ChannelManager, TelegramChannel, TelegramConfig},
    domain::FrontendKind,
    domain::{
        Agent, AgentCapabilities, AgentExecutor, AgentKind, AgentProfile, AgentToolPermissions,
        ChannelId, ExternalInput,
    },
    ecs::EntityIndex,
    llm::ExecutorRegistry,
    systems::HarnessConfig,
};
use tokio::runtime::Runtime;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn extract_short_id_from_text(text: &str) -> Option<String> {
    let start = text.find('[')? + 1;
    let end = text[start..].find(']')? + start;
    Some(text[start..end].to_string())
}

/// Spawn default-llm-agent（主 Agent）。
///
/// 同时写入 `EntityIndex.agents`，模拟 `spawn_agent` 封装的索引维护。
fn spawn_default_agent(app: &mut bevy_app::App) {
    let default_agent = Agent {
        id: harness::domain::AgentId::new(),
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
        .spawn((default_agent, harness::domain::LongTermMemory::default()))
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
            approvers: Vec::new(),
            bot_token: bot_token.to_string(),
            allowed_users: vec!["123456".to_string()],
            pairing_enabled: false,
            pairing_code: None,
        };
        let channel = Arc::new(TelegramChannel::new(cfg).with_base_url(mock_server.uri()))
            as Arc<dyn Channel>;
        let (channel_manager, _channel_handle, channel_frontends) =
            ChannelManager::new(vec![channel], input_tx.clone()).expect("valid channel names");

        let config = HarnessConfig {
            agents_config_path: "/nonexistent_agents.toml".to_string(),
            ..Default::default()
        };
        let executor: Arc<dyn AgentExecutor> = Arc::new(FixedTextExecutor::new("echo reply"));
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

        // 手动 spawn default-llm-agent（因为 agents.toml 不存在）
        spawn_default_agent(&mut app);

        // 注入一条来自 Telegram 的入向消息
        let origin = ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "123456".to_string(),
            thread_id: None,
        };
        input_tx
            .send(ExternalInput::TextWithChannel {
                sender: None,
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

/// 定时任务只占输出通道，允许并行发送并保留不同的短 ID 前缀。
#[test]
fn scheduled_tasks_share_output_channel_with_different_short_ids() {
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
            approvers: Vec::new(),
            bot_token: bot_token.to_string(),
            allowed_users: vec!["123456".to_string()],
            pairing_enabled: false,
            pairing_code: None,
        };
        let channel = Arc::new(TelegramChannel::new(cfg).with_base_url(mock_server.uri()))
            as Arc<dyn Channel>;
        let (channel_manager, _channel_handle, channel_frontends) =
            ChannelManager::new(vec![channel], input_tx.clone()).expect("valid channel names");

        let config = HarnessConfig {
            agents_config_path: "/nonexistent_agents.toml".to_string(),
            ..Default::default()
        };
        let executor: Arc<dyn AgentExecutor> = Arc::new(FixedTextExecutor::new("echo reply"));
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

        // 手动 spawn default-llm-agent（因为 agents.toml 不存在）
        spawn_default_agent(&mut app);

        let origin = ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "123456".to_string(),
            thread_id: None,
        };

        for content in ["first scheduled task", "second scheduled task"] {
            app.world_mut().spawn(harness::domain::CreateTaskMessage {
                content: content.into(),
                origin_channel: None,
                initiator: None,
                routing_policy: harness::domain::TaskRoutingPolicy::scheduled_task(
                    Some(origin.clone()),
                    "scheduled test",
                ),
            });
        }

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
