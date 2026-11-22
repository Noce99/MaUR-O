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
use crate::map::{Map, ObjectKind};
use crate::symbol_kinds::Catalogue;
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
    /// Which of the symbol set's point symbols it is drawn with: a place in
    /// [`crate::symbol_kinds::Catalogue::points`], and the channel it owns.
    pub class: usize,
    /// Where its coordinate lands, in pixels of the picture rightwards from
    /// its left edge: pixel `i` covers `i` to `i + 1`.
    pub x: f64,
    /// The same, downwards from the top edge.
    pub y: f64,
}

/// The point symbols of one picture.
#[derive(Clone, Debug)]
pub struct PointTruth {
    /// The height of the picture these labels are for, in pixels.
    pub height: u32,
    /// Its width, in pixels.
    pub width: u32,
    /// How many channels there are: the point symbols of the set.
    pub classes: usize,
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
        width: u32,
        height: u32,
        resolution: f64,
    ) -> Result<PointTruth, String> {
        let (mut map, _) = read_xml_map(path)?;
        map.resolve_references();
        Ok(PointTruth::of_map(&map, width, height, resolution))
    }

    /// [`PointTruth::from_map`] of a map already read, with its references
    /// resolved.
    ///
    /// A map with no point object on it gives labels with no point, which is
    /// an answer and not an error. An object which is not a point drawn with
    /// a point symbol of the set is skipped.
    pub fn of_map(map: &Map, width: u32, height: u32, resolution: f64) -> PointTruth {
        let catalogue = Catalogue::of(map);
        let page_transform = page_transform_for(map.scale_denominator, resolution, width, height);
        let points = map
            .objects
            .iter()
            .filter(|object| matches!(object.kind, ObjectKind::Point))
            .filter_map(|object| {
                let index = object.symbol_index?;
                let class = catalogue.points.iter().position(|e| e.index == index)?;
                let at = object.coords.first()?;
                let (x, y) = on_picture(&page_transform, at.x, at.y);
                Some(PointMark { class, x, y })
            })
            .collect();
        PointTruth {
            height,
            width,
            classes: catalogue.points.len(),
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
            points: vec![PointMark { class, x, y }],
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
            points: vec![
                PointMark {
                    class: 0,
                    x: 4.5,
                    y: 4.5,
                },
                PointMark {
                    class: 0,
                    x: 7.5,
                    y: 4.5,
                },
            ],
        };
        let map = truth.heatmap(0, 0, 32);
        assert_eq!(map.iter().filter(|&&v| v == 1.0).count(), 2);
        assert!(map.iter().all(|&v| v <= 1.0));
    }
}
