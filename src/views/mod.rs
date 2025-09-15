use std::{collections::VecDeque, io::Cursor};

use egui::{Id, Modal, Ui, Vec2};
use egui_extras::{
    Column, TableBuilder,
    syntax_highlighting::{CodeTheme, code_view_ui},
};
use tracing::debug;

use crate::{
    app::{Action, ContextSender},
    protocol::{self, AvocadoId, AvocadoPacket, AvocadoPacketReader, ProtocolError},
    spawn,
};

pub fn pretty_hex(id: impl std::hash::Hash, ui: &mut Ui, data: &[u8]) {
    const SECTIONS_PER_LINE: usize = 4;
    const CHARS_PER_SECTION: usize = 4;

    let default_spacing = ui.ctx().style().spacing.item_spacing;

    egui::Grid::new(id)
        .spacing(Vec2 {
            x: default_spacing.x * 2.0,
            ..default_spacing
        })
        .show(ui, |ui| {
            for row in data.chunks(SECTIONS_PER_LINE * CHARS_PER_SECTION) {
                ui.horizontal(|ui| {
                    for chunk in row.chunks(CHARS_PER_SECTION) {
                        ui.monospace(hex::encode_upper(chunk));
                    }
                });

                // Only display directly visible characters, control characters
                // and newlines would be a problem.
                ui.monospace(
                    String::from_utf8_lossy(row).replace(|c| !(' '..='~').contains(&c), " "),
                );

                ui.end_row();
            }
        });
}

pub fn protocol_packets_table(
    ui: &mut Ui,
    packets: &VecDeque<protocol::AvocadoPacket>,
    viewing_packet: &mut Option<protocol::AvocadoPacket>,
) {
    TableBuilder::new(ui)
        .auto_shrink(false)
        .striped(true)
        .columns(Column::auto().resizable(true), 10)
        .column(Column::remainder().resizable(true))
        .header(20.0, |mut header| {
            const FIELDS: &[&str] = &[
                "Message ID",
                "Request ID",
                "Content Type",
                "Interaction Type",
                "Encoding Type",
                "Encryption Mode",
                "Terminal ID",
                "Message Number",
                "Message Total",
                "Subpackage",
                "Data",
            ];

            for field in FIELDS {
                header.col(|ui| {
                    ui.heading(*field);
                });
            }
        })
        .body(|body| {
            body.rows(20.0, packets.len(), |mut row| {
                let packet = &packets[row.index()];

                row.col(|ui| {
                    ui.label(packet.msg_number.to_string());
                });

                row.col(|ui| {
                    ui.label(
                        packet
                            .as_json::<AvocadoId>()
                            .map(|result| result.id.to_string())
                            .unwrap_or_default(),
                    );
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
                    ui.horizontal(|ui| {
                        ui.label(format!("{} bytes", packet.data.len()));
                        ui.add_space(8.0);
                        if ui.button("View").clicked() {
                            *viewing_packet = Some(packet.clone());
                        }
                    });
                });
            });
        });

    if let Some(packet) = viewing_packet {
        let modal = Modal::new(Id::new(packet.msg_number)).show(ui.ctx(), |ui| {
            ui.set_width(380.0);
            ui.heading("Viewing Packet Data");

            pretty_hex(format!("packet-{}", packet.msg_number), ui, &packet.data);

            ui.separator();

            if let Some(data) = packet.as_json::<serde_json::Value>() {
                let theme = CodeTheme::from_memory(ui.ctx(), ui.style());
                code_view_ui(
                    ui,
                    &theme,
                    &serde_json::to_string_pretty(&data).unwrap_or_default(),
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

pub fn packet_debug(
    ctx: &egui::Context,
    tx: &ContextSender<Action>,
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

                        let _ = tx.send(Action::LoadedAvocadoPackets(avocado_packets));
                        ctx.request_repaint();
                    }
                });
            }

            match packets {
                Some(Ok(packets)) => {
                    let has_exactly_one = packets.len() == 1;

                    for (index, packet) in packets.iter().enumerate() {
                        packet_details(ui, has_exactly_one, index, packet);
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

fn packet_details(ui: &mut Ui, has_exactly_one: bool, index: usize, packet: &AvocadoPacket) {
    egui::CollapsingHeader::new(format!("Packet {}", index + 1))
        .default_open(has_exactly_one)
        .show(ui, |ui| {
            let theme = CodeTheme::from_memory(ui.ctx(), ui.style());
            ui.style_mut().spacing.item_spacing = Vec2::new(8.0, 16.0);

            code_view_ui(
                ui,
                &theme,
                &serde_json::to_string_pretty(packet).unwrap_or_default(),
                "json",
            );

            ui.heading("Packet Data (hex)");
            pretty_hex(format!("packet-{index}"), ui, &packet.data);

            if let Some(data) = packet.as_json::<serde_json::Value>() {
                ui.heading("Packet Data (json)");
                code_view_ui(
                    ui,
                    &theme,
                    &serde_json::to_string_pretty(&data).unwrap_or_default(),
                    "json",
                );
            }
        });
}
