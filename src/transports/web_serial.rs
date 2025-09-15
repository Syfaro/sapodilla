use std::borrow::Cow;
use std::io::Cursor;

use anyhow::{anyhow, bail};
use async_trait::async_trait;
use eframe::wasm_bindgen::{JsCast, JsValue};
use futures::{FutureExt, SinkExt, StreamExt, channel::mpsc};
use tracing::{debug, error, info, trace, warn};
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    ReadableStreamDefaultReader, SerialOptions, SerialPort, WritableStreamDefaultWriter, js_sys,
};

use crate::protocol::AvocadoPacket;
use crate::transports::TransportStatus;
use crate::{
    protocol,
    transports::{TransportControl, TransportEvent},
};

#[derive(Debug)]
enum TransportAction {
    SendPacket(
        (
            protocol::AvocadoPacket,
            futures::channel::oneshot::Sender<()>,
        ),
    ),
    Disconnect,
}

#[derive(Default)]
pub struct WebSerialTransport {
    tx: Option<mpsc::UnboundedSender<TransportAction>>,
}

#[async_trait(?Send)]
impl TransportControl for WebSerialTransport {
    fn name(&self) -> Cow<'static, str> {
        "Web Serial".into()
    }

    fn supports_discovery(&self) -> bool {
        false
    }

    async fn start(
        &mut self,
        mut event_tx: mpsc::UnboundedSender<TransportEvent>,
    ) -> Result<(), anyhow::Error> {
        let navigator = web_sys::window().unwrap().navigator();
        if !js_sys::Reflect::has(&navigator, &JsValue::from_str("serial")).unwrap() {
            anyhow::bail!("navigator does not have serial API");
        }

        event_tx
            .send(TransportEvent::TransportStatus(TransportStatus::Connecting))
            .await?;

        let serial = navigator.serial();
        let port = JsFuture::from(serial.request_port())
            .await
            .map_err(|err| anyhow!("could not request port: {err:?}"))?;

        let port: &SerialPort = port.dyn_ref().unwrap();

        let (action_tx, action_rx) = mpsc::unbounded();

        JsFuture::from(port.open(&SerialOptions::new(9600)))
            .await
            .map_err(|err| anyhow!("could not open port: {err:?}"))?;

        event_tx
            .send(TransportEvent::TransportStatus(TransportStatus::Connected))
            .await?;

        WebSerialHandler::start(port.to_owned(), action_rx, event_tx);

        self.tx = Some(action_tx);

        Ok(())
    }

    async fn disconnect(&mut self) -> anyhow::Result<()> {
        let Some(tx) = self.tx.as_mut() else {
            bail!("transport was not started");
        };

        tx.send(TransportAction::Disconnect).await?;

        Ok(())
    }

    async fn send_packet(
        &mut self,
        packet: AvocadoPacket,
    ) -> anyhow::Result<futures::channel::oneshot::Receiver<()>> {
        let Some(tx) = self.tx.as_mut() else {
            bail!("transport was not started");
        };

        let (send_tx, send_rx) = futures::channel::oneshot::channel();

        tx.send(TransportAction::SendPacket((packet, send_tx)))
            .await?;

        Ok(send_rx)
    }
}

struct WebSerialHandler {
    action_rx: mpsc::UnboundedReceiver<TransportAction>,
    event_tx: mpsc::UnboundedSender<TransportEvent>,
    port: SerialPort,
}

impl WebSerialHandler {
    fn start(
        port: SerialPort,
        action_rx: mpsc::UnboundedReceiver<TransportAction>,
        event_tx: mpsc::UnboundedSender<TransportEvent>,
    ) {
        let handler = Self {
            action_rx,
            event_tx,
            port,
        };

        wasm_bindgen_futures::spawn_local(handler.run());
    }

    async fn run(mut self) {
        let (stop_tx, stop_rx) = oneshot::channel::<()>();

        let reader = ReadableStreamDefaultReader::new(&self.port.readable()).unwrap();
        let writer = self.port.writable().get_writer().unwrap();

        let mut action_task = Box::pin(Self::action_task(self.action_rx, stop_tx, &writer).fuse());
        let mut read_task = Box::pin(Self::read_task(&reader, self.event_tx.clone()).fuse());

        futures::select! {
            _ = stop_rx.fuse() => {
                warn!("handler stopped");
            }

            res = action_task => {
                match res {
                    Ok(_) => info!("action task finished"),
                    Err(err) => {
                        error!("action task errored: {err}");
                        let _ = self.event_tx.send(TransportEvent::Error(err)).await;
                    }
                }
            }

            res = read_task => {
                match res {
                    Ok(_) => info!("read task finished"),
                    Err(err) => {
                        error!("read task errored: {err}");
                        let _ = self.event_tx.send(TransportEvent::Error(err)).await;
                    }
                }
            }
        }

        reader.release_lock();
        writer.release_lock();

        if let Err(err) = JsFuture::from(self.port.close())
            .await
            .map_err(|err| anyhow!("could not close port: {err:?}"))
        {
            error!("{}", err);
            let _ = self.event_tx.send(TransportEvent::Error(err)).await;
            return;
        }

        let _ = self
            .event_tx
            .send(TransportEvent::TransportStatus(
                TransportStatus::Disconnected,
            ))
            .await;

        info!("web serial handler stopped");
    }

    async fn action_task(
        mut action_rx: mpsc::UnboundedReceiver<TransportAction>,
        stop_tx: oneshot::Sender<()>,
        writer: &WritableStreamDefaultWriter,
    ) -> anyhow::Result<()> {
        while let Some(action) = action_rx.next().await {
            debug!("got action: {action:?}");

            match action {
                TransportAction::SendPacket((packet, tx)) => {
                    let data = packet.encode();
                    let data = js_sys::Uint8Array::new_from_slice(&data);

                    JsFuture::from(writer.write_with_chunk(&data))
                        .await
                        .map_err(|err| anyhow!("could not write chunk: {err:?}"))?;

                    if tx.send(()).is_err() {
                        error!("could not send message completion");
                    }
                }

                TransportAction::Disconnect => {
                    if let Err(err) = stop_tx.send(()) {
                        error!("could not send disconnect event to stop channel: {err}");
                    }

                    break;
                }
            }
        }

        Ok(())
    }

    async fn read_task(
        reader: &ReadableStreamDefaultReader,
        mut event_tx: mpsc::UnboundedSender<TransportEvent>,
    ) -> anyhow::Result<()> {
        let mut buf: Vec<u8> = Vec::new();

        loop {
            let result = JsFuture::from(reader.read())
                .await
                .map_err(|err| anyhow!("read failed: {err:?}"))?;

            let done = js_sys::Reflect::get(&result, &"done".into())
                .unwrap()
                .as_bool()
                .unwrap();
            if done {
                reader.release_lock();
                info!("read done");
                return Ok(());
            }

            let value = js_sys::Reflect::get(&result, &"value".into()).unwrap();
            let data = js_sys::Uint8Array::new(&value);

            if data.length() == 0 {
                trace!("data empty");
                continue;
            }

            let existing_buf_len = buf.len();
            let new_data_len = usize::try_from(data.length()).unwrap();

            buf.resize(buf.len() + new_data_len, 0);
            data.copy_to(&mut buf[existing_buf_len..existing_buf_len + new_data_len]);
            trace!(
                "read {} bytes, total buffer is {} bytes",
                data.length(),
                buf.len()
            );

            let mut cursor = Cursor::new(&mut buf);
            let packet = match protocol::AvocadoPacket::read_one(&mut cursor) {
                Ok(packet) => packet,
                Err(protocol::ProtocolError::Reader(err))
                    if err.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    trace!("had eof, continuing to next read");
                    continue;
                }
                Err(err) => return Err(err.into()),
            };

            let read_bytes = usize::try_from(cursor.position()).unwrap();
            buf.drain(0..read_bytes);

            debug!(read_bytes, "got packet: {packet:?}");

            event_tx.send(TransportEvent::Packet(packet)).await?;
        }
    }
}
