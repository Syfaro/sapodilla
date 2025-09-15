use std::borrow::Cow;

use async_trait::async_trait;
use futures::{
    SinkExt,
    channel::{mpsc, oneshot},
};

use crate::{
    protocol::AvocadoPacket,
    transports::{TransportControl, TransportEvent, TransportStatus},
};

#[derive(Default)]
pub struct MockTransport {}

#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
impl TransportControl for MockTransport {
    fn name(&self) -> Cow<'static, str> {
        "Mock".into()
    }

    fn supports_discovery(&self) -> bool {
        false
    }

    async fn start(
        &mut self,
        mut event_tx: mpsc::UnboundedSender<TransportEvent>,
    ) -> anyhow::Result<()> {
        event_tx
            .send(TransportEvent::TransportStatus(
                TransportStatus::Disconnected,
            ))
            .await?;

        Ok(())
    }

    async fn disconnect(&mut self) -> anyhow::Result<()> {
        Ok(())
    }

    async fn send_packet(
        &mut self,
        _packet: AvocadoPacket,
    ) -> anyhow::Result<oneshot::Receiver<()>> {
        let (tx, rx) = oneshot::channel();
        tx.send(()).unwrap();

        Ok(rx)
    }
}
