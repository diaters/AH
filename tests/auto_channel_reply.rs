use std::{sync::Arc, thread, time::Duration};

use crossbeam_channel::unbounded;
use harness::{
    AgentExecutionOutput, AgentExecutionRequest, AgentExecutor, ChannelId, ExecutorFuture,
    ExternalInput, FrontendKind, HarnessConfig, OutputContent, build_harness_app,
    channels::{Channel, ChannelManager, TelegramChannel, TelegramConfig},
};
use tokio::runtime::Runtime;
use wiremock::matchers::{body_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// 一个极简的 Executor，直接返回固定文本作为 Agent 回复。
struct EchoExecutor;

impl AgentExecutor for EchoExecutor {
    fn execute(&self, _request: AgentExecutionRequest) -> ExecutorFuture {
        Box::pin(async move {
            Ok(AgentExecutionOutput {
                content: OutputContent::Text("echo reply".to_string()),
                reasoning_content: None,
            })
        })
    }
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
            .and(body_json(serde_json::json!({
                "chat_id": "123456",
                "text": "echo reply",
            })))
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
        };
        let channel = Arc::new(TelegramChannel::new(cfg).with_base_url(mock_server.uri()))
            as Arc<dyn Channel>;
        let (channel_manager, _channel_handle, channel_frontends) =
            ChannelManager::new(vec![channel], input_tx.clone());

        let config = HarnessConfig::default();
        let executor: Arc<dyn AgentExecutor> = Arc::new(EchoExecutor);

        let mut app = build_harness_app(
            config,
            rt.clone(),
            executor,
            input_rx,
            channel_frontends,
            channel_manager,
        );

        // 初始化 Startup 系统（加载 Agent 配置等）
        app.update();

        // 注入一条来自 Telegram 的入向消息
        let origin = ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "123456".to_string(),
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
            thread::sleep(Duration::from_millis(10));
        }

        mock_server.verify().await;
    });
}
