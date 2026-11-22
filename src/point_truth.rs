//! What a network is supposed to say about the point symbols of a rendered
//! map — the third layer of the answer, over [`crate::ground_truth`] and
//! beside [`crate::line_truth`].
//!
//! A point symbol is not a region and not a band: it is one coordinate, and
//! a symbol drawn around it. What a network has to give back is the
//! coordinate, and which symbol — so the labels are **keypoints**, the way
//! object detectors which find centres rather than boxes have them:
//!
//! * a heatmap per point symbol, nought nearly everywhere, rising to a peak
//!   of exactly one at the pixel the object's coordinate falls in, a
//!   Gaussian of [`SIGMA_PIXELS`] around it;
//! * the ground keeps its label under a point symbol, and a line under one
//!   keeps its own, as they do under a line: the network has to learn to see
//!   through it;
//! * the channels are independent of each other, a sigmoid each, since two
//!   symbols of different kinds can sit next to each other.
//!
//! The peak is the coordinate the file holds, not the middle of what the
//! symbol draws. The two are not always the same place — a tower stands on
//! its coordinate, a spring opens away from it — and the coordinate is what
//! a map object needs. The network learns where it sits for each symbol, as
//! it learns what the symbol looks like.
//!
//! The Gaussian is what makes the target learnable at all: one pixel in a
//! picture of a hundred thousand is a needle, and a network penalised as
//! hard for a pixel beside it as for one a picture away has nothing to climb.
//! The ring around the peak is penalised less the nearer it is to it — see
//! `crate::net::training::points_loss` — and the peak alone counts as a
//! hit.
//!
//! # Which way a point is turned
//!
//! A slope line hangs off its contour downhill, a cave opens one way, a
//! crossing point lies across its wall: many point symbols are drawn at an
//! angle of their own, and a map read back with every one of them upright
//! is a different map. So there is one more thing to say about a point, two
//! channels shared by every class: the sine and the cosine of its angle, as
//! for the pattern of an area, given around each point whose symbol turns
//! and the zero vector everywhere else — see [`PointTruth::angles`]. The
//! angle is read back where a peak is found.
//!
//! # Where the labels come from
//!
//! The map file, as for the lines: it holds every point object, its symbol
//! and its coordinate, so [`PointTruth::from_map`] rebuilds the layer from
//! it and nothing is written to disk. What is kept is the list of points, in
//! pixels; [`PointTruth::heatmap`] draws the Gaussians for whatever crop of
//! the picture is asked for, which is a few hundred pixels of arithmetic
//! rather than a channel per symbol of the whole picture.

use std::path::Path;

use tiny_skia::Transform;

use crate::ground_truth::page_transform_for;
use crate::label_set::LabelSet;
use crate::map::Map;
use crate::xml_reader::read_xml_map;

/// How wide the Gaussian around a point is, in pixels: its standard
/// deviation.
///
/// Two pixels, whatever the symbol: a peak is where the object's coordinate
/// is, and the symbol's size says nothing about how exactly that can be
/// known. Narrower and the peak is a needle the network seldom finds; much
/// wider and two points a few pixels apart merge into one.
pub const SIGMA_PIXELS: f64 = 2.0;

/// How far from its peak a point's Gaussian is drawn, in standard
/// deviations. Past three it is under a hundredth, which the loss treats as
/// nought.
const REACH: f64 = 3.0;

/// One point object as it lands on a picture.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PointMark {
    /// Which point class it is: a place in
    /// [`crate::label_set::LabelSet::points`] — for a set with no table of
    /// labels, the point symbol it was drawn with — and the channel it owns.
    pub class: usize,
    /// Where its coordinate lands, in pixels of the picture rightwards from
    /// its left edge: pixel `i` covers `i` to `i + 1`.
    pub x: f64,
    /// The same, downwards from the top edge.
    pub y: f64,
    /// The angle the object was turned to, in radians, as the map file
    /// carries it — `None` for a symbol which does not turn.
    pub turn: Option<f64>,
}

/// The point symbols of one picture.
#[derive(Clone, Debug)]
pub struct PointTruth {
    /// The height of the picture these labels are for, in pixels.
    pub height: u32,
    /// Its width, in pixels.
    pub width: u32,
    /// How many channels there are: the point classes of the set.
    pub classes: usize,
    /// How many turns of each class look alike, in the order of the classes:
    /// what an angle is folded by before it is a target — see
    /// [`PointTruth::angles`].
    pub symmetry: Vec<u32>,
    /// Every point object of the map, wherever it lands — on the picture or
    /// off it, since a point just past the edge still reaches into it.
    pub points: Vec<PointMark>,
}

impl PointTruth {
    /// Rebuilds the labels of the point symbols on a generated map, out of
    /// the map file itself — see [`crate::ground_truth::GroundTruth::from_map`],
    /// whose `width`, `height` and `resolution` these are too.
    pub fn from_map(
        path: &Path,
        labels: &LabelSet,
        width: u32,
        height: u32,
        resolution: f64,
    ) -> Result<PointTruth, String> {
        let (mut map, _) = read_xml_map(path)?;
        map.resolve_references();
        Ok(PointTruth::of_map(&map, labels, width, height, resolution))
    }

    /// [`PointTruth::from_map`] of a map already read, with its references
    /// resolved.
    ///
    /// A map with no point object on it gives labels with no point, which is
    /// an answer and not an error. An object `labels` gives no point class is
    /// skipped.
    pub fn of_map(
        map: &Map,
        labels: &LabelSet,
        width: u32,
        height: u32,
        resolution: f64,
    ) -> PointTruth {
        let page_transform = page_transform_for(map.scale_denominator, resolution, width, height);
        let points = map
            .objects
            .iter()
            .filter_map(|object| {
                let (class, turn) = labels.point_of(object)?;
                let at = object.coords.first()?;
                let (x, y) = on_picture(&page_transform, at.x, at.y);
                Some(PointMark { class, x, y, turn })
            })
            .collect();
        PointTruth {
            height,
            width,
            classes: labels.points.len(),
            symmetry: labels
                .points
                .iter()
                .map(|entry| entry.symmetry.max(1))
                .collect(),
            points,
        }
    }

    /// The heatmaps of a `size`-pixel square of the picture whose top left
    /// pixel is `(left, top)` — which may be off the picture — as
    /// `[classes, size, size]`, row by row.
    ///
    /// Each point is a Gaussian of [`SIGMA_PIXELS`] around where it lands,
    /// and exactly one at the pixel it lands in: that pixel is the peak, the
    /// one the loss counts as the point. Where two points of one symbol
    /// overlap, the higher of the two is kept rather than their sum, so a
    /// pixel is never more than one.
    pub fn heatmap(&self, left: isize, top: isize, size: usize) -> Vec<f32> {
        let pixels = size * size;
        let mut out = vec![0.0f32; self.classes * pixels];
        let reach = (REACH * SIGMA_PIXELS).ceil() as isize;
        let spread = 2.0 * SIGMA_PIXELS * SIGMA_PIXELS;
        for mark in &self.points {
            if mark.class >= self.classes {
                continue;
            }
            let channel = &mut out[mark.class * pixels..(mark.class + 1) * pixels];
            // In the crop's own pixels.
            let (x, y) = (mark.x - left as f64, mark.y - top as f64);
            let (column, row) = (x.floor() as isize, y.floor() as isize);
            for r in (row - reach).max(0)..=(row + reach).min(size as isize - 1) {
                for c in (column - reach).max(0)..=(column + reach).min(size as isize - 1) {
                    let value = if (c, r) == (column, row) {
                        1.0
                    } else {
                        let (dx, dy) = (c as f64 + 0.5 - x, r as f64 + 0.5 - y);
                        // Never quite one off the peak, so that the peak is
                        // the one pixel which is.
                        ((-(dx * dx + dy * dy) / spread).exp() as f32).min(PEAK_BELOW)
                    };
                    let at = r as usize * size + c as usize;
                    channel[at] = channel[at].max(value);
                }
            }
        }
        out
    }
}

impl PointTruth {
    /// Which way the points of a `size`-pixel square of the picture are
    /// turned — the square [`PointTruth::heatmap`] is of — as
    /// `[2, size, size]`: the sine of the angle, then its cosine, at every
    /// pixel within [`ANGLE_REACH_PIXELS`] of a point whose symbol turns, and
    /// the zero vector everywhere else, which is no angle at all.
    ///
    /// The angle is folded by how many turns of the class look alike, as
    /// [`crate::ground_truth::GroundTruth::sin_cos_folded`] folds a pattern's:
    /// an ellipse half a turn round is the same ellipse, and a target which
    /// told the two apart would be one the picture cannot answer. It comes
    /// back out as `atan2(sin, cos) / symmetry`.
    pub fn angles(&self, left: isize, top: isize, size: usize) -> Vec<f32> {
        let pixels = size * size;
        let mut out = vec![0.0f32; 2 * pixels];
        let reach = ANGLE_REACH_PIXELS.ceil() as isize;
        for mark in &self.points {
            let Some(turn) = mark.turn else {
                continue;
            };
            let order = self.symmetry.get(mark.class).copied().unwrap_or(1).max(1) as f64;
            let (sin, cos) = (turn * order).sin_cos();
            let (x, y) = (mark.x - left as f64, mark.y - top as f64);
            let (column, row) = (x.floor() as isize, y.floor() as isize);
            for r in (row - reach).max(0)..=(row + reach).min(size as isize - 1) {
                for c in (column - reach).max(0)..=(column + reach).min(size as isize - 1) {
                    let (dx, dy) = (c as f64 + 0.5 - x, r as f64 + 0.5 - y);
                    if dx * dx + dy * dy > ANGLE_REACH_PIXELS * ANGLE_REACH_PIXELS
                        && (c, r) != (column, row)
                    {
                        continue;
                    }
                    let at = r as usize * size + c as usize;
                    out[at] = sin as f32;
                    out[pixels + at] = cos as f32;
                }
            }
        }
        out
    }
}

/// How far from a point its angle is a target, in pixels: the peak and the
/// disc of [`SIGMA_PIXELS`] twice over round it. The angle is read where the
/// peak is found, which is a pixel or so from where it is labelled, and a
/// disc this wide has it there whichever pixel that is; two points are kept
/// further apart than this by the generator, so two angles never meet.
pub const ANGLE_REACH_PIXELS: f64 = 2.0 * SIGMA_PIXELS;

/// The highest a pixel other than a peak is given, so that "is one" picks
/// out the peaks and nothing else.
const PEAK_BELOW: f32 = 0.999;

/// Where a coordinate in mm on the paper lands on the picture, in pixels.
fn on_picture(transform: &Transform, x: f64, y: f64) -> (f64, f64) {
    let t = transform;
    (
        t.sx as f64 * x + t.kx as f64 * y + t.tx as f64,
        t.ky as f64 * x + t.sy as f64 * y + t.ty as f64,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one_point(class: usize, x: f64, y: f64) -> PointTruth {
        PointTruth {
            height: 32,
            width: 32,
            classes: 3,
            symmetry: vec![1; 3],
            points: vec![PointMark {
                class,
                x,
                y,
                turn: None,
            }],
        }
    }

    #[test]
    fn a_point_peaks_at_exactly_one_in_its_own_channel_and_nowhere_else() {
        let truth = one_point(1, 10.3, 5.7);
        let map = truth.heatmap(0, 0, 32);
        let channel = |c: usize| &map[c * 1024..(c + 1) * 1024];
        assert!(channel(0).iter().all(|&v| v == 0.0));
        assert!(channel(2).iter().all(|&v| v == 0.0));
        let ones: Vec<usize> = (0..1024).filter(|&at| channel(1)[at] == 1.0).collect();
        assert_eq!(ones, vec![5 * 32 + 10]);
        // Falling off around it, and gone a picture away.
        assert!(channel(1)[5 * 32 + 11] > 0.5 && channel(1)[5 * 32 + 11] < 1.0);
        assert_eq!(channel(1)[30 * 32 + 30], 0.0);
    }

    #[test]
    fn a_crop_sees_the_point_where_it_is_in_the_crop() {
        let truth = one_point(0, 20.5, 20.5);
        let map = truth.heatmap(16, 8, 16);
        assert_eq!(map[12 * 16 + 4], 1.0);
    }

    #[test]
    fn a_point_just_off_the_crop_still_reaches_into_it() {
        let truth = one_point(0, -1.0, 4.5);
        let map = truth.heatmap(0, 0, 16);
        assert!(map[4 * 16] > 0.0);
        assert!(!map.contains(&1.0), "the peak is off the crop");
    }

    #[test]
    fn two_points_of_one_symbol_keep_both_peaks() {
        let truth = PointTruth {
            height: 32,
            width: 32,
            classes: 1,
            symmetry: vec![1],
            points: vec![
                PointMark {
                    class: 0,
                    x: 4.5,
                    y: 4.5,
                    turn: None,
                },
                PointMark {
                    class: 0,
                    x: 7.5,
                    y: 4.5,
                    turn: None,
                },
            ],
        };
        let map = truth.heatmap(0, 0, 32);
        assert_eq!(map.iter().filter(|&&v| v == 1.0).count(), 2);
        assert!(map.iter().all(|&v| v <= 1.0));
    }

    #[test]
    fn a_turned_point_carries_its_angle_round_its_peak_and_nowhere_else() {
        let mut truth = one_point(1, 10.5, 10.5);
        truth.points[0].turn = Some(std::f64::consts::FRAC_PI_2);
        let angles = truth.angles(0, 0, 32);
        let (sin, cos) = (&angles[..1024], &angles[1024..]);
        assert!((sin[10 * 32 + 10] - 1.0).abs() < 1e-6);
        assert!(cos[10 * 32 + 10].abs() < 1e-6);
        // Two pixels off is still the point's; a picture away is no angle.
        assert!((sin[10 * 32 + 12] - 1.0).abs() < 1e-6);
        assert_eq!((sin[30 * 32 + 30], cos[30 * 32 + 30]), (0.0, 0.0));
    }

    #[test]
    fn a_point_which_does_not_turn_has_no_angle() {
        let angles = one_point(0, 10.5, 10.5).angles(0, 0, 32);
        assert!(angles.iter().all(|&v| v == 0.0));
    }

    #[test]
    fn half_a_turn_of_a_point_alike_at_half_a_turn_is_the_same_target() {
        let mut truth = one_point(0, 10.5, 10.5);
        truth.symmetry[0] = 2;
        truth.points[0].turn = Some(0.3);
        let once = truth.angles(0, 0, 32);
        truth.points[0].turn = Some(0.3 + std::f64::consts::PI);
        let turned = truth.angles(0, 0, 32);
        for (a, b) in once.iter().zip(&turned) {
            assert!((a - b).abs() < 1e-5);
        }
    }
}
