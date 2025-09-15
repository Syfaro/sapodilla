use std::borrow::Cow;
use std::sync::atomic::{AtomicBool, AtomicU32};

use anyhow::bail;
use async_trait::async_trait;
use egui::ahash::HashMap;
use enum_dispatch::enum_dispatch;
use futures::{
    SinkExt, StreamExt,
    channel::{mpsc, oneshot},
    lock::Mutex,
};
use serde::Deserialize;
use tracing::{debug, error, info, instrument, trace, warn};

use crate::protocol::{
    AvocadoPacket, ContentType, EncodingType, EncryptionMode, InteractionType, JobState,
    JobStatusInfo, PrinterState, PrinterSubState,
};

use crate::transports::mock::MockTransport;
#[cfg(target_arch = "wasm32")]
use crate::transports::web_serial::WebSerialTransport;
use crate::{Rc, spawn};

pub mod mock;
#[cfg(target_arch = "wasm32")]
pub mod web_serial;

static MESSAGE_ID: AtomicU32 = AtomicU32::new(1);

pub const MAX_DATA_SIZE: usize = 896;

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
    TransportStatus(TransportStatus),
    DeviceStatus((PrinterState, PrinterSubState, String)),
    JobStatus((u32, JobStatusInfo)),
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

    async fn send_packet(&mut self, packet: AvocadoPacket)
    -> anyhow::Result<oneshot::Receiver<()>>;
}

#[derive(Clone)]
pub struct TransportManager {
    transport: Rc<Mutex<Transport>>,
    event_tx: mpsc::UnboundedSender<TransportEvent>,

    sending: Rc<AtomicBool>,
    pending: Rc<Mutex<HashMap<u32, oneshot::Sender<AvocadoPacket>>>>,
}

#[derive(Debug, Deserialize)]
struct ResultWithId {
    id: u32,
}

#[derive(Debug, Deserialize)]
pub struct AvocadoResult<T> {
    pub result: T,
}

impl TransportManager {
    pub fn new<F>(transport: Rc<Mutex<Transport>>, cb: Option<F>) -> Rc<Self>
    where
        F: Fn(TransportEvent) + Send + Sync + 'static,
    {
        let (mut event_tx, mut event_rx) = mpsc::unbounded();
        let (ready_tx, ready_rx) = oneshot::channel();

        let sending = Rc::new(AtomicBool::new(false));
        let pending: Rc<Mutex<HashMap<u32, oneshot::Sender<AvocadoPacket>>>> = Default::default();

        let manager = Rc::new(Self {
            transport: transport.clone(),
            event_tx: event_tx.clone(),

            sending: sending.clone(),
            pending: pending.clone(),
        });

        spawn({
            let manager = manager.clone();
            let mut event_tx = event_tx.clone();

            async move {
                if ready_rx.await.is_err() {
                    warn!("ready was dropped before ready");
                    return;
                }

                #[cfg(target_arch = "wasm32")]
                let mut stream = gloo_timers::future::IntervalStream::new(1_000);
                #[cfg(not(target_arch = "wasm32"))]
                let mut stream = tokio_stream::wrappers::IntervalStream::new(
                    tokio::time::interval(std::time::Duration::from_secs(1)),
                );

                while stream.next().await.is_some() {
                    info!("making status request");

                    if event_tx.is_closed() {
                        warn!("event sender was closed, ending status stream");
                        break;
                    }

                    if sending.load(std::sync::atomic::Ordering::SeqCst) {
                        trace!("skipping status request because sending data");
                        continue;
                    }

                    let id = manager.next_message_id();
                    let packet = AvocadoPacket {
                        version: 100,
                        content_type: ContentType::Message,
                        interaction_type: InteractionType::Request,
                        encoding_type: EncodingType::Json,
                        encryption_mode: EncryptionMode::None,
                        terminal_id: id,
                        msg_number: id,
                        msg_package_total: 1,
                        msg_package_num: 1,
                        is_subpackage: false,
                        data: serde_json::to_vec(&serde_json::json!({
                            "id" : id,
                            "method" : "get-prop",
                            "params" : [
                                "printer-state",
                                "printer-sub-state",
                                "printer-state-alerts",
                            ]
                        }))
                        .unwrap(),
                    };

                    let packet = match manager.wait_for_response(packet).await {
                        Ok(packet) => packet,
                        Err(err) => {
                            error!("error fetching status packet: {err}");
                            break;
                        }
                    };

                    if let Some(result) =
                        packet.as_json::<AvocadoResult<(PrinterState, PrinterSubState, String)>>()
                    {
                        info!("got status: {result:?}");
                        if let Err(err) = event_tx
                            .send(TransportEvent::DeviceStatus(result.result))
                            .await
                        {
                            error!("could not send device status: {err:?}");
                            break;
                        }
                    } else {
                        error!("could not decode printer status: {packet:?}");
                    }
                }

                info!("status interval stream ended");
            }
        });

        spawn(async move {
            let mut ready_tx = Some(ready_tx);

            while let Some(event) = event_rx.next().await {
                match &event {
                    TransportEvent::Packet(packet) => {
                        if let Some(data) = packet.as_json::<ResultWithId>()
                            && let Some(pending) = pending.lock().await.remove(&data.id)
                            && pending.send(packet.clone()).is_err()
                        {
                            error!("could not send packet to pending");
                        }
                    }
                    TransportEvent::TransportStatus(TransportStatus::Connected) => {
                        if let Some(ready_tx) = ready_tx.take() {
                            let _ = ready_tx.send(());
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
            if let Err(err) = transport.start(event_tx.clone()).await
                && let Err(err) = event_tx.send(TransportEvent::Error(err)).await
            {
                error!("could not send transport start error: {err}");
            }
        });

        manager
    }

    pub async fn disconnect(&self) -> anyhow::Result<()> {
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
        self.transport
            .lock()
            .await
            .send_packet(packet)
            .await?
            .await?;

        rx.await.map_err(Into::into)
    }

    #[instrument(skip(self))]
    pub async fn poll_job(&self, job_id: u32) -> anyhow::Result<()> {
        #[cfg(target_arch = "wasm32")]
        let mut stream = gloo_timers::future::IntervalStream::new(1_000);
        #[cfg(not(target_arch = "wasm32"))]
        let mut stream = tokio_stream::wrappers::IntervalStream::new(tokio::time::interval(
            std::time::Duration::from_secs(1),
        ));

        let mut event_tx = self.event_tx.clone();

        while stream.next().await.is_some() {
            info!("making job status request");

            if event_tx.is_closed() {
                warn!("event sender was closed, ending job status stream");
                break;
            }

            let id = self.next_message_id();
            let packet = AvocadoPacket {
                version: 100,
                content_type: ContentType::Message,
                interaction_type: InteractionType::Request,
                encoding_type: EncodingType::Json,
                encryption_mode: EncryptionMode::None,
                terminal_id: id,
                msg_number: id,
                msg_package_total: 1,
                msg_package_num: 1,
                is_subpackage: false,
                data: serde_json::to_vec(&serde_json::json!({
                    "id": id,
                    "method": "get-job-info",
                    "params": { "job-id": job_id },
                }))
                .unwrap(),
            };

            let packet = match self.wait_for_response(packet).await {
                Ok(packet) => packet,
                Err(err) => {
                    error!("error fetching job status packet: {err}");
                    break;
                }
            };

            if let Some(mut result) = packet.as_json::<AvocadoResult<Vec<JobStatusInfo>>>() {
                info!("got job status: {result:?}");
                let Some(info) = result.result.pop() else {
                    warn!("result was missing job info");
                    continue;
                };

                let is_complete = matches!(
                    info.job_state,
                    JobState::Aborted | JobState::Cancelled | JobState::Completed
                );

                if let Err(err) = event_tx
                    .send(TransportEvent::JobStatus((job_id, info)))
                    .await
                {
                    error!("could not send job status: {err:?}");
                    break;
                }

                if is_complete {
                    info!("job reached terminal state, ending status polling");
                    break;
                }
            } else {
                error!(
                    "could not decode job status: {packet:?}, {:?}",
                    packet.as_json::<serde_json::Value>()
                );
                break;
            }
        }

        Ok(())
    }

    #[instrument(skip(self, data, f))]
    pub async fn send_data<F>(&self, job_id: u32, data: &[u8], f: F) -> anyhow::Result<()>
    where
        F: Fn(usize, usize),
    {
        if self.sending.load(std::sync::atomic::Ordering::SeqCst) {
            bail!("cannot start sending data while other send is in progress");
        }

        let count = usize::div_ceil(data.len(), MAX_DATA_SIZE - 4);
        debug!(
            chunk_count = count,
            "sending data with {} bytes",
            data.len()
        );

        let _guard = SendingDropGuard::new(self.sending.clone());

        for (index, chunk) in data.chunks(MAX_DATA_SIZE - 4).enumerate() {
            debug!(index, "sending packet");

            let mut buf: Vec<u8> = Vec::with_capacity(MAX_DATA_SIZE);
            buf.extend(&job_id.to_le_bytes());
            buf.extend_from_slice(chunk);

            let id = self.next_message_id();
            let packet = AvocadoPacket {
                version: 100,
                content_type: ContentType::Data,
                interaction_type: InteractionType::Request,
                encoding_type: EncodingType::Hexadecimal,
                encryption_mode: EncryptionMode::None,
                terminal_id: id,
                msg_number: id,
                msg_package_total: u16::try_from(count).unwrap(),
                msg_package_num: u16::try_from(index + 1).unwrap(),
                is_subpackage: count > 1,
                data: buf,
            };

            self.transport
                .lock()
                .await
                .send_packet(packet)
                .await?
                .await?;
            f(count, index + 1);
        }

        Ok(())
    }
}

struct SendingDropGuard {
    sending: Rc<AtomicBool>,
}

impl SendingDropGuard {
    fn new(sending: Rc<AtomicBool>) -> Self {
        trace!("marking as sending");
        sending.store(true, std::sync::atomic::Ordering::SeqCst);
        Self { sending }
    }
}

impl Drop for SendingDropGuard {
    fn drop(&mut self) {
        trace!("sending dropped, releasing");
        self.sending
            .store(false, std::sync::atomic::Ordering::SeqCst);
    }
}
