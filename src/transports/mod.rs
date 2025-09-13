use std::borrow::Cow;

use anyhow::bail;
use async_trait::async_trait;
use enum_dispatch::enum_dispatch;
use futures::channel::mpsc;

use crate::protocol::AvocadoPacket;

use crate::transports::mock::MockTransport;
#[cfg(target_arch = "wasm32")]
use crate::transports::web_serial::WebSerialTransport;

pub mod mock;
#[cfg(target_arch = "wasm32")]
pub mod web_serial;

#[cfg(target_arch = "wasm32")]
pub type AvocadoCallbackFn = Box<dyn FnOnce(AvocadoPacket) + Send + Sync>;

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
    fn supports_discovery(&self) -> bool;

    async fn discover_devices(&mut self) -> anyhow::Result<Vec<DiscoveredDevice>> {
        bail!("discovery not supported for transport");
    }

    async fn start(
        &mut self,
        mut event_tx: mpsc::UnboundedSender<TransportEvent>,
    ) -> Result<(), anyhow::Error>;

    async fn disconnect(&mut self) -> anyhow::Result<()>;

    async fn send_packet<F>(&mut self, packet: AvocadoPacket, cb: F) -> Result<(), anyhow::Error>
    where
        F: FnOnce(AvocadoPacket) + Send + Sync + 'static;
}
