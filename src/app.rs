use std::{borrow::Cow, collections::VecDeque, sync::mpsc};

use base64::Engine;
use eframe::wasm_bindgen::JsCast;
use egui::{Id, KeyboardShortcut, Modal, Modifiers, Pos2, Vec2, emath};
use futures::{StreamExt, lock::Mutex};
use image::{EncodableLayout, GenericImage, GenericImageView};
use strum::IntoEnumIterator;
use tracing::{debug, error, info};
use uuid::Uuid;

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
    LoadedImage(#[debug(skip)] anyhow::Result<LoadedImage>),
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

    canvas_rect: egui::Rect,
    loaded_images: Vec<LoadedImage>,

    error: Option<anyhow::Error>,
}

pub struct LoadedImage {
    sized_texture: egui::load::SizedTexture,
    offset: Pos2,
    image: image::RgbImage,

    // We need this handle so egui doesn't drop the texture.
    #[allow(dead_code)]
    handle: egui::TextureHandle,
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

    fn upload_image(&self, ctx: &egui::Context) {
        let ctx = ctx.clone();
        let tx = self.tx.clone();

        spawn(async move {
            let file = rfd::AsyncFileDialog::new()
                .add_filter("image", &["jpg", "png"])
                .pick_file()
                .await;

            if let Some(file) = file {
                let data = file.read().await;

                let im = match image::load_from_memory(&data) {
                    Ok(im) => im,
                    Err(err) => {
                        tx.send(Action::LoadedImage(Err(err.into()))).unwrap();
                        return;
                    }
                };

                let (width, height) = im.dimensions();

                let im = im.to_rgb8();
                let color_image =
                    egui::ColorImage::from_rgb([width as usize, height as usize], im.as_bytes());

                let handle =
                    ctx.load_texture(Uuid::new_v4(), color_image, egui::TextureOptions::LINEAR);

                let sized_texture = egui::load::SizedTexture::new(
                    handle.id(),
                    Vec2::new(width as f32, height as f32),
                );

                tx.send(Action::LoadedImage(Ok(LoadedImage {
                    handle,
                    sized_texture,
                    image: im,
                    offset: Pos2::ZERO,
                })))
                .unwrap();
                ctx.request_repaint();
            }
        });
    }

    fn render_image(&self) -> image::DynamicImage {
        let mut buf =
            image::ImageBuffer::from_pixel(4 * 300, 6 * 300, image::Rgb([255u8, 255, 255]));

        for loaded_image in &self.loaded_images {
            let offset_x = loaded_image.offset.x as i32;
            let offset_y = loaded_image.offset.y as i32;

            let size_x = loaded_image.sized_texture.size.x as i32;
            let size_y = loaded_image.sized_texture.size.y as i32;

            let start_x = -offset_x.min(0);
            let start_y = -offset_y.min(0);

            let end_x = offset_x.max(0);
            let end_y = offset_y.max(0);

            let width_limit = (size_x - start_x).min(buf.width() as i32 - end_x);
            let height_limit = (size_y - start_y).min(buf.height() as i32 - end_y);

            debug!(
                offset_x,
                offset_y,
                size_x,
                size_y,
                start_x,
                start_y,
                width_limit,
                height_limit,
                "calculated image position"
            );

            let view = loaded_image
                .image
                .view(
                    start_x as u32,
                    start_y as u32,
                    width_limit as u32,
                    height_limit as u32,
                )
                .to_image();

            buf.copy_from(&view, end_x as u32, end_y as u32).unwrap();
        }

        buf.into()
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

            canvas_rect: egui::Rect::ZERO,
            loaded_images: Default::default(),

            error: None,
        }
    }
}

impl SapodillaApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        egui_extras::install_image_loaders(&cc.egui_ctx);

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
                Action::LoadedImage(res) => match res {
                    Ok(image) => {
                        self.loaded_images.push(image);
                    }
                    Err(err) => self.error = Some(err),
                },
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

                let image_shortcut = KeyboardShortcut::new(Modifiers::COMMAND, egui::Key::U);
                if ui.input_mut(|i| i.consume_shortcut(&image_shortcut)) {
                    self.upload_image(ctx);
                }

                ui.menu_button("Canvas", |ui| {
                    let btn = egui::Button::new("Add Image")
                        .shortcut_text(ctx.format_shortcut(&image_shortcut));

                    if ui.add(btn).clicked() {
                        self.upload_image(ctx);
                    }
                });

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

                    ui.separator();

                    if ui.button("Export Canvas").clicked() {
                        let im = self.render_image();

                        let mut buf = Vec::with_capacity(1024 * 1024);
                        let mut quality = 100;
                        loop {
                            // Image needs to be under 1MB, so decrease quality
                            // until we get there.
                            let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(
                                &mut buf, quality,
                            );
                            encoder.encode_image(&im).unwrap();
                            debug!(quality, len = buf.len(), "got jpeg size");

                            if buf.len() <= 1024 * 1024 || quality == 0 {
                                break;
                            }

                            quality -= 1;
                            buf.clear();
                        }

                        let mut output = String::from("data:image/jpeg;base64,");
                        base64::engine::general_purpose::STANDARD.encode_string(&buf, &mut output);

                        let doc = web_sys::window().unwrap().document().unwrap();
                        let link = doc.create_element("a").unwrap();
                        link.set_attribute("href", &output).unwrap();
                        link.set_attribute("download", "canvas.jpeg").unwrap();

                        let link: &web_sys::HtmlAnchorElement = link.dyn_ref().unwrap();
                        link.click();
                    }
                });
            });

            let heading_style = egui::TextStyle::resolve(&egui::TextStyle::Heading, &ctx.style());
            ui.label(egui::RichText::new("Sapodilla").font(egui::FontId {
                size: heading_style.size * 2.0,
                ..heading_style
            }));
        });

        egui::SidePanel::right("control_panel")
            .resizable(true)
            .default_width(350.0)
            .width_range(200.0..=400.0)
            .show(ctx, |ui| {
                ui.heading("Device");

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
                                            interaction_type:
                                                crate::protocol::InteractionType::Request,
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
                                    ctx.request_repaint();
                                }
                            });
                        }
                    }
                }
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            let scene = egui::Scene::new().zoom_range(0.1..=3.0);

            let mut inner_rect = egui::Rect::NAN;

            let response = scene
                .show(ui, &mut self.canvas_rect, |ui| {
                    egui::Frame::canvas(ui.style())
                        .fill(egui::Color32::WHITE)
                        .inner_margin(0.0)
                        .stroke(egui::Stroke::new(4.0, egui::Color32::BLACK))
                        .show(ui, |ui| {
                            let size = Vec2::new(4.0 * 300.0, 6.0 * 300.0);

                            ui.set_min_size(size);
                            ui.set_max_size(size);

                            let (response, painter) =
                                ui.allocate_painter(size, egui::Sense::empty());

                            let to_screen = emath::RectTransform::from_to(
                                egui::Rect::from_min_size(Pos2::ZERO, response.rect.size()),
                                response.rect,
                            );

                            let mut remove = None;

                            for (idx, image) in self.loaded_images.iter_mut().enumerate() {
                                let pos_in_screen = to_screen.transform_pos(image.offset);
                                let image_rect = egui::Rect::from_min_size(
                                    pos_in_screen,
                                    image.sized_texture.size,
                                );

                                let rect_id = response.id.with(idx);
                                let rect_response =
                                    ui.interact(image_rect, rect_id, egui::Sense::drag());

                                image.offset += rect_response.drag_delta();

                                let pos_in_screen = to_screen.transform_pos(image.offset);

                                let tint = if rect_response.hovered() {
                                    egui::Color32::LIGHT_BLUE
                                } else {
                                    egui::Color32::WHITE
                                };

                                if rect_response.hovered()
                                    && ui.input_mut(|i| {
                                        i.consume_shortcut(&KeyboardShortcut::new(
                                            Modifiers::NONE,
                                            egui::Key::Delete,
                                        )) || i.consume_shortcut(&KeyboardShortcut::new(
                                            Modifiers::NONE,
                                            egui::Key::Backspace,
                                        ))
                                    })
                                {
                                    remove = Some(idx);
                                } else {
                                    painter.image(
                                        image.sized_texture.id,
                                        egui::Rect::from_min_size(
                                            pos_in_screen,
                                            image.sized_texture.size,
                                        ),
                                        egui::Rect::from_min_max(
                                            Pos2::new(0.0, 0.0),
                                            Pos2::new(1.0, 1.0),
                                        ),
                                        tint,
                                    );
                                }
                            }

                            if let Some(remove) = remove {
                                self.loaded_images.remove(remove);
                            }
                        });
                    inner_rect = ui.min_rect();
                })
                .response;

            if response.double_clicked() {
                self.canvas_rect = inner_rect.shrink(ui.style().spacing.menu_spacing);
            }

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

        egui::Window::new("Packet Log")
            .open(&mut self.showing_packet_log)
            .default_size([1000.0, 300.0])
            .show(ctx, |ui| {
                views::protocol_packets_table(ui, &self.packets, &mut self.viewing_packet)
            });

        views::packet_debug(
            ctx,
            &self.tx,
            &mut self.showing_avocado_packet_debug,
            &self.avocado_debug_packets,
        );
    }
}
