use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use anyhow::Result;
use crossbeam_channel::Sender;
use tokio::sync::{broadcast, mpsc, oneshot};
use tracing::{error, info, warn};

use crate::domain::{ExternalInput, Frontend, FrontendKind};

use super::traits::{
    Channel, ChannelError, ChannelInboundMessage, ChannelOutboundMessage, OutboundEntry,
};

const SEND_TIMEOUT: Duration = Duration::from_secs(30);

/// `ChannelManager::new` 的返回值：管理器、监督任务句柄与前端列表。
pub type ChannelManagerSpawn = (
    ChannelManager,
    tokio::task::JoinHandle<()>,
    Vec<Box<dyn Frontend>>,
);

#[derive(Clone, crate::prelude::Resource)]
pub struct ChannelManager {
    channels: Vec<Arc<dyn Channel>>,
    outbound_tx: mpsc::UnboundedSender<OutboundEntry>,
    shutdown_tx: broadcast::Sender<()>,
    closing: Arc<AtomicBool>,
}

impl ChannelManager {
    /// 创建空 ChannelManager（不启动任何通道），用于测试和未配置通道的场景。
    pub fn empty() -> (Self, Vec<Box<dyn Frontend>>) {
        let (outbound_tx, _) = mpsc::unbounded_channel::<OutboundEntry>();
        let (shutdown_tx, _) = broadcast::channel::<()>(1);
        (
            Self {
                channels: vec![],
                outbound_tx,
                shutdown_tx,
                closing: Arc::new(AtomicBool::new(false)),
            },
            vec![],
        )
    }

    pub fn new(
        channels: Vec<Arc<dyn Channel>>,
        external_input_tx: Sender<ExternalInput>,
    ) -> Result<ChannelManagerSpawn, String> {
        let (outbound_tx, mut outbound_rx) = mpsc::unbounded_channel::<OutboundEntry>();
        let (shutdown_tx, _) = broadcast::channel::<()>(1);

        let mut frontends: Vec<Box<dyn Frontend>> = Vec::with_capacity(channels.len());
        for ch in &channels {
            let kind = FrontendKind::from_channel_name(ch.name())?;
            frontends.push(Box::new(super::ChannelFrontend::new(
                kind,
                ch.name().to_string(),
                outbound_tx.clone(),
            )) as Box<dyn Frontend>);
        }

        let supervisor_channels = channels.clone();
        let supervisor_shutdown = shutdown_tx.clone();
        let supervisor_input_tx = external_input_tx;
        let closing = Arc::new(AtomicBool::new(false));
        let send_closing = closing.clone();
        // 先订阅再启动任务，shutdown 不能在首次调度前丢失。
        let mut send_shutdown = shutdown_tx.subscribe();

        let handle = tokio::spawn(async move {
            for channel in &supervisor_channels {
                if send_closing.load(Ordering::SeqCst) {
                    break;
                }
                let ch = channel.clone();
                let input_tx = supervisor_input_tx.clone();
                let mut shutdown_rx = supervisor_shutdown.subscribe();
                let name = ch.name().to_string();
                tokio::spawn(async move {
                    let mut backoff = Duration::from_secs(1);
                    info!(event = "ChannelListenStart", channel = %name, "starting channel listener");
                    loop {
                        let (inbound_tx, inbound_rx) =
                            crossbeam_channel::bounded::<ChannelInboundMessage>(256);
                        let bridge_input_tx = input_tx.clone();

                        let bridge_handle = tokio::task::spawn_blocking(move || {
                            while let Ok(msg) = inbound_rx.recv() {
                                match msg.to_external_input() {
                                    Ok(input) => {
                                        if bridge_input_tx.send(input).is_err() {
                                            break;
                                        }
                                    }
                                    Err(e) => {
                                        warn!(
                                            event = "ChannelInboundDropped",
                                            channel = %msg.channel_name,
                                            error = %e,
                                            "dropping inbound message with unknown channel name"
                                        );
                                    }
                                }
                            }
                        });

                        // 在独立 task 中运行 listen，通过 oneshot 通知完成
                        let (done_tx, done_rx) = oneshot::channel();
                        let listen_ch = ch.clone();
                        tokio::spawn(async move {
                            let res = listen_ch.listen(inbound_tx).await;
                            let _ = done_tx.send(res);
                        });

                        tokio::select! {
                            _ = shutdown_rx.recv() => {
                                info!(event = "ChannelListenStopped", channel = %name, "shutdown signal received");
                                let _ = bridge_handle.await;
                                break;
                            }
                            res = done_rx => {
                                match res {
                                    Ok(Ok(())) => {
                                        info!(event = "ChannelListenEnd", channel = %name, "listener exited cleanly");
                                        let _ = bridge_handle.await;
                                        break;
                                    }
                                    Ok(Err(e)) => {
                                        warn!(event = "ChannelListenExit", channel = %name, error = %e, "listener failed, will restart");
                                        let _ = bridge_handle.await;
                                        tokio::select! {
                                            _ = shutdown_rx.recv() => break,
                                            _ = tokio::time::sleep(backoff) => {}
                                        }
                                        backoff = (backoff * 2).min(Duration::from_secs(60));
                                    }
                                    Err(_) => {
                                        warn!(event = "ChannelListenExit", channel = %name, "listener dropped without result");
                                        let _ = bridge_handle.await;
                                        break;
                                    }
                                }
                            }
                        }
                    }
                });
            }

            let send_channels = supervisor_channels;
            loop {
                if send_closing.load(Ordering::SeqCst) {
                    break;
                }
                let entry = tokio::select! {
                    biased;
                    _ = send_shutdown.recv() => break,
                    msg = outbound_rx.recv() => {
                        let Some(entry) = msg else { break };
                        entry
                    }
                };
                if send_closing.load(Ordering::SeqCst) {
                    finish_outbound(
                        entry,
                        Err("channel manager shutdown before delivery".into()),
                    );
                    break;
                }
                let result = if let Some(channel) = send_channels
                    .iter()
                    .find(|c| c.name() == entry.channel_name)
                {
                    tokio::select! {
                        biased;
                        _ = send_shutdown.recv() => Err("channel manager shutdown; in-flight delivery unknown".into()),
                        result = tokio::time::timeout(SEND_TIMEOUT, channel.send(&entry.message)) => match result {
                            Ok(Ok(message_id)) => Ok(message_id),
                            Ok(Err(error)) => Err(send_failure(&error)),
                            Err(_) => Err("channel send timed out; delivery unknown".into()),
                        }
                    }
                } else {
                    Err("channel not found for outbound message".into())
                };
                if let Err(error) = &result {
                    error!(event = "ChannelSendFailed", channel = %entry.channel_name,
                        error_type = "Delivery", error = %error, "failed to send outbound message");
                }
                finish_outbound(entry, result);
            }
            // close 与 drain 连用：并发发送要么被接收并明确失败，要么同步入队失败。
            outbound_rx.close();
            while let Some(entry) = outbound_rx.recv().await {
                finish_outbound(
                    entry,
                    Err("channel manager shutdown before delivery".into()),
                );
            }
        });

        Ok((
            Self {
                channels,
                outbound_tx,
                shutdown_tx,
                closing,
            },
            handle,
            frontends,
        ))
    }

    /// 同步入队出向消息，立即返回。网络发送在后台执行。
    pub fn send(&self, channel_name: String, message: ChannelOutboundMessage) -> Result<()> {
        self.enqueue(OutboundEntry {
            channel_name,
            message,
            on_sent: None,
            on_result: None,
        })
    }

    /// 入队并等待平台实际发送结果回调；同步入队失败时不调用回调。
    pub fn send_with_result(
        &self,
        channel_name: String,
        message: ChannelOutboundMessage,
        on_result: impl FnOnce(Result<Option<String>, String>) + Send + Sync + 'static,
    ) -> Result<()> {
        self.enqueue(OutboundEntry {
            channel_name,
            message,
            on_sent: None,
            on_result: Some(Box::new(on_result)),
        })
    }

    fn enqueue(&self, entry: OutboundEntry) -> Result<()> {
        if self.closing.load(Ordering::SeqCst) {
            anyhow::bail!("channel manager shutdown");
        }
        if !self.channels.iter().any(|c| c.name() == entry.channel_name) {
            anyhow::bail!("channel not found");
        }
        self.outbound_tx
            .send(entry)
            .map_err(|_| anyhow::anyhow!("channel manager outbound channel closed"))
    }

    /// 通知所有 listen / send 任务退出。
    pub fn shutdown(&self) {
        self.closing.store(true, Ordering::SeqCst);
        let _ = self.shutdown_tx.send(());
    }
}

fn finish_outbound(entry: OutboundEntry, result: Result<Option<String>, String>) {
    if let Ok(message_id) = &result
        && let Some(on_sent) = entry.on_sent
    {
        on_sent(message_id.clone());
    }
    if let Some(on_result) = entry.on_result {
        on_result(result);
    }
}

/// Provider 错误可能包含含 token 的 URL、服务端响应或正文，回执仅暴露稳定分类。
fn send_failure(error: &ChannelError) -> String {
    match error {
        ChannelError::Network(_) => "channel network request failed".into(),
        ChannelError::Api { code, .. } => format!("channel API error {code}"),
        ChannelError::Auth => "channel authentication failed".into(),
        ChannelError::RateLimited => "channel rate limited".into(),
        ChannelError::NotConfigured => "channel not configured".into(),
        ChannelError::NotSupported => "channel operation not supported".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use crossbeam_channel::unbounded;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct DummyChannel {
        name: String,
        send_count: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Channel for DummyChannel {
        fn name(&self) -> &str {
            &self.name
        }
        async fn send(
            &self,
            _msg: &ChannelOutboundMessage,
        ) -> Result<Option<String>, super::super::traits::ChannelError> {
            self.send_count.fetch_add(1, Ordering::SeqCst);
            Ok(None)
        }
        async fn listen(
            &self,
            tx: Sender<ChannelInboundMessage>,
        ) -> Result<(), super::super::traits::ChannelError> {
            let _ = tx.send(ChannelInboundMessage {
                channel_name: self.name.clone(),
                sender_id: "u1".to_string(),
                chat_id: "c1".to_string(),
                thread_id: None,
                content: "ping".to_string(),
                timestamp_secs: 0,
                confirmation: None,
            });
            Err(super::super::traits::ChannelError::NotConfigured)
        }
    }

    #[tokio::test]
    async fn manager_receives_inbound_and_sends_outbound() {
        let (input_tx, input_rx) = unbounded::<ExternalInput>();
        let send_count = Arc::new(AtomicUsize::new(0));
        let channel = Arc::new(DummyChannel {
            name: "telegram".to_string(),
            send_count: send_count.clone(),
        }) as Arc<dyn Channel>;
        let (manager, _handle, _frontends) =
            ChannelManager::new(vec![channel], input_tx).expect("valid channel names");

        // 等待入向消息到达（桥接任务需要从 spawn_blocking 转发）
        let input = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(msg) = input_rx.try_recv() {
                    break msg;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("receive inbound timeout");
        match input {
            ExternalInput::TextWithChannel { content, .. } => assert_eq!(content, "ping"),
            _ => panic!("unexpected"),
        }

        manager
            .send(
                "telegram".to_string(),
                ChannelOutboundMessage {
                    recipient: "c1".to_string(),
                    thread_id: None,
                    content: "pong".to_string(),
                    parse_mode: None,
                    reply_markup: None,
                    attachments: vec![],
                    message_kind: super::super::traits::MessageKind::Other,
                },
            )
            .expect("queue outbound");

        tokio::time::timeout(Duration::from_secs(2), async {
            while send_count.load(Ordering::SeqCst) == 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("send timeout");
        assert!(send_count.load(Ordering::SeqCst) >= 1);

        manager.shutdown();
    }

    #[tokio::test]
    async fn send_unknown_channel_errors() {
        let (input_tx, _input_rx) = unbounded::<ExternalInput>();
        let (manager, _handle, _frontends) =
            ChannelManager::new(vec![], input_tx).expect("valid channel names");
        let result = manager.send(
            "nope".to_string(),
            ChannelOutboundMessage {
                recipient: "x".to_string(),
                thread_id: None,
                content: "x".to_string(),
                parse_mode: None,
                reply_markup: None,
                attachments: vec![],
                message_kind: super::super::traits::MessageKind::Other,
            },
        );
        assert!(result.is_err());
    }

    struct ReceiptChannel {
        reply: Result<Option<String>, super::super::traits::ChannelError>,
        entered: Arc<tokio::sync::Notify>,
        blocked: bool,
    }

    #[async_trait]
    impl Channel for ReceiptChannel {
        fn name(&self) -> &str {
            "telegram"
        }
        async fn send(
            &self,
            _: &ChannelOutboundMessage,
        ) -> Result<Option<String>, super::super::traits::ChannelError> {
            self.entered.notify_one();
            if self.blocked {
                std::future::pending::<()>().await;
            }
            match &self.reply {
                Ok(id) => Ok(id.clone()),
                Err(_) => Err(super::super::traits::ChannelError::Api {
                    code: 400,
                    message: "private-token-and-body".into(),
                }),
            }
        }
        async fn listen(
            &self,
            _: Sender<ChannelInboundMessage>,
        ) -> Result<(), super::super::traits::ChannelError> {
            Ok(())
        }
    }

    fn receipt_message() -> ChannelOutboundMessage {
        ChannelOutboundMessage {
            recipient: "c1".into(),
            thread_id: None,
            content: "private-token-and-body".into(),
            parse_mode: None,
            reply_markup: None,
            attachments: vec![],
            message_kind: super::super::traits::MessageKind::Other,
        }
    }

    #[tokio::test]
    async fn send_receipt_reports_platform_result_including_missing_message_id() {
        for expected in [Some("platform-id".to_string()), None] {
            let channel = Arc::new(ReceiptChannel {
                reply: Ok(expected.clone()),
                entered: Arc::default(),
                blocked: false,
            });
            let (input, _) = unbounded();
            let (manager, handle, _) = ChannelManager::new(vec![channel], input).unwrap();
            let (tx, rx) = oneshot::channel();
            manager
                .send_with_result("telegram".into(), receipt_message(), move |result| {
                    let _ = tx.send(result);
                })
                .unwrap();
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(2), rx)
                    .await
                    .unwrap()
                    .unwrap(),
                Ok(expected)
            );
            manager.shutdown();
            handle.await.unwrap();
        }
    }

    #[tokio::test]
    async fn send_receipt_reports_failure_without_leaking_provider_body() {
        let channel = Arc::new(ReceiptChannel {
            reply: Err(super::super::traits::ChannelError::Auth),
            entered: Arc::default(),
            blocked: false,
        });
        let (input, _) = unbounded();
        let (manager, handle, _) = ChannelManager::new(vec![channel], input).unwrap();
        let (tx, rx) = oneshot::channel();
        manager
            .send_with_result("telegram".into(), receipt_message(), move |result| {
                let _ = tx.send(result);
            })
            .unwrap();
        let error = tokio::time::timeout(Duration::from_secs(2), rx)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert!(error.contains("API"));
        assert!(!error.contains("private-token-and-body"));
        manager.shutdown();
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn shutdown_returns_one_failure_for_every_accepted_message() {
        let entered = Arc::new(tokio::sync::Notify::new());
        let channel = Arc::new(ReceiptChannel {
            reply: Ok(None),
            entered: entered.clone(),
            blocked: true,
        });
        let (input, _) = unbounded();
        let (manager, handle, _) = ChannelManager::new(vec![channel], input).unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel();
        for sequence in 0..10 {
            let tx = tx.clone();
            manager
                .send_with_result("telegram".into(), receipt_message(), move |result| {
                    let _ = tx.send((sequence, result));
                })
                .unwrap();
        }
        entered.notified().await;
        manager.shutdown();
        tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .unwrap()
            .unwrap();
        drop(tx);
        let mut seen = Vec::new();
        while let Some((sequence, result)) = rx.recv().await {
            assert!(result.unwrap_err().contains("shutdown"));
            seen.push(sequence);
        }
        seen.sort();
        assert_eq!(seen, (0..10).collect::<Vec<_>>());
        assert!(
            manager
                .send_with_result("telegram".into(), receipt_message(), |_| panic!(
                    "not accepted"
                ))
                .is_err()
        );
    }

    #[tokio::test]
    async fn receipt_enqueue_errors_do_not_invoke_callback() {
        let (manager, _) = ChannelManager::empty();
        assert!(
            manager
                .send_with_result("unknown".into(), receipt_message(), |_| panic!(
                    "not accepted"
                ))
                .is_err()
        );
    }

    #[tokio::test]
    async fn shutdown_before_supervisor_first_poll_returns_queued_failure() {
        let channel = Arc::new(ReceiptChannel {
            reply: Ok(None),
            entered: Arc::default(),
            blocked: false,
        });
        let (input, _) = unbounded();
        let (manager, handle, _) = ChannelManager::new(vec![channel], input).unwrap();
        let (tx, rx) = oneshot::channel();
        manager
            .send_with_result("telegram".into(), receipt_message(), move |result| {
                let _ = tx.send(result);
            })
            .unwrap();
        // No yield before shutdown: the supervisor has not subscribed or read the queue yet.
        manager.shutdown();
        let result = tokio::time::timeout(Duration::from_secs(2), rx)
            .await
            .unwrap()
            .unwrap();
        assert!(result.unwrap_err().contains("shutdown"));
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn legacy_on_sent_runs_only_for_success_and_unknown_channel_reports_failure() {
        for success in [true, false] {
            let channel = Arc::new(ReceiptChannel {
                reply: if success {
                    Ok(Some("sent".into()))
                } else {
                    Err(ChannelError::Auth)
                },
                entered: Arc::default(),
                blocked: false,
            });
            let (input, _) = unbounded();
            let (manager, handle, _) = ChannelManager::new(vec![channel], input).unwrap();
            let calls = Arc::new(AtomicUsize::new(0));
            let on_sent_calls = calls.clone();
            let (tx, rx) = oneshot::channel();
            manager
                .outbound_tx
                .send(OutboundEntry {
                    channel_name: "telegram".into(),
                    message: receipt_message(),
                    on_sent: Some(Box::new(move |_| {
                        on_sent_calls.fetch_add(1, Ordering::SeqCst);
                    })),
                    on_result: Some(Box::new(move |result| {
                        let _ = tx.send(result);
                    })),
                })
                .unwrap();
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(2), rx)
                    .await
                    .unwrap()
                    .unwrap()
                    .is_ok(),
                success
            );
            assert_eq!(calls.load(Ordering::SeqCst), usize::from(success));
            let (tx, rx) = oneshot::channel();
            manager
                .outbound_tx
                .send(OutboundEntry {
                    channel_name: "unknown".into(),
                    message: receipt_message(),
                    on_sent: None,
                    on_result: Some(Box::new(move |result| {
                        let _ = tx.send(result);
                    })),
                })
                .unwrap();
            assert!(
                tokio::time::timeout(Duration::from_secs(2), rx)
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap_err()
                    .contains("not found")
            );
            manager.shutdown();
            handle.await.unwrap();
        }
    }
}
