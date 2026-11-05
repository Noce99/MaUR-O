//! The fastest way across a map, as a runner would take it.
//!
//! Given a grid of running speeds -- [`crate::runnability`] builds one -- and
//! two points on it, this finds the route between them that takes the least
//! time, and says how long it takes and what it crosses. It is what turns "how
//! fast is this ground" into "which way would you go", and it is the question
//! a course setter asks of every leg.
//!
//! # What decides a route
//!
//! Time, not distance. The cost of crossing a cell is its length divided by
//! the speed of the ground, multiplied by what the climb costs -- Tobler's
//! hiking function, which makes going up expensive, going gently down slightly
//! cheaper than flat, and going steeply down expensive again. A route around
//! a hill and a route over it are compared by the clock.
//!
//! Elevation is optional. Without it the terrain is flat, Tobler's factor is
//! the same everywhere, and the route is decided by the ground alone.
//!
//! # How it is searched
//!
//! A* over the grid, eight directions, with an admissible heuristic: the
//! straight-line cell distance at the best speed anywhere in the window. The
//! search runs in a window around the two points rather than over the whole
//! map, and the window grows and the search repeats whenever the route it
//! found hugs an edge of that window -- so a detour well outside the direct
//! line is still found, without paying for the whole map every time.
//!
//! The raw path steps cell to cell, which over-counts distance the way a
//! staircase over-counts a diagonal, so it is then pulled straight: each
//! vertex is dropped where the straight segment past it costs no more than
//! going through it.
//!
//! # Units
//!
//! The grid's bounds and cell size are in whatever unit the caller measures
//! its map in, and `meters_per_unit` relates that to the ground. Everything
//! reported -- distances, climb, times -- is in metres and seconds; the route
//! itself comes back in the caller's units.
//!
//! Ported from pyorienteering's `routeoptimizer`.

use crate::geometry::Rect;

/// A speed of zero, or less: ground nothing crosses.
const IMPASSABLE: f32 = 0.0;
const SQRT2: f64 = std::f64::consts::SQRT_2;

/// Tobler's hiking function as a cost multiplier: 1 on the gentle downhill a
/// walker is fastest on, and rising in both directions from there.
fn tobler(slope: f64) -> f64 {
    (3.5 * (slope + 0.05).abs()).exp()
}

/// The grid a route is searched over.
#[derive(Clone, Copy)]
pub struct Grid<'a> {
    /// One speed per cell, row by row. `0` and `NaN` are both impassable.
    pub values: &'a [f32],
    /// Cells across.
    pub width: usize,
    /// Cells down.
    pub height: usize,
    /// The length of a cell's side, in the caller's units.
    pub pixel_size: f64,
    /// What the grid covers, in the caller's units.
    pub bounds: Rect,
    /// Ground metres per unit of the caller's coordinates.
    pub meters_per_unit: f64,
    /// Which symbol each cell's speed came from, for describing the route.
    pub code_index: Option<&'a [i32]>,
    /// The codes [`code_index`](Self::code_index) refers to.
    pub codes: &'a [String],
}

/// Elevations to weigh the climb with, on a grid of their own.
#[derive(Clone, Copy)]
pub struct Elevation<'a> {
    /// Metres above sea level, row by row; `NaN` where unknown.
    pub values: &'a [f32],
    /// Cells across.
    pub width: usize,
    /// Cells down.
    pub height: usize,
    /// What the grid covers, in the caller's units.
    pub bounds: Rect,
}

impl Elevation<'_> {
    /// The elevation at a point, interpolated between the four cell centres
    /// around it. `NaN` outside the grid.
    ///
    /// Interpolated rather than read off the nearest cell because the route is
    /// sampled at half a cell at a time: on a nearest-cell lookup the whole
    /// step between two cells lands on one pair of samples, doubling the slope
    /// there, and Tobler's function turns that into a cost too high by enough
    /// to re-rank one route choice against another.
    pub fn sample(&self, x: f64, y: f64) -> f64 {
        if self.width == 0 || self.height == 0 {
            return f64::NAN;
        }
        let units_per_px_x = self.bounds.width() / self.width as f64;
        let units_per_px_y = self.bounds.height() / self.height as f64;
        // Cell coordinates measured from cell centres.
        let fx = (x - self.bounds.left()) / units_per_px_x - 0.5;
        let fy = (y - self.bounds.top()) / units_per_px_y - 0.5;
        if fx < -0.5 || fy < -0.5 || fx > self.width as f64 - 0.5 || fy > self.height as f64 - 0.5 {
            return f64::NAN;
        }
        let c0 = fx.floor().clamp(0.0, self.width as f64 - 1.0) as usize;
        let r0 = fy.floor().clamp(0.0, self.height as f64 - 1.0) as usize;
        let c1 = (c0 + 1).min(self.width - 1);
        let r1 = (r0 + 1).min(self.height - 1);
        let tx = (fx - c0 as f64).clamp(0.0, 1.0);
        let ty = (fy - r0 as f64).clamp(0.0, 1.0);

        let at = |r: usize, c: usize| f64::from(self.values[r * self.width + c]);
        let (v00, v01, v10, v11) = (at(r0, c0), at(r0, c1), at(r1, c0), at(r1, c1));
        if v00.is_finite() && v01.is_finite() && v10.is_finite() && v11.is_finite() {
            let top = v00 + (v01 - v00) * tx;
            let bottom = v10 + (v11 - v10) * tx;
            return top + (bottom - top) * ty;
        }
        // A nodata neighbour: take the nearest cell rather than spreading the
        // hole over four.
        let r = (fy.round().clamp(0.0, self.height as f64 - 1.0)) as usize;
        let c = (fx.round().clamp(0.0, self.width as f64 - 1.0)) as usize;
        f64::from(self.values[r * self.width + c])
    }
}

/// A point on the map, in the caller's units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    /// Rightwards.
    pub x: f64,
    /// Downwards, as a map's own coordinates run.
    pub y: f64,
}

/// One stretch of a route over ground of one kind at one steepness.
#[derive(Clone, Debug)]
pub struct Segment {
    /// How far it runs, on the ground.
    pub length_m: f64,
    /// Rise over run, averaged along the segment.
    pub avg_slope: f64,
    /// The running speed of the ground it crosses.
    pub value: f32,
    /// The symbol code of that ground, where the grid says.
    pub code: Option<String>,
    /// How long a kilometre of this would take, at the pace it was crossed.
    pub pace_min_per_km: f64,
    /// How long this stretch takes.
    pub time_s: f64,
    /// The segment itself, in the caller's units.
    pub path: Vec<Point>,
}

/// A solved leg.
#[derive(Clone, Debug)]
pub struct Leg {
    /// The route, in the caller's units.
    pub path: Vec<Point>,
    /// How far it runs, on the ground.
    pub distance_m: f64,
    /// How long it takes.
    pub time_s: f64,
    /// Metres climbed, counting only the rises.
    pub climb_m: f64,
    /// What the leg would be as the crow flies, for comparison.
    pub straight_m: f64,
    /// The stretches it crosses, in order.
    pub segments: Vec<Segment>,
}

/// Which search to run.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Algorithm {
    /// A* in a window which grows until the route it finds is clear of the
    /// window's own edges. What a caller wants.
    #[default]
    AStar,
    /// Dijkstra over the whole map at once: no heuristic, no window, and much
    /// slower. Here to check the other against.
    Dijkstra,
}

/// The knobs on a solve. The defaults are the ones the reference
/// implementation uses.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Which search to run.
    pub algorithm: Algorithm,
    /// The pace, in minutes per kilometre, that a speed of `1.0` means.
    pub reference_min_per_km: f64,
    /// How far, in cells, to look for passable ground when a control sits on
    /// something nothing crosses.
    pub snap_radius: i64,
    /// The first window's margin, as a fraction of the leg's straight length.
    pub margin_frac: f64,
    /// The first window's margin, at least, in metres.
    pub margin_floor_m: f64,
    /// How much the slope may vary within one reported segment.
    pub slope_tolerance: f64,
    /// Segments shorter than this are folded into a neighbour of the same
    /// ground.
    pub min_segment_m: f64,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            algorithm: Algorithm::AStar,
            reference_min_per_km: 3.0,
            snap_radius: 10,
            margin_frac: 0.5,
            margin_floor_m: 100.0,
            slope_tolerance: 0.05,
            min_segment_m: 25.0,
        }
    }
}

/// Why a leg could not be solved.
#[derive(Debug, Clone)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

// ---------------------------------------------------------------------------
// The heap

/// A binary min-heap of nodes by cost.
///
/// Its own rather than the standard library's, because which of two equally
/// cheap routes is found depends on the order equal keys come out in, and that
/// order is part of what this reproduces.
#[derive(Default)]
struct MinHeap {
    prio: Vec<f64>,
    node: Vec<usize>,
}

impl MinHeap {
    fn len(&self) -> usize {
        self.node.len()
    }

    fn push(&mut self, node: usize, priority: f64) {
        let mut i = self.node.len();
        self.prio.push(priority);
        self.node.push(node);
        while i > 0 {
            let parent = (i - 1) >> 1;
            if self.prio[parent] <= self.prio[i] {
                break;
            }
            self.swap(i, parent);
            i = parent;
        }
    }

    fn pop(&mut self) -> usize {
        let top = self.node[0];
        let last = self.node.len() - 1;
        self.swap(0, last);
        self.prio.pop();
        self.node.pop();
        let mut i = 0;
        loop {
            let l = 2 * i + 1;
            let r = l + 1;
            let mut smallest = i;
            if l < self.node.len() && self.prio[l] < self.prio[smallest] {
                smallest = l;
            }
            if r < self.node.len() && self.prio[r] < self.prio[smallest] {
                smallest = r;
            }
            if smallest == i {
                return top;
            }
            self.swap(i, smallest);
            i = smallest;
        }
    }

    fn swap(&mut self, a: usize, b: usize) {
        self.prio.swap(a, b);
        self.node.swap(a, b);
    }
}

// ---------------------------------------------------------------------------
// The window

/// A rectangular crop of the grid, with the elevations lined up to it.
struct Window {
    r0: usize,
    c0: usize,
    rows: usize,
    cols: usize,
    /// Speeds, one per window cell.
    run: Vec<f32>,
    /// Elevations, one per window cell; all zero when there is no DEM.
    elev: Vec<f32>,
    /// Ground metres per cell.
    cell_m: f64,
    has_elevation: bool,
}

impl Window {
    fn build(
        grid: &Grid,
        elevation: Option<&Elevation>,
        r0: usize,
        r1: usize,
        c0: usize,
        c1: usize,
    ) -> Window {
        let rows = r1 - r0;
        let cols = c1 - c0;
        let mut run = vec![0f32; rows * cols];
        let mut elev = vec![0f32; rows * cols];
        for r in 0..rows {
            let src = (r + r0) * grid.width + c0;
            run[r * cols..(r + 1) * cols].copy_from_slice(&grid.values[src..src + cols]);
            if let Some(elevation) = elevation {
                let y = grid.bounds.top() + ((r + r0) as f64 + 0.5) * grid.pixel_size;
                for c in 0..cols {
                    let x = grid.bounds.left() + ((c + c0) as f64 + 0.5) * grid.pixel_size;
                    elev[r * cols + c] = elevation.sample(x, y) as f32;
                }
            }
        }
        Window {
            r0,
            c0,
            rows,
            cols,
            run,
            elev,
            cell_m: grid.pixel_size * grid.meters_per_unit,
            has_elevation: elevation.is_some(),
        }
    }

    /// Whether a cell can be crossed at all: it has a speed, and -- where
    /// there is a DEM -- an elevation.
    fn passable(&self, r: i64, c: i64) -> bool {
        if r < 0 || c < 0 || r >= self.rows as i64 || c >= self.cols as i64 {
            return false;
        }
        let i = r as usize * self.cols + c as usize;
        let s = self.run[i];
        // Greater than zero, and not merely "not less" -- a cell the map
        // knows nothing about holds NaN, and NaN is not passable either.
        if !matches!(
            s.partial_cmp(&IMPASSABLE),
            Some(std::cmp::Ordering::Greater)
        ) {
            return false;
        }
        if self.has_elevation && !self.elev[i].is_finite() {
            return false;
        }
        true
    }

    /// The nearest cell to `(r, c)` that can be crossed, for a control which
    /// landed on something that cannot.
    fn nearest_passable(&self, r: i64, c: i64, max_radius: i64) -> Option<(i64, i64)> {
        if self.passable(r, c) {
            return Some((r, c));
        }
        let mut best = None;
        let mut best_dist = i64::MAX;
        for dr in -max_radius..=max_radius {
            for dc in -max_radius..=max_radius {
                if self.passable(r + dr, c + dc) {
                    let d = dr * dr + dc * dc;
                    if d < best_dist {
                        best_dist = d;
                        best = Some((r + dr, c + dc));
                    }
                }
            }
        }
        best
    }

    /// The elevation at a fractional cell position, interpolated between the
    /// four cell centres around it. See [`Elevation::sample`] for why.
    fn elevation_at(&self, rf: f64, cf: f64) -> f64 {
        let r0 = (rf.floor()).clamp(0.0, self.rows as f64 - 1.0) as usize;
        let c0 = (cf.floor()).clamp(0.0, self.cols as f64 - 1.0) as usize;
        let r1 = (r0 + 1).min(self.rows - 1);
        let c1 = (c0 + 1).min(self.cols - 1);
        let tr = (rf - r0 as f64).clamp(0.0, 1.0);
        let tc = (cf - c0 as f64).clamp(0.0, 1.0);
        let at = |r: usize, c: usize| f64::from(self.elev[r * self.cols + c]);
        let (v00, v01, v10, v11) = (at(r0, c0), at(r0, c1), at(r1, c0), at(r1, c1));
        if v00.is_finite() && v01.is_finite() && v10.is_finite() && v11.is_finite() {
            let top = v00 + (v01 - v00) * tc;
            let bottom = v10 + (v11 - v10) * tc;
            return top + (bottom - top) * tr;
        }
        let r = (rf.round()).clamp(0.0, self.rows as f64 - 1.0) as usize;
        let c = (cf.round()).clamp(0.0, self.cols as f64 - 1.0) as usize;
        f64::from(self.elev[r * self.cols + c])
    }
}

// ---------------------------------------------------------------------------
// The search

/// Which way the eight neighbours lie, straight ones first.
const NEIGHBOR_DR: [i64; 8] = [1, -1, 0, 0, 1, 1, -1, -1];
const NEIGHBOR_DC: [i64; 8] = [0, 0, 1, -1, 1, -1, 1, -1];

struct Search {
    came_from: Vec<i64>,
    cost: Vec<f64>,
    found: bool,
}

/// A* from `start` to `goal`, or Dijkstra when there is no heuristic to use.
fn search(
    w: &Window,
    ref_min_per_km: f64,
    start: usize,
    goal: usize,
    use_heuristic: bool,
) -> Search {
    let n = w.rows * w.cols;
    let mut cost = vec![f64::INFINITY; n];
    let mut came_from = vec![-1i64; n];
    let mut heap = MinHeap::default();

    // The heuristic has to be a cost no route could beat, or the first route
    // found need not be the cheapest: straight-line cell distance, at the best
    // speed anywhere in the window, on the friendliest slope there is.
    let mut min_cost_per_m = 0.0;
    if use_heuristic {
        let mut max_speed = 0f32;
        for &s in &w.run {
            if s > max_speed {
                max_speed = s;
            }
        }
        min_cost_per_m = if max_speed > 0.0 {
            ref_min_per_km / f64::from(max_speed)
        } else {
            ref_min_per_km
        };
    }
    let goal_r = (goal / w.cols) as i64;
    let goal_c = (goal % w.cols) as i64;
    let heuristic = |r: i64, c: i64| -> f64 {
        if !use_heuristic {
            return 0.0;
        }
        let dr = (r - goal_r).abs() as f64;
        let dc = (c - goal_c).abs() as f64;
        (dr.max(dc) + (SQRT2 - 1.0) * dr.min(dc)) * w.cell_m * min_cost_per_m
    };

    cost[start] = 0.0;
    heap.push(start, 0.0);

    while heap.len() > 0 {
        let current = heap.pop();
        if current == goal {
            return Search {
                came_from,
                cost,
                found: true,
            };
        }
        let r = (current / w.cols) as i64;
        let c = (current % w.cols) as i64;
        let s_here = w.run[current];
        let e_here = w.elev[current];

        for k in 0..8 {
            let nr = r + NEIGHBOR_DR[k];
            let nc = c + NEIGHBOR_DC[k];
            if !w.passable(nr, nc) {
                continue;
            }
            let ni = nr as usize * w.cols + nc as usize;
            let dist = (if k >= 4 { SQRT2 } else { 1.0 }) * w.cell_m;
            let mean_speed = (f64::from(s_here) + f64::from(w.run[ni])) / 2.0;
            let slope = (f64::from(w.elev[ni]) - f64::from(e_here)) / dist;
            let edge = dist * (ref_min_per_km / mean_speed) * tobler(slope);
            let new_cost = cost[current] + edge;
            if new_cost < cost[ni] {
                cost[ni] = new_cost;
                came_from[ni] = current as i64;
                heap.push(ni, new_cost + heuristic(nr, nc));
            }
        }
    }
    Search {
        came_from,
        cost,
        found: false,
    }
}

fn reconstruct(came_from: &[i64], goal: usize) -> Vec<usize> {
    let mut path = Vec::new();
    let mut node = goal as i64;
    while node >= 0 {
        path.push(node as usize);
        node = came_from[node as usize];
    }
    path.reverse();
    path
}

// ---------------------------------------------------------------------------
// Straightening, and what the route crosses

/// The cost of going straight between two cells, sampled at half a cell at a
/// time. Infinite if the line crosses something impassable.
fn segment_cost(w: &Window, ref_min_per_km: f64, a: usize, b: usize) -> f64 {
    let r1 = (a / w.cols) as f64;
    let c1 = (a % w.cols) as f64;
    let r2 = (b / w.cols) as f64;
    let c2 = (b % w.cols) as f64;
    let cell_len = (r2 - r1).hypot(c2 - c1);
    if cell_len == 0.0 {
        return 0.0;
    }
    let n = ((2.0 * (r2 - r1).abs().max((c2 - c1).abs())).ceil() as i64).max(1);
    let ds = (cell_len * w.cell_m) / n as f64;

    let mut total = 0.0;
    let mut prev_s = 0f32;
    let mut prev_e = 0.0;
    for k in 0..=n {
        let t = k as f64 / n as f64;
        let rf = r1 + t * (r2 - r1);
        let cf = c1 + t * (c2 - c1);
        let r = rf.round();
        let c = cf.round();
        if !w.passable(r as i64, c as i64) {
            return f64::INFINITY;
        }
        let s = w.run[r as usize * w.cols + c as usize];
        let e = w.elevation_at(rf, cf);
        if k > 0 {
            let slope = (e - prev_e) / ds;
            total +=
                ds * (ref_min_per_km / ((f64::from(prev_s) + f64::from(s)) / 2.0)) * tobler(slope);
        }
        prev_s = s;
        prev_e = e;
    }
    total
}

/// Pulls the cell-by-cell path straight: a vertex goes wherever the straight
/// line past it costs no more than the way through it did.
fn smooth(w: &Window, ref_min_per_km: f64, path: &[usize], cost: &[f64]) -> Vec<usize> {
    if path.len() <= 2 {
        return path.to_vec();
    }
    let mut smoothed = vec![path[0]];
    let mut anchor = path[0];
    for i in 1..path.len() - 1 {
        let candidate = path[i + 1];
        let seg = segment_cost(w, ref_min_per_km, anchor, candidate);
        let orig = cost[candidate] - cost[anchor];
        if seg.is_finite() && seg <= orig * 1.001 {
            continue;
        }
        smoothed.push(path[i]);
        anchor = path[i];
    }
    smoothed.push(path[path.len() - 1]);
    smoothed
}

/// One place along the route where it was measured.
struct Sample {
    /// The cell it falls in, for reading the ground off.
    i: usize,
    /// Where it falls, in fractional window cells.
    rf: f64,
    cf: f64,
    /// How far it is from the sample before it, in metres.
    ds: f64,
    /// The elevation there, interpolated rather than read off the cell.
    elev: f64,
}

/// Walks the route at half a cell at a time.
fn sample_path(w: &Window, path: &[usize]) -> Vec<Sample> {
    let mut samples = Vec::new();
    let mut first = true;
    for p in 0..path.len().saturating_sub(1) {
        let a = path[p];
        let b = path[p + 1];
        let r1 = (a / w.cols) as f64;
        let c1 = (a % w.cols) as f64;
        let r2 = (b / w.cols) as f64;
        let c2 = (b % w.cols) as f64;
        let cell_len = (r2 - r1).hypot(c2 - c1);
        if cell_len == 0.0 {
            continue;
        }
        let n = ((2.0 * (r2 - r1).abs().max((c2 - c1).abs())).ceil() as i64).max(1);
        let ds = (cell_len * w.cell_m) / n as f64;
        let start_k = if first { 0 } else { 1 };
        for k in start_k..=n {
            let t = k as f64 / n as f64;
            let rf = (r1 + t * (r2 - r1)).clamp(0.0, w.rows as f64 - 1.0);
            let cf = (c1 + t * (c2 - c1)).clamp(0.0, w.cols as f64 - 1.0);
            samples.push(Sample {
                i: rf.round() as usize * w.cols + cf.round() as usize,
                rf,
                cf,
                ds: if first && k == 0 { 0.0 } else { ds },
                elev: w.elevation_at(rf, cf),
            });
        }
        first = false;
    }
    samples
}

/// The slope at each sample, measured back over a stretch rather than to the
/// sample before it -- half a cell apart, single steps are mostly rounding.
fn windowed_slopes(samples: &[Sample], window_m: f64) -> Vec<f64> {
    let n = samples.len();
    let mut slopes = vec![0.0; n];
    let mut dist = vec![0.0; n];
    for i in 1..n {
        dist[i] = dist[i - 1] + samples[i].ds;
    }
    let mut j = 0usize;
    for i in 1..n {
        while j + 1 < i && dist[i] - dist[j + 1] >= window_m {
            j += 1;
        }
        let baseline = dist[i] - dist[j];
        if baseline > 0.0 {
            slopes[i] = (samples[i].elev - samples[j].elev) / baseline;
        }
    }
    slopes
}

/// One segment being accumulated, before it is worth reporting.
struct Accumulator {
    /// The code index the ground has here, or `-2` where the grid cannot say.
    key: i32,
    value: f32,
    length_m: f64,
    time_s: f64,
    elev_start: f64,
    elev_end: f64,
    slope_min: f64,
    slope_max: f64,
    points: Vec<Point>,
}

/// Breaks a route into stretches of one kind of ground at one steepness.
///
/// This is what makes a route readable: not four hundred half-cell steps, but
/// "180 m of track, then 60 m of fight uphill".
#[allow(clippy::too_many_arguments)]
fn summarize(
    w: &Window,
    grid: &Grid,
    ref_min_per_km: f64,
    path: &[usize],
    point_at: impl Fn(f64, f64) -> Point,
    slope_tolerance: f64,
    min_segment_m: f64,
) -> Vec<Segment> {
    let samples = sample_path(w, path);
    if samples.len() < 2 {
        return Vec::new();
    }
    let slopes = windowed_slopes(&samples, (4.0 * w.cell_m).max(8.0));

    let key_of = |i: usize| -> i32 {
        match grid.code_index {
            Some(codes) => {
                let r = i / w.cols + w.r0;
                let c = i % w.cols + w.c0;
                codes[r * grid.width + c]
            }
            None => -2,
        }
    };

    let mut segments: Vec<Segment> = Vec::new();
    let mut cur: Option<Accumulator> = None;

    // A route doubling back on itself would otherwise repeat a point.
    fn add_point(points: &mut Vec<Point>, pt: Point) {
        match points.last() {
            Some(last) if last.x == pt.x && last.y == pt.y => {}
            _ => points.push(pt),
        }
    }

    let flush =
        |cur: &mut Option<Accumulator>, segments: &mut Vec<Segment>, closing: Option<Point>| {
            if let Some(mut acc) = cur.take() {
                if acc.length_m > 0.0 {
                    if let Some(closing) = closing {
                        add_point(&mut acc.points, closing);
                    }
                    let rise = acc.elev_end - acc.elev_start;
                    segments.push(Segment {
                        length_m: acc.length_m,
                        avg_slope: rise / acc.length_m,
                        value: acc.value,
                        code: (acc.key >= 0)
                            .then(|| grid.codes.get(acc.key as usize).cloned())
                            .flatten(),
                        pace_min_per_km: acc.time_s / 60.0 / (acc.length_m / 1000.0),
                        time_s: acc.time_s,
                        path: acc.points,
                    });
                }
            }
        };

    for i in 1..samples.len() {
        let prev = &samples[i - 1];
        let next = &samples[i];
        let ds = next.ds;
        if ds <= 0.0 {
            continue;
        }
        let s_a = w.run[prev.i];
        let e_a = prev.elev;
        let from_pt = point_at(prev.rf, prev.cf);
        let key = key_of(prev.i);
        let mean_speed = ((f64::from(s_a) + f64::from(w.run[next.i])) / 2.0).max(1e-6);
        let step_slope = (next.elev - e_a) / ds;
        let step_time = ds * (ref_min_per_km / mean_speed) * tobler(step_slope) * 0.06;
        let local_slope = slopes[i];

        let mut start_new = match &cur {
            None => true,
            Some(acc) => acc.key != key,
        };
        if !start_new && slope_tolerance > 0.0 {
            if let Some(acc) = &cur {
                let new_min = acc.slope_min.min(local_slope);
                let new_max = acc.slope_max.max(local_slope);
                if new_max - new_min > slope_tolerance {
                    start_new = true;
                }
            }
        }

        if start_new {
            flush(&mut cur, &mut segments, Some(from_pt));
            cur = Some(Accumulator {
                key,
                value: s_a,
                length_m: 0.0,
                time_s: 0.0,
                elev_start: e_a,
                elev_end: e_a,
                slope_min: local_slope,
                slope_max: local_slope,
                points: vec![from_pt],
            });
        } else if let Some(acc) = &mut cur {
            add_point(&mut acc.points, from_pt);
            acc.slope_min = acc.slope_min.min(local_slope);
            acc.slope_max = acc.slope_max.max(local_slope);
        }
        if let Some(acc) = &mut cur {
            acc.length_m += ds;
            acc.time_s += step_time;
            acc.elev_end = next.elev;
        }
    }
    let last = &samples[samples.len() - 1];
    flush(&mut cur, &mut segments, Some(point_at(last.rf, last.cf)));

    merge_short(segments, min_segment_m)
}

/// Whether two stretches are the same ground, and so worth joining.
fn mergeable(a: &Segment, b: &Segment) -> bool {
    a.value == b.value && a.code == b.code
}

fn combine(a: &Segment, b: &Segment) -> Segment {
    let length_m = a.length_m + b.length_m;
    let rise = a.avg_slope * a.length_m + b.avg_slope * b.length_m;
    let time_s = a.time_s + b.time_s;
    let mut path = a.path.clone();
    let joins = match (a.path.last(), b.path.first()) {
        (Some(la), Some(fb)) => la.x == fb.x && la.y == fb.y,
        _ => false,
    };
    path.extend_from_slice(if joins { &b.path[1..] } else { &b.path[..] });
    let longer = if a.length_m >= b.length_m { a } else { b };
    Segment {
        length_m,
        avg_slope: if length_m > 0.0 { rise / length_m } else { 0.0 },
        value: a.value,
        code: longer.code.clone(),
        pace_min_per_km: if length_m > 0.0 {
            time_s / 60.0 / (length_m / 1000.0)
        } else {
            0.0
        },
        time_s,
        path,
    }
}

/// Folds a stretch too short to be worth reporting into whichever neighbour
/// it is most like -- but only into ground of its own kind.
fn merge_short(segments: Vec<Segment>, min_segment_m: f64) -> Vec<Segment> {
    if min_segment_m <= 0.0 || segments.len() < 2 {
        return segments;
    }
    let mut segs = segments;
    loop {
        let mut target: Option<usize> = None;
        for i in 0..segs.len() {
            if segs[i].length_m >= min_segment_m {
                continue;
            }
            let has_left = i > 0 && mergeable(&segs[i - 1], &segs[i]);
            let has_right = i + 1 < segs.len() && mergeable(&segs[i + 1], &segs[i]);
            if (has_left || has_right) && target.is_none_or(|t| segs[i].length_m < segs[t].length_m)
            {
                target = Some(i);
            }
        }
        let Some(target) = target else {
            return segs;
        };
        let has_left = target > 0 && mergeable(&segs[target - 1], &segs[target]);
        let has_right = target + 1 < segs.len() && mergeable(&segs[target + 1], &segs[target]);
        let mut merge_left = has_left;
        if has_left && has_right {
            // Whichever neighbour it runs at more nearly the same angle to.
            let left_diff = (segs[target - 1].avg_slope - segs[target].avg_slope).abs();
            let right_diff = (segs[target + 1].avg_slope - segs[target].avg_slope).abs();
            merge_left = left_diff <= right_diff;
        }
        if merge_left {
            let merged = combine(&segs[target - 1], &segs[target]);
            segs.splice(target - 1..target + 1, [merged]);
        } else {
            let merged = combine(&segs[target], &segs[target + 1]);
            segs.splice(target..target + 2, [merged]);
        }
    }
}

// ---------------------------------------------------------------------------
// Solving a leg

/// Whether the route runs along an edge of the window that is not an edge of
/// the map -- which means the window was too small and the real route may go
/// outside it.
fn touches_artificial_boundary(w: &Window, grid: &Grid, path: &[usize]) -> bool {
    let buffer = 2usize;
    let top = w.r0 > 0;
    let bottom = w.r0 + w.rows < grid.height;
    let left = w.c0 > 0;
    let right = w.c0 + w.cols < grid.width;
    if !top && !bottom && !left && !right {
        return false;
    }
    for &i in path {
        let r = i / w.cols;
        let c = i % w.cols;
        if top && r <= buffer {
            return true;
        }
        if bottom && r + 1 + buffer >= w.rows {
            return true;
        }
        if left && c <= buffer {
            return true;
        }
        if right && c + 1 + buffer >= w.cols {
            return true;
        }
    }
    false
}

/// Solves one leg: the fastest way from `start` to `goal`.
///
/// Fails where a control sits on ground nothing crosses and there is none
/// nearby, and where the two ends are not connected by passable ground at all.
pub fn solve_leg(
    grid: &Grid,
    elevation: Option<&Elevation>,
    start: Point,
    goal: Point,
    options: &Options,
) -> Result<Leg, Error> {
    let ref_min_per_km = options.reference_min_per_km;
    let cell_m = grid.pixel_size * grid.meters_per_unit;
    let to_cell = |p: Point| -> (i64, i64) {
        (
            ((p.y - grid.bounds.top()) / grid.pixel_size).floor() as i64,
            ((p.x - grid.bounds.left()) / grid.pixel_size).floor() as i64,
        )
    };
    let (sr, sc) = to_cell(start);
    let (gr, gc) = to_cell(goal);
    let straight_m = (goal.x - start.x).hypot(goal.y - start.y) * grid.meters_per_unit;

    let no_way_through = || {
        Error("A control sits on impassable or unmapped ground and no passable cell was found nearby.".to_string())
    };
    let not_connected = || {
        Error(
            "No route found — the leg's endpoints are not connected by passable terrain."
                .to_string(),
        )
    };

    if options.algorithm == Algorithm::Dijkstra {
        let w = Window::build(grid, elevation, 0, grid.height, 0, grid.width);
        let s_local = w
            .nearest_passable(sr, sc, options.snap_radius)
            .ok_or_else(no_way_through)?;
        let g_local = w
            .nearest_passable(gr, gc, options.snap_radius)
            .ok_or_else(no_way_through)?;
        let start_idx = s_local.0 as usize * w.cols + s_local.1 as usize;
        let goal_idx = g_local.0 as usize * w.cols + g_local.1 as usize;
        let found = search(&w, ref_min_per_km, start_idx, goal_idx, false);
        if !found.found {
            return Err(not_connected());
        }
        let path = reconstruct(&found.came_from, goal_idx);
        return Ok(build_leg(
            &w,
            grid,
            ref_min_per_km,
            &path,
            &found.cost,
            straight_m,
            options,
        ));
    }

    // The window starts big enough to hold a reasonable detour, and doubles
    // whenever the route it found ran along its own edge.
    let mut margin = (options.margin_frac * ((sr - gr) as f64).hypot((sc - gc) as f64))
        .max(if cell_m > 0.0 {
            options.margin_floor_m / cell_m
        } else {
            0.0
        })
        .max((options.snap_radius + 2) as f64)
        .floor() as i64;

    loop {
        let r0 = (sr.min(gr) - margin).max(0) as usize;
        let r1 = ((sr.max(gr) + margin + 1).max(0) as usize).min(grid.height);
        let c0 = (sc.min(gc) - margin).max(0) as usize;
        let c1 = ((sc.max(gc) + margin + 1).max(0) as usize).min(grid.width);
        let w = Window::build(grid, elevation, r0, r1, c0, c1);
        let full_map = r0 == 0 && c0 == 0 && r1 == grid.height && c1 == grid.width;

        let s_local = w
            .nearest_passable(sr - r0 as i64, sc - c0 as i64, options.snap_radius)
            .ok_or_else(no_way_through)?;
        let g_local = w
            .nearest_passable(gr - r0 as i64, gc - c0 as i64, options.snap_radius)
            .ok_or_else(no_way_through)?;
        let start_idx = s_local.0 as usize * w.cols + s_local.1 as usize;
        let goal_idx = g_local.0 as usize * w.cols + g_local.1 as usize;

        let found = search(&w, ref_min_per_km, start_idx, goal_idx, true);
        if found.found {
            let raw = reconstruct(&found.came_from, goal_idx);
            if full_map || !touches_artificial_boundary(&w, grid, &raw) {
                return Ok(build_leg(
                    &w,
                    grid,
                    ref_min_per_km,
                    &raw,
                    &found.cost,
                    straight_m,
                    options,
                ));
            }
        } else if full_map {
            return Err(not_connected());
        }
        margin = (margin + 1).max(margin * 2);
    }
}

/// Solves a leg that has to pass through somewhere: one solve per stretch,
/// joined end to end.
pub fn solve_leg_via(
    grid: &Grid,
    elevation: Option<&Elevation>,
    start: Point,
    via: &[Point],
    goal: Point,
    options: &Options,
) -> Result<Leg, Error> {
    let mut points = Vec::with_capacity(via.len() + 2);
    points.push(start);
    points.extend_from_slice(via);
    points.push(goal);

    let mut parts = Vec::new();
    for pair in points.windows(2) {
        parts.push(solve_leg(grid, elevation, pair[0], pair[1], options)?);
    }
    let straight_m = (goal.x - start.x).hypot(goal.y - start.y) * grid.meters_per_unit;
    Ok(combine_legs(parts, straight_m))
}

fn combine_legs(parts: Vec<Leg>, straight_m: f64) -> Leg {
    let mut path: Vec<Point> = Vec::new();
    let mut segments = Vec::new();
    let mut distance_m = 0.0;
    let mut time_s = 0.0;
    let mut climb_m = 0.0;

    for part in parts {
        distance_m += part.distance_m;
        time_s += part.time_s;
        climb_m += part.climb_m;
        let joins = match (path.last(), part.path.first()) {
            (Some(last), Some(first)) => last.x == first.x && last.y == first.y,
            _ => false,
        };
        path.extend_from_slice(if joins {
            &part.path[1..]
        } else {
            &part.path[..]
        });
        segments.extend(part.segments);
    }

    Leg {
        path,
        distance_m,
        time_s,
        climb_m,
        straight_m,
        segments,
    }
}

/// Turns a found path into the leg the caller gets: straightened, measured,
/// and broken into the stretches it crosses.
fn build_leg(
    w: &Window,
    grid: &Grid,
    ref_min_per_km: f64,
    raw_path: &[usize],
    cost: &[f64],
    straight_m: f64,
    options: &Options,
) -> Leg {
    let path = smooth(w, ref_min_per_km, raw_path, cost);

    // The time is re-measured along the straightened route rather than taken
    // from the search, which counted the staircase. Where a straightened
    // segment clips the corner of something impassable and so cannot be
    // measured, the search's own cost for that step stands in.
    let mut leg_cost = 0.0;
    for pair in path.windows(2) {
        let sc = segment_cost(w, ref_min_per_km, pair[0], pair[1]);
        leg_cost += if sc.is_finite() {
            sc
        } else {
            cost[pair[1]] - cost[pair[0]]
        };
    }

    let cell_to_point = |i: usize| -> Point {
        let r = i / w.cols + w.r0;
        let c = i % w.cols + w.c0;
        Point {
            x: grid.bounds.left() + (c as f64 + 0.5) * grid.pixel_size,
            y: grid.bounds.top() + (r as f64 + 0.5) * grid.pixel_size,
        }
    };
    let point_at = |rf: f64, cf: f64| -> Point {
        Point {
            x: grid.bounds.left() + (cf + w.c0 as f64 + 0.5) * grid.pixel_size,
            y: grid.bounds.top() + (rf + w.r0 as f64 + 0.5) * grid.pixel_size,
        }
    };

    let mut distance_m = 0.0;
    for pair in path.windows(2) {
        let r1 = (pair[0] / w.cols) as f64;
        let c1 = (pair[0] % w.cols) as f64;
        let r2 = (pair[1] / w.cols) as f64;
        let c2 = (pair[1] % w.cols) as f64;
        distance_m += (r2 - r1).hypot(c2 - c1) * w.cell_m;
    }

    // Climb counts the rises only, sampled as finely as the route is.
    let mut climb_m = 0.0;
    if w.has_elevation {
        let samples = sample_path(w, &path);
        for i in 1..samples.len() {
            let rise = samples[i].elev - samples[i - 1].elev;
            if rise > 0.0 {
                climb_m += rise;
            }
        }
    }

    Leg {
        path: path.iter().map(|&i| cell_to_point(i)).collect(),
        distance_m,
        time_s: leg_cost * 0.06,
        climb_m,
        straight_m,
        segments: summarize(
            w,
            grid,
            ref_min_per_km,
            &path,
            point_at,
            options.slope_tolerance,
            options.min_segment_m,
        ),
    }
}

/// Roughly how long a leg takes as the crow flies, for a map with no grid to
/// search: reference pace over open forest, on flat ground.
pub fn estimate_straight_time_s(
    distance_m: f64,
    reference_min_per_km: f64,
    background_speed: f64,
) -> f64 {
    distance_m * (reference_min_per_km / background_speed) * tobler(0.0) * 0.06
}
