use egui::{
    Color32, Key, KeyboardShortcut, Modifiers, Painter, Pos2, Rect, Response, Sense, Shape, Stroke,
    Ui, Vec2,
    emath::{self, RectTransform},
};
use geo::MultiPolygon;
use tracing::instrument;

use crate::{SapodillaApp, app::LoadedImage};

const CUT_LINE_WIDTH: f32 = 3.0;

const DELETE_SHORTCUT: KeyboardShortcut = KeyboardShortcut::new(Modifiers::NONE, Key::Delete);
const BACKSPACE_SHORTCUT: KeyboardShortcut = KeyboardShortcut::new(Modifiers::NONE, Key::Backspace);

const NORMAL_UV: Rect = Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1.0, 1.0));

static FUN_COLORS: [Color32; 7] = [
    Color32::from_rgb(249, 65, 68),
    Color32::from_rgb(243, 114, 44),
    Color32::from_rgb(248, 150, 30),
    Color32::from_rgb(249, 199, 79),
    Color32::from_rgb(144, 190, 109),
    Color32::from_rgb(67, 170, 139),
    Color32::from_rgb(87, 117, 144),
];

pub fn canvas_editor(ui: &mut Ui, state: &mut SapodillaApp) {
    let scene = egui::Scene::new().zoom_range(0.1..=3.0);

    let mut inner_rect = egui::Rect::NAN;
    let mut canvas_rect = state.canvas_rect;

    let response = scene
        .show(ui, &mut canvas_rect, |ui| {
            egui::Frame::canvas(ui.style())
                .fill(egui::Color32::WHITE)
                .inner_margin(0.0)
                .stroke(egui::Stroke::new(4.0, egui::Color32::BLACK))
                .show(ui, |ui| {
                    frame(ui, state);
                });
            inner_rect = ui.min_rect();
        })
        .response;

    state.canvas_rect = canvas_rect;

    if response.double_clicked() {
        state.canvas_rect = inner_rect.shrink(ui.style().spacing.menu_spacing);
    }
}

fn frame(ui: &mut egui::Ui, state: &mut SapodillaApp) {
    let size = Vec2::new(4.0 * 300.0, 6.0 * 300.0);

    ui.set_min_size(size);
    ui.set_max_size(size);

    let (response, painter) = ui.allocate_painter(size, egui::Sense::empty());

    let to_screen = emath::RectTransform::from_to(
        egui::Rect::from_min_size(Pos2::ZERO, response.rect.size()),
        response.rect,
    );

    let mut remove = None;

    for (idx, image) in state.loaded_images.iter_mut().enumerate() {
        paint_image(ui, &to_screen, &response, &painter, image, idx, &mut remove);
    }

    paint_polygons(&to_screen, &painter, &state.cut_shapes);

    if let Some(remove) = remove {
        state.loaded_images.remove(remove);
    }
}

#[instrument(skip_all)]
fn paint_image(
    ui: &mut Ui,
    to_screen: &RectTransform,
    response: &Response,
    painter: &Painter,
    image: &mut LoadedImage,
    idx: usize,
    remove: &mut Option<usize>,
) {
    let pos_in_screen = to_screen.transform_pos(image.offset);
    let image_rect = Rect::from_min_size(pos_in_screen, image.size());

    let rect_id = response.id.with(idx);
    let rect_response = ui.interact(image_rect, rect_id, Sense::drag());

    image.offset += rect_response.drag_delta();

    let pos_in_screen = to_screen.transform_pos(image.offset);

    let tint = if rect_response.hovered() {
        Color32::LIGHT_BLUE
    } else {
        Color32::WHITE
    };

    if rect_response.hovered()
        && ui.input_mut(|i| {
            i.consume_shortcut(&DELETE_SHORTCUT) || i.consume_shortcut(&BACKSPACE_SHORTCUT)
        })
    {
        *remove = Some(idx);
    } else {
        painter.image(
            image.sized_texture.id,
            Rect::from_min_size(pos_in_screen, image.size()),
            NORMAL_UV,
            tint,
        );
    }
}

#[instrument(skip_all)]
fn paint_polygons(to_screen: &RectTransform, painter: &Painter, cut_shapes: &[MultiPolygon<f32>]) {
    let mut count = 0;

    for multi_polygon in cut_shapes.iter() {
        for polygon in multi_polygon.iter() {
            // Make each cut line visually distinguishable.
            let stroke = Stroke::new(CUT_LINE_WIDTH, FUN_COLORS[count % FUN_COLORS.len()]);

            // Get the lines for the exterior and interior shapes.
            let lines = polygon.exterior().lines().chain(
                polygon
                    .interiors()
                    .iter()
                    .flat_map(|interior| interior.lines()),
            );

            // Create a line shape for each line from all our polygons.
            let shapes = lines.map(|line| {
                let start = to_screen.transform_pos(Pos2::new(line.start.x, line.start.y));
                let end = to_screen.transform_pos(Pos2::new(line.end.x, line.end.y));

                Shape::line(vec![start, end], stroke)
            });

            painter.extend(shapes);
            count += 1;
        }
    }
}
