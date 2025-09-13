use std::{collections::VecDeque, io::Cursor, rc::Rc, sync::mpsc};

use futures::{StreamExt, lock::Mutex};
use tracing::{debug, trace};

use crate::{
    protocol::{AvocadoPacket, AvocadoPacketReader, EncodingType, EncryptionMode, ProtocolError},
    transports::{
        Transport, TransportControl, TransportEvent, TransportStatus,
        web_serial::WebSerialTransport,
    },
    views,
};

enum Action {
    TransportConnected(Transport),
    TransportEvent(TransportEvent),
    GotPacket(AvocadoPacket),
    LoadedAvocadoPackets(Result<Vec<AvocadoPacket>, ProtocolError>),
}

pub struct SapodillaApp {
    tx: mpsc::Sender<Action>,
    rx: mpsc::Receiver<Action>,

    transport: Option<Rc<Mutex<Transport>>>,
    transport_status: TransportStatus,
    packet_id: u32,
    packets: VecDeque<AvocadoPacket>,
    viewing_packet: Option<AvocadoPacket>,

    showing_avocado_packets: bool,
    avocado_packets: Option<Result<Vec<AvocadoPacket>, ProtocolError>>,
}

impl Default for SapodillaApp {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();

        Self {
            tx,
            rx,

            transport: None,
            transport_status: TransportStatus::Disconnected,
            packet_id: 0,
            packets: Default::default(),
            viewing_packet: None,

            showing_avocado_packets: false,
            avocado_packets: Default::default(),
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
            match action {
                Action::TransportConnected(transport) => {
                    self.transport = Some(Rc::new(Mutex::new(transport)))
                }
                Action::TransportEvent(event) => match event {
                    TransportEvent::StatusChange(status) => {
                        self.transport_status = status;

                        if status == TransportStatus::Disconnected {
                            self.transport = None;
                        }
                    }
                    TransportEvent::Error(err) => {
                        panic!("transport error: {err}");
                    }
                },
                Action::GotPacket(packet) => {
                    if self.packets.len() >= 999 {
                        self.packets.pop_back();
                    }

                    self.packets.push_front(packet);
                }
                Action::LoadedAvocadoPackets(packets) => self.avocado_packets = Some(packets),
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
                    ui.add_space(16.0);
                }

                ui.menu_button("Debug Tools", |ui| {
                    if ui.button("Decode Packets").clicked() {
                        self.showing_avocado_packets = true;
                        let ctx = ctx.clone();
                        let tx = self.tx.clone();

                        wasm_bindgen_futures::spawn_local(async move {
                            let file = rfd::AsyncFileDialog::new().pick_file().await;
                            if let Some(file) = file {
                                let mut data = file.read().await;
                                data.retain(|c| !c.is_ascii_whitespace());
                                trace!("got data: {data:?}");
                                let data = hex::decode(&data).unwrap_or(data);
                                debug!("processed data: {}", hex::encode(&data));
                                let cursor = Cursor::new(data);
                                let avocado_packets: Result<Vec<_>, _> =
                                    AvocadoPacketReader::new(cursor).collect();
                                tx.send(Action::LoadedAvocadoPackets(avocado_packets))
                                    .unwrap();
                                ctx.request_repaint();
                            }
                        });
                    }
                });
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Sapodilla");

            if let Some(transport) = self.transport.clone() {
                if ui.button("disconnect").clicked() {
                    wasm_bindgen_futures::spawn_local(async move {
                        transport.lock().await.disconnect().await.unwrap();
                    });
                } else if ui.button("send packet").clicked() {
                    self.packet_id += 1;
                    let id = self.packet_id;
                    let tx = self.tx.clone();
                    let ctx = ctx.clone();

                    wasm_bindgen_futures::spawn_local(async move {
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
            } else if ui.button("connect to web serial").clicked() {
                let tx = self.tx.clone();
                let ctx = ctx.clone();

                wasm_bindgen_futures::spawn_local(async move {
                    let mut transport = WebSerialTransport::default();
                    let mut events = transport.start().await.unwrap();

                    tx.send(Action::TransportConnected(transport.into()))
                        .unwrap();
                    ctx.request_repaint();

                    while let Some(event) = events.next().await {
                        tx.send(Action::TransportEvent(event)).unwrap();
                        ctx.request_repaint();
                    }
                });
            }

            egui::CollapsingHeader::new(format!("{} packets", self.packets.len()))
                .id_salt("show_packets")
                .show(ui, |ui| {
                    views::protocol_packets_table(ui, &self.packets, &mut self.viewing_packet);
                });
        });

        egui::Window::new("Avocado Packets")
            .open(&mut self.showing_avocado_packets)
            .default_width(480.0)
            .default_height(320.0)
            .resizable([true, true])
            .scroll(true)
            .show(ctx, |ui| match &self.avocado_packets {
                Some(Ok(packets)) => {
                    for (index, packet) in packets.iter().enumerate() {
                        ui.collapsing(format!("Packet {}", index + 1), |ui| {
                            let theme = egui_extras::syntax_highlighting::CodeTheme::from_memory(
                                ui.ctx(),
                                ui.style(),
                            );
                            egui_extras::syntax_highlighting::code_view_ui(
                                ui,
                                &theme,
                                &serde_json::to_string_pretty(packet).unwrap(),
                                "json",
                            );

                            if packet.encoding_type == EncodingType::Json
                                && packet.encryption_mode == EncryptionMode::None
                            {
                                ui.separator();
                                ui.heading("Packet Data");
                                if let Ok(data) =
                                    serde_json::from_slice::<serde_json::Value>(&packet.data)
                                {
                                    egui_extras::syntax_highlighting::code_view_ui(
                                        ui,
                                        &theme,
                                        &serde_json::to_string_pretty(&data).unwrap(),
                                        "json",
                                    );
                                } else {
                                    ui.monospace(String::from_utf8_lossy(&packet.data));
                                }
                            }
                        });
                    }
                }
                Some(Err(err)) => {
                    ui.label(format!("error! {err}"));
                }
                None => {
                    ui.label("no packets loaded");
                }
            });
    }
}
