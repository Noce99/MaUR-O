//! A tree of cuts, as the polygons it cuts the picture into.
//!
//! [`crate::net::tree`] answers a picture with curves: it cuts the square in
//! two by a Bézier, each half again, and every leaf of what is left carries a
//! symbol. That is already a vector drawing, and this is what turns it into
//! one without a pixel in between — which is the point, since a grid of cells
//! is a staircase and rounding the staircase off again is a way of getting
//! back, not quite, to the curve it came from.
//!
//! Each cut is a Bézier sampled at `samples` points and continued past both
//! ends by a straight line, as the network itself measures its distances
//! (see [`crate::net::tree`]). Its positive side — where the signed distance
//! is above nought, the first half of the split — is a polygon: the curve,
//! then a long way off to the side it leaves on the left, and back. A piece
//! of the picture is divided by clipping it with that polygon and with what is
//! left of it, which the boolean operations of `geo` do exactly, and two
//! pieces made that way share the boundary between them to the last digit.
//!
//! The coordinates are the picture's: `[0, 1]` across and down, x to the
//! right and y downwards, the network's own.
//!
//! No part of this knows what a network is. It takes the control points and
//! the symbol of each leaf as plain numbers.

use geo::{Area, BooleanOps, Coord, LineString, MultiPolygon, Polygon, Simplify};

/// How far a cut is continued past each end, in picture widths. Long enough
/// that it leaves the square from wherever it starts. The network draws its
/// cuts with the same number, since what it trains on has to be what is
/// drawn.
pub const EXTENSION: f64 = 4.0;

/// How far from the curve the far side of its half-plane polygon is put, in
/// picture widths.
pub const FAR: f64 = 16.0;

/// The smallest piece worth keeping, as a share of the picture: what is
/// left of an area which two cuts nearly met.
const SLIVER: f64 = 1e-7;

/// A tree of cuts and what each of its leaves is.
#[derive(Clone, Debug)]
pub struct CutTree {
    /// Cuts from the root to a leaf. There are `2^depth` leaves.
    pub depth: usize,
    /// The control points of each cut's Bézier, in picture coordinates: the
    /// root first, then level by level, so the cut of the node at place `p`
    /// of level `l` is `cuts[2^l - 1 + p]`.
    pub cuts: Vec<Vec<(f64, f64)>>,
    /// The class of each leaf, left to right. A class is a place in a symbol
    /// list, and any class not under `classes` — the frame — is not drawn.
    pub leaf_class: Vec<usize>,
}

/// A piece of the picture of one class.
pub type Region = (usize, MultiPolygon<f64>);

/// The Bézier of `control` points sampled at `samples` places, continued by
/// a straight line past each end.
pub fn sample_cut(control: &[(f64, f64)], samples: usize) -> Vec<(f64, f64)> {
    let degree = control.len() - 1;
    let samples = samples.max(3);
    let mut line = Vec::with_capacity(samples + 2);
    for m in 0..samples {
        let t = m as f64 / (samples - 1) as f64;
        let (mut x, mut y, mut choose) = (0.0, 0.0, 1.0_f64);
        for (k, point) in control.iter().enumerate() {
            if k > 0 {
                choose = choose * (degree + 1 - k) as f64 / k as f64;
            }
            let weight = choose * t.powi(k as i32) * (1.0 - t).powi((degree - k) as i32);
            x += weight * point.0;
            y += weight * point.1;
        }
        line.push((x, y));
    }
    let first = unit(line[0], line[1]);
    let last = unit(line[samples - 1], line[samples - 2]);
    let start = (
        line[0].0 + first.0 * EXTENSION,
        line[0].1 + first.1 * EXTENSION,
    );
    let end = (
        line[samples - 1].0 + last.0 * EXTENSION,
        line[samples - 1].1 + last.1 * EXTENSION,
    );
    line.insert(0, start);
    line.push(end);
    line
}

/// The unit vector from `to` to `from`'s opposite: pointing away from `to`.
fn unit(from: (f64, f64), to: (f64, f64)) -> (f64, f64) {
    let (dx, dy) = (from.0 - to.0, from.1 - to.1);
    let length = dx.hypot(dy).max(1e-12);
    (dx / length, dy / length)
}

/// The side of the cut where the signed distance is positive, as a polygon.
///
/// Positive is where `cross(direction, point - start)` is above nought, which
/// is the side `(-dy, dx)` points to from a segment running `(dx, dy)`.
fn positive_side(line: &[(f64, f64)]) -> Polygon<f64> {
    let normal = |from: (f64, f64), to: (f64, f64)| {
        let (dx, dy) = (to.0 - from.0, to.1 - from.1);
        let length = dx.hypot(dy).max(1e-12);
        (-dy / length, dx / length)
    };
    let (first, last) = (line[0], line[line.len() - 1]);
    let n_first = normal(first, line[1]);
    let n_last = normal(line[line.len() - 2], last);
    let mut ring: Vec<Coord<f64>> = line.iter().map(|&(x, y)| Coord { x, y }).collect();
    ring.push(Coord {
        x: last.0 + n_last.0 * FAR,
        y: last.1 + n_last.1 * FAR,
    });
    ring.push(Coord {
        x: first.0 + n_first.0 * FAR,
        y: first.1 + n_first.1 * FAR,
    });
    Polygon::new(LineString::new(ring), vec![])
}

fn square() -> MultiPolygon<f64> {
    let ring = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)]
        .iter()
        .map(|&(x, y)| Coord { x, y })
        .collect();
    MultiPolygon::new(vec![Polygon::new(LineString::new(ring), vec![])])
}

impl CutTree {
    /// Whether leaves `first..first + span` all say one class.
    fn uniform(&self, first: usize, span: usize) -> bool {
        self.leaf_class[first..first + span]
            .iter()
            .all(|&class| class == self.leaf_class[first])
    }

    /// The piece of the picture each leaf holds, left to right.
    ///
    /// Where every leaf under a node says the same thing the node is not
    /// split: its piece goes whole to the first of them and the rest are
    /// empty. A cut which only divides an area from itself would otherwise
    /// leave a seam through it.
    pub fn leaf_pieces(&self, samples: usize) -> Vec<MultiPolygon<f64>> {
        let leaves = 1usize << self.depth;
        let sides: Vec<Polygon<f64>> = self
            .cuts
            .iter()
            .map(|control| positive_side(&sample_cut(control, samples)))
            .collect();
        let mut pieces = vec![MultiPolygon::new(vec![]); leaves];
        self.split(0, 0, square(), &sides, &mut pieces);
        pieces
    }

    fn split(
        &self,
        level: usize,
        place: usize,
        piece: MultiPolygon<f64>,
        sides: &[Polygon<f64>],
        out: &mut [MultiPolygon<f64>],
    ) {
        if piece.0.is_empty() {
            return;
        }
        let span = 1usize << (self.depth - level);
        let first = place * span;
        if span == 1 || self.uniform(first, span) {
            out[first] = piece;
            return;
        }
        let side = &sides[(1 << level) - 1 + place];
        let positive = piece.intersection(side);
        let negative = piece.difference(side);
        self.split(level + 1, 2 * place, positive, sides, out);
        self.split(level + 1, 2 * place + 1, negative, sides, out);
    }

    /// The pieces of the picture, one per class: the leaves of a class joined
    /// into one, the classes `classes` and above — the frame — left out, and
    /// what is left of the boundaries simplified to within `tolerance`
    /// picture widths, which is what removes the nodes a sampled curve has
    /// more of than it needs.
    pub fn regions(&self, samples: usize, classes: usize, tolerance: f64) -> Vec<Region> {
        let pieces = self.leaf_pieces(samples);
        let mut by_class: Vec<Option<MultiPolygon<f64>>> = vec![None; classes];
        for (leaf, piece) in pieces.into_iter().enumerate() {
            let class = self.leaf_class[leaf];
            if class >= classes || piece.0.is_empty() {
                continue;
            }
            by_class[class] = Some(match by_class[class].take() {
                Some(joined) => joined.union(&piece),
                None => piece,
            });
        }
        by_class
            .into_iter()
            .enumerate()
            .filter_map(|(class, joined)| {
                let kept: Vec<Polygon<f64>> = joined?
                    .0
                    .into_iter()
                    .filter(|polygon| polygon.unsigned_area() > SLIVER)
                    .collect();
                if kept.is_empty() {
                    return None;
                }
                let simplified = if tolerance > 0.0 {
                    MultiPolygon::new(kept).simplify(tolerance)
                } else {
                    MultiPolygon::new(kept)
                };
                Some((class, simplified))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A straight cut from `a` to `b`, as the two control points of a line.
    fn line(a: (f64, f64), b: (f64, f64)) -> Vec<(f64, f64)> {
        vec![a, b]
    }

    fn area(regions: &[Region], class: usize) -> f64 {
        regions
            .iter()
            .filter(|(c, _)| *c == class)
            .map(|(_, shape)| shape.unsigned_area())
            .sum()
    }

    /// A vertical line at x = 0.25 running downwards has its positive side, by
    /// `(-dy, dx)`, on the left: the narrow strip.
    #[test]
    fn a_cut_splits_the_square_into_the_two_sides_it_has() {
        let tree = CutTree {
            depth: 1,
            cuts: vec![line((0.25, 0.0), (0.25, 1.0))],
            leaf_class: vec![0, 1],
        };
        let regions = tree.regions(8, 2, 0.0);
        assert_eq!(regions.len(), 2);
        // dy = 1, so the normal is (-1, 0): the first leaf is the left strip.
        assert!((area(&regions, 0) - 0.25).abs() < 1e-9);
        assert!((area(&regions, 1) - 0.75).abs() < 1e-9);
    }

    #[test]
    fn the_leaves_tile_the_square_and_the_frame_is_left_out() {
        // A vertical cut, then each half cut across at its own height.
        let tree = CutTree {
            depth: 2,
            cuts: vec![
                line((0.5, 0.0), (0.5, 1.0)),
                line((0.0, 0.25), (0.5, 0.25)),
                line((0.5, 0.75), (1.0, 0.75)),
            ],
            leaf_class: vec![0, 1, 2, 3],
        };
        let regions = tree.regions(8, 4, 0.0);
        let total: f64 = regions.iter().map(|(_, shape)| shape.unsigned_area()).sum();
        assert!((total - 1.0).abs() < 1e-9, "{total}");

        // The last class is the frame, and is not a region.
        let regions = tree.regions(8, 3, 0.0);
        assert!(regions.iter().all(|(class, _)| *class < 3));
        let total: f64 = regions.iter().map(|(_, shape)| shape.unsigned_area()).sum();
        assert!(total < 1.0 - 0.1, "{total}");
    }

    /// Two leaves of one class are one area: no seam where the cut between
    /// them ran.
    #[test]
    fn leaves_of_one_class_are_joined_into_one_area() {
        let tree = CutTree {
            depth: 2,
            cuts: vec![
                line((0.5, 0.0), (0.5, 1.0)),
                line((0.0, 0.5), (0.5, 0.5)),
                line((0.5, 0.5), (1.0, 0.5)),
            ],
            leaf_class: vec![0, 0, 0, 1],
        };
        let regions = tree.regions(8, 2, 0.0);
        let zero = regions.iter().find(|(c, _)| *c == 0).unwrap();
        assert_eq!(zero.1 .0.len(), 1, "one polygon, not three");
        assert!((zero.1.unsigned_area() - 0.75).abs() < 1e-9);
    }

    /// A curved cut bends: the area it takes is not a half.
    #[test]
    fn a_curved_cut_takes_the_area_between_it_and_the_edge() {
        // A quadratic from (0, 0.5) to (1, 0.5) pulled up to y = 0.1 at the
        // middle control point, so the curve peaks at y = 0.3.
        let tree = CutTree {
            depth: 1,
            cuts: vec![vec![(0.0, 0.5), (0.5, 0.1), (1.0, 0.5)]],
            leaf_class: vec![0, 1],
        };
        let regions = tree.regions(64, 2, 0.0);
        // Running left to right, dx = 1: the normal is (0, 1), downwards. The
        // first leaf is below the curve. The area between a quadratic and its
        // chord is two thirds of the triangle on them: 2/3 * (1 * 0.4 / 2).
        let above = 0.5 - (2.0 / 3.0) * (1.0 * 0.4 / 2.0) * 1.0;
        let below = area(&regions, 0);
        assert!(((1.0 - below) - above).abs() < 1e-3, "{below} {above}");
    }
}
