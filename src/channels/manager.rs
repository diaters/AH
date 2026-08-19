use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use crossbeam_channel::Sender;
use tokio::sync::{broadcast, mpsc, oneshot};
use tracing::{error, info, warn};

use crate::domain::{ExternalInput, Frontend, FrontendKind};

use super::traits::{Channel, ChannelInboundMessage, ChannelOutboundMessage, OutboundEntry};

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

        let handle = tokio::spawn(async move {
            for channel in &supervisor_channels {
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

            let send_channels = supervisor_channels.clone();
            let mut send_shutdown = supervisor_shutdown.subscribe();
            loop {
                tokio::select! {
                    _ = send_shutdown.recv() => break,
                    msg = outbound_rx.recv() => {
                        let Some(entry) = msg else { break };
                        if let Some(channel) = send_channels.iter().find(|c| c.name() == entry.channel_name) {
                            match channel.send(&entry.message).await {
                                Ok(msg_id) => {
                                    if let Some(on_sent) = entry.on_sent {
                                        on_sent(msg_id);
                                    }
                                }
                                Err(e) => {
                                    error!(event = "ChannelSendFailed", channel = %entry.channel_name, error = %e, "failed to send outbound message");
                                }
                            }
                        } else {
                            warn!(event = "ChannelNotFound", channel = %entry.channel_name, "no such channel for outbound message");
                        }
                    }
                }
            }
        });

        Ok((
            Self {
                channels,
                outbound_tx,
                shutdown_tx,
            },
            handle,
            frontends,
        ))
    }

    /// 同步入队出向消息，立即返回。网络发送在后台执行。
    pub fn send(&self, channel_name: String, message: ChannelOutboundMessage) -> Result<()> {
        if !self.channels.iter().any(|c| c.name() == channel_name) {
            anyhow::bail!("channel not found: {channel_name}");
        }
        let entry = OutboundEntry {
            channel_name,
            message,
            on_sent: None,
        };
        self.outbound_tx
            .send(entry)
            .map_err(|_| anyhow::anyhow!("channel manager outbound channel closed"))?;
        Ok(())
    }

    /// 通知所有 listen / send 任务退出。
    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(());
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
}
