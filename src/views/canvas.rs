use egui::{
    Color32, Frame, Key, KeyboardShortcut, Modifiers, Painter, Pos2, Rect, Scene, Sense, Shape,
    Stroke, Ui,
    emath::{self, RectTransform},
};
use geo::MultiPolygon;
use tracing::instrument;

use crate::{SapodillaApp, protocol::DEVICES};

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
    let scene = Scene::new().zoom_range(0.1..=3.0);

    let mut inner_rect = Rect::NAN;
    let mut canvas_rect = state.canvas_rect;

    let response = scene
        .show(ui, &mut canvas_rect, |ui| {
            Frame::canvas(ui.style())
                .fill(Color32::WHITE)
                .inner_margin(0.0)
                .stroke(Stroke::new(4.0, Color32::BLACK))
                .show(ui, |ui| {
                    frame(ui, state);
                });
            inner_rect = ui.min_rect();
        })
        .response;

    state.canvas_rect = canvas_rect;

    if response.double_clicked() || state.previous_canvas_size != state.get_canvas().size {
        state.canvas_rect = inner_rect.shrink(ui.style().spacing.menu_spacing);
        state.previous_canvas_size = state.get_canvas().size;
    }
}

fn frame(ui: &mut Ui, state: &mut SapodillaApp) {
    let size = state.get_canvas().size;

    ui.set_min_size(size);
    ui.set_max_size(size);

    let (response, mut painter) = ui.allocate_painter(size, Sense::empty());

    let to_screen = emath::RectTransform::from_to(
        Rect::from_min_size(Pos2::ZERO, response.rect.size()),
        response.rect,
    );

    let mut hovers = Vec::new();
    let mut remove = None;

    for (idx, image) in state.loaded_images.iter_mut().enumerate() {
        let pos_in_screen = to_screen.transform_pos(image.offset);
        let image_rect = Rect::from_min_size(pos_in_screen, image.size());

        let rect_id = response.id.with(idx);
        let rect_response = ui.interact(image_rect, rect_id, Sense::drag());

        image.offset += rect_response.drag_delta();

        let pos_in_screen = to_screen.transform_pos(image.offset);

        if rect_response.hovered() {
            hovers.push(image_rect);
        }

        if rect_response.hovered()
            && ui.input_mut(|i| {
                i.consume_shortcut(&DELETE_SHORTCUT) || i.consume_shortcut(&BACKSPACE_SHORTCUT)
            })
        {
            remove = Some(idx);
        } else {
            painter.image(
                image.sized_texture.id,
                Rect::from_min_size(pos_in_screen, image.size()),
                NORMAL_UV,
                Color32::WHITE,
            );
        }
    }

    paint_polygons(&to_screen, &painter, &state.cut_shapes);

    let safe_area = DEVICES[state.selected_device].modes[state.selected_mode].canvas_sizes
        [state.selected_canvas_size]
        .safe_area;

    if safe_area != size {
        let safe_lines = Rect::from_center_size((size / 2.0).to_pos2(), safe_area);

        painter.rect_stroke(
            to_screen.transform_rect(safe_lines),
            0,
            Stroke::new(5.0, Color32::from_rgba_unmultiplied(139, 0, 0, 128)),
            egui::StrokeKind::Outside,
        );
    }

    painter.set_clip_rect(ui.clip_rect());

    let stroke = Stroke::new(5.0, Color32::from_rgba_unmultiplied(173, 216, 230, 192));
    for rect in hovers {
        painter.rect_stroke(rect, 0, stroke, egui::StrokeKind::Outside);
    }

    if let Some(remove) = remove {
        state.loaded_images.remove(remove);
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
