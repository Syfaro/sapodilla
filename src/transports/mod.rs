use std::borrow::Cow;

use anyhow::bail;
use async_trait::async_trait;
use enum_dispatch::enum_dispatch;
use futures::channel::mpsc;

use crate::protocol::AvocadoPacket;

#[cfg(target_arch = "wasm32")]
use crate::transports::web_serial::WebSerialTransport;

#[cfg(target_arch = "wasm32")]
pub mod web_serial;

pub type AvocadoCallbackFn = Box<dyn FnOnce(AvocadoPacket) + Send + Sync>;

#[enum_dispatch(TransportControl)]
pub enum Transport {
    #[cfg(target_arch = "wasm32")]
    WebSerialTransport,
}

#[allow(dead_code)]
pub struct DiscoveredDevice {
    pub name: String,
    pub details: Option<String>,
}

#[allow(dead_code)]
pub enum TransportEvent {
    StatusChange(TransportStatus),
    Error(anyhow::Error),
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Hash)]
pub enum TransportStatus {
    Connecting,
    Connected,
    Disconnected,
}

#[async_trait(?Send)]
#[enum_dispatch]
pub trait TransportControl {
    fn name(&self) -> Cow<'static, str>;
    fn supports_discovery(&self) -> bool;

    async fn discover_devices(&mut self) -> anyhow::Result<Vec<DiscoveredDevice>> {
        bail!("discovery not supported for transport");
    }

    async fn start(&mut self) -> Result<mpsc::UnboundedReceiver<TransportEvent>, anyhow::Error>;

    async fn disconnect(&mut self) -> anyhow::Result<()>;

    async fn send_packet<F>(&mut self, packet: AvocadoPacket, cb: F) -> Result<(), anyhow::Error>
    where
        F: FnOnce(AvocadoPacket) + Send + Sync + 'static;
}
