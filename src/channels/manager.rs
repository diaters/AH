use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use crossbeam_channel::Sender;
use tokio::sync::{broadcast, mpsc, oneshot};
use tracing::{error, info, warn};

use crate::domain::ExternalInput;

use super::traits::{Channel, ChannelInboundMessage, ChannelOutboundMessage};

#[derive(Clone)]
pub struct ChannelManager {
    channels: Vec<Arc<dyn Channel>>,
    outbound_tx: mpsc::UnboundedSender<(String, ChannelOutboundMessage)>,
    shutdown_tx: broadcast::Sender<()>,
}

impl ChannelManager {
    pub fn new(
        channels: Vec<Arc<dyn Channel>>,
        external_input_tx: Sender<ExternalInput>,
    ) -> (Self, tokio::task::JoinHandle<()>) {
        let (outbound_tx, mut outbound_rx) =
            mpsc::unbounded_channel::<(String, ChannelOutboundMessage)>();
        let (shutdown_tx, _) = broadcast::channel::<()>(1);

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
                                if bridge_input_tx.send(msg.to_external_input()).is_err() {
                                    break;
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
                        let Some((name, message)) = msg else { break };
                        if let Some(channel) = send_channels.iter().find(|c| c.name() == name) {
                            if let Err(e) = channel.send(&message).await {
                                error!(event = "ChannelSendFailed", channel = %name, error = %e, "failed to send outbound message");
                            }
                        } else {
                            warn!(event = "ChannelNotFound", channel = %name, "no such channel for outbound message");
                        }
                    }
                }
            }
        });

        (
            Self {
                channels,
                outbound_tx,
                shutdown_tx,
            },
            handle,
        )
    }

    /// 同步入队出向消息，立即返回。网络发送在后台执行。
    pub fn send(&self, channel_name: String, message: ChannelOutboundMessage) -> Result<()> {
        if !self.channels.iter().any(|c| c.name() == channel_name) {
            anyhow::bail!("channel not found: {channel_name}");
        }
        self.outbound_tx
            .send((channel_name, message))
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
        ) -> Result<(), super::super::traits::ChannelError> {
            self.send_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
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
            });
            Err(super::super::traits::ChannelError::NotConfigured)
        }
    }

    #[tokio::test]
    async fn manager_receives_inbound_and_sends_outbound() {
        let (input_tx, input_rx) = unbounded::<ExternalInput>();
        let send_count = Arc::new(AtomicUsize::new(0));
        let channel = Arc::new(DummyChannel {
            name: "dummy".to_string(),
            send_count: send_count.clone(),
        }) as Arc<dyn Channel>;
        let (manager, _handle) = ChannelManager::new(vec![channel], input_tx);

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
                "dummy".to_string(),
                ChannelOutboundMessage {
                    recipient: "c1".to_string(),
                    thread_id: None,
                    content: "pong".to_string(),
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
        let (manager, _handle) = ChannelManager::new(vec![], input_tx);
        let result = manager.send(
            "nope".to_string(),
            ChannelOutboundMessage {
                recipient: "x".to_string(),
                thread_id: None,
                content: "x".to_string(),
            },
        );
        assert!(result.is_err());
    }
}
