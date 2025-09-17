use std::{collections::VecDeque, io::Cursor, ops::RangeInclusive};

use egui::{Id, Modal, Pos2, ProgressBar, Ui, Vec2};
use egui_extras::{
    Column, TableBuilder,
    syntax_highlighting::{CodeTheme, code_view_ui},
};
use tracing::debug;

use crate::{
    app::{Action, ContextSender, LoadedImage},
    cut::CutTuning,
    protocol::{self, AvocadoId, AvocadoPacket, AvocadoPacketReader, ProtocolError},
    spawn,
};

pub use canvas::canvas_editor;

mod canvas;

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

pub fn loaded_images(ui: &mut Ui, loaded_images: &mut Vec<LoadedImage>) {
    ui.heading("Images");

    let mut remove = None;

    for (index, image) in loaded_images.iter_mut().enumerate() {
        image_controls(ui, image, index, &mut remove);

        ui.add_space(16.0);
    }

    if let Some(remove) = remove {
        loaded_images.remove(remove);
    }
}

pub fn image_controls(
    ui: &mut Ui,
    image: &mut LoadedImage,
    index: usize,
    remove_index: &mut Option<usize>,
) {
    ui.horizontal(|ui| {
        let (response, painter) = ui.allocate_painter(Vec2::splat(50.0), egui::Sense::empty());

        painter.image(
            image.sized_texture.id,
            response.rect,
            egui::Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1.0, 1.0)),
            egui::Color32::WHITE,
        );

        ui.vertical(|ui| {
            ui.spacing_mut().interact_size.x = 72.0;
            ui.spacing_mut().item_spacing.y = 8.0;

            ui.horizontal(|ui| {
                ui.monospace("X:");
                ui.add(px_slider(
                    &mut image.offset.x,
                    300.0,
                    (-image.sized_texture.size.x * 2.0)
                        ..=((4.0 * 300.0) + image.sized_texture.size.x * 2.0),
                ));

                ui.monospace("Y:");
                ui.add(px_slider(
                    &mut image.offset.y,
                    300.0,
                    (-image.sized_texture.size.y * 2.0)
                        ..=((6.0 * 300.0) + image.sized_texture.size.y * 2.0),
                ));
            });

            ui.horizontal(|ui| {
                ui.monospace("W:");
                let mut width = image.size().x;
                ui.add(px_slider(&mut width, 300.0, 1.0..=(4.0 * 300.0 * 10.0)));

                if width != image.size().x {
                    let new_scale = if image.scale_locked {
                        width / image.size().x * image.scale
                    } else {
                        Vec2 {
                            x: width / image.size().x * image.scale.x,
                            ..image.scale
                        }
                    };

                    image.rescale(new_scale);
                }

                ui.monospace("H:");
                let mut height = image.size().y;
                ui.add(px_slider(&mut height, 300.0, 1.0..=(4.0 * 300.0 * 10.0)));

                if height != image.size().y {
                    let new_scale = if image.scale_locked {
                        height / image.size().y * image.scale
                    } else {
                        Vec2 {
                            y: height / image.size().y * image.scale.y,
                            ..image.scale
                        }
                    };

                    image.rescale(new_scale);
                }

                if ui
                    .small_button(if image.scale_locked { "Unlock" } else { "Lock" })
                    .clicked()
                {
                    image.scale_locked = !image.scale_locked;
                }
            });

            if ui.small_button("Remove").clicked() {
                *remove_index = Some(index);
            }
        });
    });
}

pub fn px_slider<'a>(
    value: &'a mut f32,
    dpi: f64,
    range: RangeInclusive<f32>,
) -> egui::DragValue<'a> {
    egui::DragValue::new(value)
        .max_decimals(0)
        .suffix(" px")
        .range(range)
        .custom_parser(move |val| {
            let lower = val.trim().to_ascii_lowercase();

            if let Some(val) = lower.strip_suffix("in") {
                val.trim().parse().map(|val: f64| val * dpi).ok()
            } else {
                val.strip_suffix("px").unwrap_or(&lower).trim().parse().ok()
            }
        })
}

pub fn cut_controls(
    ui: &mut Ui,
    cut_tuning: &mut CutTuning,
    progress: Option<(usize, usize)>,
    has_intersections: bool,
    off_canvas: bool,
) {
    ui.heading("Cut Preparation");

    let progress_pct = progress
        .map(|(completed, total)| completed as f32 / total as f32)
        .unwrap_or(0.0);

    ui.add_visible(
        progress.is_some(),
        ProgressBar::new(progress_pct)
            .animate(progress.is_some())
            .show_percentage(),
    );

    ui.checkbox(&mut cut_tuning.internal, "Allow Internal Cuts");

    let mut buffer = cut_tuning.buffer / 300.0 * 25.4;
    ui.add(
        egui::Slider::new(&mut buffer, 0.0..=5.0)
            .suffix(" mm")
            .text("Padding Distance"),
    )
    .on_hover_text("Padding between the edges of the sticker and the cutline");
    cut_tuning.buffer = buffer * 300.0 / 25.4;

    let mut minimum_length = cut_tuning.minimum_length / 300.0;
    ui.add(
        egui::Slider::new(&mut minimum_length, 0.05..=1.0)
            .suffix(" in")
            .text("Minimum Cut Length"),
    )
    .on_hover_text("Minimum length to cut, anything smaller will be ignored");
    cut_tuning.minimum_length = minimum_length * 300.0;

    ui.collapsing("Advanced Settings", |ui| {
        ui.add(egui::Slider::new(&mut cut_tuning.simplify, 0.0..=5.0).text("Simplify Amount"))
            .on_hover_text("Simplification epsilon, decreases total number of line segments");

        ui.horizontal(|ui| {
            ui.add(
                egui::DragValue::new(&mut cut_tuning.smoothing)
                    .range(0..=10)
                    .speed(0.05),
            );
            ui.label("Smoothing Steps");
        })
        .response
        .on_hover_text("Increases number of smoothing iterations");
    });

    let error_messages: Vec<_> = [
        has_intersections.then_some("Cut Lines Overlap"),
        off_canvas.then_some("Cut Lines Out of Bounds"),
    ]
    .into_iter()
    .flatten()
    .collect();

    if error_messages.is_empty() {
        ui.add_visible(
            false,
            egui::Label::new(
                egui::RichText::new("Error Message")
                    .strong()
                    .color(egui::Color32::RED),
            ),
        );
    } else {
        ui.horizontal(|ui| {
            for message in error_messages {
                ui.add(egui::Label::new(
                    egui::RichText::new(message)
                        .strong()
                        .color(egui::Color32::RED),
                ));
            }
        });
    }
}
