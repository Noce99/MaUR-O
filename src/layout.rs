//! The layout of a random map: a square of ground divided into cells whose
//! sides wander.
//!
//! Step two of generating a random map. The ground a map covers is a square
//! `layout size` cells across, each cell `cell size` meters across, and every
//! cell is a piece of terrain of its own — a field, a marsh, a stand of trees
//! — which the next step fills in. What the cells must not look like is a
//! grid: nothing in a forest is square, and a generator whose regions all
//! meet at right angles teaches a renderer nothing about the shapes it will
//! actually be given.
//!
//! So the cell *corners* are pinned to the grid, at `(i · cell size, j · cell
//! size)`, and everything between two corners wanders. A side is a chain of
//! straight segments and cubic beziers through points scattered to either
//! side of the straight line the two corners define, by an amount which
//! tapers to nothing at the corners themselves — so the sides bulge in the
//! middle and stay put at the ends, which is what keeps neighbouring cells
//! from wandering into one another.
//!
//! A side is stored once and used twice: it is the right side of one cell and
//! the left side of the next, the second time round in reverse. Building both
//! cells out of the same side, rather than out of two sides which happen to
//! agree, is what makes the boundary between two regions of the finished map
//! a single line with nothing showing through it.
//!
//! The outermost sides are left straight: they are the edge of the map, not a
//! boundary between two pieces of terrain, and a map with a ragged outline
//! would be measuring the frame rather than the drawing.

use std::f64::consts::PI;

use crate::geometry::flatten;
use crate::map::{Coord, CoordList, Point};
use crate::path_builder::{coord_at, PathBuilder};
use crate::random::Random;

/// How many points a side wanders through, at the fewest and at the most,
/// not counting the two corners it is pinned to.
const WANDERING_POINTS: (usize, usize) = (3, 7);

/// How far a side may stray from the straight line between its corners, as a
/// fraction of the cell size.
///
/// Below a half by a comfortable margin: two sides straying towards each
/// other from opposite sides of a cell must not meet, and the taper at the
/// corners means the widest part of one is up against the widest part of the
/// other.
const MAX_SWAY: f64 = 0.3;

/// How far along its side a wandering point may slide from its even share of
/// the way, as a fraction of that share. Below a half, so the points keep the
/// order they were laid down in.
const MAX_SLIDE: f64 = 0.35;

/// How often a segment between two wandering points is a curve rather than a
/// straight line.
const CURVE_CHANCE: f64 = 0.6;

/// How many points of a cell's bounding box are tried before a point symbol
/// is put in the middle of the cell instead. A cell fills a good half of its
/// box, so a try lands inside far more often than not.
const PLACEMENT_TRIES: usize = 32;

/// How far along the way to the next point a curve's control points sit, at
/// the least and at the most, as a fraction of the segment's length.
///
/// A third is what draws the arc a circle would; more bulges, less flattens.
const PULL: (f64, f64) = (0.2, 0.45);

/// One piece of a cell side, from wherever the previous piece ended.
#[derive(Clone, Copy, Debug)]
enum Segment {
    /// A straight segment to this point.
    Line(Point),
    /// A cubic bezier: two control points, then the point it ends at.
    Curve(Point, Point, Point),
}

impl Segment {
    /// Where the piece ends.
    fn end(self) -> Point {
        match self {
            Segment::Line(end) => end,
            Segment::Curve(_, _, end) => end,
        }
    }
}

/// One side of a cell: a chain of segments from one grid corner to the next.
///
/// Positions are in meters on the ground.
#[derive(Clone, Debug)]
pub struct Side {
    /// The corner the side starts at.
    start: Point,
    /// The pieces it is made of, in the order it runs.
    segments: Vec<Segment>,
}

impl Side {
    /// A straight side from `from` to `to`.
    fn straight(from: Point, to: Point) -> Side {
        Side {
            start: from,
            segments: vec![Segment::Line(to)],
        }
    }

    /// A side from `from` to `to` which wanders by up to [`MAX_SWAY`] of
    /// `cell_size` on its way.
    fn wandering(from: Point, to: Point, cell_size: f64, random: &mut Random) -> Side {
        let along = to - from;
        let across = along.perp_right_unit();
        let count = random.in_range(WANDERING_POINTS.0, WANDERING_POINTS.1);
        let share = 1.0 / (count + 1) as f64;

        let mut points = Vec::with_capacity(count + 2);
        points.push(from);
        for step in 1..=count {
            // Where along the side this point sits, its even share of the way
            // give or take; and how far to one side of it, tapering to
            // nothing at either corner so that the corners stay corners.
            let at = (step as f64 + random.symmetric(MAX_SLIDE)) * share;
            let sway = random.symmetric(MAX_SWAY * cell_size) * (PI * at).sin();
            points.push(from + along * at + across * sway);
        }
        points.push(to);

        // Which way the side is heading as it passes each point: the line
        // through the point before it and the point after it, which is what
        // makes two curves meeting at a point meet smoothly instead of
        // cornering. The two ends head straight at their neighbour, since
        // there is nothing on the other side of them to be smooth with.
        let heading = |at: usize| -> Point {
            let before = points[at.saturating_sub(1)];
            let after = points[(at + 1).min(points.len() - 1)];
            (after - before).normalized()
        };
        // Nothing a side is made of, its control points included, strays
        // further from the straight line between the corners than a wandering
        // point may: a control point which would is pulled back to that, which
        // flattens the curve rather than letting it lean into the next cell.
        let limit = MAX_SWAY * cell_size;
        let held_back = |point: Point| -> Point {
            let strayed = (point - from).dot(across);
            point - across * (strayed - strayed.clamp(-limit, limit))
        };

        let mut segments = Vec::with_capacity(points.len() - 1);
        for (at, pair) in points.windows(2).enumerate() {
            let (start, end) = (pair[0], pair[1]);
            if random.chance(CURVE_CHANCE) {
                // One control point out along the heading at each end, by a
                // length of its own: how far each end pulls the curve its way
                // before it gives in to the other.
                let length = (end - start).length();
                let pull = |random: &mut Random| random.between(PULL.0, PULL.1) * length;
                segments.push(Segment::Curve(
                    held_back(start + heading(at) * pull(random)),
                    held_back(end - heading(at + 1) * pull(random)),
                    end,
                ));
            } else {
                segments.push(Segment::Line(end));
            }
        }
        Side {
            start: from,
            segments,
        }
    }

    /// Where the side ends.
    pub fn end(&self) -> Point {
        self.segments.last().map_or(self.start, |s| s.end())
    }

    /// Where the side starts.
    pub fn start(&self) -> Point {
        self.start
    }

    /// The same side, run the other way: the side of the cell on the other
    /// side of it.
    fn reversed(&self) -> Side {
        let mut starts = Vec::with_capacity(self.segments.len());
        let mut at = self.start;
        for segment in &self.segments {
            starts.push(at);
            at = segment.end();
        }
        let segments = self
            .segments
            .iter()
            .zip(&starts)
            .rev()
            .map(|(segment, &start)| match *segment {
                Segment::Line(_) => Segment::Line(start),
                // A cubic bezier run backwards is the same curve with its
                // control points swapped.
                Segment::Curve(first, second, _) => Segment::Curve(second, first, start),
            })
            .collect();
        Side {
            start: at,
            segments,
        }
    }

    /// Appends the side to a path which already ends at
    /// [`start`](Self::start).
    fn append_to(&self, builder: &mut PathBuilder) {
        for segment in &self.segments {
            match *segment {
                Segment::Line(end) => builder.line_to(end),
                Segment::Curve(first, second, end) => builder.curve_to(first, second, end),
            }
        }
    }
}

/// A square of ground divided into cells with wandering sides.
///
/// The square is centered on the origin, so a layout's coordinates run from
/// `-side_length / 2` to `+side_length / 2` in both directions — where a map
/// is usually drawn, and where the frame a renderer puts round it comes out
/// even.
pub struct Layout {
    /// How many cells the square is across.
    size: usize,
    /// How wide one cell is, in meters on the ground.
    cell_size: f64,
    /// The sides running left to right, `size` of them for each of the
    /// `size + 1` rows of corners, row by row.
    horizontal: Vec<Side>,
    /// The sides running top to bottom, `size + 1` of them for each of the
    /// `size` rows of cells, row by row.
    vertical: Vec<Side>,
}

impl Layout {
    /// A random layout of `size` by `size` cells, each `cell_size` meters
    /// across.
    ///
    /// Everything random about it comes out of `random`, so the same
    /// generator in the same state gives the same layout.
    pub fn random(size: usize, cell_size: f64, random: &mut Random) -> Layout {
        let mut layout = Layout {
            size,
            cell_size,
            horizontal: Vec::with_capacity(size * (size + 1)),
            vertical: Vec::with_capacity(size * (size + 1)),
        };

        for row in 0..=size {
            for column in 0..size {
                let (from, to) = (layout.corner(column, row), layout.corner(column + 1, row));
                // The top and bottom rows of sides are the edge of the map.
                layout.horizontal.push(if row == 0 || row == size {
                    Side::straight(from, to)
                } else {
                    Side::wandering(from, to, cell_size, random)
                });
            }
        }
        for row in 0..size {
            for column in 0..=size {
                let (from, to) = (layout.corner(column, row), layout.corner(column, row + 1));
                layout.vertical.push(if column == 0 || column == size {
                    Side::straight(from, to)
                } else {
                    Side::wandering(from, to, cell_size, random)
                });
            }
        }
        layout
    }

    /// How many cells the square is across.
    pub fn size(&self) -> usize {
        self.size
    }

    /// How wide one cell is, in meters on the ground.
    pub fn cell_size(&self) -> f64 {
        self.cell_size
    }

    /// How wide the whole square is, in meters on the ground.
    pub fn side_length(&self) -> f64 {
        self.size as f64 * self.cell_size
    }

    /// How many cells there are.
    pub fn cell_count(&self) -> usize {
        self.size * self.size
    }

    /// The grid corner `(column, row)` of the `size + 1` by `size + 1` of
    /// them, in meters on the ground.
    pub fn corner(&self, column: usize, row: usize) -> Point {
        let half = self.side_length() / 2.0;
        Point::new(
            column as f64 * self.cell_size - half,
            row as f64 * self.cell_size - half,
        )
    }

    /// The side above cell `(column, row)`, running left to right.
    fn horizontal(&self, column: usize, row: usize) -> &Side {
        &self.horizontal[row * self.size + column]
    }

    /// The side to the left of cell `(column, row)`, running top to bottom.
    fn vertical(&self, column: usize, row: usize) -> &Side {
        &self.vertical[row * (self.size + 1) + column]
    }

    /// The outline of one cell as a closed path, in the coordinates a map
    /// file holds — see [`PathBuilder`].
    ///
    /// It runs clockwise from the cell's top left corner: the side above the
    /// cell, the one to its right, the one below it backwards, the one to its
    /// left backwards.
    pub fn cell_outline(&self, column: usize, row: usize, mm_per_meter: f64) -> CoordList {
        let mut builder = PathBuilder::new(mm_per_meter);
        builder.move_to(self.corner(column, row));
        self.horizontal(column, row).append_to(&mut builder);
        self.vertical(column + 1, row).append_to(&mut builder);
        self.horizontal(column, row + 1)
            .reversed()
            .append_to(&mut builder);
        self.vertical(column, row)
            .reversed()
            .append_to(&mut builder);
        builder.close();
        builder.coords
    }

    /// The outline of every cell, row by row.
    pub fn cell_outlines(&self, mm_per_meter: f64) -> Vec<CoordList> {
        let mut outlines = Vec::with_capacity(self.cell_count());
        for row in 0..self.size {
            for column in 0..self.size {
                outlines.push(self.cell_outline(column, row, mm_per_meter));
            }
        }
        outlines
    }

    /// Every side of the layout as an open path, each one once: the
    /// boundaries the cells share and the four edges of the square.
    ///
    /// A boundary belongs to the two cells at once, so it is here once
    /// rather than twice — something drawn along it is drawn along both
    /// cells, which is what a fence between two fields is.
    pub fn side_paths(&self, mm_per_meter: f64) -> Vec<CoordList> {
        self.horizontal
            .iter()
            .chain(&self.vertical)
            .map(|side| {
                let mut builder = PathBuilder::new(mm_per_meter);
                builder.move_to(side.start());
                side.append_to(&mut builder);
                builder.coords
            })
            .collect()
    }

    /// The middle of cell `(column, row)`'s grid square, in meters on the
    /// ground.
    ///
    /// Inside the cell whatever its sides did: a side strays at most
    /// [`MAX_SWAY`] of a cell from the straight line between its corners,
    /// which leaves the middle of the square well clear of all four.
    pub fn cell_center(&self, column: usize, row: usize) -> Point {
        let corner = self.corner(column, row);
        Point::new(
            corner.x + self.cell_size / 2.0,
            corner.y + self.cell_size / 2.0,
        )
    }

    /// A coordinate somewhere inside cell `(column, row)`, in what a map file
    /// holds.
    ///
    /// Anywhere in the cell, corners and boundaries included: a point symbol
    /// dropped near a boundary draws over it, which is where a renderer has
    /// to decide what covers what. Found by trying points of the cell's
    /// bounding box until one lands inside the cell itself, and after
    /// [`PLACEMENT_TRIES`] of those the middle of the square, which is
    /// inside every cell there can be.
    pub fn random_point_in_cell(
        &self,
        column: usize,
        row: usize,
        mm_per_meter: f64,
        random: &mut Random,
    ) -> Coord {
        let outline = self.cell_outline(column, row, mm_per_meter);
        let flattened = flatten(&outline);
        if let Some(polygon) = flattened.first().map(|part| part.points.as_slice()) {
            let (left, top, right, bottom) = bounds(polygon);
            for _ in 0..PLACEMENT_TRIES {
                let point = Point::new(random.between(left, right), random.between(top, bottom));
                if is_inside(polygon, point) {
                    return coord_at(point);
                }
            }
        }
        coord_at(self.cell_center(column, row) * mm_per_meter)
    }
}

/// The bounding box of a polygon, as left, top, right and bottom.
fn bounds(polygon: &[Point]) -> (f64, f64, f64, f64) {
    polygon.iter().fold(
        (f64::MAX, f64::MAX, f64::MIN, f64::MIN),
        |(left, top, right, bottom), point| {
            (
                left.min(point.x),
                top.min(point.y),
                right.max(point.x),
                bottom.max(point.y),
            )
        },
    )
}

/// Whether the point is inside the polygon, by the crossing rule: a ray from
/// the point crosses the outline an odd number of times.
fn is_inside(polygon: &[Point], point: Point) -> bool {
    let mut inside = false;
    for at in 0..polygon.len() {
        let (from, to) = (polygon[at], polygon[(at + 1) % polygon.len()]);
        if (from.y > point.y) != (to.y > point.y) {
            let crossing = from.x + (point.y - from.y) / (to.y - from.y) * (to.x - from.x);
            if point.x < crossing {
                inside = !inside;
            }
        }
    }
    inside
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map::coord_flag;

    /// A layout at 1:10000, where one meter on the ground is 0.1 mm on the
    /// paper.
    const MM_PER_METER: f64 = 0.1;

    fn layout(size: usize, seed: u64) -> Layout {
        Layout::random(size, 30.0, &mut Random::from_seed(seed))
    }

    #[test]
    fn the_square_is_the_cell_size_by_the_layout_size() {
        let layout = layout(3, 1);
        assert_eq!(layout.side_length(), 90.0);
        assert_eq!(layout.cell_count(), 9);
        // Centered on the origin.
        assert_eq!(layout.corner(0, 0), Point::new(-45.0, -45.0));
        assert_eq!(layout.corner(3, 3), Point::new(45.0, 45.0));
    }

    #[test]
    fn every_cell_outline_is_closed() {
        let layout = layout(3, 2);
        for outline in layout.cell_outlines(MM_PER_METER) {
            let last = outline.last().expect("a cell has an outline");
            assert!(last.is_close_point(), "{last:?}");
            assert_eq!((last.x, last.y), (outline[0].x, outline[0].y));
        }
    }

    #[test]
    fn a_cell_corner_sits_exactly_on_the_grid() {
        let layout = layout(3, 3);
        for row in 0..3 {
            for column in 0..3 {
                let outline = layout.cell_outline(column, row, MM_PER_METER);
                let corner = layout.corner(column, row);
                assert_eq!(outline[0].x, corner.x * MM_PER_METER);
                assert_eq!(outline[0].y, corner.y * MM_PER_METER);
            }
        }
    }

    /// The point of storing a side once: the boundary two cells share is one
    /// line, run one way and then the other, rather than two lines which look
    /// alike.
    #[test]
    fn two_cells_share_the_side_between_them() {
        let layout = layout(3, 4);
        let left = layout.cell_outline(0, 0, MM_PER_METER);
        let right = layout.cell_outline(1, 0, MM_PER_METER);
        let side = layout.vertical(1, 0);

        // The side as each cell holds it: the second corner of the left cell
        // is where the shared side starts.
        let mut forwards = PathBuilder::new(MM_PER_METER);
        forwards.move_to(side.start());
        side.append_to(&mut forwards);
        let mut backwards = PathBuilder::new(MM_PER_METER);
        backwards.move_to(side.end());
        side.reversed().append_to(&mut backwards);

        let positions =
            |coords: &CoordList| -> Vec<(f64, f64)> { coords.iter().map(|c| (c.x, c.y)).collect() };
        let one = positions(&forwards.coords);
        let other: Vec<(f64, f64)> = positions(&backwards.coords).into_iter().rev().collect();
        assert_eq!(one, other);

        // And both cells are drawn along it.
        let inside = |outline: &CoordList, wanted: &[(f64, f64)]| {
            let all = positions(outline);
            wanted.iter().all(|point| all.contains(point))
        };
        assert!(inside(&left, &one));
        assert!(inside(&right, &one));
    }

    #[test]
    fn a_reversed_side_is_the_same_curve_the_other_way_round() {
        let mut random = Random::from_seed(5);
        let side = Side::wandering(
            Point::new(0.0, 0.0),
            Point::new(30.0, 0.0),
            30.0,
            &mut random,
        );
        let back = side.reversed();
        assert_eq!(back.start(), side.end());
        assert_eq!(back.end(), side.start());
        assert_eq!(back.reversed().segments.len(), side.segments.len());
    }

    #[test]
    fn a_side_stays_within_reach_of_the_line_between_its_corners() {
        let mut random = Random::from_seed(6);
        for _ in 0..200 {
            let side = Side::wandering(
                Point::new(0.0, 0.0),
                Point::new(30.0, 0.0),
                30.0,
                &mut random,
            );
            let mut at = side.start();
            for segment in &side.segments {
                for point in match *segment {
                    Segment::Line(end) => vec![end],
                    Segment::Curve(first, second, end) => vec![first, second, end],
                } {
                    // Every point, control points included, keeps to the
                    // sway: a curve is inside the hull of its control points,
                    // so no part of the side reaches the next cell along.
                    assert!(
                        point.y.abs() <= MAX_SWAY * 30.0 + 1e-9,
                        "{point:?} after {at:?}"
                    );
                    at = point;
                }
            }
        }
    }

    #[test]
    fn the_edge_of_the_map_is_straight() {
        let layout = layout(4, 7);
        for column in 0..4 {
            for row in [0, 4] {
                let side = layout.horizontal(column, row);
                assert_eq!(side.segments.len(), 1);
                assert!(matches!(side.segments[0], Segment::Line(_)));
            }
        }
        for row in 0..4 {
            for column in [0, 4] {
                assert_eq!(layout.vertical(column, row).segments.len(), 1);
            }
        }
        // The corners of the square are where they should be, and the outline
        // of a corner cell is two straight sides and two wandering ones.
        let outline = layout.cell_outline(0, 0, MM_PER_METER);
        assert!(outline
            .iter()
            .any(|c| c.flags & coord_flag::CURVE_START != 0));
    }

    /// Each side once, whichever cells it belongs to: a 3 by 3 layout has
    /// four rows of three sides across and three rows of four down.
    #[test]
    fn every_side_is_a_path_of_its_own() {
        let layout = layout(3, 8);
        let sides = layout.side_paths(MM_PER_METER);
        assert_eq!(sides.len(), 2 * 3 * 4);
        for side in &sides {
            // An open path: a boundary is drawn along, not filled.
            assert!(!side.last().unwrap().is_close_point());
            assert!(side.len() >= 2);
        }
        // A side of the grid runs corner to corner, however it wandered on
        // the way.
        let first = &sides[0];
        let (start, end) = (first[0], *first.last().unwrap());
        assert_eq!((start.x, start.y), (-4.5, -4.5));
        assert_eq!((end.x, end.y), (-1.5, -4.5));
    }

    #[test]
    fn a_point_dropped_in_a_cell_lands_in_that_cell() {
        let layout = layout(3, 9);
        let mut random = Random::from_seed(10);
        let polygon = |column, row| {
            let outline = layout.cell_outline(column, row, MM_PER_METER);
            flatten(&outline)[0].points.clone()
        };
        let (mine, neighbour) = (polygon(1, 1), polygon(2, 1));

        let mut seen = std::collections::HashSet::new();
        for _ in 0..200 {
            let point = layout.random_point_in_cell(1, 1, MM_PER_METER, &mut random);
            let at = Point::new(point.x, point.y);
            assert!(is_inside(&mine, at), "{at:?} is outside its own cell");
            // The two cells share the side between them, so a point which
            // is in one is in neither the other nor the boundary itself.
            assert!(!is_inside(&neighbour, at), "{at:?} is in the next cell");
            seen.insert((point.x.to_bits(), point.y.to_bits()));
        }
        // Scattered over the cell rather than falling back to its middle.
        assert!(seen.len() > 190, "{} different points", seen.len());
    }

    #[test]
    fn the_same_seed_gives_the_same_layout() {
        let one = layout(3, 11).cell_outlines(MM_PER_METER);
        let other = layout(3, 11).cell_outlines(MM_PER_METER);
        let different = layout(3, 12).cell_outlines(MM_PER_METER);
        let positions = |cells: &[CoordList]| -> Vec<(f64, f64, i32)> {
            cells
                .iter()
                .flatten()
                .map(|c| (c.x, c.y, c.flags))
                .collect()
        };
        assert_eq!(positions(&one), positions(&other));
        assert_ne!(positions(&one), positions(&different));
    }
}
