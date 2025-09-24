use std::{borrow::Cow, collections::VecDeque, io::Write, sync::mpsc};

use egui::{Id, KeyboardShortcut, Modal, Modifiers, Pos2, Vec2};
use futures::{StreamExt, lock::Mutex};
use image::{EncodableLayout, GenericImageView};
use serde::Deserialize;
use sha1::Digest;
use strum::IntoEnumIterator;
use tracing::{debug, error, info, trace};
use uuid::Uuid;

use crate::{
    Rc,
    cut::{CutAction, CutGenerator, CutTuning},
    protocol::*,
    spawn,
    transports::*,
    views,
};

#[derive(derive_more::Debug)]
pub enum Action {
    Error(anyhow::Error),
    ChangeTransport(usize),
    TransportEvent(TransportEvent),
    LoadedAvocadoPackets(Result<Vec<AvocadoPacket>, ProtocolError>),
    LoadedImage(#[debug(skip)] anyhow::Result<LoadedImage>),
    SendProgress(f32),
    Cut(CutAction),
}

pub struct SapodillaApp {
    pub tx: ContextSender<Action>,
    pub rx: mpsc::Receiver<Action>,

    pub transports: Vec<Rc<Mutex<Transport>>>,
    pub transport_names: Vec<Cow<'static, str>>,
    pub selected_transport_index: usize,

    pub transport_manager: Option<Rc<TransportManager>>,
    pub transport_status: TransportStatus,

    pub selected_device: usize,
    pub selected_mode: usize,
    pub selected_canvas_size: usize,
    pub previous_canvas_size: Vec2,
    pub copies: usize,

    pub device_status: Option<(PrinterState, PrinterSubState, String)>,
    pub job_status: Option<JobStatusInfo>,
    pub send_progress: Option<f32>,

    pub packets: VecDeque<AvocadoPacket>,
    pub viewing_packet: Option<AvocadoPacket>,
    pub cut_tuning: CutTuning,
    pub cut_shapes: Vec<geo::MultiPolygon<f32>>,
    pub has_intersections: bool,
    pub off_canvas: bool,
    pub cut_progress: Option<(usize, usize)>,

    pub showing_packet_log: bool,
    pub showing_avocado_packet_debug: bool,
    pub avocado_debug_packets: Option<Result<Vec<AvocadoPacket>, ProtocolError>>,

    pub canvas_rect: egui::Rect,
    pub loaded_images: Vec<LoadedImage>,

    pub error: Option<anyhow::Error>,
}

pub struct ContextSender<A> {
    tx: mpsc::Sender<A>,
    ctx: egui::Context,
}

impl<A> Clone for ContextSender<A> {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
            ctx: self.ctx.clone(),
        }
    }
}

impl<A> ContextSender<A> {
    pub fn new(tx: mpsc::Sender<A>, ctx: egui::Context) -> Self {
        Self { tx, ctx }
    }

    pub fn send(&self, action: A) -> Result<(), mpsc::SendError<A>> {
        self.tx.send(action)?;
        self.ctx.request_repaint();
        Ok(())
    }
}

#[derive(Clone)]
pub struct LoadedImage {
    pub image: image::RgbaImage,
    pub sized_texture: egui::load::SizedTexture,

    pub offset: Pos2,
    pub scale: Vec2,
    pub scale_locked: bool,

    // We need this handle so egui doesn't drop the texture.
    #[allow(dead_code)]
    handle: egui::TextureHandle,
}

impl LoadedImage {
    pub fn new(ctx: &egui::Context, data: &[u8], offset: Option<Pos2>) -> anyhow::Result<Self> {
        let im = image::load_from_memory(data)?;
        trace!("loaded image");

        let (width, height) = im.dimensions();
        trace!(width, height, "got image size");

        let im = im.to_rgba8();
        let color_image = egui::ColorImage::from_rgba_unmultiplied(
            [width as usize, height as usize],
            im.as_bytes(),
        );

        let handle = ctx.load_texture(Uuid::new_v4(), color_image, egui::TextureOptions::LINEAR);
        let sized_texture =
            egui::load::SizedTexture::new(handle.id(), Vec2::new(width as f32, height as f32));
        trace!(id = ?handle.id(), "finished loading texture");

        Ok(LoadedImage {
            image: im,
            sized_texture,
            offset: offset.unwrap_or(Pos2::ZERO),
            scale: Vec2::splat(1.0),
            scale_locked: true,
            handle,
        })
    }

    pub fn size(&self) -> Vec2 {
        self.sized_texture.size * self.scale
    }

    pub fn rescale(&mut self, new_scale: Vec2) {
        if self.scale == new_scale {
            return;
        }

        let current_size = self.size();
        self.scale = new_scale;
        let new_size = self.size();

        let change = (new_size - current_size) / 2.0;
        self.offset -= change;
    }
}

impl SapodillaApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        egui_extras::install_image_loaders(&cc.egui_ctx);

        let (tx, rx) = mpsc::channel();
        let tx = ContextSender::new(tx, cc.egui_ctx.clone());

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
            transport_manager: None,

            selected_device: 0,
            selected_mode: 0,
            selected_canvas_size: 0,
            previous_canvas_size: Vec2::ZERO,
            copies: 1,

            device_status: None,
            job_status: None,
            send_progress: None,

            packets: Default::default(),
            viewing_packet: None,
            cut_tuning: Default::default(),
            cut_shapes: Vec::new(),
            has_intersections: false,
            off_canvas: false,
            cut_progress: None,

            showing_packet_log: false,
            showing_avocado_packet_debug: false,
            avocado_debug_packets: Default::default(),

            canvas_rect: egui::Rect::ZERO,
            loaded_images: Default::default(),

            error: None,
        }
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

                let action = match LoadedImage::new(&ctx, &data, None) {
                    Ok(image) => Action::LoadedImage(Ok(image)),
                    Err(err) => Action::LoadedImage(Err(err)),
                };

                tx.send(action).unwrap();
            }
        });
    }

    fn render_image(&self) -> image::DynamicImage {
        let canvas = self.get_canvas().size;

        let mut buf = image::ImageBuffer::from_pixel(
            canvas.x as u32,
            canvas.y as u32,
            image::Rgba::<u8>([255, 255, 255, 255]),
        );

        for loaded_image in &self.loaded_images {
            let image_size = loaded_image.size();

            let resized_image = if loaded_image.scale == Vec2::ONE {
                Cow::Borrowed(&loaded_image.image)
            } else {
                Cow::Owned(image::imageops::resize(
                    &loaded_image.image,
                    image_size.x as u32,
                    image_size.y as u32,
                    image::imageops::FilterType::Lanczos3,
                ))
            };

            let offset_x = loaded_image.offset.x as i32;
            let offset_y = loaded_image.offset.y as i32;

            let size_x = image_size.x as i32;
            let size_y = image_size.y as i32;

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

            let view = resized_image
                .view(
                    start_x as u32,
                    start_y as u32,
                    width_limit as u32,
                    height_limit as u32,
                )
                .to_image();

            image::imageops::overlay(&mut buf, &view, end_x as i64, end_y as i64);
        }

        buf.into()
    }

    pub fn get_canvas(&self) -> &'static CanvasSize {
        &DEVICES[self.selected_device].modes[self.selected_mode].canvas_sizes
            [self.selected_canvas_size]
    }

    fn apply_actions(&mut self) {
        while let Ok(action) = self.rx.try_recv() {
            info!("got action: {action:?}");

            match action {
                Action::Error(err) => {
                    self.error = Some(err);

                    if let Some(manager) = self.transport_manager.take() {
                        spawn(async move {
                            if let Err(err) = manager.disconnect().await {
                                error!("could not disconnect from transport after error: {err}");
                            }
                        });
                    }
                }
                Action::ChangeTransport(index) => {
                    self.selected_transport_index = index;
                }
                Action::TransportEvent(event) => match event {
                    TransportEvent::Packet(packet) => {
                        if self.packets.len() >= 999 {
                            self.packets.pop_back();
                        }

                        self.packets.push_front(packet);
                    }
                    TransportEvent::TransportStatus(status) => {
                        self.transport_status = status;

                        if status == TransportStatus::Disconnecting {
                            self.device_status = None;
                        }
                    }
                    TransportEvent::DeviceStatus(status) => {
                        self.device_status = Some(status);
                    }
                    TransportEvent::JobStatus(status) => {
                        self.job_status = Some(status);
                    }
                    TransportEvent::Error(err) => {
                        self.error = Some(err);
                    }
                },

                Action::LoadedAvocadoPackets(packets) => self.avocado_debug_packets = Some(packets),
                Action::LoadedImage(res) => match res {
                    Ok(image) => {
                        self.loaded_images.push(image);
                    }
                    Err(err) => self.error = Some(err),
                },
                Action::SendProgress(pct) => {
                    self.send_progress = Some(pct);
                }
                Action::Cut(action) => match action {
                    CutAction::Progress { completed, total } => {
                        self.cut_progress = Some((completed, total));
                    }
                    CutAction::Done(result) => {
                        self.has_intersections = result.has_intersections;
                        self.cut_shapes = result.polygons;
                        self.cut_progress = None;
                        self.off_canvas = result.off_canvas;
                    }
                },
            }
        }
    }

    fn print_canvas(&mut self) {
        let im = self.render_image();
        let encoded_image = encode_image(&im);
        let encoded_image_len = encoded_image.len();
        let mode = &DEVICES[self.selected_device].modes[self.selected_mode];
        let canvas_size = &mode.canvas_sizes[self.selected_canvas_size];
        let plt = encode_plt(
            &self.cut_shapes,
            DEVICES[self.selected_device]
                .cutter_calibration
                .clone()
                .unwrap_or_default(),
            canvas_size,
        );

        trace!("plt: {}", String::from_utf8(plt.clone()).unwrap());

        let manager = self.transport_manager.clone();
        let tx = self.tx.clone();
        let copies = self.copies;
        self.send_progress = None;

        let hash = sha1::Sha1::digest(&encoded_image);
        debug!("calculated image hash: {}", hex::encode(hash));

        let time = current_timestamp_millis();

        let packet_data = if mode.mode_type.has_cutting() {
            let mut buf = Vec::with_capacity(encoded_image.len() + plt.len());
            buf.extend_from_slice(&plt);
            buf.extend_from_slice(&encoded_image);
            buf
        } else {
            encoded_image
        };

        spawn(async move {
            let manager = manager.unwrap();
            let id = manager.next_message_id();

            let data = if mode.mode_type.has_cutting() {
                serde_json::json!({
                    "id": id,
                    "method": "combo-job",
                    "params": [
                        {
                            "method": "print-job",
                            "params": {
                                "media-size": canvas_size.media_size,
                                "media-type": canvas_size.media_type,
                                "job-type": mode.mode_type.job_type(),
                                "channel": mode.mode_type.channel(),
                                "file-size": encoded_image_len,
                                "document-format": 9,
                                "document-name": format!("{}.jpeg", time),
                                "hash-method": 1,
                                "hash-value": hex::encode(hash),
                                "user-account": "000000.00000000000000000000000000000000.0000",
                                "job-send-time": time / 1000,
                                "link-type": mode.mode_type.link_type(),
                                "copies": copies,
                            }
                        },
                        {
                            "method": "cut-job",
                            "params": {
                                "copies": copies,
                                "media-size": canvas_size.media_size,
                                "document-name": format!("{}.plt", time),
                                "file-size": plt.len(),
                                "channel": mode.mode_type.channel(),
                                "media-type": canvas_size.media_type,
                                "job-type": mode.mode_type.job_type(),
                                "document-format": 18,
                                "job-send-time": time / 1000,
                            }
                        }
                    ],
                })
            } else {
                serde_json::json!({
                    "id": id,
                    "method": "print-job",
                    "params": {
                        "media-size": canvas_size.media_size,
                        "media-type": canvas_size.media_type,
                        "job-type": mode.mode_type.job_type(),
                        "channel": mode.mode_type.channel(),
                        "file-size": encoded_image_len,
                        "document-format": 9,
                        "document-name": format!("{}.jpeg", time),
                        "hash-method": 1,
                        "hash-value": hex::encode(hash),
                        "user-account": "000000.00000000000000000000000000000000.0000",
                        "link-type": mode.mode_type.link_type(),
                        "job-send-time": time / 1000,
                        "copies": copies,
                    },
                })
            };

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
                data: serde_json::to_vec(&data).unwrap(),
            };
            debug!(?packet, "built print job packet");

            let packet = manager.wait_for_response(packet).await.unwrap();
            debug!(?packet, "got response packet");

            #[derive(Debug, Deserialize)]
            #[serde(rename_all = "kebab-case")]
            struct JobResult {
                job_id: u32,
            }

            let job_id = packet
                .as_json::<AvocadoResult<JobResult>>()
                .unwrap()
                .result
                .job_id;
            debug!(job_id, "got job id");

            manager
                .send_data(job_id, &packet_data, |total, sent| {
                    debug!(total, sent, "sent data packet");
                    let _ = tx.send(Action::SendProgress(sent as f32 / total as f32));
                })
                .await
                .unwrap();

            manager.poll_job(job_id).await.unwrap();
            info!("finished sending data");
        });
    }

    fn menu(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
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

        let image_shortcut =
            KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::SHIFT, egui::Key::U);
        if ui.input_mut(|i| i.consume_shortcut(&image_shortcut)) {
            self.upload_image(ctx);
        }

        ui.menu_button("Canvas", |ui| {
            let btn =
                egui::Button::new("Add Image").shortcut_text(ctx.format_shortcut(&image_shortcut));

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
                        if let Some(manager) = self.transport_manager.take() {
                            let tx = self.tx.clone();
                            spawn(async move {
                                let action = if let Err(err) = manager.disconnect().await {
                                    Action::Error(err)
                                } else {
                                    Action::ChangeTransport(index)
                                };

                                if let Err(err) = tx.send(action) {
                                    error!("could not send action: {err}");
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

            if let Some(manager) = &self.transport_manager
                && ui.button("Send Get Prop Packet").clicked()
            {
                let manager = manager.clone();
                let id = manager.next_message_id();

                spawn(async move {
                    let packet = manager
                        .wait_for_response(AvocadoPacket {
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
                                    "auto-off-interval",
                                    "media-size",
                                ]
                            }))
                            .unwrap(),
                        })
                        .await
                        .unwrap();

                    info!("got info packet: {packet:?}");
                });
            }

            if let Some(manager) = &self.transport_manager
                && ui.button("Send Resume Printer").clicked()
            {
                let manager = manager.clone();
                let id = manager.next_message_id();

                spawn(async move {
                    let packet = manager
                        .wait_for_response(AvocadoPacket {
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
                                "method" : "resume-printer",
                                "params" : []
                            }))
                            .unwrap(),
                        })
                        .await
                        .unwrap();

                    info!("got resume packet: {packet:?}");
                });
            }

            ui.separator();

            if ui.button("Export Canvas").clicked() {
                let im = self.render_image();
                let buf = encode_image(&im);

                spawn(async move {
                    let Some(handle) = rfd::AsyncFileDialog::new()
                        .set_file_name("canvas.jpg")
                        .save_file()
                        .await
                    else {
                        return;
                    };

                    if let Err(err) = handle.write(&buf).await {
                        error!("could not write canvas image: {err}");
                    }
                });
            }
        });
    }

    fn device_status(&mut self, ui: &mut egui::Ui) {
        match self.transport_status {
            TransportStatus::Connected => {
                if ui.button("Disconnect").clicked()
                    && let Some(manager) = self.transport_manager.take()
                {
                    let tx = self.tx.clone();
                    spawn(async move {
                        if let Err(err) = manager.disconnect().await {
                            tx.send(Action::Error(err)).unwrap();
                        }
                    });
                }

                if let Some(status) = &self.device_status {
                    ui.horizontal(|ui| {
                        ui.label("State: ");
                        ui.label(serde_plain::to_string(&status.0).unwrap());
                    });

                    ui.horizontal(|ui| {
                        ui.label("Sub State: ");
                        ui.label(serde_plain::to_string(&status.1).unwrap());
                    });

                    ui.horizontal(|ui| {
                        ui.label("Alerts: ");
                        ui.label(&status.2);
                    });
                }

                ui.separator();

                ui.heading("Current Job");

                if self.send_progress.is_none() && self.job_status.is_none() {
                    if ui.button("Print Canvas").clicked() {
                        self.print_canvas();
                    }
                } else {
                    if let Some(send_progress) = self.send_progress {
                        ui.horizontal(|ui| {
                            ui.label("Data transfer: ");
                            ui.add(
                                egui::ProgressBar::new(send_progress)
                                    .show_percentage()
                                    .animate(true),
                            );
                        });
                    }

                    if let Some(status) = &self.job_status {
                        ui.horizontal(|ui| {
                            ui.label("State: ");
                            ui.label(serde_plain::to_string(&status.job_state).unwrap_or_default());
                        });

                        ui.horizontal(|ui| {
                            ui.label("Sub State: ");
                            ui.label(
                                serde_plain::to_string(&status.job_sub_state).unwrap_or_default(),
                            );
                        });
                    }
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
                    let tx = self.tx.clone();

                    let manager = TransportManager::new(self.get_transport(), move |event| {
                        if let Err(err) = tx.send(Action::TransportEvent(event)) {
                            error!("could not send transport event: {err}");
                        }
                    });

                    self.transport_manager = Some(manager);
                }
            }
        }
    }
}

impl eframe::App for SapodillaApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.apply_actions();

        egui::TopBottomPanel::top("top_panel").show(ctx, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                self.menu(ui, ctx);
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
            .width_range(150.0..=400.0)
            .show(ctx, |ui| {
                ui.heading("Connection");

                self.device_status(ui);

                ui.separator();

                ui.heading("Settings");

                let previous = self.selected_device;
                egui::ComboBox::from_label("Device")
                    .selected_text(&DEVICES[self.selected_device].name)
                    .show_index(ui, &mut self.selected_device, DEVICES.len(), |i| {
                        &DEVICES[i].name
                    });
                if self.selected_device != previous {
                    self.selected_mode = 0;
                    self.selected_canvas_size = 0;
                }

                let previous = self.selected_mode;
                egui::ComboBox::from_label("Mode")
                    .selected_text(
                        DEVICES[self.selected_device].modes[self.selected_mode]
                            .mode_type
                            .name(),
                    )
                    .show_index(
                        ui,
                        &mut self.selected_mode,
                        DEVICES[self.selected_device].modes.len(),
                        |i| DEVICES[self.selected_device].modes[i].mode_type.name(),
                    );
                if self.selected_mode != previous {
                    self.selected_canvas_size = 0;
                }

                egui::ComboBox::from_label("Canvas Size")
                    .selected_text(
                        &DEVICES[self.selected_device].modes[self.selected_mode].canvas_sizes
                            [self.selected_canvas_size]
                            .name,
                    )
                    .show_index(
                        ui,
                        &mut self.selected_canvas_size,
                        DEVICES[self.selected_device].modes[self.selected_mode]
                            .canvas_sizes
                            .len(),
                        |i| {
                            &DEVICES[self.selected_device].modes[self.selected_mode].canvas_sizes[i]
                                .name
                        },
                    );

                ui.horizontal(|ui| {
                    ui.add(egui::DragValue::new(&mut self.copies).range(1..=10));
                    ui.label("Copies");
                });

                if DEVICES[self.selected_device].modes[self.selected_mode]
                    .mode_type
                    .has_cutting()
                {
                    ui.separator();

                    views::cut_controls(
                        ui,
                        DEVICES[self.selected_device].dpi,
                        &mut self.cut_tuning,
                        self.cut_progress,
                        self.has_intersections,
                        self.off_canvas,
                    );

                    if ui
                        .add_enabled(
                            self.cut_progress.is_none(),
                            egui::Button::new("Generate Cut Lines"),
                        )
                        .clicked()
                    {
                        self.cut_shapes.clear();
                        self.has_intersections = false;
                        self.off_canvas = false;
                        self.cut_progress = None;

                        let tx = self.tx.clone();
                        let mut rx = CutGenerator::start(
                            self.loaded_images.clone(),
                            self.cut_tuning.clone(),
                            self.get_canvas(),
                        );

                        spawn(async move {
                            while let Some(action) = rx.next().await {
                                debug!(?action, "got cut action");

                                if let Err(err) = tx.send(Action::Cut(action)) {
                                    error!("could not send cut action: {err}");
                                }
                            }
                        });
                    }
                }

                if !self.loaded_images.is_empty() {
                    ui.separator();
                    views::loaded_images(
                        ui,
                        DEVICES[self.selected_device].dpi,
                        self.get_canvas().size,
                        &mut self.loaded_images,
                    );
                }
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            views::canvas_editor(ui, self);

            ctx.input(|i| {
                if i.raw.dropped_files.is_empty() {
                    return;
                }

                let mut files: Vec<Vec<u8>> = Vec::with_capacity(i.raw.dropped_files.len());

                for file in i.raw.dropped_files.iter() {
                    debug!("processing file");
                    let data = if cfg!(target_arch = "wasm32") {
                        match &file.bytes {
                            Some(bytes) => bytes.to_vec(),
                            None => continue,
                        }
                    } else if let Some(path) = &file.path {
                        let mut file = std::fs::File::open(path).unwrap();
                        let mut buf = Vec::new();
                        std::io::Read::read_to_end(&mut file, &mut buf).unwrap();
                        buf
                    } else {
                        continue;
                    };

                    debug!("got file contents");
                    files.push(data);
                }

                let ctx = ctx.clone();
                let tx = self.tx.clone();
                spawn(async move {
                    for file in files {
                        tx.send(Action::LoadedImage(LoadedImage::new(&ctx, &file, None)))
                            .unwrap();
                        ctx.request_repaint();
                    }
                })
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

fn encode_image(im: &image::DynamicImage) -> Vec<u8> {
    let mut buf = Vec::with_capacity(1024 * 1024);
    let mut quality = 100;
    loop {
        // Image needs to be under 1MB, so decrease quality
        // until we get there.
        let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, quality);
        encoder.encode_image(im).unwrap();
        debug!(quality, len = buf.len(), "got jpeg size");

        if buf.len() <= 1024 * 1024 || quality == 0 {
            break;
        }

        quality -= 1;
        buf.clear();
    }

    buf
}

fn encode_plt(
    cut_shapes: &[geo::MultiPolygon<f32>],
    cutter_calibration: CutterCalibration,
    canvas_size: &CanvasSize,
) -> Vec<u8> {
    let mut buf = b"IN VER0.1.0 KP42".to_vec();

    let flipped = CutGenerator::mirror_cuts(cut_shapes.iter(), canvas_size.size);

    let mut polygons: Vec<_> = flipped
        .flat_map(|multi_polygon| multi_polygon.0.into_iter())
        .collect();
    polygons.sort_by(|a, b| {
        let a_start = *a.exterior().0.first().unwrap();
        let b_start = *b.exterior().0.first().unwrap();

        a_start
            .y
            .total_cmp(&b_start.y)
            .then(a_start.x.total_cmp(&b_start.x))
    });

    for polygon in polygons {
        write_line_string(&cutter_calibration, &mut buf, polygon.exterior());

        for interior in polygon.interiors() {
            write_line_string(&cutter_calibration, &mut buf, interior);
        }
    }

    write!(buf, " U6476,0 @ ").unwrap();

    buf
}

fn write_line_string(
    cutter_calibration: &CutterCalibration,
    buf: &mut Vec<u8>,
    line_shape: &geo::LineString<f32>,
) {
    write!(
        buf,
        " U{:.0},{:.0}",
        (line_shape.0[0].y + cutter_calibration.offset.y) * cutter_calibration.scale_factor,
        (line_shape.0[0].x + cutter_calibration.offset.x) * cutter_calibration.scale_factor
    )
    .unwrap();

    for point in line_shape.coords() {
        write!(
            buf,
            " D{:.0},{:.0}",
            (point.y + cutter_calibration.offset.y) * cutter_calibration.scale_factor,
            (point.x + cutter_calibration.offset.x) * cutter_calibration.scale_factor
        )
        .unwrap();
    }
}

#[cfg(target_arch = "wasm32")]
fn current_timestamp_millis() -> u64 {
    web_sys::window().unwrap().performance().unwrap().now() as u64
}

#[cfg(not(target_arch = "wasm32"))]
fn current_timestamp_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}
