use std::{borrow::Cow, collections::VecDeque, sync::mpsc};

use egui::{Id, Modal};
use futures::{StreamExt, lock::Mutex};
use strum::IntoEnumIterator;
use tracing::{error, info};

use crate::{
    Rc,
    protocol::{AvocadoPacket, EncodingType, EncryptionMode, ProtocolError},
    spawn,
    transports::{Transport, TransportControl, TransportEvent, TransportStatus},
    views,
};

#[derive(derive_more::Debug)]
pub enum Action {
    Error(anyhow::Error),
    ChangeTransport(usize),
    TransportEvent(TransportEvent),
    GotPacket(AvocadoPacket),
    LoadedAvocadoPackets(Result<Vec<AvocadoPacket>, ProtocolError>),
}

pub struct SapodillaApp {
    tx: mpsc::Sender<Action>,
    rx: mpsc::Receiver<Action>,

    transports: Vec<Rc<Mutex<Transport>>>,
    transport_names: Vec<Cow<'static, str>>,
    selected_transport_index: usize,

    transport_status: TransportStatus,

    packet_id: u32,
    packets: VecDeque<AvocadoPacket>,
    viewing_packet: Option<AvocadoPacket>,

    showing_packet_log: bool,
    showing_avocado_packet_debug: bool,
    avocado_debug_packets: Option<Result<Vec<AvocadoPacket>, ProtocolError>>,

    error: Option<anyhow::Error>,
}

impl SapodillaApp {
    fn has_connected_transport(&self) -> bool {
        self.transport_status != TransportStatus::Disconnected
    }

    fn get_transport(&self) -> Rc<Mutex<Transport>> {
        self.transports
            .get(self.selected_transport_index)
            .cloned()
            .unwrap()
    }
}

impl Default for SapodillaApp {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();

        Self {
            tx,
            rx,

            transports: Transport::iter()
                .map(|transport| Rc::new(Mutex::new(transport)))
                .collect(),
            transport_names: Transport::iter()
                .map(|transport| transport.name())
                .collect(),
            selected_transport_index: 0,
            transport_status: TransportStatus::Disconnected,

            packet_id: 0,
            packets: Default::default(),
            viewing_packet: None,

            showing_packet_log: false,
            showing_avocado_packet_debug: false,
            avocado_debug_packets: Default::default(),

            error: None,
        }
    }
}

impl SapodillaApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        Default::default()
    }
}

impl eframe::App for SapodillaApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        while let Ok(action) = self.rx.try_recv() {
            info!("got action: {action:?}");

            match action {
                Action::Error(err) => {
                    self.error = Some(err);

                    if self.has_connected_transport() {
                        self.transport_status = TransportStatus::Disconnected;

                        let transport = self.get_transport();

                        spawn(async move {
                            if let Err(err) = transport.lock().await.disconnect().await {
                                error!("could not disconnect from transport after error: {err}");
                            }
                        });
                    }
                }
                Action::ChangeTransport(index) => {
                    self.selected_transport_index = index;
                }
                Action::TransportEvent(event) => match event {
                    TransportEvent::StatusChange(status) => {
                        self.transport_status = status;
                    }
                    TransportEvent::Error(err) => {
                        self.error = Some(err);
                    }
                },
                Action::GotPacket(packet) => {
                    if self.packets.len() >= 999 {
                        self.packets.pop_back();
                    }

                    self.packets.push_front(packet);
                }
                Action::LoadedAvocadoPackets(packets) => self.avocado_debug_packets = Some(packets),
            }
        }

        egui::TopBottomPanel::top("top_panel").show(ctx, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                egui::widgets::global_theme_preference_switch(ui);

                ui.separator();

                let is_web = cfg!(target_arch = "wasm32");
                if !is_web {
                    ui.menu_button("File", |ui| {
                        if ui.button("Quit").clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    });
                }

                ui.menu_button("Connection", |ui| {
                    ui.menu_button("Transport", |ui| {
                        for (index, transport) in self.transport_names.iter().enumerate() {
                            if ui
                                .radio(self.selected_transport_index == index, transport.as_ref())
                                .clicked()
                            {
                                if self.has_connected_transport() {
                                    let tx = self.tx.clone();
                                    self.transport_status = TransportStatus::Disconnecting;

                                    let transport = self.get_transport();

                                    spawn(async move {
                                        if let Err(err) = transport.lock().await.disconnect().await
                                        {
                                            tx.send(Action::Error(err)).unwrap();
                                        } else {
                                            tx.send(Action::ChangeTransport(index)).unwrap();
                                        }
                                    });
                                } else {
                                    self.selected_transport_index = index;
                                }
                            }
                        }
                    });
                });

                ui.menu_button("Debug Tools", |ui| {
                    ui.checkbox(&mut self.showing_packet_log, "Show Packet Log");
                    ui.checkbox(
                        &mut self.showing_avocado_packet_debug,
                        "Saved Packet Debugger",
                    );
                });
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Sapodilla");

            match self.transport_status {
                TransportStatus::Connected => {
                    if ui.button("Disconnect").clicked() {
                        let transport = self.get_transport();
                        spawn(async move {
                            transport.lock().await.disconnect().await.unwrap();
                        });
                    }

                    if ui.button("Send Get Prop Packet").clicked() {
                        self.packet_id += 1;
                        let transport = self.get_transport();
                        let id = self.packet_id;
                        let tx = self.tx.clone();
                        let ctx = ctx.clone();

                        spawn(async move {
                            transport
                                .lock()
                                .await
                                .send_packet(
                                    AvocadoPacket {
                                        version: 100,
                                        content_type: crate::protocol::ContentType::Message,
                                        interaction_type: crate::protocol::InteractionType::Request,
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
                                                "model",
                                                "mac-address",
                                                "serial-number",
                                                "sn-pcba",
                                                "firmware-revision",
                                                "hardware-revision",
                                                "bt-phone-mac",
                                                "printer-state",
                                                "printer-sub-state",
                                                "printer-state-alerts",
                                                "auto-off-interval"
                                            ]
                                        }))
                                        .unwrap(),
                                    },
                                    move |packet| {
                                        tx.send(Action::GotPacket(packet)).unwrap();
                                        ctx.request_repaint();
                                    },
                                )
                                .await
                                .unwrap();
                        });
                    }
                }
                TransportStatus::Connecting => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Connecting");
                    });
                }

                TransportStatus::Disconnecting => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Disconnecting");
                    });
                }

                TransportStatus::Disconnected => {
                    if ui.button("Connect").clicked() {
                        let transport = self.get_transport();

                        let tx = self.tx.clone();
                        let ctx = ctx.clone();

                        spawn(async move {
                            let (event_tx, mut event_rx) = futures::channel::mpsc::unbounded();

                            spawn({
                                let tx = tx.clone();
                                let ctx = ctx.clone();

                                async move {
                                    while let Some(event) = event_rx.next().await {
                                        tx.send(Action::TransportEvent(event)).unwrap();
                                        ctx.request_repaint();
                                    }
                                }
                            });

                            let mut transport = transport.lock().await;
                            if let Err(err) = transport.start(event_tx).await {
                                tx.send(Action::Error(err)).unwrap();
                            }
                        });
                    }
                }
            }

            egui::Window::new("Packet Log")
                .open(&mut self.showing_packet_log)
                .default_size([1000.0, 300.0])
                .show(ctx, |ui| {
                    views::protocol_packets_table(ui, &self.packets, &mut self.viewing_packet)
                });

            if let Some(err) = &self.error {
                let modal = Modal::new(Id::new("error_modal")).show(ui.ctx(), |ui| {
                    ui.set_width(380.0);
                    ui.heading("Error");

                    ui.label(err.to_string());

                    if ui.button("Close").clicked() {
                        ui.close();
                    }
                });

                if modal.should_close() {
                    self.error = None;
                }
            }
        });

        views::packet_debug(
            ctx,
            &self.tx,
            &mut self.showing_avocado_packet_debug,
            &self.avocado_debug_packets,
        );
    }
}
