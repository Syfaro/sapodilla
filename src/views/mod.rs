use std::{collections::VecDeque, io::Cursor};

use egui::{Id, Modal, Ui, Vec2};
use egui_extras::{Column, TableBuilder};
use tracing::debug;

use crate::{
    app::Action,
    protocol::{self, AvocadoPacket, AvocadoPacketReader, ProtocolError},
    spawn,
};

pub fn protocol_packets_table(
    ui: &mut Ui,
    packets: &VecDeque<protocol::AvocadoPacket>,
    viewing_packet: &mut Option<protocol::AvocadoPacket>,
) {
    TableBuilder::new(ui)
        .auto_shrink(false)
        .striped(true)
        .columns(Column::auto().resizable(true), 9)
        .column(Column::remainder().resizable(true))
        .header(20.0, |mut header| {
            header.col(|ui| {
                ui.heading("Message ID");
            });

            header.col(|ui| {
                ui.heading("Content Type");
            });

            header.col(|ui| {
                ui.heading("Interaction Type");
            });

            header.col(|ui| {
                ui.heading("Encoding Type");
            });

            header.col(|ui| {
                ui.heading("Encryption Mode");
            });

            header.col(|ui| {
                ui.heading("Terminal ID");
            });

            header.col(|ui| {
                ui.heading("Message Number");
            });

            header.col(|ui| {
                ui.heading("Message Total");
            });

            header.col(|ui| {
                ui.heading("Subpackage");
            });

            header.col(|ui| {
                ui.heading("Data");
            });
        })
        .body(|body| {
            body.rows(20.0, packets.len(), |mut row| {
                let packet = &packets[row.index()];

                row.col(|ui| {
                    ui.label(packet.msg_number.to_string());
                });

                row.col(|ui| {
                    ui.label(packet.content_type.to_string());
                });

                row.col(|ui| {
                    ui.label(packet.interaction_type.to_string());
                });

                row.col(|ui| {
                    ui.label(packet.encoding_type.to_string());
                });

                row.col(|ui| {
                    ui.label(packet.encryption_mode.to_string());
                });

                row.col(|ui| {
                    ui.label(packet.terminal_id.to_string());
                });

                row.col(|ui| {
                    ui.label(packet.msg_package_num.to_string());
                });

                row.col(|ui| {
                    ui.label(packet.msg_package_total.to_string());
                });

                row.col(|ui| {
                    ui.label(packet.is_subpackage.to_string());
                });

                row.col(|ui| {
                    if ui
                        .button(format!("view {} bytes", packet.data.len()))
                        .clicked()
                    {
                        *viewing_packet = Some(packet.clone());
                    }
                });
            });
        });

    if let Some(packet) = viewing_packet {
        let modal = Modal::new(Id::new(packet.msg_number)).show(ui.ctx(), |ui| {
            ui.set_width(380.0);
            ui.heading("Viewing Packet Data");

            pretty_hex(ui, &packet.data);

            ui.separator();

            if let Some(data) = packet.as_json::<serde_json::Value>() {
                let theme =
                    egui_extras::syntax_highlighting::CodeTheme::from_memory(ui.ctx(), ui.style());
                egui_extras::syntax_highlighting::code_view_ui(
                    ui,
                    &theme,
                    &serde_json::to_string_pretty(&data).unwrap(),
                    "json",
                );
            };

            if ui.button("Close").clicked() {
                ui.close();
            }
        });

        if modal.should_close() {
            *viewing_packet = None;
        }
    }
}

pub fn pretty_hex(ui: &mut Ui, data: &[u8]) {
    let default_spacing = ui.ctx().style().spacing.item_spacing;

    egui::Grid::new("hex_grid")
        .spacing(Vec2 {
            x: default_spacing.x * 2.0,
            ..default_spacing
        })
        .show(ui, |ui| {
            for row in data.chunks(4 * 4) {
                ui.horizontal(|ui| {
                    for chunk in row.chunks(4) {
                        ui.monospace(hex::encode_upper(chunk));
                    }
                });

                ui.monospace(
                    String::from_utf8_lossy(row).replace(|c| !(' '..='~').contains(&c), " "),
                );

                ui.end_row();
            }
        });
}

pub fn packet_debug(
    ctx: &egui::Context,
    tx: &std::sync::mpsc::Sender<Action>,
    show: &mut bool,
    packets: &Option<Result<Vec<AvocadoPacket>, ProtocolError>>,
) {
    egui::Window::new("Saved Packet Debugger")
        .open(show)
        .default_width(480.0)
        .default_height(320.0)
        .resizable([true, true])
        .scroll(true)
        .show(ctx, |ui| {
            if ui.button("Select File").clicked() {
                let ctx = ctx.clone();
                let tx = tx.clone();

                spawn(async move {
                    let file = rfd::AsyncFileDialog::new().pick_file().await;
                    if let Some(file) = file {
                        let data = file.read().await;

                        let mut maybe_hex_data = data.clone();
                        maybe_hex_data.retain(|c| !c.is_ascii_whitespace());

                        let data = hex::decode(&maybe_hex_data).unwrap_or(data);
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

            match packets {
                Some(Ok(packets)) => {
                    for (index, packet) in packets.iter().enumerate() {
                        egui::CollapsingHeader::new(format!("Packet {}", index + 1))
                            .default_open(packets.len() == 1)
                            .show(ui, |ui| {
                                let theme =
                                    egui_extras::syntax_highlighting::CodeTheme::from_memory(
                                        ui.ctx(),
                                        ui.style(),
                                    );
                                egui_extras::syntax_highlighting::code_view_ui(
                                    ui,
                                    &theme,
                                    &serde_json::to_string_pretty(packet).unwrap(),
                                    "json",
                                );

                                ui.separator();
                                ui.heading("Packet Data - Hex");
                                pretty_hex(ui, &packet.data);

                                if let Some(data) = packet.as_json::<serde_json::Value>() {
                                    ui.separator();
                                    ui.heading("Packet Data - JSON");
                                    egui_extras::syntax_highlighting::code_view_ui(
                                        ui,
                                        &theme,
                                        &serde_json::to_string_pretty(&data).unwrap(),
                                        "json",
                                    );
                                }
                            });
                    }
                }
                Some(Err(err)) => {
                    ui.label(format!("Error! {err}"));
                }
                None => {
                    ui.label("No packets loaded");
                }
            }
        });
}
