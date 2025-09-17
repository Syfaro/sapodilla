use std::{collections::HashMap, sync::mpsc};

use egui::Vec2;
use geo::{
    Buffer, ChaikinSmoothing, Contains, Coord, Euclidean, Intersects, LineString, MultiPolygon,
    Polygon, Rect, Scale, Simplify, Validation, Winding, coord, line_measures::LengthMeasurable,
};
use image::imageops::{self, FilterType};
use imageproc::contours::BorderType;
use itertools::Itertools;
use tracing::{debug, error, instrument, trace, warn};

use crate::{app::LoadedImage, spawn};

#[derive(Debug)]
pub enum CutAction {
    Progress { completed: usize, total: usize },
    Done(CutResult),
}

#[derive(Debug)]
pub struct CutResult {
    pub has_intersections: bool,
    pub off_canvas: bool,
    pub polygons: Vec<MultiPolygon<f32>>,
}

#[derive(Clone)]
pub struct CutTuning {
    pub buffer: f32,
    pub minimum_length: f32,
    pub smoothing: usize,
    pub simplify: f32,
    pub internal: bool,
}

impl Default for CutTuning {
    fn default() -> Self {
        Self {
            buffer: 300.0 / 25.4,         // 1mm
            minimum_length: 0.25 * 300.0, // 1/4in
            smoothing: 2,
            simplify: 1.5,
            internal: false,
        }
    }
}

pub struct CutGenerator {
    tx: mpsc::Sender<CutAction>,
    images: Vec<LoadedImage>,
    tuning: CutTuning,
    canvas_size: Vec2,
}

impl CutGenerator {
    pub fn start(
        images: Vec<LoadedImage>,
        tuning: CutTuning,
        canvas_size: Vec2,
    ) -> mpsc::Receiver<CutAction> {
        let (tx, rx) = mpsc::channel();

        let cut_generator = Self {
            tx,
            images,
            tuning,
            canvas_size,
        };

        spawn(async move {
            if let Err(err) = cut_generator.process() {
                error!("could not process cuts: {err}");
            }
        });

        rx
    }

    fn process(self) -> anyhow::Result<()> {
        let total = self.images.len();

        self.tx.send(CutAction::Progress {
            completed: 0,
            total,
        })?;

        let mut polygons = Vec::new();

        for (index, image) in self.images.iter().enumerate() {
            let polygon = self.image(image);

            if let Some(polygon) = polygon {
                polygons.push(polygon);
            }

            self.tx.send(CutAction::Progress {
                completed: index + 1,
                total,
            })?;
        }

        let has_intersections = polygons
            .iter()
            .combinations(2)
            .any(|polygons| polygons[0].intersects(polygons[1]));

        let canvas_polygon = Rect::new(
            coord! { x: 0., y: 0.},
            coord! { x: self.canvas_size.x, y: self.canvas_size.y },
        )
        .to_polygon();

        let off_canvas = polygons
            .iter()
            .any(|polygons| !canvas_polygon.contains(polygons));

        self.tx.send(CutAction::Done(CutResult {
            has_intersections,
            off_canvas,
            polygons,
        }))?;

        Ok(())
    }

    #[instrument(skip_all)]
    fn image(&self, image: &LoadedImage) -> Option<MultiPolygon<f32>> {
        trace!("starting processing image");

        // Resize image to the expected dimensions. Doesn't need to be a high
        // quality resize, so nearest filter is fine.
        let size = image.size();
        let resized = imageops::resize(
            &image.image,
            size.x as u32,
            size.y as u32,
            FilterType::Nearest,
        );

        // Invert the colors, unlike a normal image we need blacks to be visible
        // but don't care about white. Normally transparent pixels turn black
        // but we need them to be white for our inversion.
        let mut im = image::ImageBuffer::from_pixel(
            resized.width(),
            resized.height(),
            image::Rgba([255, 255, 255, 255]),
        );
        image::imageops::overlay(&mut im, &resized, 0, 0);
        imageops::colorops::invert(&mut im);

        // `find_contours` only works on grayscale images, so convert it.
        let grayscale = imageops::grayscale(&im);

        let contours = imageproc::contours::find_contours::<u32>(&grayscale);

        // Keep track of the outer parts of contours separately from holes, so
        // we can construct a MultiPolygon with an exterior and interiors.
        let mut outers = HashMap::new();
        let mut holes: HashMap<usize, Vec<LineString<f32>>> = HashMap::new();

        for (index, contour) in contours.into_iter().enumerate() {
            // Create the line from the points in the contour, offest by the
            // position of the image in the canvas. We need to have these
            // offsets here to check if anything overlaps.
            let mut line_string = LineString::from_iter(contour.points.into_iter().map(|point| {
                (
                    point.x as f32 + image.offset.x,
                    point.y as f32 + image.offset.y,
                )
            }));

            line_string.close();

            if !line_string.is_valid() {
                warn!("line string was not valid");
                continue;
            }

            // Based on the border type, determine where to put this polygon.
            // It's also possible for a hole to not have a parent, and in those
            // cases we can promote it to a outer type.
            match contour.border_type {
                BorderType::Outer => {
                    line_string.make_cw_winding();
                    outers.insert(index, line_string);
                }
                BorderType::Hole => {
                    if let Some(parent) = contour.parent {
                        line_string.make_ccw_winding();
                        holes.entry(parent).or_default().push(line_string);
                    } else {
                        warn!(index, "hole did not have parent, using as outer");
                        line_string.make_cw_winding();
                        outers.insert(index, line_string);
                    };
                }
            }
        }

        if outers.is_empty() {
            warn!("image had no sections");
            return None;
        }

        // Now we can create polygons from our line strings, filtering out the
        // ones that are too small.
        let mut polygons = Vec::with_capacity(outers.len());
        for (index, outer) in outers {
            let outer_length = outer.length(&Euclidean);
            if outer_length < self.tuning.minimum_length {
                debug!(
                    outer_length,
                    minimum_length = self.tuning.minimum_length,
                    "exterior length was too short"
                );
                continue;
            } else {
                debug!(outer_length);
            }

            let holes = if self.tuning.internal {
                holes
                    .remove(&index)
                    .map(|line_strings| self.filter_small_holes(line_strings).collect())
                    .unwrap_or_default()
            } else {
                Vec::new()
            };

            let polygon = Polygon::new(outer, holes);
            polygons.push(polygon);
        }

        // And now that we've filtered everything, we can refine the polygons
        // based on our tuning settings to smooth, simplify, and buffer it to
        // make it a reasonable cut path.
        let mut refined_polygons = Vec::with_capacity(polygons.len());
        for polygon in polygons.iter() {
            if !self.tuning.internal
                && polygons
                    .iter()
                    .any(|other| other != polygon && other.contains(polygon))
            {
                warn!("polygon was contained by other when we didn't want internal holes");
                continue;
            }

            let simplified_polygon = polygon
                .chaikin_smoothing(self.tuning.smoothing)
                .simplify(self.tuning.simplify);

            let buffered_polygon = simplified_polygon.buffer(self.tuning.buffer);

            // We only want to grow our shapes, so we don't have to worry about
            // the exterior becoming too small. We do however have to worry
            // about it for the interiors.
            refined_polygons.extend(buffered_polygon.0.into_iter().map(|polygon| {
                let (exterior, interiors) = polygon.into_inner();
                let interiors = self.filter_small_holes(interiors).collect();
                Polygon::new(exterior, interiors)
            }));
        }

        trace!("finished processing image");

        Some(MultiPolygon::new(refined_polygons))
    }

    fn filter_small_holes(
        &self,
        line_strings: impl IntoIterator<Item = LineString<f32>>,
    ) -> impl Iterator<Item = LineString<f32>> {
        line_strings.into_iter().filter(|line_string| {
            let length = line_string.length(&Euclidean);
            if length < self.tuning.minimum_length {
                debug!(
                    length,
                    minimum_length = self.tuning.minimum_length,
                    "interior length was too short"
                );
                false
            } else {
                debug!(interior_length = length);
                true
            }
        })
    }

    /// Mirror generated cut lines for sending to the device.
    #[allow(dead_code)]
    pub fn mirror_cuts(
        polygons: impl IntoIterator<Item = MultiPolygon<f32>>,
        canvas_size: Vec2,
    ) -> impl Iterator<Item = MultiPolygon<f32>> {
        let point = Coord::from((canvas_size.x, canvas_size.y / 2.0));

        polygons
            .into_iter()
            .map(move |polygon| polygon.scale_around_point(1.0, -1.0, point))
    }
}
