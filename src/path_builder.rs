//! Builds the coordinate list of a path object out of moves, lines and
//! curves.
//!
//! The shape of a generated object is thought about in meters on the ground —
//! a 50 m circle, a cell 30 m across — and stored in the file as an integer
//! number of 1/1000 mm on the paper. This is where the one becomes the other,
//! and it is the only place either generator does the conversion, so that a
//! coordinate two shapes share comes out of both of them as the same
//! coordinate.
//!
//! The rounding is Mapper's own: a `MapCoord` is made of whole native units
//! as soon as it is made at all, and everything downstream — the extents a
//! layout is measured with, the outline a neighbouring cell is built from —
//! sees the rounded shape rather than the exact one.

use crate::geometry::qround;
use crate::map::{coord_flag, Coord, CoordList, Point};

/// A position in mm on the paper, as the file holds it.
///
/// `MapCoord(qreal, qreal)` rounds millimetres to native units, by the rule
/// qRound uses rather than by Rust's, see [`qround`]. Anything which puts a
/// coordinate in a file goes through here, so that a position two shapes
/// share is the same coordinate in both of them.
pub fn coord_at(mm: Point) -> Coord {
    Coord::new(
        qround(mm.x * 1000.0) / 1000.0,
        qround(mm.y * 1000.0) / 1000.0,
        0,
    )
}

/// A coordinate list under construction, in the manner of a drawing context:
/// [`move_to`](Self::move_to) to start a part, [`line_to`](Self::line_to) and
/// [`curve_to`](Self::curve_to) to extend it, [`close`](Self::close) to shut
/// it.
///
/// Positions are given in meters on the ground, relative to wherever the
/// shape considers its own origin.
pub struct PathBuilder {
    /// How many mm on the paper one meter on the ground is: `1000 / scale`.
    mm_per_meter: f64,
    /// What has been built so far, in mm on the paper.
    pub coords: CoordList,
    /// Where the part being built began, for [`close`](Self::close).
    part_start: usize,
}

impl PathBuilder {
    /// An empty builder for a map at the given scale, expressed as the mm on
    /// the paper one meter on the ground takes.
    pub fn new(mm_per_meter: f64) -> PathBuilder {
        PathBuilder {
            mm_per_meter,
            coords: Vec::new(),
            part_start: 0,
        }
    }

    /// A point in meters on the ground, as the file holds it.
    fn to_coord(&self, point: Point) -> Coord {
        coord_at(point * self.mm_per_meter)
    }

    /// Starts a new part. Any previous part becomes a hole of this object.
    pub fn move_to(&mut self, point: Point) {
        if let Some(last) = self.coords.last_mut() {
            last.flags |= coord_flag::HOLE_POINT;
        }
        self.part_start = self.coords.len();
        let coord = self.to_coord(point);
        self.coords.push(coord);
    }

    /// Appends a straight segment.
    pub fn line_to(&mut self, point: Point) {
        let coord = self.to_coord(point);
        self.coords.push(coord);
    }

    /// Appends a cubic bezier segment.
    pub fn curve_to(&mut self, control_1: Point, control_2: Point, point: Point) {
        if let Some(last) = self.coords.last_mut() {
            last.flags |= coord_flag::CURVE_START;
        }
        for p in [control_1, control_2, point] {
            let coord = self.to_coord(p);
            self.coords.push(coord);
        }
    }

    /// Closes the current part.
    ///
    /// The last coordinate of a closed part repeats the first one. If the
    /// part already ends at its start position, that coordinate is reused.
    pub fn close(&mut self) {
        let start = self.coords[self.part_start];
        let last = *self.coords.last().expect("a part has been started");
        // Compared as the file holds them, which is what
        // MapCoord::isPositionEqualTo compares.
        if last.x == start.x && last.y == start.y {
            self.coords.last_mut().unwrap().flags |= coord_flag::CLOSE_POINT;
        } else {
            let mut closing = start;
            closing.flags &= !coord_flag::CURVE_START;
            closing.flags |= coord_flag::CLOSE_POINT;
            self.coords.push(closing);
        }
    }
}
