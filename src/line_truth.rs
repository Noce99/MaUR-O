//! What a network is supposed to say about the lines of a rendered map, pixel
//! by pixel — the second layer of the answer, over [`crate::ground_truth`].
//!
//! The ground truth of [`crate::ground_truth`] is one class per pixel: which
//! opaque area is *under* it. A line is not under anything, it is drawn over
//! the ground, and the ground is still there between the dashes of a fence.
//! So lines get a layer of their own rather than a class of the first one:
//!
//! * the ground keeps its label wherever a line runs over it, which is what
//!   the network has to learn to see through the line;
//! * a pixel can be on several lines at once — two paths crossing, a fence
//!   along a stream — so the layer is not a class per pixel but a yes-or-no
//!   per pixel *per line symbol*: a multi-label answer, scored with a sigmoid
//!   per channel and not a softmax across them.
//!
//! # Where the labels come from
//!
//! The geometry of the line, not the pixels it was drawn with. A dashed line
//! is labelled as one unbroken stroke, because the object *is* one unbroken
//! line and the gaps are a way of drawing it; a network trained on the gaps
//! would hand back a line in pieces. The stroke is as wide as the symbol's
//! own, but never thinner than [`MIN_WIDTH_PIXELS`]: a one-pixel line is a
//! target a network almost never hits, and the width is thrown away anyway
//! when the answer is thinned back to a centre line.
//!
//! Nothing is written to disk. The map file holds every line object and its
//! symbol, so [`LineTruth::from_map`] rebuilds the layer from it, the way
//! [`crate::ground_truth::GroundTruth::from_map`] can rebuild the first.
//! Only the channels of the symbols that actually are on the map are held
//! in memory; the rest are all zeros and [`LineTruth::channel`] says so.

use std::path::Path;

use tiny_skia::{FillRule, LineCap, LineJoin, Mask, Stroke, Transform};

use crate::geometry::to_painter_path;
use crate::ground_truth::page_transform_for;
use crate::map::{CoordList, Map, ObjectKind, Symbol};
use crate::renderer::to_skia_path;
use crate::symbol_kinds::Catalogue;
use crate::xml_reader::read_xml_map;

/// The least a line is labelled wide, in pixels.
pub const MIN_WIDTH_PIXELS: f32 = 3.0;

/// One line as it was drawn, which is one stroke of the answer.
pub struct LineStroke {
    /// Which of the symbol set's lines it was drawn with: a place in
    /// [`crate::symbol_kinds::Catalogue::lines`], and the channel it owns.
    pub class: usize,
    /// The width of the symbol's own stroke, in mm on the paper.
    pub width: f64,
    /// The path, in the coordinates a map file holds — mm on the paper.
    pub path: CoordList,
}

/// The lines of one image, a yes-or-no per pixel for each line symbol present.
pub struct LineTruth {
    /// The height of the image these labels are for, in pixels.
    pub height: u32,
    /// Its width, in pixels.
    pub width: u32,
    /// How many channels there are: the line symbols of the set.
    pub classes: usize,
    /// The channels which have any pixel on, as `(class, mask)` with a mask of
    /// `height * width` bytes, one where a line of that class runs and zero
    /// where it does not. Sorted by class.
    pub layers: Vec<(usize, Vec<u8>)>,
}

impl LineTruth {
    /// The labels for an image of `width` by `height` pixels showing
    /// `strokes`, drawn through `page_transform`.
    ///
    /// Strokes of the same class are merged into one channel, and strokes of
    /// different classes do not interact: a pixel two lines cross is on in
    /// both channels.
    pub fn rasterize(
        strokes: &[LineStroke],
        classes: usize,
        width: u32,
        height: u32,
        page_transform: Transform,
    ) -> Result<LineTruth, String> {
        let pixels = width as usize * height as usize;
        let mut mask = Mask::new(width, height)
            .ok_or_else(|| format!("cannot label an image of {width}x{height} pixels"))?;
        // Pixels per mm: the transform is a plain scale and a shift.
        let scale = page_transform.sx;
        let mut layers: Vec<(usize, Vec<u8>)> = Vec::new();

        for stroke in strokes {
            if stroke.class >= classes {
                return Err(format!(
                    "the line is drawn with symbol {} of {classes}",
                    stroke.class
                ));
            }
            let Some(path) = to_skia_path(&to_painter_path(&stroke.path, false)) else {
                continue;
            };
            let outline = tiny_skia::Stroke {
                width: (stroke.width as f32).max(MIN_WIDTH_PIXELS / scale),
                line_cap: LineCap::Round,
                line_join: LineJoin::Round,
                ..Stroke::default()
            };
            let Some(filled) = path.stroke(&outline, scale) else {
                continue;
            };
            mask.clear();
            mask.fill_path(&filled, FillRule::Winding, false, page_transform);

            let at = match layers.binary_search_by_key(&stroke.class, |(class, _)| *class) {
                Ok(at) => at,
                Err(at) => {
                    layers.insert(at, (stroke.class, vec![0; pixels]));
                    at
                }
            };
            for (out, &covered) in layers[at].1.iter_mut().zip(mask.data()) {
                if covered != 0 {
                    *out = 1;
                }
            }
        }
        Ok(LineTruth {
            height,
            width,
            classes,
            layers,
        })
    }

    /// Rebuilds the labels of the lines on a generated map, out of the map
    /// file itself — see [`crate::ground_truth::GroundTruth::from_map`], whose
    /// `width`, `height` and `resolution` these are too.
    ///
    /// A map with no line on it gives labels with no channel on, which is an
    /// answer and not an error. An object which is not a path drawn with a
    /// line symbol is skipped.
    pub fn from_map(
        path: &Path,
        width: u32,
        height: u32,
        resolution: f64,
    ) -> Result<LineTruth, String> {
        let (mut map, _) = read_xml_map(path)?;
        map.resolve_references();
        LineTruth::of_map(&map, width, height, resolution)
    }

    /// [`LineTruth::from_map`] of a map already read, with its references
    /// resolved — for a caller which wants more than the lines out of it.
    pub fn of_map(
        map: &Map,
        width: u32,
        height: u32,
        resolution: f64,
    ) -> Result<LineTruth, String> {
        let catalogue = Catalogue::of(map);

        let strokes: Vec<LineStroke> = map
            .objects
            .iter()
            .filter(|object| matches!(object.kind, ObjectKind::Path(_)))
            .filter_map(|object| {
                let index = object.symbol_index?;
                let class = catalogue.lines.iter().position(|e| e.index == index)?;
                let Some(Symbol::Line(line)) = map.symbols.get(index) else {
                    return None;
                };
                Some(LineStroke {
                    class,
                    // A symbol with no colour draws no stroke at all: its
                    // width is the room its mid symbols take up, which is
                    // not where the line is.
                    width: if line.color < 0 { 0.0 } else { line.line_width },
                    path: object.coords.clone(),
                })
            })
            .collect();

        let page_transform = page_transform_for(map.scale_denominator, resolution, width, height);
        LineTruth::rasterize(
            &strokes,
            catalogue.lines.len(),
            width,
            height,
            page_transform,
        )
    }

    /// The mask of one channel, or `None` where nothing of that class is on
    /// the image and every pixel is zero.
    pub fn channel(&self, class: usize) -> Option<&[u8]> {
        self.layers
            .binary_search_by_key(&class, |(c, _)| *c)
            .ok()
            .map(|at| self.layers[at].1.as_slice())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map::Coord;

    /// A horizontal line from `(0, y)` to `(8, y)`, in mm.
    fn horizontal(y: f64) -> CoordList {
        vec![Coord::new(0.0, y, 0), Coord::new(8.0, y, 0)]
    }

    fn one_to_one() -> Transform {
        Transform::identity()
    }

    #[test]
    fn a_line_is_on_in_its_own_channel_and_in_no_other() {
        let strokes = [LineStroke {
            class: 1,
            width: 1.0,
            path: horizontal(4.0),
        }];
        let truth = LineTruth::rasterize(&strokes, 3, 8, 8, one_to_one()).unwrap();
        assert!(truth.channel(0).is_none());
        assert!(truth.channel(2).is_none());
        let on = truth.channel(1).expect("channel 1 is on");
        // The middle row is on, the top row is not.
        assert_eq!(on[4 * 8 + 4], 1);
        assert_eq!(on[0], 0);
    }

    #[test]
    fn a_thin_line_is_labelled_at_least_the_minimum_width() {
        let strokes = [LineStroke {
            class: 0,
            width: 0.01,
            path: horizontal(4.0),
        }];
        let truth = LineTruth::rasterize(&strokes, 1, 8, 8, one_to_one()).unwrap();
        let on = truth.channel(0).unwrap();
        let rows = (0..8).filter(|&row| on[row * 8 + 4] == 1).count();
        assert!(rows >= 3, "{rows} rows on");
    }

    #[test]
    fn two_lines_crossing_are_both_on_where_they_cross() {
        let vertical = vec![Coord::new(4.0, 0.0, 0), Coord::new(4.0, 8.0, 0)];
        let strokes = [
            LineStroke {
                class: 0,
                width: 1.0,
                path: horizontal(4.0),
            },
            LineStroke {
                class: 1,
                width: 1.0,
                path: vertical,
            },
        ];
        let truth = LineTruth::rasterize(&strokes, 2, 8, 8, one_to_one()).unwrap();
        let crossing = 4 * 8 + 4;
        assert_eq!(truth.channel(0).unwrap()[crossing], 1);
        assert_eq!(truth.channel(1).unwrap()[crossing], 1);
    }

    #[test]
    fn a_class_past_the_end_is_refused() {
        let strokes = [LineStroke {
            class: 5,
            width: 1.0,
            path: horizontal(4.0),
        }];
        assert!(LineTruth::rasterize(&strokes, 2, 8, 8, one_to_one()).is_err());
    }
}
