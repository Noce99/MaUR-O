//! Path handling: splitting into parts, flattening bezier curves, lengths
//! and tangents along a polyline, and parallel offsets. Ported from
//! `geometry.h`/`geometry.cpp`.

use crate::map::*;
use crate::qbezier::QBezier;

/// How much longer than its chord, in mm, a bezier segment may be before it
/// is split further while flattening a curve.
pub const BEZIER_ERROR: f64 = 0.005;
/// The maximum squared length, in mm, of a flattened bezier segment.
pub const BEZIER_SEGMENT_MAXLEN_SQUARED: f64 = 1.0;

fn distance(p: Point) -> f64 {
    p.x.hypot(p.y)
}

fn unit(v: Point) -> Point {
    v.normalized()
}

fn fuzzy_compare_f32(p1: f32, p2: f32) -> bool {
    (p1 - p2).abs() * 100_000.0 <= p1.abs().min(p2.abs())
}

/// A rectangle, mirroring `QRectF`'s `x, y, width, height` layout and its
/// "null" (all-zero) sentinel used as an empty-accumulator state.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub fn new(x: f64, y: f64, w: f64, h: f64) -> Rect {
        Rect { x, y, w, h }
    }
    pub fn from_ltrb(l: f64, t: f64, r: f64, b: f64) -> Rect {
        Rect { x: l, y: t, w: r - l, h: b - t }
    }
    pub fn left(&self) -> f64 { self.x }
    pub fn top(&self) -> f64 { self.y }
    pub fn right(&self) -> f64 { self.x + self.w }
    pub fn bottom(&self) -> f64 { self.y + self.h }
    pub fn width(&self) -> f64 { self.w }
    pub fn height(&self) -> f64 { self.h }
    pub fn is_null(&self) -> bool { self.w == 0.0 && self.h == 0.0 }

    pub fn set_left(&mut self, v: f64) { self.w = self.right() - v; self.x = v; }
    pub fn set_top(&mut self, v: f64) { self.h = self.bottom() - v; self.y = v; }
    pub fn set_right(&mut self, v: f64) { self.w = v - self.x; }
    pub fn set_bottom(&mut self, v: f64) { self.h = v - self.y; }

    pub fn adjusted(&self, dl: f64, dt: f64, dr: f64, db: f64) -> Rect {
        Rect::from_ltrb(self.left() + dl, self.top() + dt, self.right() + dr, self.bottom() + db)
    }

    pub fn united(&self, other: &Rect) -> Rect {
        if self.is_null() { return *other; }
        if other.is_null() { return *self; }
        Rect::from_ltrb(
            self.left().min(other.left()),
            self.top().min(other.top()),
            self.right().max(other.right()),
            self.bottom().max(other.bottom()),
        )
    }
}

/// `qRound`: a whole number, halves rounded **up**, i.e. toward positive
/// infinity, which is not what Rust's own `round` does.
///
/// Qt spells it as a branch on the sign, and the negative branch comes out at
/// `floor(d + 0.5)` rather than at the away-from-zero rounding it looks like:
/// `qRound(-1237.5)` is `-1237`, where `(-1237.5f64).round()` is `-1238`. It
/// matters wherever a shape is quantized to the whole 1/1000 mm a map
/// coordinate holds: a shape centred on the origin has its two halves rounded
/// in opposite directions, so the whole shape comes out a unit wider under
/// the wrong rule.
pub fn qround(value: f64) -> f64 {
    (value + 0.5).floor()
}

fn rect_include(rect: &mut Rect, point: Point) {
    if point.x < rect.left() { rect.set_left(point.x); }
    else if point.x > rect.right() { rect.set_right(point.x); }
    if point.y < rect.top() { rect.set_top(point.y); }
    else if point.y > rect.bottom() { rect.set_bottom(point.y); }
}

/// A drawing command, mirroring the subset of `QPainterPath`'s element
/// stream this project needs. Kept as our own IR (rather than building a
/// `tiny_skia::Path` directly) so `connect_path`/`control_point_rect` can be
/// implemented exactly like `QPainterPath`'s, and so this module stays
/// independent of the rendering backend.
#[derive(Clone, Copy, Debug)]
pub enum PathCommand {
    MoveTo(Point),
    LineTo(Point),
    CubicTo(Point, Point, Point),
    Close,
}

#[derive(Clone, Debug, Default)]
pub struct Path {
    pub commands: Vec<PathCommand>,
}

impl Path {
    pub fn new() -> Path { Path::default() }
    pub fn is_empty(&self) -> bool { self.commands.is_empty() }
    pub fn move_to(&mut self, p: Point) { self.commands.push(PathCommand::MoveTo(p)); }
    pub fn line_to(&mut self, p: Point) { self.commands.push(PathCommand::LineTo(p)); }
    pub fn cubic_to(&mut self, c1: Point, c2: Point, end: Point) {
        self.commands.push(PathCommand::CubicTo(c1, c2, end));
    }
    pub fn close_subpath(&mut self) { self.commands.push(PathCommand::Close); }

    /// Appends another path's subpaths as new subpaths, like
    /// `QPainterPath::addPath`.
    pub fn add_path(&mut self, other: &Path) {
        self.commands.extend_from_slice(&other.commands);
    }

    /// Appends another path's drawing commands directly onto the current
    /// subpath (dropping its leading `MoveTo`), connecting them instead of
    /// jumping — `QPainterPath::connectPath`.
    pub fn connect_path(&mut self, other: &Path) {
        for (i, cmd) in other.commands.iter().enumerate() {
            if i == 0 {
                if let PathCommand::MoveTo(_) = cmd { continue; }
            }
            self.commands.push(*cmd);
        }
    }

    /// The bounding box of every point and control point in the path, like
    /// `QPainterPath::controlPointRect()`. For the one shape this crate
    /// measures without an explicit override (a circle, via `add_ellipse`),
    /// this coincides exactly with the tight geometric bounds.
    pub fn control_point_rect(&self) -> Rect {
        let mut first = true;
        let (mut xmin, mut xmax, mut ymin, mut ymax) = (0.0, 0.0, 0.0, 0.0);
        let mut consider = |p: Point| {
            if first {
                xmin = p.x; xmax = p.x; ymin = p.y; ymax = p.y;
                first = false;
            } else {
                xmin = xmin.min(p.x); xmax = xmax.max(p.x);
                ymin = ymin.min(p.y); ymax = ymax.max(p.y);
            }
        };
        for cmd in &self.commands {
            match *cmd {
                PathCommand::MoveTo(p) | PathCommand::LineTo(p) => consider(p),
                PathCommand::CubicTo(c1, c2, end) => { consider(c1); consider(c2); consider(end); }
                PathCommand::Close => {}
            }
        }
        if first { Rect::default() } else { Rect::from_ltrb(xmin, ymin, xmax, ymax) }
    }
}

/// Appends a circle/ellipse, via the standard 4-arc kappa approximation
/// (the same constant Qt's own `addCircle` uses), like
/// `QPainterPath::addEllipse`.
pub fn add_ellipse(path: &mut Path, center: Point, rx: f64, ry: f64) {
    const K: f64 = 0.5522847498;
    let (cx, cy) = (center.x, center.y);
    path.move_to(Point::new(cx + rx, cy));
    path.cubic_to(Point::new(cx + rx, cy + ry * K), Point::new(cx + rx * K, cy + ry), Point::new(cx, cy + ry));
    path.cubic_to(Point::new(cx - rx * K, cy + ry), Point::new(cx - rx, cy + ry * K), Point::new(cx - rx, cy));
    path.cubic_to(Point::new(cx - rx, cy - ry * K), Point::new(cx - rx * K, cy - ry), Point::new(cx, cy - ry));
    path.cubic_to(Point::new(cx + rx * K, cy - ry), Point::new(cx + rx, cy - ry * K), Point::new(cx + rx, cy));
    path.close_subpath();
}

/// The index range of one part of a coordinate list.
struct PartRange {
    begin: usize,
    end: usize,
    closed: bool,
}

/// Splits a coordinate list at the hole points.
fn part_ranges(coords: &CoordList) -> Vec<PartRange> {
    let mut parts = Vec::new();
    let mut begin = 0usize;
    for i in 0..coords.len() {
        if coords[i].is_hole_point() || i + 1 == coords.len() {
            if i + 1 - begin >= 2 {
                parts.push(PartRange { begin, end: i + 1, closed: coords[i].is_close_point() });
            }
            begin = i + 1;
        }
    }
    parts
}

/// Splits a bezier curve at parameter `p`, returning the two inner control
/// points of the first section, the split position, and the two inner
/// control points of the second section — `(o0, o1, o2, o3, o4)`.
fn split_bezier(c0: Point, c1: Point, c2: Point, c3: Point, p: f32) -> (Point, Point, Point, Point, Point) {
    if p >= 1.0 {
        (c1, c2, c3, c3, c3)
    } else if p <= 0.0 {
        (c0, c0, c0, c1, c2)
    } else {
        let pf = p as f64;
        let c12 = c1 + (c2 - c1) * pf;
        let o0 = c0 + (c1 - c0) * pf;
        let tmp_o1 = o0 + (c12 - o0) * pf;
        let o4 = c2 + (c3 - c2) * pf;
        let o3 = c12 + (o4 - c12) * pf;
        let o2 = tmp_o1 + (o3 - tmp_o1) * pf;
        (o0, tmp_o1, o2, o3, o4)
    }
}

/// Appends a flattened cubic bezier curve, excluding both of its end
/// points. A segment which is short enough and close enough to its chord
/// contributes the midpoint of its two inner control points, rather than a
/// point on the curve — this is what Mapper does, and dash/symbol placement
/// follow the resulting polyline, so the vertices have to be the same ones.
fn flatten_cubic(out: &mut Vec<Point>, out_params: &mut Vec<f32>, c0: Point, c1: Point, c2: Point, c3: Point, p0: f32, p1: f32, depth: i32) {
    flatten_cubic_tol(out, out_params, c0, c1, c2, c3, p0, p1, depth, BEZIER_ERROR);
}

/// Same recursive flattening as [`flatten_cubic`], but with the chord/arc
/// error tolerance passed in rather than fixed at [`BEZIER_ERROR`].
///
/// [`BEZIER_ERROR`] (0.005mm) is tuned for *visual* fidelity: it is the
/// tolerance Mapper itself flattens curves to for stroking, dashing and
/// symbol placement, and matching it exactly is what this port targets
/// there. But `Path::contains_even_odd` (see below) uses a flattened
/// polygon only as a stand-in for `QPainterPath::contains`, which tests the
/// *true* cubic curve (Qt keeps `cubicTo` segments in the path rather than
/// pre-flattening it) -- a pattern point can legitimately sit a few microns
/// inside a curved area boundary, well within Mapper's 0.005mm visual
/// tolerance but on the wrong side of a polygon flattened to it. Hit-testing
/// isn't performance sensitive the way per-frame flattening is, so it uses a
/// much tighter tolerance here instead.
fn flatten_cubic_tol(out: &mut Vec<Point>, out_params: &mut Vec<f32>, c0: Point, c1: Point, c2: Point, c3: Point, p0: f32, p1: f32, depth: i32, error: f64) {
    let p_half = ((p0 as f64 + p1 as f64) * 0.5) as f32;
    let c12 = (c1 + c2) / 2.0;

    let inner = c3 - c0;
    let inner_length_squared = inner.dot(inner);
    if depth >= 48
        || (inner_length_squared <= BEZIER_SEGMENT_MAXLEN_SQUARED
            && distance(c1 - c0) + distance(c2 - c1) + distance(c3 - c2) - inner_length_squared.sqrt() <= error)
    {
        out.push(c12);
        out_params.push(p_half);
        return;
    }

    let c01 = (c0 + c1) / 2.0;
    let c23 = (c2 + c3) / 2.0;
    let c012 = (c01 + c12) / 2.0;
    let c123 = (c12 + c23) / 2.0;
    let c0123 = (c012 + c123) / 2.0;

    flatten_cubic_tol(out, out_params, c0, c01, c012, c0123, p0, p_half, depth + 1, error);
    flatten_cubic_tol(out, out_params, c0123, c123, c23, c3, p_half, p1, depth + 1, error);
}

/// One connected part of a path: a polyline, plus the length of each vertex
/// measured along the polyline from its start.
pub struct PathPart {
    pub points: Vec<Point>,
    /// Cumulative length at each point, in mm. Accumulated in single
    /// precision, as Mapper measures paths in floats, then widened; the
    /// rounding decides which side of a tie a dash layout falls on.
    pub lengths: Vec<f64>,
    /// Vertices which restart the dash pattern.
    pub dash_points: Vec<bool>,
    /// Interpolated curve points, as opposed to vertices of the path.
    pub curve_points: Vec<bool>,
    /// For a vertex its coordinate index, for a curve point the index of
    /// the curve start.
    pub coord_index: Vec<usize>,
    /// Position of a curve point on its curve, 0 at the vertices.
    pub params: Vec<f32>,
    /// The coordinate flags at each vertex, 0 at curve points.
    pub point_flags: Vec<i32>,
    pub first_coord: usize,
    pub last_coord: usize,
    pub closed: bool,
}

fn upper_bound(lengths: &[f64], position: f64) -> usize {
    lengths.partition_point(|&x| x <= position)
}

impl PathPart {
    /// The total length of the part, in mm.
    pub fn length(&self) -> f64 {
        self.lengths.last().copied().unwrap_or(0.0)
    }

    /// Returns the position at the given length along the part.
    pub fn point_at(&self, position: f64) -> Point {
        if self.points.is_empty() { return Point::ZERO; }
        if position <= 0.0 || self.points.len() == 1 { return self.points[0]; }
        if position >= *self.lengths.last().unwrap() { return *self.points.last().unwrap(); }

        let i = upper_bound(&self.lengths, position);
        let segment_length = self.lengths[i] - self.lengths[i - 1];
        if segment_length <= 0.0 { return self.points[i]; }

        let t = (position - self.lengths[i - 1]) / segment_length;
        self.points[i - 1] * (1.0 - t) + self.points[i] * t
    }

    /// Returns the unit tangent at the given length along the part.
    pub fn tangent_at(&self, position: f64) -> Point {
        if self.points.len() < 2 { return Point::new(1.0, 0.0); }
        let mut i = 1usize;
        if position > 0.0 {
            i = upper_bound(&self.lengths, position).max(1).min(self.points.len() - 1);
        }
        let delta = self.points[i] - self.points[i - 1];
        let norm = distance(delta);
        if norm > 0.0 { delta / norm } else { Point::new(1.0, 0.0) }
    }

    /// Returns the polyline between two lengths along the part.
    pub fn slice(&self, from: f64, to: f64) -> Vec<Point> {
        let mut result = Vec::new();
        if self.points.is_empty() || to <= from { return result; }
        let from = from.max(0.0);
        let to = to.min(*self.lengths.last().unwrap());
        if to <= from { return result; }

        result.push(self.point_at(from));
        for i in 0..self.points.len() {
            if self.lengths[i] > from && self.lengths[i] < to {
                result.push(self.points[i]);
            }
        }
        result.push(self.point_at(to));
        result
    }
}

/// Splits a coordinate list into its parts and flattens all curves.
pub fn flatten(coords: &CoordList) -> Vec<PathPart> {
    let mut parts = Vec::new();
    for range in part_ranges(coords) {
        let mut part = PathPart {
            points: Vec::new(),
            lengths: Vec::new(),
            dash_points: Vec::new(),
            curve_points: Vec::new(),
            coord_index: Vec::new(),
            params: Vec::new(),
            point_flags: Vec::new(),
            first_coord: range.begin,
            last_coord: range.end - 1,
            closed: range.closed,
        };
        part.points.push(coords[range.begin].pos());
        part.dash_points.push(coords[range.begin].is_dash_point());
        part.curve_points.push(false);
        part.coord_index.push(range.begin);
        part.params.push(0.0);
        part.point_flags.push(coords[range.begin].flags);

        let mut i = range.begin;
        while i + 1 < range.end {
            let previous_size = part.points.len();
            if coords[i].is_curve_start() && i + 3 < range.end {
                flatten_cubic(&mut part.points, &mut part.params,
                    coords[i].pos(), coords[i + 1].pos(), coords[i + 2].pos(), coords[i + 3].pos(),
                    0.0, 1.0, 0);
                part.curve_points.resize(part.points.len(), true);
                part.coord_index.resize(part.points.len(), i);
                part.point_flags.resize(part.points.len(), 0);
                i += 3;
                part.points.push(coords[i].pos());
                part.params.push(0.0);
            } else {
                part.points.push(coords[i + 1].pos());
                part.params.push(0.0);
                i += 1;
            }
            part.curve_points.resize(part.points.len(), false);
            part.coord_index.resize(part.points.len(), i);
            part.point_flags.resize(part.points.len(), coords[i].flags);
            part.dash_points.resize(part.points.len(), false);
            if part.points.len() > previous_size {
                let last = part.dash_points.len() - 1;
                part.dash_points[last] = coords[i].is_dash_point();
            }
        }

        part.lengths.reserve(part.points.len());
        part.lengths.push(0.0);
        let mut clen: f32 = 0.0;
        for i in 1..part.points.len() {
            clen += distance(part.points[i] - part.points[i - 1]) as f32;
            part.lengths.push(clen as f64);
        }

        if part.points.len() >= 2 {
            parts.push(part);
        }
    }
    parts
}

/// Below this dot product between two consecutive edge directions, a vertex
/// folds back on itself closely enough to count as a cusp: matches
/// tiny_skia's own `AngleType::Nearly180` threshold (`(1.0 + dot)
/// .is_nearly_zero()` at `1.0 / 4096.0`), since that is the failure mode
/// this constant exists to detect (see [`fold_kind`]).
const NEARLY_REVERSED_DOT: f64 = -1.0 + 1.0 / 4096.0;

/// The circumradius of the circle through three points, or `f64::INFINITY`
/// for (near-)collinear ones, which have none.
fn circumradius(a: Point, b: Point, c: Point) -> f64 {
    let ab = distance(b - a);
    let bc = distance(c - b);
    let ca = distance(a - c);
    let area2 = ((b - a).x * (c - a).y - (b - a).y * (c - a).x).abs();
    if area2 < 1e-12 { f64::INFINITY } else { ab * bc * ca / (2.0 * area2) }
}

/// What kind of sharp fold, if any, stroking the given flattened parts at
/// the given half width can hit tiny_skia's `LineJoin::MiterClip` failure
/// mode with.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FoldKind {
    /// No vertex folds back sharply enough for `MiterClip` to misbehave.
    None,
    /// Every fold found is an exact (bit-for-bit) direction reversal at a
    /// shared vertex -- as when a path is closed by repeating its first
    /// coordinate as its last, so the closing edge doubles back on the
    /// opening one precisely. `MiterClip`'s clip-point formula divides by
    /// the sine of the half-angle between the two edges, which is exactly
    /// zero here rather than merely close to it; tiny_skia special-cases
    /// that (unlike the near-zero case below) and produces the short
    /// rectangular stub Mapper's own miter join draws past the vertex, so
    /// this case does not need the fallback the other one does.
    ExactReversal,
    /// At least one fold is a near-but-not-exact reversal, or a bend along
    /// a curve tighter than the pen is wide -- both make `MiterClip`
    /// compute a clip point via a formula that divides by a (near) zero
    /// that is not exactly zero, and spikes wildly off to one side.
    Dangerous,
}

/// Whether stroking the given flattened parts at the given half width can
/// hit tiny_skia's `LineJoin::MiterClip` failure mode: either a cusp (an
/// interior vertex whose incoming and outgoing edges point in very nearly
/// opposite directions), or a bend along a curve tighter than the pen is
/// wide.
///
/// A line offset sideways by more than its local radius of curvature (as a
/// border is, off its main line) folds onto itself; on a straight polyline
/// that shows up directly as a cusp, but a curve making of several bezier
/// segments spreads the same fold across the join between two of them --
/// which is a real vertex of the path, not one of the interpolated points
/// within a segment, but still surrounded by curve on at least one side.
/// Either way the circumradius through the point and its two neighbors (the
/// radius of curvature it approximates) comes out below the half width
/// doing the offending offsetting; a plain sharp vertex -- like a star's
/// point, where curvature is not the issue -- has no curve segment on
/// either side, so checking it only next to one does not misfire there.
///
/// Either failure mode makes `MiterClip` -- otherwise the right match for
/// Mapper's own miter join, see `TINY_SKIA_MITER_LIMIT` in renderer.rs --
/// spike wildly off to one side in `Dangerous` cases; callers fall back to
/// a plain, spike-free `LineJoin::Miter` there. An `ExactReversal`, though,
/// is what `MiterClip` gets right and a plain miter falls short on -- a
/// plain miter draws no protrusion past the vertex at all, where Mapper
/// draws a short one -- so callers keep `MiterClip` for it.
pub fn fold_kind(coords: &CoordList, parts: &[PathPart], half_width: f64) -> FoldKind {
    let is_fold = |prev: Point, next: Point| {
        let prev = prev.normalized();
        let next = next.normalized();
        prev.dot(next) < NEARLY_REVERSED_DOT
    };
    // Exact bit-for-bit opposites add to exactly zero in IEEE754, with no
    // rounding either from the subtraction that built them (b - a is
    // exactly -(a - b) for finite doubles) or from this addition.
    let is_exact_reversal = |prev: Point, next: Point| prev + next == Point::ZERO;
    // The direction of the edge arriving at flattened vertex `to` from
    // `from`, and the one leaving a vertex towards its neighbor. A short
    // curve can flatten to a single interior point -- too coarse a chord to
    // tell a near-total reversal at the curve's own end from a merely sharp
    // one -- so where that edge is (part of) a curve, its exact tangent is
    // taken from the curve's own control points instead: `coord_index` gives
    // the original coordinate index of a true vertex, and a curve occupies
    // four consecutive coordinates (start, two controls, end), so the
    // control point next to either end sits right beside it in `coords`.
    let incoming = |part: &PathPart, from: usize, to: usize| -> Point {
        // `to` must be a true vertex for its `coord_index` to be its own --
        // an interior point of a curve subdivided into more than one
        // segment carries its curve's start index instead (see
        // `PathPart::coord_index`), which isn't useful here.
        if part.curve_points[from] && !part.curve_points[to] {
            let end = part.coord_index[to];
            coords[end].pos() - coords[end - 1].pos()
        } else {
            part.points[to] - part.points[from]
        }
    };
    let outgoing = |part: &PathPart, from: usize, to: usize| -> Point {
        if part.curve_points[to] && !part.curve_points[from] {
            let start = part.coord_index[from];
            coords[start + 1].pos() - coords[start].pos()
        } else {
            part.points[to] - part.points[from]
        }
    };
    let check = |prev: Point, next: Point| -> FoldKind {
        if is_exact_reversal(prev, next) {
            FoldKind::ExactReversal
        } else if is_fold(prev, next) {
            FoldKind::Dangerous
        } else {
            FoldKind::None
        }
    };
    // `Dangerous` always wins over `ExactReversal`, which always wins over
    // `None`, so the worst kind found anywhere in the path is the answer.
    let worse = |a: FoldKind, b: FoldKind| if a == FoldKind::Dangerous || b == FoldKind::Dangerous {
        FoldKind::Dangerous
    } else if a == FoldKind::ExactReversal || b == FoldKind::ExactReversal {
        FoldKind::ExactReversal
    } else {
        FoldKind::None
    };
    let mut found = FoldKind::None;
    for part in parts {
        let points = &part.points;
        let curve = &part.curve_points;
        let n = points.len();
        if n < 3 { continue; }
        for i in 1..n - 1 {
            found = worse(found, check(incoming(part, i - 1, i), outgoing(part, i, i + 1)));
            if (curve[i] || curve[i - 1] || curve[i + 1])
                && circumradius(points[i - 1], points[i], points[i + 1]) < half_width {
                found = FoldKind::Dangerous;
            }
        }
        if part.closed && points[0] == points[n - 1] && n >= 3 {
            found = worse(found, check(incoming(part, n - 2, n - 1), outgoing(part, 0, 1)));
        }
    }
    found
}

/// Converts a coordinate list to a path, preserving bezier curves.
///
/// With `honor_gaps`, the sections between two gap points are left out, as
/// they are when a line is drawn; without, gap flags are ignored, as they
/// are when an area is filled.
pub fn to_painter_path(coords: &CoordList, honor_gaps: bool) -> Path {
    to_painter_path_impl(coords, honor_gaps, false)
}

/// Like [`to_painter_path`], but flattens curves into line segments instead
/// of keeping them as `cubic_to` commands.
///
/// tiny_skia's own curve-aware stroker can leave a small gap unfilled at the
/// centre of a closed loop stroked wider than its own curve radius -- the
/// same geometry [`fold_kind`] already watches for -- where its
/// straight-segment stroker (used once a curve is flattened first) handles
/// the identical shape correctly. Callers fall back to this only where that
/// can occur, since flattening ahead of time gives up the adaptive
/// flattening tiny_skia would otherwise do at the output resolution.
pub fn to_flattened_painter_path(coords: &CoordList, honor_gaps: bool) -> Path {
    to_painter_path_impl(coords, honor_gaps, true)
}

fn to_painter_path_impl(coords: &CoordList, honor_gaps: bool, flatten_curves: bool) -> Path {
    let mut path = Path::new();
    for range in part_ranges(coords) {
        let last = range.end - 1;

        let mut part_path = Path::new();
        let mut first_subpath: Option<Path> = None;

        let mut gap = honor_gaps && coords[range.begin].is_gap_point();
        let mut hole = false;
        part_path.move_to(coords[range.begin].pos());

        let mut i = range.begin + 1;
        while i <= last {
            if gap {
                if coords[i].is_hole_point() {
                    gap = false;
                    hole = true;
                } else if coords[i].is_gap_point() {
                    gap = false;
                    if first_subpath.is_none() && range.closed {
                        first_subpath = Some(std::mem::take(&mut part_path));
                    }
                    part_path.move_to(coords[i].pos());
                }
                i += 1;
                continue;
            }

            if hole {
                if first_subpath.is_none() && range.closed {
                    first_subpath = Some(std::mem::take(&mut part_path));
                }
                part_path.move_to(coords[i].pos());
                hole = false;
                i += 1;
                continue;
            }

            if coords[i - 1].is_curve_start() && i + 2 <= last {
                if flatten_curves {
                    let mut pts = Vec::new();
                    let mut params = Vec::new();
                    flatten_cubic(&mut pts, &mut params,
                        coords[i - 1].pos(), coords[i].pos(), coords[i + 1].pos(), coords[i + 2].pos(),
                        0.0, 1.0, 0);
                    for p in pts {
                        part_path.line_to(p);
                    }
                    part_path.line_to(coords[i + 2].pos());
                } else {
                    part_path.cubic_to(coords[i].pos(), coords[i + 1].pos(), coords[i + 2].pos());
                }
                i += 2;
            } else {
                part_path.line_to(coords[i].pos());
            }

            if honor_gaps && coords[i].is_hole_point() {
                hole = true;
            } else if honor_gaps && coords[i].is_gap_point() {
                gap = true;
            }
            i += 1;
        }

        if range.closed {
            match &first_subpath {
                None => part_path.close_subpath(),
                Some(fs) => part_path.connect_path(fs),
            }
        }
        path.add_path(&part_path);
    }
    path
}

/// Converts a polyline to a path.
pub fn polyline_to_path(polyline: &[Point], closed: bool) -> Path {
    let mut path = Path::new();
    if polyline.is_empty() { return path; }
    path.move_to(polyline[0]);
    for &p in &polyline[1..] {
        path.line_to(p);
    }
    if closed { path.close_subpath(); }
    path
}

/// Below this squared length a segment has no usable direction.
const TANGENT_EPSILON_SQUARED: f64 = 0.000625; // about 0.025 mm
/// The pen miter limit used by Mapper, in units of half the pen width.
const STROKE_MITER_LIMIT: f64 = 1.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PenCap { Flat, Square, Round }
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PenJoin { Miter, Bevel, Round }

/// The incoming direction at a coordinate, skipping degenerate segments.
pub fn coord_incoming_tangent(coords: &CoordList, first: usize, last: usize, closed: bool, i: usize) -> Option<Point> {
    let mut k = i;
    while k > first {
        k -= 1;
        let tangent = coords[i].pos() - coords[k].pos();
        if tangent.dot(tangent) > TANGENT_EPSILON_SQUARED { return Some(tangent); }
    }
    if closed && last > i + 1 {
        let mut k = last;
        while k > i {
            let tangent = coords[i].pos() - coords[k].pos();
            if tangent.dot(tangent) > TANGENT_EPSILON_SQUARED { return Some(tangent); }
            k -= 1;
        }
    }
    None
}

/// The outgoing direction at a coordinate, skipping degenerate segments.
pub fn coord_outgoing_tangent(coords: &CoordList, first: usize, last: usize, closed: bool, i: usize) -> Option<Point> {
    let mut k = i + 1;
    while k <= last {
        let tangent = coords[k].pos() - coords[i].pos();
        if tangent.dot(tangent) > TANGENT_EPSILON_SQUARED { return Some(tangent); }
        k += 1;
    }
    if closed {
        let mut k = first;
        while k < i {
            let tangent = coords[k].pos() - coords[i].pos();
            if tangent.dot(tangent) > TANGENT_EPSILON_SQUARED { return Some(tangent); }
            k += 1;
        }
    }
    None
}

/// The combined tangent Mapper uses for the direction of a cap: the sum of
/// the normalized incoming and outgoing directions where both exist.
fn combined_tangent(coords: &CoordList, first: usize, last: usize, closed: bool, i: usize) -> Point {
    let to_coord = coord_incoming_tangent(coords, first, last, closed, i);
    let to_next = coord_outgoing_tangent(coords, first, last, closed, i);
    match to_next {
        None => to_coord.unwrap_or(Point::ZERO),
        Some(tn) => match to_coord {
            Some(tc) => tc.with_length(1.0) + tn.with_length(1.0),
            None => tn,
        },
    }
}

/// Adds the cap at one end of a stroked path to the extent.
fn extent_include_cap(extent: &mut Rect, coord: Point, tangent: Point, half_width: f64, cap: PenCap, end_cap: bool) {
    if half_width < 0.0005 {
        rect_include(extent, coord);
        return;
    }
    if cap == PenCap::Round {
        rect_include(extent, coord - Point::new(half_width, half_width));
        rect_include(extent, coord + Point::new(half_width, half_width));
        return;
    }

    let right = tangent.perp_right_unit();
    rect_include(extent, coord + right * half_width);
    rect_include(extent, coord - right * half_width);

    if cap == PenCap::Square {
        let mut back = right.perp_right_unit();
        if end_cap { back = -back; }
        rect_include(extent, coord + (back - right) * half_width);
        rect_include(extent, coord + (back + right) * half_width);
    }
}

/// Adds the join at an interior vertex of a stroked path to the extent.
fn extent_include_join(extent: &mut Rect, coord: Point, incoming: Point, outgoing: Point, half_width: f64, join: PenJoin) {
    if half_width < 0.0005 {
        rect_include(extent, coord);
        return;
    }
    if join == PenJoin::Round {
        rect_include(extent, coord - Point::new(half_width, half_width));
        rect_include(extent, coord + Point::new(half_width, half_width));
        return;
    }

    let r0 = incoming.perp_right_unit() * half_width;
    let r1 = outgoing.perp_right_unit() * half_width;
    let coord_rhs = coord + r0;
    let coord_lhs = coord - r0;
    let next_rhs = coord + r1;
    let next_lhs = coord - r1;
    if join == PenJoin::Bevel {
        rect_include(extent, coord_rhs);
        rect_include(extent, coord_lhs);
        rect_include(extent, next_rhs);
        rect_include(extent, next_lhs);
        return;
    }

    let limit = 2.0 * half_width * STROKE_MITER_LIMIT;
    let to_coord = incoming.with_length(limit);
    let to_next = outgoing.with_length(limit);

    let scaling = to_coord.y * to_next.x - to_coord.x * to_next.y;
    if scaling == 0.0 || !scaling.is_finite() {
        if to_coord == -to_next {
            rect_include(extent, coord + to_coord.with_length(2.0 * half_width));
        }
        return;
    }

    let boundary = |extent: &mut Rect, from: Point, to: Point| {
        let p = from - to;
        let factor = (to_next.y * p.x - to_next.x * p.y) / scaling;
        if factor > 1.0 {
            rect_include(extent, from + to_coord);
            rect_include(extent, to - to_next);
        } else if factor > 0.0 {
            rect_include(extent, from + to_coord * factor);
        } else {
            rect_include(extent, from);
            rect_include(extent, to);
        }
    };
    boundary(extent, coord_rhs, next_rhs);
    boundary(extent, coord_lhs, next_lhs);
}

/// The extent of one stroked part. Shared by the coordinate-list and
/// polyline overloads by wrapping a plain polyline in zero-flag `Coord`s
/// (`NoFlags` in the original never sets any flag anyway, so a `Coord` with
/// `flags: 0` behaves identically for every check used here).
fn stroked_part_extent(coords: &CoordList, first: usize, last: usize, closed: bool, half_width: f64, cap: PenCap, join: PenJoin) -> Rect {
    let join_at = |extent: &mut Rect, i: usize| {
        let to_coord = coord_incoming_tangent(coords, first, last, closed, i);
        let to_next = coord_outgoing_tangent(coords, first, last, closed, i);
        let (tc, tn) = match (to_coord, to_next) {
            (None, None) => return,
            (Some(tc), None) => (tc, tc),
            (None, Some(tn)) => (tn, tn),
            (Some(tc), Some(tn)) => (tc, tn),
        };
        extent_include_join(extent, coords[i].pos(), tc, tn, half_width, join);
    };

    let p_first = coords[first].pos();
    let mut extent = Rect::new(p_first.x, p_first.y, 0.0001, 0.0001);
    let mut gap = coords[first].is_gap_point();
    let mut hole = false;
    extent_include_cap(&mut extent, p_first, combined_tangent(coords, first, last, closed, first), half_width, cap, false);

    let mut i = first + 1;
    while i <= last {
        if gap {
            if coords[i].is_hole_point() {
                gap = false;
                hole = true;
            } else if coords[i].is_gap_point() {
                gap = false;
                extent_include_cap(&mut extent, coords[i].pos(), combined_tangent(coords, first, last, closed, i), half_width, cap, false);
            }
            i += 1;
            continue;
        }
        if hole {
            extent_include_cap(&mut extent, coords[i].pos(), combined_tangent(coords, first, last, closed, i), half_width, cap, false);
            hole = false;
            i += 1;
            continue;
        }

        if coords[i - 1].is_curve_start() && i + 2 <= last {
            i += 2;
        }

        if coords[i].is_hole_point() { hole = true; }
        else if coords[i].is_gap_point() { gap = true; }

        if (i < last && !hole && !gap) || (i == last && closed) {
            join_at(&mut extent, i);
        } else {
            extent_include_cap(&mut extent, coords[i].pos(), combined_tangent(coords, first, last, closed, i), half_width, cap, true);
        }
        i += 1;
    }
    extent
}

/// The extent of a coordinate list stroked with the given pen, following
/// Mapper's own rules: a flat cap adds nothing along the line, a miter join
/// reaches beyond the vertex up to the miter limit, and along the interior
/// of a bezier curve the pen only extends perpendicular to the flattened
/// polyline. The parts must be the result of `flatten()` on the same
/// coordinates.
pub fn stroked_path_extent(coords: &CoordList, parts: &[PathPart], half_width: f64, cap: PenCap, join: PenJoin) -> Rect {
    let mut extent = Rect::default();
    for (part_index, range) in part_ranges(coords).into_iter().enumerate() {
        let mut part_extent = stroked_part_extent(coords, range.begin, range.end - 1, range.closed, half_width, cap, join);

        if let Some(part) = parts.get(part_index) {
            for i in 1..part.points.len() - 1 {
                if !part.curve_points[i] { continue; }
                let pos = part.points[i];
                let to_coord = (pos - part.points[i - 1]).with_length(1.0);
                let to_next = (part.points[i + 1] - pos).with_length(1.0);
                let mut right = Point::new(-(to_coord.y + to_next.y), to_coord.x + to_next.x);
                right = right.with_length(half_width);
                rect_include(&mut part_extent, pos + right);
                rect_include(&mut part_extent, pos - right);
            }
        }

        extent = if extent.is_null() { part_extent } else { extent.united(&part_extent) };
    }
    extent
}

/// The extent of a plain polyline stroked with the given pen, as above.
pub fn stroked_polyline_extent(polygon: &[Point], half_width: f64, cap: PenCap, join: PenJoin) -> Rect {
    let count = polygon.len();
    if count == 0 { return Rect::default(); }
    if count == 1 { return Rect::new(polygon[0].x, polygon[0].y, 0.0001, 0.0001); }
    let closed = polygon[0] == polygon[count - 1];
    let coords: CoordList = polygon.iter().map(|p| Coord::new(p.x, p.y, 0)).collect();
    stroked_part_extent(&coords, 0, count - 1, closed, half_width, cap, join)
}

/// The bounding box of the flattened parts of a path.
pub fn flattened_extent(parts: &[PathPart]) -> Rect {
    let mut extent = Rect::default();
    for part in parts {
        if part.points.is_empty() { continue; }
        let mut part_extent = Rect::new(part.points[0].x, part.points[0].y, 0.0001, 0.0001);
        for &p in &part.points { rect_include(&mut part_extent, p); }
        extent = if extent.is_null() { part_extent } else { extent.united(&part_extent) };
    }
    extent
}

/// A position along a part, resolved to the underlying coordinates.
struct Split {
    /// Index of the flattened point at or after the position.
    upper: usize,
    /// Parameter on the curve, 0 at a node.
    param: f32,
    pos: Point,
    /// Coordinate index of the edge carrying the position.
    edge: usize,
    /// The position is on a bezier curve.
    on_curve: bool,
    /// The position is exactly a vertex of the path.
    at_node: bool,
}

fn split_at(coords: &CoordList, part: &PathPart, length: f64) -> Split {
    let size = part.points.len();
    let set_at_point = |index: usize| -> Split {
        Split {
            upper: index,
            pos: part.points[index],
            param: part.params[index],
            edge: part.coord_index[index],
            on_curve: part.params[index] != 0.0,
            at_node: part.params[index] == 0.0,
        }
    };
    if size == 0 {
        return Split { upper: 0, param: 0.0, pos: Point::ZERO, edge: 0, on_curve: false, at_node: true };
    }
    if length <= 0.0 || size == 1 {
        return set_at_point(0);
    }
    if length >= *part.lengths.last().unwrap() {
        return set_at_point(size - 1);
    }

    let mut index = part.lengths.partition_point(|&x| x < length);
    if index < 1 { index = 1; }
    let prev = index - 1;
    let segment_length = (part.lengths[index] - part.lengths[prev]) as f32;

    if fuzzy_compare_f32(1.0 + length as f32, 1.0 + part.lengths[index] as f32)
        || fuzzy_compare_f32(1.0 + segment_length, 1.0)
    {
        return set_at_point(index);
    }

    let edge = part.coord_index[prev];
    let is_curve = coords[edge].is_curve_start() && edge + 3 <= part.last_coord;

    let mut factor = ((length - part.lengths[prev]) / segment_length as f64) as f32;
    factor = factor.min(1.0).max(0.0);

    if is_curve {
        let prev_param = part.params[prev];
        let mut current_param = part.params[index];
        if current_param == 0.0 { current_param = 1.0; }
        let param = prev_param + (current_param - prev_param) * factor;
        if param >= 1.0 {
            return set_at_point(index);
        }
        let (_o0, _o1, o2, _o3, _o4) = split_bezier(coords[edge].pos(), coords[edge + 1].pos(), coords[edge + 2].pos(), coords[edge + 3].pos(), param);
        Split { upper: index, param, pos: o2, edge, on_curve: true, at_node: false }
    } else {
        let pos = part.points[prev] + (part.points[index] - part.points[prev]) * factor as f64;
        Split { upper: index, param: 0.0, pos, edge, on_curve: false, at_node: false }
    }
}

pub struct PathLocation {
    pub pos: Point,
    /// Not normalized; null if the path has no direction.
    pub tangent: Point,
}

/// Returns the position and tangent at the given length along the part.
///
/// Unlike `PathPart::point_at()`, a position on a bezier curve is
/// evaluated on the curve itself, not on the flattened polyline; this is
/// how Mapper places symbols along a line.
pub fn locate_on_path(coords: &CoordList, part: &PathPart, length: f64) -> PathLocation {
    if part.points.is_empty() {
        return PathLocation { pos: Point::ZERO, tangent: Point::ZERO };
    }

    let split = split_at(coords, part, length);
    let pos = split.pos;

    const EPS2: f64 = 0.000625;
    let significant = |c: Point| c.dot(c) > EPS2;
    let size = part.points.len();

    if split.at_node {
        let out = coord_outgoing_tangent(coords, part.first_coord, part.last_coord, part.closed, split.edge);
        let inc = coord_incoming_tangent(coords, part.first_coord, part.last_coord, part.closed, split.edge);
        let tangent = out.map(unit).unwrap_or(Point::ZERO) + inc.map(unit).unwrap_or(Point::ZERO);
        return PathLocation { pos, tangent };
    }

    let mut forward = Point::ZERO;
    let mut forward_found = false;
    if split.on_curve {
        let e = split.edge;
        let (_o0, _o1, o2, o3, o4) = split_bezier(coords[e].pos(), coords[e + 1].pos(), coords[e + 2].pos(), coords[e + 3].pos(), split.param);
        for candidate in [o3 - pos, o4 - pos, coords[e + 3].pos() - pos] {
            if significant(candidate) { forward = candidate; forward_found = true; break; }
        }
        let _ = o2;
    }
    if !forward_found {
        for k in split.upper..size {
            let candidate = part.points[k] - pos;
            if significant(candidate) { forward = candidate; forward_found = true; break; }
        }
    }
    if !forward_found && part.closed {
        for k in 0..split.upper {
            let candidate = part.points[k] - pos;
            if significant(candidate) { forward = candidate; forward_found = true; break; }
        }
    }

    let mut backward = Point::ZERO;
    let mut backward_found = false;
    if split.on_curve {
        let e = split.edge;
        let (o0, o1, _o2, _o3, _o4) = split_bezier(coords[e].pos(), coords[e + 1].pos(), coords[e + 2].pos(), coords[e + 3].pos(), split.param);
        for candidate in [pos - o1, pos - o0, pos - coords[e].pos()] {
            if significant(candidate) { backward = candidate; backward_found = true; break; }
        }
    }
    if !backward_found {
        let backward_from = split.upper - 1;
        for k in (0..=backward_from).rev() {
            let candidate = pos - part.points[k];
            if significant(candidate) { backward = candidate; backward_found = true; break; }
        }
    }
    if !backward_found && part.closed {
        for k in (split.upper + 1..size).rev() {
            let candidate = pos - part.points[k];
            if significant(candidate) { backward = candidate; backward_found = true; break; }
        }
    }

    let tangent = (if forward_found { unit(forward) } else { Point::ZERO })
        + (if backward_found { unit(backward) } else { Point::ZERO });
    PathLocation { pos, tangent }
}

/// Returns the path between two lengths along the part, as exact geometry.
///
/// Sections of bezier curves are cut out of the curves themselves, so a
/// dash on a curve is itself a curve.
pub fn slice_path(coords: &CoordList, part: &PathPart, from: f64, to: f64) -> Path {
    let mut path = Path::new();
    if part.points.is_empty() || part.lengths.is_empty() { return path; }

    let from = from.max(0.0);
    let to = to.min(*part.lengths.last().unwrap());
    if to <= from { return path; }

    let a = split_at(coords, part, from);
    let b = split_at(coords, part, to);
    path.move_to(a.pos);

    let pos = |i: usize| coords[i].pos();
    let curve_piece = |path: &mut Path, edge: usize, t0: f32, start: Point, t1: f32, end: Point| {
        let (_o0, _o1, o2, o3, o4) = split_bezier(pos(edge), pos(edge + 1), pos(edge + 2), pos(edge + 3), t0);
        let remainder = if t1 >= 1.0 || t0 >= 1.0 { 1.0 } else { (t1 - t0) / (1.0 - t0) };
        if remainder >= 1.0 {
            path.cubic_to(o3, o4, end);
            return;
        }
        let (q0, q1, _q2, _q3, _q4) = split_bezier(start, o3, o4, pos(edge + 3), remainder);
        path.cubic_to(q0, q1, end);
        let _ = o2;
    };

    let mut cursor: usize;
    if a.at_node {
        cursor = a.edge;
    } else if a.on_curve {
        let edge = a.edge;
        if b.edge == a.edge && !b.at_node {
            curve_piece(&mut path, edge, a.param, a.pos, b.param, b.pos);
            return path;
        }
        if b.edge == edge + 3 && b.at_node {
            curve_piece(&mut path, edge, a.param, a.pos, 1.0, b.pos);
            return path;
        }
        curve_piece(&mut path, edge, a.param, a.pos, 1.0, pos(edge + 3));
        cursor = edge + 3;
    } else {
        let edge = a.edge;
        if b.edge == a.edge && !b.at_node {
            path.line_to(b.pos);
            return path;
        }
        if b.edge == edge + 1 && b.at_node {
            path.line_to(b.pos);
            return path;
        }
        path.line_to(pos(edge + 1));
        cursor = edge + 1;
    }

    while cursor < part.last_coord {
        if b.at_node && cursor == b.edge {
            return path;
        }
        if coords[cursor].is_curve_start() && cursor + 3 <= part.last_coord {
            if !b.at_node && b.edge == cursor {
                let (o0, o1, _o2, _o3, _o4) = split_bezier(pos(cursor), pos(cursor + 1), pos(cursor + 2), pos(cursor + 3), b.param);
                path.cubic_to(o0, o1, b.pos);
                return path;
            }
            path.cubic_to(pos(cursor + 1), pos(cursor + 2), pos(cursor + 3));
            cursor += 3;
        } else {
            if !b.at_node && b.edge == cursor {
                path.line_to(b.pos);
                return path;
            }
            path.line_to(pos(cursor + 1));
            cursor += 1;
        }
    }
    path
}

/// Appends the coordinates between two lengths along the part to a list.
///
/// This is the coordinate-level equivalent of `slice_path()`: curve
/// sections stay curves. Consecutive slices connect seamlessly, and the
/// flags of the coordinates are carried along, which is what dashed lines
/// are made of: the whole path is copied slice by slice, with gap flags
/// added at the cuts.
pub fn copy_path_slice(coords: &CoordList, part: &PathPart, from: f64, to: f64, out: &mut CoordList) {
    if part.points.is_empty() || part.lengths.is_empty() { return; }

    let from = from.max(0.0);
    let to = to.min(*part.lengths.last().unwrap());
    let a = split_at(coords, part, from);
    let b = split_at(coords, part, to.max(from));

    const COPIED_FLAGS_AT_START: i32 = coord_flag::GAP_POINT | coord_flag::DASH_POINT;
    const COPIED_FLAGS_AT_END: i32 = coord_flag::GAP_POINT | coord_flag::DASH_POINT | coord_flag::HOLE_POINT | coord_flag::CLOSE_POINT;

    let pos = |i: usize| coords[i].pos();
    let push = |out: &mut CoordList, p: Point, flags: i32| out.push(Coord::new(p.x, p.y, flags));

    let a_curve = a.on_curve
        || (a.at_node && coords[a.edge].is_curve_start() && a.edge + 3 <= part.last_coord);
    let (mut a_cs0, mut a_cs1) = (Point::ZERO, Point::ZERO);
    if a_curve {
        let e = a.edge;
        let (_o0, _o1, _o2, o3, o4) = split_bezier(pos(e), pos(e + 1), pos(e + 2), pos(e + 3), a.param);
        a_cs0 = o3;
        a_cs1 = o4;
    }

    let mut b_curve = false;
    let mut b_arriving_edge: Option<usize> = None;
    if b.on_curve {
        b_curve = true;
        b_arriving_edge = Some(b.edge);
    } else if b.at_node && b.edge >= 3 && b.edge >= part.first_coord + 3 && coords[b.edge - 3].is_curve_start() {
        b_curve = true;
        b_arriving_edge = Some(b.edge - 3);
    }
    let (mut b_ce0, mut b_ce1) = (Point::ZERO, Point::ZERO);
    if b_curve {
        if b_arriving_edge == Some(a.edge) && a_curve {
            if b.at_node {
                b_ce0 = a_cs0;
                b_ce1 = a_cs1;
            } else {
                let e = a.edge;
                let remainder = if a.param >= 1.0 { 1.0 } else { (b.param - a.param) / (1.0 - a.param) };
                let (q0, q1, _q2, _q3, _q4) = split_bezier(a.pos, a_cs0, a_cs1, pos(e + 3), remainder);
                b_ce0 = q0;
                b_ce1 = q1;
            }
        } else if b.at_node {
            b_ce0 = pos(b.edge - 2);
            b_ce1 = pos(b.edge - 1);
        } else {
            let e = b.edge;
            let (o0, o1, _o2, _o3, _o4) = split_bezier(pos(e), pos(e + 1), pos(e + 2), pos(e + 3), b.param);
            b_ce0 = o0;
            b_ce1 = o1;
        }
    }

    let need_push_a = match out.last() {
        None => true,
        Some(last) => last.is_hole_point() || last.x != a.pos.x || last.y != a.pos.y,
    };
    if need_push_a {
        push(out, a.pos, 0);
    }
    {
        let mut first_flags = out.last().unwrap().flags & COPIED_FLAGS_AT_START;
        if a.at_node {
            first_flags |= coords[a.edge].flags & COPIED_FLAGS_AT_START;
        }
        out.last_mut().unwrap().flags = first_flags;
    }

    if a.edge == b.edge {
        if b_curve && b.param != a.param {
            out.last_mut().unwrap().flags |= coord_flag::CURVE_START;
        }
    } else {
        if a_curve {
            out.last_mut().unwrap().flags |= coord_flag::CURVE_START;
        }

        let mut stop_index = b.edge;
        if b.at_node {
            stop_index -= if b_curve { 3 } else { 1 };
        }

        let mut index = a.edge + 1;
        if a_curve && index < stop_index {
            push(out, a_cs0, 0);
            push(out, a_cs1, 0);
            index += 2;
        }
        while index <= stop_index {
            push(out, pos(index), coords[index].flags);
            index += 1;
        }
    }

    if out.last().unwrap().is_curve_start() {
        push(out, b_ce0, 0);
        push(out, b_ce1, 0);
    }

    push(out, b.pos, if b.at_node { coords[b.edge].flags & COPIED_FLAGS_AT_END } else { 0 });
}

/// Returns the outline of a pointed line cap, as a closed polygon.
///
/// A pointed cap tapers the line from its full width down to zero over the
/// length of the cap.
pub fn pointed_cap_outline(coords: &CoordList, part: &PathPart, cap_start: f64, cap_end: f64, is_end: bool, line_half_width: f64, cap_length: f64) -> CoordList {
    let tan_angle = if cap_length > 0.0 { line_half_width / cap_length } else { 0.0 };

    let mut middle = CoordList::new();
    copy_path_slice(coords, part, cap_start, cap_end, &mut middle);
    let size = middle.len();
    if size < 2 { return middle; }

    let mut lengths = vec![0.0f64; size];
    {
        let middle_parts = flatten(&middle);
        if let Some(mp) = middle_parts.first() {
            let mut last_length = 0.0f64;
            let mut next = 0usize;
            for i in 0..mp.points.len() {
                if mp.curve_points[i] { continue; }
                let coord_i = mp.coord_index[i];
                while next <= coord_i && next < size {
                    lengths[next] = last_length;
                    next += 1;
                }
                last_length = mp.lengths[i];
                if coord_i < size {
                    lengths[coord_i] = mp.lengths[i];
                }
            }
            while next < size {
                lengths[next] = last_length;
                next += 1;
            }
        }
    }
    let total = cap_end - cap_start;

    let perp_right = |v: Point| Point::new(-v.y, v.x);
    let sign = if is_end { -1.0 } else { 1.0 };
    let last = size - 1;

    let mut right_coords: Vec<Point> = Vec::with_capacity(2 * size);
    let mut left_coords: Vec<Point> = Vec::with_capacity(2 * size);
    let mut out_flags: Vec<i32> = Vec::with_capacity(2 * size);

    let mut i = 0usize;
    while i < size {
        let dist_from_start = if is_end { total - lengths[i] } else { lengths[i] };
        let mut factor = if cap_length > 0.0 { dist_from_start / cap_length } else { 1.0 };
        factor = factor.min(1.0).max(0.0);

        let to_coord_opt = coord_incoming_tangent(&middle, 0, last, false, i);
        let to_next_opt = coord_outgoing_tangent(&middle, 0, last, false, i);
        let mut scaling = 1.0f64;
        let to_next_final = match (to_coord_opt, to_next_opt) {
            (_, None) => to_coord_opt.unwrap_or(Point::ZERO),
            (Some(to_coord_raw), Some(to_next_raw)) => {
                let to_coord = unit(to_coord_raw);
                let to_next = unit(to_next_raw);
                if to_next == -to_coord {
                    scaling = f64::INFINITY;
                    perp_right(to_next)
                } else {
                    let combined = unit(to_next + to_coord);
                    scaling = 1.0 / (combined.x * to_coord.x + combined.y * to_coord.y);
                    combined
                }
            }
            (None, Some(to_next_raw)) => to_next_raw,
        };
        let right_vector = unit(perp_right(to_next_final));

        let radius = (line_half_width * factor * scaling).max(0.0).min(line_half_width * 2.0);

        let pos = middle[i].pos();
        middle[i].flags &= !(coord_flag::HOLE_POINT | coord_flag::CLOSE_POINT);
        right_coords.push(pos + right_vector * radius);
        left_coords.push(pos - right_vector * radius);
        out_flags.push(middle[i].flags);

        if i >= 3 && middle[i - 3].is_curve_start() {
            let rb = *right_coords.last().unwrap();
            let lb = *left_coords.last().unwrap();
            right_coords.push(rb);
            left_coords.push(lb);
            out_flags.push(0);
            let tangent = middle[i].pos() - middle[i - 1].pos();
            let right_scale = distance(tangent) * tan_angle * sign;
            let at = right_coords.len() - 1;
            right_coords[at - 1] = right_coords[at] - tangent - right_vector * right_scale;
            left_coords[at - 1] = left_coords[at] - tangent + right_vector * right_scale;
        }
        if middle[i].is_curve_start() && i + 2 < size {
            let tangent = middle[i + 1].pos() - middle[i].pos();
            let right_scale = distance(tangent) * tan_angle * sign;
            let rb = *right_coords.last().unwrap();
            let lb = *left_coords.last().unwrap();
            right_coords.push(rb + tangent + right_vector * right_scale);
            left_coords.push(lb + tangent - right_vector * right_scale);
            out_flags.push(0);
            i += 2;
        }
        i += 1;
    }

    // A small overlap where the cap meets the line, to avoid glitches.
    const OVERLAP_LENGTH: f64 = 0.05;
    if total > 4.0 * OVERLAP_LENGTH {
        let end_pos = if is_end { 0 } else { last };
        let end_cap_pos = if is_end { 0 } else { right_coords.len() - 1 };
        let tangent_opt = if is_end {
            coord_outgoing_tangent(&middle, 0, last, false, end_pos)
        } else {
            coord_incoming_tangent(&middle, 0, last, false, end_pos)
        };
        if let Some(raw_tangent) = tangent_opt {
            let tangent = unit(raw_tangent) * (OVERLAP_LENGTH * sign);
            let right = perp_right(if is_end { tangent } else { -tangent });
            let shifted = right_coords[end_cap_pos] + tangent + right;
            let shifted_left = left_coords[end_cap_pos] + tangent - right;
            if is_end {
                right_coords.insert(0, shifted);
                left_coords.insert(0, shifted_left);
                out_flags.insert(0, 0);
            } else {
                right_coords.push(shifted);
                left_coords.push(shifted_left);
                out_flags.push(0);
            }
        }
    }

    // Concatenate: down the right side, back up the left side.
    let mut outline = CoordList::with_capacity(2 * right_coords.len());
    for i in 0..right_coords.len() {
        outline.push(Coord::new(right_coords[i].x, right_coords[i].y, out_flags[i]));
    }

    let left_size = left_coords.len();
    if !is_end {
        let mut i = left_size;
        while i > 0 {
            i -= 1;
            if i >= 3 && (out_flags[i - 3] & coord_flag::CURVE_START) != 0 {
                outline.push(Coord::new(left_coords[i].x, left_coords[i].y, coord_flag::CURVE_START));
                outline.push(Coord::new(left_coords[i - 1].x, left_coords[i - 1].y, 0));
                outline.push(Coord::new(left_coords[i - 2].x, left_coords[i - 2].y, 0));
                i -= 2;
            } else {
                outline.push(Coord::new(left_coords[i].x, left_coords[i].y, 0));
            }
        }
    } else {
        let mut i = left_size - 1;
        while i > 0 {
            i -= 1;
            if i >= 3 && (out_flags[i - 3] & coord_flag::CURVE_START) != 0 {
                outline.push(Coord::new(left_coords[i].x, left_coords[i].y, coord_flag::CURVE_START));
                outline.push(Coord::new(left_coords[i - 1].x, left_coords[i - 1].y, 0));
                outline.push(Coord::new(left_coords[i - 2].x, left_coords[i - 2].y, 0));
                i -= 2;
            } else if i >= 2 && i == left_size - 2 && (out_flags[i - 2] & coord_flag::CURVE_START) != 0 {
                let flags = outline.last().unwrap().flags | coord_flag::CURVE_START;
                outline.last_mut().unwrap().flags = flags;
                outline.push(Coord::new(left_coords[i].x, left_coords[i].y, 0));
                outline.push(Coord::new(left_coords[i - 1].x, left_coords[i - 1].y, 0));
                i -= 1;
            } else {
                outline.push(Coord::new(left_coords[i].x, left_coords[i].y, 0));
            }
        }
    }
    outline
}

/// Returns the border line of a line symbol: the coordinates of one part,
/// moved sideways off the path.
///
/// A positive shift moves the line to the right hand side, seen in the
/// direction of the path, in a coordinate system with the y axis pointing
/// down. The shift is split into the half width of the main line and the
/// shift of the border itself, because a corner treats the two
/// differently. Bezier curves are shifted as curves via [`QBezier::shifted`];
/// the flags of the coordinates are carried along.
pub fn shift_coordinates(coords: &CoordList, first: usize, last: usize, closed: bool, main_shift: f64, border_shift: f64, join_style_value: i32) -> CoordList {
    const CURVE_THRESHOLD: f32 = 0.03;
    const MAX_OFFSET: usize = 16;
    // LineSymbol::miterLimit() is 1 in Mapper.
    const MITER_LIMIT: f64 = 2.0;
    let miter_reference = if join_style_value == join_style::MITER {
        (4.0f64 / MITER_LIMIT).atan().cos()
    } else {
        0.0
    };

    let shift = main_shift + border_shift;

    let mut out = CoordList::new();
    out.reserve(4 * (last - first + 1));
    let push = |out: &mut CoordList, p: Point, flags: i32| out.push(Coord::new(p.x, p.y, flags));

    let mut i = first;
    while i <= last {
        let coord = coords[i].pos();
        let flags_i = coords[i].flags;

        let mut vector_in = coord_incoming_tangent(coords, first, last, closed, i);
        let mut vector_out = coord_outgoing_tangent(coords, first, last, closed, i);
        if vector_in.is_none() { vector_in = vector_out; }
        if vector_out.is_none() { vector_out = vector_in; }
        let ok_in = vector_in.is_some();
        let ok_out = vector_out.is_some();
        let tangent_in = if ok_in { vector_in.unwrap().normalized() } else { Point::ZERO };
        let tangent_out = if ok_out { vector_out.unwrap().normalized() } else { Point::ZERO };

        // Always overwritten in one of the branches below (mirrors the
        // uninitialized-then-assigned C++ original); silence the lint
        // rather than restructure already cross-checked logic.
        #[allow(unused_assignments)]
        let mut segment_start = Point::ZERO;
        if !ok_in && !ok_out {
            segment_start = coord;
        } else if i == first && !closed {
            segment_start = coord + tangent_out.perp_right() * shift;
        } else if i == last && !closed {
            segment_start = coord + tangent_in.perp_right() * shift;
        } else {
            // Corner point.
            let mut right_vector = tangent_out.perp_right();
            let middle0 = (tangent_in + tangent_out).normalized();
            #[allow(unused_assignments)]
            let mut offset = 0.0f64;

            let a = (tangent_out.x * tangent_in.y - tangent_in.x * tangent_out.y) * shift;
            if a > 0.0 {
                // Outer side of the corner.
                if join_style_value == join_style::BEVEL || join_style_value == join_style::ROUND {
                    let middle1 = (tangent_in + middle0).normalized();
                    let phi1 = middle1.dot(tangent_in).acos();
                    offset = phi1.tan() * border_shift.abs();

                    if i > first && !offset.is_nan() {
                        push(&mut out, coord + tangent_in.perp_right() * shift + tangent_in * offset, 0);
                        if join_style_value == join_style::ROUND {
                            push(&mut out, coord + middle0.perp_right() * shift, 0);
                        }
                    }
                } else {
                    // join_style == MiterJoin
                    let miter_check = middle0.dot(tangent_in);
                    if miter_check <= miter_reference {
                        // The miter exceeds the limit: two border corner points.
                        let middle1 = (tangent_in + middle0).normalized();
                        let phi1 = middle1.dot(tangent_in).acos();
                        offset = MITER_LIMIT * main_shift.abs() + phi1.tan() * border_shift.abs();

                        if i > first && !offset.is_nan() {
                            push(&mut out, coord + tangent_in.perp_right() * shift + tangent_in * offset, 0);
                        }
                    } else {
                        let phi = middle0.perp_right().dot(tangent_in).acos();
                        offset = (1.0 / phi.tan() * shift).abs();
                    }
                }

                if offset.is_nan() { offset = 0.0; }
                segment_start = coord + right_vector * shift - tangent_out * offset;
            } else if i > first + 2 && coords[i - 3].is_curve_start() && coords[i].is_curve_start() {
                // Inner side of the corner, both sides are beziers.
                right_vector = middle0.perp_right();
                let phi = right_vector.dot(tangent_in).acos();
                let sin_phi = phi.sin();
                let inset = if sin_phi > 1.0 / MITER_LIMIT { 1.0 / sin_phi } else { MITER_LIMIT };
                segment_start = coord + right_vector * (shift * inset);
            } else {
                // Inner side of the corner, at most one bezier involved.
                let phi = middle0.perp_right().dot(tangent_in).acos();
                let tan_phi = phi.tan();
                offset = -(shift / tan_phi).abs();

                if tan_phi >= 1.0 {
                    segment_start = coord + right_vector * shift - tangent_out * offset;
                } else {
                    // Critical case.
                    let len_in = vector_in.unwrap().length();
                    let len_out = vector_out.unwrap().length();
                    let excess = if offset.is_nan() { 0.0 } else { offset.abs() - len_in.min(len_out) };

                    if excess < 0.0 {
                        segment_start = coord + right_vector * shift - tangent_out * offset;
                    } else if len_in < len_out {
                        segment_start = coord + right_vector * shift + tangent_out * len_in;
                    } else {
                        right_vector = tangent_in.perp_right();
                        segment_start = coord + right_vector * shift - tangent_in * len_out;
                    }
                }
            }
        }

        push(&mut out, segment_start, flags_i & !coord_flag::CURVE_START);

        if coords[i].is_curve_start() && i + 3 <= last {
            if shift > 0.0 {
                let bezier = QBezier::from_points(coords[i + 3].pos(), coords[i + 2].pos(), coords[i + 1].pos(), coord);
                let segments = bezier.shifted(MAX_OFFSET, shift.abs(), CURVE_THRESHOLD);
                let count = segments.len();
                for (j, seg) in segments.iter().enumerate().rev() {
                    out.last_mut().unwrap().flags |= coord_flag::CURVE_START;
                    push(&mut out, seg.pt3(), 0);
                    push(&mut out, seg.pt2(), 0);
                    if j > 0 {
                        push(&mut out, seg.pt1(), 0);
                    }
                }
                let _ = count;
            } else {
                let bezier = QBezier::from_points(coord, coords[i + 1].pos(), coords[i + 2].pos(), coords[i + 3].pos());
                let segments = bezier.shifted(MAX_OFFSET, shift.abs(), CURVE_THRESHOLD);
                let count = segments.len();
                for (j, seg) in segments.iter().enumerate() {
                    out.last_mut().unwrap().flags |= coord_flag::CURVE_START;
                    push(&mut out, seg.pt2(), 0);
                    push(&mut out, seg.pt3(), 0);
                    if j < count - 1 {
                        push(&mut out, seg.pt4(), 0);
                    }
                }
            }
            i += 2;
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square() -> CoordList {
        // A 4x4 closed square, matching shapes.xmap's first object.
        vec![
            Coord::new(0.0, 0.0, 0),
            Coord::new(4.0, 0.0, 0),
            Coord::new(4.0, 4.0, 0),
            Coord::new(0.0, 4.0, 0),
            Coord::new(0.0, 0.0, coord_flag::HOLE_POINT | coord_flag::CLOSE_POINT),
        ]
    }

    #[test]
    fn flatten_closed_square() {
        let parts = flatten(&square());
        assert_eq!(parts.len(), 1);
        let part = &parts[0];
        assert!(part.closed);
        assert_eq!(part.points.len(), 5);
        assert!((part.length() - 16.0).abs() < 1e-6);
    }

    #[test]
    fn flattened_extent_matches_bounds() {
        let parts = flatten(&square());
        let extent = flattened_extent(&parts);
        assert!((extent.left() - 0.0).abs() < 1e-9);
        assert!((extent.top() - 0.0).abs() < 1e-9);
        assert!((extent.right() - 4.0).abs() < 1e-9);
        assert!((extent.bottom() - 4.0).abs() < 1e-9);
    }

    #[test]
    fn stroked_path_extent_open_line_with_flat_cap() {
        // A straight open line from (0,0) to (10,0), half-width 1.
        let coords = vec![Coord::new(0.0, 0.0, 0), Coord::new(10.0, 0.0, 0)];
        let parts = flatten(&coords);
        let extent = stroked_path_extent(&coords, &parts, 1.0, PenCap::Flat, PenJoin::Miter);
        // Flat cap: no extension along the line; full extension perpendicular.
        assert!((extent.left() - 0.0).abs() < 1e-6, "left={}", extent.left());
        assert!((extent.right() - 10.0).abs() < 1e-6, "right={}", extent.right());
        assert!((extent.top() - -1.0).abs() < 1e-6, "top={}", extent.top());
        assert!((extent.bottom() - 1.0).abs() < 1e-6, "bottom={}", extent.bottom());
    }

    #[test]
    fn stroked_path_extent_round_cap_extends_along_line() {
        let coords = vec![Coord::new(0.0, 0.0, 0), Coord::new(10.0, 0.0, 0)];
        let parts = flatten(&coords);
        let extent = stroked_path_extent(&coords, &parts, 1.0, PenCap::Round, PenJoin::Miter);
        assert!((extent.left() - -1.0).abs() < 1e-6, "left={}", extent.left());
        assert!((extent.right() - 11.0).abs() < 1e-6, "right={}", extent.right());
    }

    #[test]
    fn locate_on_path_midpoint_of_straight_line() {
        let coords = vec![Coord::new(0.0, 0.0, 0), Coord::new(10.0, 0.0, 0)];
        let parts = flatten(&coords);
        let loc = locate_on_path(&coords, &parts[0], 5.0);
        assert!((loc.pos.x - 5.0).abs() < 1e-9);
        assert!((loc.pos.y - 0.0).abs() < 1e-9);
        // Tangent should point in +x.
        assert!(loc.tangent.x > 0.0);
        assert!(loc.tangent.y.abs() < 1e-9);
    }

    #[test]
    fn shift_coordinates_offsets_a_straight_segment() {
        let coords = vec![Coord::new(0.0, 0.0, 0), Coord::new(10.0, 0.0, 0)];
        let shifted = shift_coordinates(&coords, 0, 1, false, 1.0, 0.0, join_style::MITER);
        assert_eq!(shifted.len(), 2);
        // Shift to the "right" of the direction (+x), with y axis down,
        // means +y for a rightward-pointing segment.
        assert!((shifted[0].y - 1.0).abs() < 1e-9, "y={}", shifted[0].y);
        assert!((shifted[1].y - 1.0).abs() < 1e-9, "y={}", shifted[1].y);
    }

    #[test]
    fn copy_path_slice_round_trip_covers_whole_part() {
        let coords = vec![Coord::new(0.0, 0.0, 0), Coord::new(10.0, 0.0, 0), Coord::new(10.0, 10.0, 0)];
        let parts = flatten(&coords);
        let mut out = CoordList::new();
        copy_path_slice(&coords, &parts[0], 0.0, parts[0].length(), &mut out);
        assert_eq!(out.first().unwrap().pos(), Point::new(0.0, 0.0));
        assert_eq!(out.last().unwrap().pos(), Point::new(10.0, 10.0));
    }
}

impl Path {
    /// Flattens each subpath into a polyline, like
    /// `QPainterPath::toSubpathPolygons()`. Used as the stroke-extent
    /// fallback for un-curved paths (this crate's one no-explicit-bounds
    /// `stroke()` call site is always a straight two-point segment), so the
    /// bezier flattening here does not need to match Mapper's exactly.
    pub fn to_subpath_polygons(&self) -> Vec<Vec<Point>> {
        self.to_subpath_polygons_tol(BEZIER_ERROR)
    }

    /// Same as [`to_subpath_polygons`](Self::to_subpath_polygons), but with
    /// the bezier flattening tolerance passed in. `contains_even_odd` needs
    /// a much tighter one than [`BEZIER_ERROR`] -- see
    /// [`flatten_cubic_tol`]'s doc comment.
    pub fn to_subpath_polygons_tol(&self, error: f64) -> Vec<Vec<Point>> {
        let mut result = Vec::new();
        let mut current: Vec<Point> = Vec::new();
        let mut last = Point::ZERO;
        for cmd in &self.commands {
            match *cmd {
                PathCommand::MoveTo(p) => {
                    if current.len() > 1 { result.push(std::mem::take(&mut current)); }
                    else { current.clear(); }
                    current.push(p);
                    last = p;
                }
                PathCommand::LineTo(p) => {
                    current.push(p);
                    last = p;
                }
                PathCommand::CubicTo(c1, c2, end) => {
                    let mut pts = Vec::new();
                    let mut params = Vec::new();
                    flatten_cubic_tol(&mut pts, &mut params, last, c1, c2, end, 0.0, 1.0, 0, error);
                    current.extend(pts);
                    current.push(end);
                    last = end;
                }
                PathCommand::Close => {
                    if let Some(&first) = current.first() {
                        current.push(first);
                    }
                }
            }
        }
        if current.len() > 1 { result.push(current); }
        result
    }
}

impl Path {
    /// Point-in-path test using the odd-even fill rule -- `QPainterPath`'s
    /// default (`Qt::OddEvenFill`), which is what `toPainterPath()` (used
    /// for area outlines) implicitly uses, and what `QPainterPath::contains`
    /// therefore tests with. Implemented as a standard crossing-number test
    /// over the flattened subpaths, which handles multi-subpath holes
    /// correctly under odd-even counting without needing a dedicated
    /// winding pass.
    ///
    /// Flattens at a much tighter tolerance than [`BEZIER_ERROR`], not the
    /// default one `to_subpath_polygons()` uses for stroke extents: Qt's
    /// `QPainterPath::contains` tests the true cubic curve, not a flattened
    /// approximation, so a point pattern's dot can sit a few microns inside
    /// a curved area boundary -- well within visual tolerance, but wrongly
    /// outside a polygon flattened to it. This is not performance sensitive
    /// the way per-frame flattening is (it runs once per pattern point, on
    /// already-computed outlines), so it can afford to.
    pub fn contains_even_odd(&self, point: Point) -> bool {
        const CONTAINS_TOLERANCE: f64 = 1e-9;
        let mut crossings = 0u32;
        for polygon in self.to_subpath_polygons_tol(CONTAINS_TOLERANCE) {
            let n = polygon.len();
            if n < 2 { continue; }
            for i in 0..n {
                let a = polygon[i];
                let b = polygon[(i + 1) % n];
                if (a.y > point.y) != (b.y > point.y) {
                    let x_intersect = a.x + (point.y - a.y) / (b.y - a.y) * (b.x - a.x);
                    if point.x < x_intersect {
                        crossings += 1;
                    }
                }
            }
        }
        crossings % 2 == 1
    }
}
