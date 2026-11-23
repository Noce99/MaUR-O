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

/// How far a line running through the inside of a cell may stray from the
/// straight line between the points it passes through, as a fraction of the
/// cell size: well short of [`MAX_SWAY`], so that a line from the middle of a
/// cell to the middle of one of its sides stays inside the cell however the
/// sides around it wander.
const INTERIOR_SWAY: f64 = 0.08;

/// How far a leg of a line through the cells may stray from the straight
/// line between its ends, as a fraction of its own length, where that is
/// less than `INTERIOR_SWAY` of a cell: a short leg swaying as far as a long
/// one curls up.
const LEG_SWAY: f64 = 0.15;

/// How far apart, as a fraction of the cell size, the points a leg of a line
/// through the cells wanders through are at the closest: one to five of
/// them, fewer on a shorter leg.
const LEG_POINT_SPACING: f64 = 0.12;

/// How far from the middle of its grid square the point a line through a
/// cell passes it at may be, in each direction, as a fraction of the cell
/// size: enough that the lines of a dataset do not all meet at one point of
/// every cell, and little enough to stay well clear of the sides.
const CENTER_JITTER: f64 = 0.1;

/// How near the outline of a cell, in meters, an end of a leg is taken to
/// be on it: well over the thousandth a map file's coordinates are rounded
/// to, and well under anything a line would be seen to stray by.
const ON_A_SIDE: f64 = 0.005;

/// How near another leg, as a fraction of the cell size, a leg of a line
/// through the cells is taken to be on it.
const NEAR_ANOTHER: f64 = 0.015;

/// How much of a leg may lie near another, as a multiple of what "near" is:
/// four is a crossing at thirty degrees or more, as a crossing at an angle
/// `a` lies near the other leg for `2 near / sin a`. Anything flatter is
/// running along it.
const ALONGSIDE: f64 = 4.0;

/// How many wandering legs [`Layout::leg`] tries before a straight one.
const LEG_TRIES: usize = 8;

/// How close to where it crosses a side a leg of a line through the cells
/// may come out of its cell, as a fraction of the cell size: the point it
/// crosses at is on the side, which is neither inside nor out.
const CROSSING_SLACK: f64 = 0.05;

/// How far from the middle of a straight side -- an edge of the square -- the
/// point a line leaves the map by may be, as a fraction of the side.
const EDGE_SLIDE: f64 = 0.15;

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
        Side::wandering_by(from, to, MAX_SWAY * cell_size, WANDERING_POINTS, random)
    }

    /// A chain from `from` to `to` which strays up to `limit` meters from
    /// the straight line between them, tapering to nothing at either end,
    /// through between `points.0` and `points.1` wandering points.
    fn wandering_by(
        from: Point,
        to: Point,
        limit: f64,
        points: (usize, usize),
        random: &mut Random,
    ) -> Side {
        let along = to - from;
        let across = along.perp_right_unit();
        let count = random.in_range(points.0, points.1);
        let share = 1.0 / (count + 1) as f64;

        let mut points = Vec::with_capacity(count + 2);
        points.push(from);
        for step in 1..=count {
            // Where along the side this point sits, its even share of the way
            // give or take; and how far to one side of it, tapering to
            // nothing at either corner so that the corners stay corners.
            let at = (step as f64 + random.symmetric(MAX_SLIDE)) * share;
            let sway = random.symmetric(limit) * (PI * at).sin();
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

    /// A point the side passes through, near its middle: the middle one of
    /// the points it wanders through, or, for a straight side, somewhere
    /// within `EDGE_SLIDE` of its middle.
    ///
    /// On the side itself rather than near it, so that a line which crosses
    /// a side there goes from one cell straight into the next.
    fn middle(&self, random: &mut Random) -> Point {
        match self.segments.len() {
            1 => {
                let (from, to) = (self.start, self.end());
                from + (to - from) * (0.5 + random.symmetric(EDGE_SLIDE))
            }
            count => self.segments[(count - 1) / 2].end(),
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

/// One stretch of a line through the inside of the cells, inside one cell:
/// see [`Layout::leg`].
#[derive(Clone, Debug)]
pub struct Leg(Side);

impl Leg {
    /// The points it passes through, flattened, in meters on the ground.
    pub fn points(&self) -> Vec<Point> {
        let mut builder = PathBuilder::new(1.0);
        builder.move_to(self.0.start());
        self.0.append_to(&mut builder);
        flatten(&builder.coords)
            .into_iter()
            .flat_map(|part| part.points)
            .collect()
    }

    /// The same leg run the other way.
    pub fn reversed(&self) -> Leg {
        Leg(self.0.reversed())
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

    /// The cells either side of each side, in the order of
    /// [`Layout::side_paths`], as places in [`Layout::cell_outlines`]: above
    /// and below a side running left to right, left and right of one running
    /// top to bottom. A side along the edge of the square has one cell, and
    /// `None` where the other would be.
    pub fn side_cells(&self) -> Vec<[Option<usize>; 2]> {
        let size = self.size;
        let mut cells = Vec::with_capacity(self.horizontal.len() + self.vertical.len());
        for row in 0..=size {
            for column in 0..size {
                cells.push([
                    (row > 0).then(|| (row - 1) * size + column),
                    (row < size).then(|| row * size + column),
                ]);
            }
        }
        for row in 0..size {
            for column in 0..=size {
                cells.push([
                    (column > 0).then(|| row * size + column - 1),
                    (column < size).then(|| row * size + column),
                ]);
            }
        }
        cells
    }

    /// The four sides of cell `(column, row)`, as places in
    /// [`Layout::side_paths`] (and [`Layout::side_cells`]): the one above
    /// it, the one to its right, the one below it and the one to its left.
    pub fn cell_sides(&self, column: usize, row: usize) -> [usize; 4] {
        let (size, across) = (self.size, self.horizontal.len());
        [
            row * size + column,
            across + row * (size + 1) + column + 1,
            (row + 1) * size + column,
            across + row * (size + 1) + column,
        ]
    }

    /// A point on side `side` -- a place in [`Layout::side_paths`] -- near its
    /// middle, in meters on the ground: where a line running through the
    /// cells crosses from one into the next. See `Side::middle`.
    pub fn side_middle(&self, side: usize, random: &mut Random) -> Point {
        let across = self.horizontal.len();
        if side < across {
            self.horizontal[side].middle(random)
        } else {
            self.vertical[side - across].middle(random)
        }
    }

    /// A point near the middle of cell `(column, row)`, within
    /// `CENTER_JITTER` of the cell size of it each way, in meters on the
    /// ground: where a line running through the cell passes it.
    pub fn near_center(&self, column: usize, row: usize, random: &mut Random) -> Point {
        let jitter = CENTER_JITTER * self.cell_size;
        let center = self.cell_center(column, row);
        Point::new(
            center.x + random.symmetric(jitter),
            center.y + random.symmetric(jitter),
        )
    }

    /// One leg of a line through the inside of the cells: a path from
    /// `from` to `to`, both in cell `(column, row)` or on its sides, which
    /// wanders by up to `INTERIOR_SWAY` of a cell (or `LEG_SWAY` of itself,
    /// where that is less) and stays inside the
    /// cell the whole way, crossing the legs of `avoid` if it must but never
    /// running along one — or `None` where none of `LEG_TRIES` wandering
    /// ones nor a straight one does.
    ///
    /// The sides of a cell wander by far more than that, and one can bulge
    /// into the cell across the way from where it is to be crossed; a leg
    /// which went round it would cross the ground of the next cell, whose
    /// colours nobody checked the line against, or a border nobody wanted it
    /// on.
    pub fn leg(
        &self,
        column: usize,
        row: usize,
        from: Point,
        to: Point,
        avoid: &[Leg],
        random: &mut Random,
    ) -> Option<Leg> {
        let outline: Vec<Point> = flatten(&self.cell_outline(column, row, 1.0))
            .into_iter()
            .next()?
            .points;
        // Only an end on a side, where the leg crosses it, has slack: an end
        // inside the cell, where the leg stops short, is held to the cell
        // like the rest of it.
        let on_a_side = |end: Point| {
            outline.windows(2).any(|pair| {
                let (a, b) = (pair[0], pair[1]);
                let along = b - a;
                let length = along.dot(along);
                let t = if length > 0.0 {
                    ((end - a).dot(along) / length).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                // The outline is in coordinates rounded to a thousandth.
                (end - (a + along * t)).length() < ON_A_SIDE
            })
        };
        let slack = CROSSING_SLACK * self.cell_size;
        let (from_slack, to_slack) = (
            if on_a_side(from) { slack } else { 0.0 },
            if on_a_side(to) { slack } else { 0.0 },
        );
        let near = NEAR_ANOTHER * self.cell_size;
        let others: Vec<Vec<Point>> = avoid.iter().map(Leg::points).collect();
        let fits = |leg: &Leg| {
            let points = leg.points();
            let inside = points.iter().all(|&point| {
                (point - from).length() < from_slack
                    || (point - to).length() < to_slack
                    || is_inside(&outline, point)
            });
            // As much of it as lies near another leg, in steps of a fraction
            // of "near" so that a long straight piece crossing one counts
            // for the crossing alone: a crossing comes to a little, running
            // along it to a lot.
            let alongside = length_near(&points, &others, near);
            inside && alongside <= ALONGSIDE * near
        };
        // A short leg wanders by little and through few points: by as much
        // as a long one, it would fold over on itself.
        let length = (to - from).length();
        let sway = (INTERIOR_SWAY * self.cell_size).min(LEG_SWAY * length);
        let most = ((length / (LEG_POINT_SPACING * self.cell_size)).round() as usize).clamp(1, 5);
        let points = (most.div_ceil(2), most);
        (0..LEG_TRIES)
            .map(|_| Leg(Side::wandering_by(from, to, sway, points, random)))
            .chain(std::iter::once(Leg(Side::straight(from, to))))
            .find(|leg| fits(leg))
    }

    /// The legs of a line, one after another, as an open path in the
    /// coordinates a map file holds. Each leg is to start where the one
    /// before it ended.
    pub fn path_of(&self, legs: &[Leg], mm_per_meter: f64) -> CoordList {
        let mut builder = PathBuilder::new(mm_per_meter);
        if let Some(first) = legs.first() {
            builder.move_to(first.0.start());
            for leg in legs {
                leg.0.append_to(&mut builder);
            }
        }
        builder.coords
    }

    /// The middle of cell `(column, row)`'s grid square, in meters on the
    /// ground.
    ///
    /// Inside the cell whatever its sides did: a side strays at most
    /// `MAX_SWAY` of a cell from the straight line between its corners,
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
    /// `PLACEMENT_TRIES` of those the middle of the square, which is
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
/// How much of the polyline `line` lies within `near` of any of `others`,
/// measured in steps of a quarter of `near`.
pub(crate) fn length_near(line: &[Point], others: &[Vec<Point>], near: f64) -> f64 {
    let step = near / 4.0;
    let mut total = 0.0;
    for pair in line.windows(2) {
        let (from, to) = (pair[0], pair[1]);
        let length = (to - from).length();
        let pieces = (length / step).ceil().max(1.0);
        for piece in 0..pieces as usize {
            let middle = from + (to - from) * ((piece as f64 + 0.5) / pieces);
            if others
                .iter()
                .any(|other| distance_to_polyline(middle, other) < near)
            {
                total += length / pieces;
            }
        }
    }
    total
}

/// The distance from `at` to the nearest point of the polyline `line`.
fn distance_to_polyline(at: Point, line: &[Point]) -> f64 {
    line.windows(2)
        .map(|pair| {
            let (from, to) = (pair[0], pair[1]);
            let along = to - from;
            let length = along.dot(along);
            let t = if length > 0.0 {
                ((at - from).dot(along) / length).clamp(0.0, 1.0)
            } else {
                0.0
            };
            (at - (from + along * t)).length()
        })
        .fold(f64::INFINITY, f64::min)
}

pub(crate) fn is_inside(polygon: &[Point], point: Point) -> bool {
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

    /// A leg of a line through a cell stays inside the cell, and nearly
    /// always one can be found from near its middle to near the middle of
    /// each of its sides; the four sides of `cell_sides` are the cell's own.
    #[test]
    fn a_leg_through_a_cell_stays_inside_it() {
        let (mut legs, mut missing) = (0, 0);
        for seed in 0..40 {
            let layout = layout(3, seed);
            let mut random = Random::from_seed(seed + 1000);
            for row in 0..3 {
                for column in 0..3 {
                    let outline: Vec<Point> = flatten(&layout.cell_outline(column, row, 1.0))
                        .remove(0)
                        .points;
                    for side in layout.cell_sides(column, row) {
                        assert!(layout.side_cells()[side].contains(&Some(row * 3 + column)));
                        let from = layout.near_center(column, row, &mut random);
                        let to = layout.side_middle(side, &mut random);
                        legs += 1;
                        let Some(leg) = layout.leg(column, row, from, to, &[], &mut random) else {
                            missing += 1;
                            continue;
                        };
                        let path = layout.path_of(&[leg], 1.0);
                        let points = flatten(&path).remove(0).points;
                        assert!((points[0] - from).length() < 1e-3);
                        assert!((*points.last().unwrap() - to).length() < 1e-3);
                        for point in points {
                            if (point - to).length() > CROSSING_SLACK * layout.cell_size() {
                                assert!(is_inside(&outline, point), "seed {seed}");
                            }
                        }
                    }
                }
            }
        }
        assert!(
            missing * 100 <= legs,
            "{missing} of {legs} legs found no way"
        );
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

    /// A side lies between the two cells whose outlines it is part of: each
    /// cell named beside it has the side's middle on its own boundary.
    #[test]
    fn a_side_lies_between_the_cells_it_names() {
        let layout = layout(3, 8);
        let (sides, cells) = (layout.side_paths(MM_PER_METER), layout.side_cells());
        let outlines = layout.cell_outlines(MM_PER_METER);
        assert_eq!(cells.len(), sides.len());
        // The edge of the square has one cell, a side within it two.
        assert_eq!(cells.iter().filter(|c| c.contains(&None)).count(), 4 * 3);
        for (side, pair) in sides.iter().zip(&cells) {
            let middle = side[side.len() / 2];
            for cell in pair.iter().flatten() {
                assert!(
                    outlines[*cell]
                        .iter()
                        .any(|c| (c.x - middle.x).abs() < 1e-9 && (c.y - middle.y).abs() < 1e-9),
                    "cell {cell} does not run along the side"
                );
            }
        }
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
