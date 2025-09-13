use std::borrow::Cow;
use std::io::Cursor;
use std::rc::Rc;

use anyhow::{anyhow, bail};
use async_trait::async_trait;
use eframe::wasm_bindgen::{JsCast, JsValue};
use egui::ahash::HashMap;
use egui::mutex::Mutex;
use futures::channel::mpsc;
use futures::{FutureExt, SinkExt, StreamExt};
use serde::Deserialize;
use tracing::{debug, error, info, trace, warn};
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    ReadableStreamDefaultReader, SerialOptions, SerialPort, WritableStreamDefaultWriter, js_sys,
};

use crate::transports::{AvocadoCallbackFn, TransportStatus};
use crate::{
    protocol,
    transports::{TransportControl, TransportEvent},
};

#[derive(Debug)]
enum TransportAction {
    SendPacket(TransportPacketCallback),
    Disconnect,
}

struct TransportPacketCallback {
    packet: protocol::AvocadoPacket,
    cb: AvocadoCallbackFn,
}

impl std::fmt::Debug for TransportPacketCallback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TransportPacketCallback")
            .field("packet", &self.packet)
            .finish_non_exhaustive()
    }
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
            .send(TransportEvent::StatusChange(TransportStatus::Connecting))
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
            .send(TransportEvent::StatusChange(TransportStatus::Connected))
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

    async fn send_packet<F>(
        &mut self,
        packet: protocol::AvocadoPacket,
        cb: F,
    ) -> Result<(), anyhow::Error>
    where
        F: FnOnce(protocol::AvocadoPacket) + Send + Sync + 'static,
    {
        let Some(tx) = self.tx.as_mut() else {
            bail!("transport was not started");
        };

        tx.send(TransportAction::SendPacket(TransportPacketCallback {
            packet,
            cb: Box::new(cb),
        }))
        .await?;

        Ok(())
    }
}

struct WebSerialHandler {
    action_rx: mpsc::UnboundedReceiver<TransportAction>,
    event_tx: mpsc::UnboundedSender<TransportEvent>,
    port: SerialPort,
    pending: HashMap<u32, AvocadoCallbackFn>,
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
            pending: Default::default(),
        };

        wasm_bindgen_futures::spawn_local(handler.run());
    }

    async fn run(mut self) {
        let (stop_tx, stop_rx) = oneshot::channel::<()>();

        let reader = ReadableStreamDefaultReader::new(&self.port.readable()).unwrap();
        let writer = self.port.writable().get_writer().unwrap();

        let pending = Rc::new(Mutex::new(self.pending));

        let mut action_task =
            Box::pin(Self::action_task(self.action_rx, stop_tx, &writer, pending.clone()).fuse());
        let mut read_task = Box::pin(Self::read_task(&reader, pending).fuse());

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
            .send(TransportEvent::StatusChange(TransportStatus::Disconnected))
            .await;

        info!("web serial handler stopped");
    }

    async fn action_task(
        mut action_rx: mpsc::UnboundedReceiver<TransportAction>,
        stop_tx: oneshot::Sender<()>,
        writer: &WritableStreamDefaultWriter,
        pending: Rc<Mutex<HashMap<u32, AvocadoCallbackFn>>>,
    ) -> anyhow::Result<()> {
        while let Some(action) = action_rx.next().await {
            debug!("got action: {action:?}");

            match action {
                TransportAction::SendPacket(TransportPacketCallback { packet, cb }) => {
                    pending.lock().insert(packet.msg_number, cb);

                    let data = packet.encode();
                    let data = js_sys::Uint8Array::new_from_slice(&data);

                    JsFuture::from(writer.write_with_chunk(&data))
                        .await
                        .map_err(|err| anyhow!("could not write chunk: {err:?}"))?;
                }

                TransportAction::Disconnect => {
                    writer.release_lock();

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
        pending: Rc<Mutex<HashMap<u32, AvocadoCallbackFn>>>,
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

            #[derive(Deserialize)]
            struct PacketDataWithId {
                id: u32,
            }

            if let Some(data) = packet.as_json::<PacketDataWithId>() {
                if let Some(cb) = pending.lock().remove(&data.id) {
                    cb(packet);
                }
            }
        }
    }
}
