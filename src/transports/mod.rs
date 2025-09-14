use std::borrow::Cow;
use std::sync::atomic::AtomicU32;

use anyhow::bail;
use async_trait::async_trait;
use egui::ahash::HashMap;
use enum_dispatch::enum_dispatch;
use futures::channel::mpsc;
use futures::lock::Mutex;
use futures::{SinkExt, StreamExt, channel::oneshot};
use tracing::{debug, error, info, trace};

use crate::protocol::AvocadoPacket;

use crate::transports::mock::MockTransport;
#[cfg(target_arch = "wasm32")]
use crate::transports::web_serial::WebSerialTransport;
use crate::{Rc, spawn};

pub mod mock;
#[cfg(target_arch = "wasm32")]
pub mod web_serial;

static MESSAGE_ID: AtomicU32 = AtomicU32::new(0);

#[enum_dispatch(TransportControl)]
#[derive(strum::EnumIter)]
pub enum Transport {
    #[cfg(target_arch = "wasm32")]
    WebSerialTransport,
    MockTransport,
}

#[allow(dead_code)]
pub struct DiscoveredDevice {
    pub name: String,
    pub details: Option<String>,
}

#[allow(dead_code)]
#[derive(Debug)]
pub enum TransportEvent {
    StatusChange(TransportStatus),
    Packet(AvocadoPacket),
    Error(anyhow::Error),
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Hash)]
pub enum TransportStatus {
    Connecting,
    Connected,
    Disconnecting,
    Disconnected,
}

#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[enum_dispatch]
pub trait TransportControl {
    fn name(&self) -> Cow<'static, str>;
    #[allow(dead_code)]
    fn supports_discovery(&self) -> bool;

    #[allow(dead_code)]
    async fn discover_devices(&mut self) -> anyhow::Result<Vec<DiscoveredDevice>> {
        bail!("discovery not supported for transport");
    }

    async fn start(
        &mut self,
        mut event_tx: mpsc::UnboundedSender<TransportEvent>,
    ) -> Result<(), anyhow::Error>;

    async fn disconnect(&mut self) -> anyhow::Result<()>;

    async fn send_packet(&mut self, packet: AvocadoPacket) -> anyhow::Result<()>;
}

#[derive(Clone)]
pub struct TransportManager {
    transport: Rc<Mutex<Transport>>,
    pending: Rc<Mutex<HashMap<u32, oneshot::Sender<AvocadoPacket>>>>,
}

impl TransportManager {
    pub fn new<F>(transport: Rc<Mutex<Transport>>, cb: Option<F>) -> Self
    where
        F: Fn(TransportEvent) + Send + Sync + 'static,
    {
        let pending: Rc<Mutex<HashMap<u32, oneshot::Sender<AvocadoPacket>>>> = Default::default();

        let manager = Self {
            transport: transport.clone(),
            pending: pending.clone(),
        };

        let (mut tx, mut rx) = mpsc::unbounded();

        spawn(async move {
            while let Some(event) = rx.next().await {
                match &event {
                    TransportEvent::Packet(packet) => {
                        if let Some(pending) = pending.lock().await.remove(&packet.msg_number)
                            && pending.send(packet.clone()).is_err()
                        {
                            error!("could not send packet to pending");
                        }
                    }
                    _ => trace!("got other event: {event:?}"),
                }

                if let Some(cb) = &cb {
                    cb(event);
                }
            }
        });

        spawn(async move {
            let mut transport = transport.lock().await;
            if let Err(err) = transport.start(tx.clone()).await
                && let Err(err) = tx.send(TransportEvent::Error(err)).await
            {
                error!("could not send transport start error: {err}");
            }
        });

        manager
    }

    pub async fn disconnect(self) -> anyhow::Result<()> {
        info!("disconnecting transport");
        self.transport.lock().await.disconnect().await
    }

    pub fn next_message_id(&self) -> u32 {
        let id = MESSAGE_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        trace!(id, "generated next message id");
        id
    }

    pub async fn wait_for_response(&self, packet: AvocadoPacket) -> anyhow::Result<AvocadoPacket> {
        let (tx, rx) = oneshot::channel();

        debug!(id = packet.msg_number, "sending and awaiting packet");
        self.pending.lock().await.insert(packet.msg_number, tx);
        self.transport.lock().await.send_packet(packet).await?;

        rx.await.map_err(Into::into)
    }
}
