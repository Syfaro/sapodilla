use std::{borrow::Cow, collections::VecDeque, sync::mpsc};

use egui::{Id, KeyboardShortcut, Modal, Modifiers, Pos2, Vec2};
use futures::lock::Mutex;
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
        let mut buf =
            image::ImageBuffer::from_pixel(4 * 300, 6 * 300, image::Rgba([255u8, 255, 255, 255]));

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

            image::imageops::overlay(&mut buf, &view, end_x as i64, end_y as i64);
        }

        buf.into()
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
}

impl eframe::App for SapodillaApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
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

                let image_shortcut =
                    KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::SHIFT, egui::Key::U);
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
                ui.heading("Device");

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
                                let im = self.render_image();
                                let buf = encode_image(&im);

                                let manager = self.transport_manager.clone().unwrap();
                                let tx = self.tx.clone();
                                self.send_progress = None;

                                let hash = sha1::Sha1::digest(&buf);
                                debug!("calculated image hash: {}", hex::encode(hash));

                                let time = current_timestamp_millis();

                                spawn(async move {
                                    let id = manager.next_message_id();
                                    let packet = AvocadoPacket {
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
                                            "id": id,
                                            "method": "print-job",
                                            "params": {
                                                "media-size": 5012,
                                                "media-type": 2010,
                                                "job-type": 0,
                                                "channel": 30784,
                                                "file-size": buf.len(),
                                                "document-format": 9,
                                                "document-name": format!("{}.jpeg", time),
                                                "hash-method": 1,
                                                "hash-value": hex::encode(hash),
                                                "user-account": "000000.00000000000000000000000000000000.0000",
                                                "link-type": 1000,
                                                "job-send-time": time / 1000,
                                                "copies": 1,
                                            },
                                        })).unwrap(),
                                    };
                                    debug!(?packet, "built print job packet");
                                    let packet = manager.wait_for_response(packet).await.unwrap();
                                    debug!(?packet, "got response packet");

                                    #[derive(Debug, Deserialize)]
                                    #[serde(rename_all = "kebab-case")]
                                    struct JobResult {
                                        job_id: u32,
                                    }

                                    let job_id = packet.as_json::<AvocadoResult<JobResult>>().unwrap().result.job_id;
                                    debug!(job_id, "got job id");

                                    manager
                                        .send_data(job_id, &buf, |total, sent| {
                                            debug!(total, sent, "sent data packet");
                                            let _ = tx.send(Action::SendProgress(
                                                sent as f32 / total as f32,
                                            ));
                                        })
                                        .await
                                        .unwrap();

                                    manager.poll_job(job_id).await.unwrap();
                                    info!("finished sending data");
                                });
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

                            let manager = TransportManager::new(
                                self.get_transport(),
                                move |event| {
                                    if let Err(err) = tx.send(Action::TransportEvent(event)) {
                                        error!("could not send transport event: {err}");
                                    }
                                },
                            );

                            self.transport_manager = Some(manager);
                        }
                    }
                }

                ui.separator();

                views::cut_controls(ui, &mut self.cut_tuning, self.cut_progress, self.has_intersections, self.off_canvas);

                if ui.add_enabled(
                    self.cut_progress.is_none(),
                    egui::Button::new("Generate Cut Lines")
                ).clicked() {
                    self.cut_shapes.clear();
                    self.has_intersections = false;
                    self.off_canvas = false;
                    self.cut_progress = None;

                    let tx = self.tx.clone();
                    let rx = CutGenerator::start(self.loaded_images.clone(), self.cut_tuning.clone(), Vec2::new(4.0 * 300.0, 6.0 * 300.0));

                    spawn(async move {
                        while let Ok(action) = rx.recv() {
                            debug!(?action, "got cut action");

                            if let Err(err) = tx.send(Action::Cut(action)) {
                                error!("could not send cut action: {err}");
                            }
                        }
                    });
                }

                if !self.loaded_images.is_empty() {
                    ui.separator();
                    views::loaded_images(ui, &mut self.loaded_images);
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
