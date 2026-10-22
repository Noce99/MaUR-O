//! Lines sharing a street drawn side by side: the bus and train lines of a
//! map laid out as bundles, the way a transit map draws them, rather than
//! one over the other.
//!
//! Everything here is on the paper, in mm, and knows nothing of where the
//! routes or the streets come from: [`crate::osm_convert`] gives it the
//! street and rail network as [`Segment`]s and the routes as polylines, and
//! takes back where to draw each line and its stops.
//!
//! # Which lines share a street
//!
//! Two lines on the same street are two routes that only roughly coincide —
//! a timetable's shapes are traced one by one, a few metres apart, with
//! their own vertices. So they are compared on the network instead
//! ([`lay_out`]): each route is resampled every [`Settings::sample`] mm and
//! every sample matched to the nearest segment of the network running the
//! same way, within [`Settings::tolerance`]. A segment many lines are
//! matched to is a bundle of that many lines, and each of them is drawn
//! along the segment itself, so that all of them are parallel to it and to
//! each other.
//!
//! # Side by side
//!
//! In a bundle the lines keep one order — by their rank, which is their
//! number — left to right across a direction the bundle is given. That
//! direction is carried straight on from segment to segment, through a
//! junction too where the street goes on, so that the order does not turn
//! over, and the lines cross, wherever a segment happens to have been
//! mapped the other way. A line is shifted from the segment by its place in
//! the order, its width apart from the next, the bundle centred on the
//! street; the shift changes smoothly, over [`Settings::smooth`] mm either
//! way, where the bundle grows or shrinks.
//!
//! # Width
//!
//! A bundle is never wider than [`Settings::cap`]: where its lines at their
//! full width would make it wider, all of them are drawn thinner, by one of
//! the [`WIDTH_FACTORS`], which is why a line comes back in pieces, each
//! drawn at one width ([`Laid::pieces`]).
//!
//! # Stops
//!
//! A stop on a bundle is one bar across all of it ([`stops`]), rather
//! than a circle on each line: the stops of several lines at the same place
//! are the same bar, and so are those a few mm apart along it, the two
//! sides of a street. An arrow past either end says which way the buses
//! stopping on that side run. A stop of the way back on a parallel street
//! goes across whichever bundle runs there; one with no bundle at all is a
//! circle, with an arrow on the right of the way buses run.

use std::collections::{HashMap, HashSet};

/// The widths a line may be drawn at, as fractions of its full width: the
/// first is the full width, and a bundle too wide for the cap takes the
/// first which brings it under, or the last.
pub const WIDTH_FACTORS: [f64; 8] = [1.0, 0.8, 2.0 / 3.0, 0.533, 0.4, 1.0 / 3.0, 0.267, 0.2];

/// One stretch of street or track between two nodes, on the paper.
#[derive(Clone, Debug)]
pub struct Segment {
    /// Where it starts, in mm.
    pub a: (f64, f64),
    /// Where it ends, in mm.
    pub b: (f64, f64),
    /// The ids of the nodes at its two ends, which is how two segments are
    /// known to meet.
    pub nodes: (i64, i64),
    /// Whether it is a railway rather than a street: a train is matched to
    /// the one, a bus to the other.
    pub rail: bool,
}

/// The network routes are matched to, indexed for finding the segments
/// near a point.
pub struct Network {
    segments: Vec<Segment>,
    cell: f64,
    grid: HashMap<(i64, i64), Vec<usize>>,
}

impl Network {
    /// The network made of `segments`, indexed in cells `cell` mm across:
    /// about the distance a point is matched within.
    pub fn new(segments: Vec<Segment>, cell: f64) -> Network {
        let mut grid: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
        for (i, s) in segments.iter().enumerate() {
            let (x0, x1) = (s.a.0.min(s.b.0), s.a.0.max(s.b.0));
            let (y0, y1) = (s.a.1.min(s.b.1), s.a.1.max(s.b.1));
            for cx in (x0 / cell).floor() as i64..=(x1 / cell).floor() as i64 {
                for cy in (y0 / cell).floor() as i64..=(y1 / cell).floor() as i64 {
                    grid.entry((cx, cy)).or_default().push(i);
                }
            }
        }
        Network {
            segments,
            cell,
            grid,
        }
    }

    /// The segments of the network.
    pub fn segments(&self) -> &[Segment] {
        &self.segments
    }

    /// The segments with some part in the cells around `p`, each once.
    fn near(&self, p: (f64, f64)) -> Vec<usize> {
        let (cx, cy) = (
            (p.0 / self.cell).floor() as i64,
            (p.1 / self.cell).floor() as i64,
        );
        let mut found: Vec<usize> = Vec::new();
        for dx in -1..=1 {
            for dy in -1..=1 {
                if let Some(cell) = self.grid.get(&(cx + dx, cy + dy)) {
                    found.extend(cell);
                }
            }
        }
        found.sort_unstable();
        found.dedup();
        found
    }

    /// Whether two segments meet at a node.
    fn connected(&self, i: usize, j: usize) -> bool {
        let (a, b) = (&self.segments[i].nodes, &self.segments[j].nodes);
        a.0 == b.0 || a.0 == b.1 || a.1 == b.0 || a.1 == b.1
    }
}

/// How a route is matched and its lines laid out, all lengths in mm on the
/// paper.
#[derive(Clone, Copy, Debug)]
pub struct Settings {
    /// How far apart a route is sampled.
    pub sample: f64,
    /// How far from a segment a sample may be and still be on it.
    pub tolerance: f64,
    /// The most a route's direction may differ from a segment's, in
    /// radians, for the route to be along it rather than across it.
    pub max_angle: f64,
    /// How much nearer a segment not joined to the last one matched has to
    /// be to be taken instead: a route keeps to its street rather than
    /// jumping to a parallel one that comes a little closer.
    pub jump: f64,
    /// How far either way a line's shift is averaged over, so that it moves
    /// across smoothly where its bundle changes.
    pub smooth: f64,
    /// How far either way the middle of a bundle is averaged over: what
    /// rounds its corners, so that a line shifted to the inside of one does
    /// not fold back on itself. About half the widest a bundle may be.
    pub round: f64,
    /// The widest a bundle may be.
    pub cap: f64,
    /// How far from a route a stop may be and still be one of its stops on
    /// the bundle.
    pub stop_reach: f64,
    /// How far a stop's bar reaches past either side of the bundle.
    pub bar_overhang: f64,
    /// How close two stops on a bundle, or two circles, may be before they
    /// are drawn as one.
    pub stop_merge: f64,
    /// How far from the middle of its bundle a stop has to be to be on one
    /// side of it rather than in the middle.
    pub stop_side: f64,
    /// How far past the end of a bar the middle of its arrow is.
    pub arrow_gap: f64,
    /// How long an arrow is.
    pub arrow_length: f64,
}

/// One route to lay out: a stretch of one line, on the paper.
#[derive(Clone, Debug)]
pub struct RouteInput {
    /// Which line it is a stretch of; two stretches of one line count once
    /// in a bundle.
    pub line: usize,
    /// Where the line goes in a bundle, smallest leftmost.
    pub rank: usize,
    /// Whether it runs on rails.
    pub rail: bool,
    /// The line's full width, in mm.
    pub width: f64,
    /// The route, in mm.
    pub points: Vec<(f64, f64)>,
}

/// One route laid out.
#[derive(Clone, Debug, Default)]
pub struct Laid {
    /// Where to draw it, sample by sample: shifted to its place in its
    /// bundle.
    pub points: Vec<(f64, f64)>,
    /// The middle of the bundle at each sample: the street itself, where the
    /// route is on one.
    pub centre: Vec<(f64, f64)>,
    /// Which of [`WIDTH_FACTORS`] it is drawn at, sample by sample.
    pub width_class: Vec<usize>,
    /// The segment each sample is matched to, if any.
    pub segment: Vec<Option<usize>>,
    /// How wide the whole bundle is at each sample, in mm.
    pub bundle_width: Vec<f64>,
}

impl Laid {
    /// The route in pieces of one width each, as `(width class, points)`,
    /// two neighbouring pieces sharing the point where one ends and the
    /// next begins.
    pub fn pieces(&self) -> Vec<(usize, Vec<(f64, f64)>)> {
        let mut pieces: Vec<(usize, Vec<(f64, f64)>)> = Vec::new();
        for (i, &p) in self.points.iter().enumerate() {
            let class = self.width_class[i];
            match pieces.last_mut() {
                Some((c, points)) if *c == class => points.push(p),
                Some((_, points)) => {
                    let shared = *points.last().expect("a piece has points");
                    pieces.push((class, vec![shared, p]));
                }
                None => pieces.push((class, vec![p])),
            }
        }
        pieces.retain(|(_, points)| points.len() >= 2);
        for (_, points) in &mut pieces {
            *points = simplify(points, 0.02);
        }
        pieces
    }
}

/// `points` resampled every `step` mm along them, ends included.
fn resample(points: &[(f64, f64)], step: f64) -> Vec<(f64, f64)> {
    let mut out = vec![points[0]];
    let mut carry = 0.0;
    for pair in points.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let length = (b.0 - a.0).hypot(b.1 - a.1);
        let mut at = step - carry;
        while at < length {
            let t = at / length;
            out.push((a.0 + t * (b.0 - a.0), a.1 + t * (b.1 - a.1)));
            at += step;
        }
        carry = (carry + length) % step;
    }
    let last = *points.last().expect("a route has points");
    if out.last() != Some(&last) {
        out.push(last);
    }
    out
}

/// The unit direction of `points` at sample `i`, taken over a few samples
/// either side so that one short wiggle does not turn it.
fn direction(points: &[(f64, f64)], i: usize, reach: usize) -> (f64, f64) {
    let lo = i.saturating_sub(reach);
    let hi = (i + reach).min(points.len() - 1);
    let (dx, dy) = (points[hi].0 - points[lo].0, points[hi].1 - points[lo].1);
    let length = dx.hypot(dy);
    if length > 0.0 {
        (dx / length, dy / length)
    } else {
        (1.0, 0.0)
    }
}

/// The point of segment `s` nearest `p`, and how far it is.
fn foot(s: &Segment, p: (f64, f64)) -> ((f64, f64), f64) {
    let (dx, dy) = (s.b.0 - s.a.0, s.b.1 - s.a.1);
    let length2 = dx * dx + dy * dy;
    let t = if length2 > 0.0 {
        (((p.0 - s.a.0) * dx + (p.1 - s.a.1) * dy) / length2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let f = (s.a.0 + t * dx, s.a.1 + t * dy);
    (f, (p.0 - f.0).hypot(p.1 - f.1))
}

/// The unit direction of segment `s`, from `a` to `b`.
fn heading(s: &Segment) -> (f64, f64) {
    let (dx, dy) = (s.b.0 - s.a.0, s.b.1 - s.a.1);
    let length = dx.hypot(dy).max(1e-12);
    (dx / length, dy / length)
}

/// The samples of one route and the segments they are matched to.
struct Matched {
    samples: Vec<(f64, f64)>,
    segment: Vec<Option<usize>>,
    centre: Vec<(f64, f64)>,
}

fn match_route(network: &Network, route: &RouteInput, settings: &Settings) -> Matched {
    let samples = resample(&route.points, settings.sample);
    let reach = ((1.0 / settings.sample).ceil() as usize).max(1);
    let cos_max = settings.max_angle.cos();
    let mut segment: Vec<Option<usize>> = Vec::with_capacity(samples.len());
    let mut previous: Option<usize> = None;
    for i in 0..samples.len() {
        let p = samples[i];
        let t = direction(&samples, i, reach);
        let mut best: Option<(f64, usize)> = None;
        for s in network.near(p) {
            let seg = &network.segments[s];
            if seg.rail != route.rail {
                continue;
            }
            let (_, distance) = foot(seg, p);
            if distance > settings.tolerance {
                continue;
            }
            let h = heading(seg);
            if (h.0 * t.0 + h.1 * t.1).abs() < cos_max {
                continue;
            }
            let score = match previous {
                Some(prev) if prev != s && !network.connected(prev, s) => distance + settings.jump,
                _ => distance,
            };
            if best.is_none_or(|(b, _)| score < b) {
                best = Some((score, s));
            }
        }
        let matched = best.map(|(_, s)| s);
        if matched.is_some() {
            previous = matched;
        }
        segment.push(matched);
    }
    // A run of one or two samples on some other segment, with the same
    // segment either side of it, is a slip at a junction: undone.
    let mut i = 0;
    while i < segment.len() {
        let mut j = i;
        while j < segment.len() && segment[j] == segment[i] {
            j += 1;
        }
        if i > 0 && j < segment.len() && j - i <= 2 && segment[i - 1] == segment[j] {
            for k in i..j {
                segment[k] = segment[j];
            }
        }
        i = j;
    }
    let centre = samples
        .iter()
        .zip(&segment)
        .map(|(&p, s)| match s {
            Some(s) => foot(&network.segments[*s], p).0,
            None => p,
        })
        .collect();
    Matched {
        samples,
        segment,
        centre,
    }
}

/// The direction each used segment is given, +1 along `a` to `b` and -1
/// against: carried on from segment to segment so that a bundle keeps its
/// left and its right. At a junction it goes on to the segment sharing the
/// most lines with the last — round a corner too, where the whole bundle
/// turns it — or, where none shares any, straight on within a third of a
/// right angle.
fn orientations(network: &Network, used: &HashMap<usize, Vec<usize>>) -> HashMap<usize, f64> {
    let mut at_node: HashMap<i64, Vec<usize>> = HashMap::new();
    for &s in used.keys() {
        let n = network.segments[s].nodes;
        at_node.entry(n.0).or_default().push(s);
        at_node.entry(n.1).or_default().push(s);
    }
    let mut order: Vec<usize> = used.keys().copied().collect();
    // The busiest first: they set the direction of the streets the others
    // join.
    order.sort_by_key(|s| (std::cmp::Reverse(used[s].len()), *s));
    let mut orient: HashMap<usize, f64> = HashMap::new();
    let straight = (30.0f64).to_radians().cos();
    for start in order {
        if orient.contains_key(&start) {
            continue;
        }
        orient.insert(start, 1.0);
        // Forwards from the end it is given, then backwards from its start.
        for forwards in [true, false] {
            let mut current = start;
            loop {
                let seg = &network.segments[current];
                let sign = orient[&current];
                let along = (sign > 0.0) == forwards;
                let node = if along { seg.nodes.1 } else { seg.nodes.0 };
                let h = heading(seg);
                let going = if forwards {
                    (h.0 * sign, h.1 * sign)
                } else {
                    (-h.0 * sign, -h.1 * sign)
                };
                let next = at_node
                    .get(&node)
                    .into_iter()
                    .flatten()
                    .copied()
                    .filter(|s| *s != current && !orient.contains_key(s))
                    .filter_map(|s| {
                        let other = &network.segments[s];
                        let out = heading(other);
                        // Away from the node.
                        let away = if other.nodes.0 == node {
                            out
                        } else {
                            (-out.0, -out.1)
                        };
                        let cos = going.0 * away.0 + going.1 * away.1;
                        let shared = used[&s]
                            .iter()
                            .filter(|l| used[&current].contains(l))
                            .count();
                        // Never back the way it came.
                        let goes_on = cos > -0.5 && (shared > 0 || cos >= straight);
                        goes_on.then_some((shared, cos, s, other.nodes.0 == node))
                    })
                    .max_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
                let Some((_, _, s, leaves_from_a)) = next else {
                    break;
                };
                // Going forwards the next one leaves the node; backwards it
                // arrives at it.
                let sign = if leaves_from_a == forwards { 1.0 } else { -1.0 };
                orient.insert(s, sign);
                current = s;
            }
        }
    }
    orient
}

/// Every route laid out, in the order given.
pub fn lay_out(network: &Network, routes: &[RouteInput], settings: &Settings) -> Vec<Laid> {
    let matched: Vec<Matched> = routes
        .iter()
        .map(|r| match_route(network, r, settings))
        .collect();

    // Which lines, in rank order, each segment carries.
    let mut used: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut seen: HashSet<(usize, usize)> = HashSet::new();
    for (route, m) in routes.iter().zip(&matched) {
        for s in m.segment.iter().flatten() {
            if seen.insert((*s, route.line)) {
                used.entry(*s).or_default().push(route.line);
            }
        }
    }
    let rank_of: HashMap<usize, (usize, f64)> =
        routes.iter().map(|r| (r.line, (r.rank, r.width))).collect();
    for lines in used.values_mut() {
        lines.sort_by_key(|l| (rank_of[l].0, *l));
    }
    let orient = orientations(network, &used);

    // A segment's width class: the first that keeps its bundle within the
    // cap, at the full width of its widest line.
    let class_of = |s: usize| -> (usize, f64) {
        let lines = &used[&s];
        let full = lines.iter().map(|l| rank_of[l].1).fold(0.0f64, f64::max);
        let k = lines.len() as f64;
        let class = WIDTH_FACTORS
            .iter()
            .position(|f| full * f * k <= settings.cap + 1e-9)
            .unwrap_or(WIDTH_FACTORS.len() - 1);
        (class, full * WIDTH_FACTORS[class])
    };

    routes
        .iter()
        .zip(matched)
        .map(|(route, m)| {
            let n = m.samples.len();
            let reach = ((1.0 / settings.sample).ceil() as usize).max(1);
            let mut target = vec![0.0; n];
            let mut width_class = vec![0; n];
            let mut bundle_width = vec![route.width; n];
            for i in 0..n {
                let Some(s) = m.segment[i] else {
                    continue;
                };
                let lines = &used[&s];
                let (class, width) = class_of(s);
                let k = lines.len();
                let slot = lines
                    .iter()
                    .position(|&l| l == route.line)
                    .expect("a route's own segment carries it");
                let across = (slot as f64 - (k as f64 - 1.0) / 2.0) * width;
                // The bundle's direction against the route's own: a route
                // running the other way has the bundle's right on its left.
                let h = heading(&network.segments[s]);
                let o = orient.get(&s).copied().unwrap_or(1.0);
                let t = direction(&m.centre, i, reach);
                let same_way = (h.0 * o * t.0 + h.1 * o * t.1) >= 0.0;
                target[i] = if same_way { across } else { -across };
                width_class[i] = class;
                bundle_width[i] = width * k as f64;
            }

            // The shift averaged over `smooth` either way.
            let mut along = vec![0.0; n];
            for i in 1..n {
                let (a, b) = (m.centre[i - 1], m.centre[i]);
                along[i] = along[i - 1] + (b.0 - a.0).hypot(b.1 - a.1);
            }
            let centre = rounded(&m.centre, &along, settings.round);
            let mut shift = vec![0.0; n];
            let (mut lo, mut hi, mut sum) = (0usize, 0usize, 0.0);
            for i in 0..n {
                while hi < n && along[hi] <= along[i] + settings.smooth {
                    sum += target[hi];
                    hi += 1;
                }
                while along[lo] < along[i] - settings.smooth {
                    sum -= target[lo];
                    lo += 1;
                }
                shift[i] = sum / (hi - lo) as f64;
            }

            let points = (0..n)
                .map(|i| {
                    let t = direction(&centre, i, reach);
                    // The right of the direction of travel, the paper's y
                    // pointing down.
                    let right = (-t.1, t.0);
                    (
                        centre[i].0 + shift[i] * right.0,
                        centre[i].1 + shift[i] * right.1,
                    )
                })
                .collect();
            Laid {
                points,
                centre,
                width_class,
                segment: m.segment,
                bundle_width,
            }
        })
        .collect()
}

/// `points` averaged over `reach` either way along them (`along` being how
/// far along each one is), a reach shortened near either end to what lies
/// on both sides: which rounds a corner off and leaves a straight stretch,
/// and both ends, where they are.
fn rounded(points: &[(f64, f64)], along: &[f64], reach: f64) -> Vec<(f64, f64)> {
    let n = points.len();
    let mut sum_x = vec![0.0; n + 1];
    let mut sum_y = vec![0.0; n + 1];
    for (i, p) in points.iter().enumerate() {
        sum_x[i + 1] = sum_x[i] + p.0;
        sum_y[i + 1] = sum_y[i] + p.1;
    }
    let total = along[n - 1];
    (0..n)
        .map(|i| {
            let r = reach.min(along[i]).min(total - along[i]);
            let lo = along.partition_point(|&a| a < along[i] - r);
            let hi = along.partition_point(|&a| a <= along[i] + r);
            let count = (hi - lo) as f64;
            (
                (sum_x[hi] - sum_x[lo]) / count,
                (sum_y[hi] - sum_y[lo]) / count,
            )
        })
        .collect()
}

/// A stop of a line on the paper, and the way the line runs through it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StopInput {
    /// Where it is, in mm.
    pub at: (f64, f64),
    /// The unit direction the line runs in there, where known.
    pub heading: Option<(f64, f64)>,
}

/// An arrow beside a stop, pointing the way the buses stopping there run.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Arrow {
    /// Its middle, in mm.
    pub at: (f64, f64),
    /// The unit direction it points in.
    pub heading: (f64, f64),
}

/// Stops drawn as a bar across their bundle: those of every line there
/// within [`Settings::stop_merge`] of each other along it, the two of a
/// street's opposite sides among them.
#[derive(Clone, Debug, PartialEq)]
pub struct Bar {
    /// Its two ends, in mm.
    pub ends: [(f64, f64); 2],
    /// Its middle, in mm.
    pub centre: (f64, f64),
    /// An arrow past the end on the side of the street each stop is on,
    /// along the bundle the way buses run there: two, pointing opposite
    /// ways, for a pair of stops either side of the street.
    pub arrows: Vec<Arrow>,
}

/// Stops with no bundle to cross, drawn as a circle: those of every line
/// within [`Settings::stop_merge`] of each other.
#[derive(Clone, Debug, PartialEq)]
pub struct Circle {
    /// Its middle, in mm.
    pub centre: (f64, f64),
    /// The lines stopping there, each once, in order.
    pub lines: Vec<usize>,
    /// The unit directions the buses stopping there run in, each once: an
    /// arrow goes on the right of each, as buses stop on the right.
    pub headings: Vec<(f64, f64)>,
}

/// Where the stops go: the bars and the circles, and where each line's own
/// stops ended up, in mm.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stops {
    /// The stops on bundles.
    pub bars: Vec<Bar>,
    /// The stops off every bundle.
    pub circles: Vec<Circle>,
    /// For each line, where its stops are drawn: the middle of the bar, or
    /// the stop itself in a circle.
    pub drawn_at: HashMap<usize, Vec<(f64, f64)>>,
}

/// One stop on a bundle, before the stops close together are merged.
struct OnBundle {
    /// The bundle's middle there, its direction, and half the bar's length.
    centre: (f64, f64),
    tangent: (f64, f64),
    half: f64,
    /// Which side of the bundle the stop is on: +1 to the right of
    /// `tangent`, -1 to the left.
    side: Option<f64>,
    heading: Option<(f64, f64)>,
}

fn dot(a: (f64, f64), b: (f64, f64)) -> f64 {
    a.0 * b.0 + a.1 * b.1
}

/// To the right of `t`: the paper's y points down.
fn right_of(t: (f64, f64)) -> (f64, f64) {
    (-t.1, t.0)
}

/// Lays out the stops of each line (`stops_of[line]`).
///
/// A stop within [`Settings::stop_reach`] of its line's own route goes on
/// that route's bundle; one which is not — a stop of the way back, on a
/// parallel street — on any other line's bundle there is; the rest are
/// circles. Stops on one bundle less than [`Settings::stop_merge`] apart are
/// one bar, as are circles that close.
pub fn stops(
    routes: &[RouteInput],
    laid: &[Laid],
    stops_of: &HashMap<usize, Vec<StopInput>>,
    settings: &Settings,
) -> Stops {
    let reach = ((1.0 / settings.sample).ceil() as usize).max(1);
    let mut bars: Vec<Vec<OnBundle>> = Vec::new();
    let mut loose: Vec<Vec<(usize, StopInput)>> = Vec::new();
    let mut drawn_at: HashMap<usize, Vec<(f64, f64)>> = HashMap::new();
    let mut lines: Vec<usize> = stops_of.keys().copied().collect();
    lines.sort_unstable();
    for line in lines {
        for &stop in &stops_of[&line] {
            // The nearest sample of a route: the line's own, or any.
            let nearest = |own: bool| {
                let mut best: Option<(f64, usize, usize)> = None;
                for (r, (route, l)) in routes.iter().zip(laid).enumerate() {
                    if own && route.line != line {
                        continue;
                    }
                    for (i, c) in l.centre.iter().enumerate() {
                        let d = (c.0 - stop.at.0).hypot(c.1 - stop.at.1);
                        if d <= settings.stop_reach && best.is_none_or(|(b, _, _)| d < b) {
                            best = Some((d, r, i));
                        }
                    }
                }
                best
            };
            let Some((_, r, i)) = nearest(true).or_else(|| nearest(false)) else {
                drawn_at.entry(line).or_default().push(stop.at);
                let near = loose.iter_mut().find(|circle| {
                    let first = circle[0].1.at;
                    (first.0 - stop.at.0).hypot(first.1 - stop.at.1) < settings.stop_merge
                });
                match near {
                    Some(circle) => circle.push((line, stop)),
                    None => loose.push(vec![(line, stop)]),
                }
                continue;
            };
            let l = &laid[r];
            let tangent = direction(&l.centre, i, reach);
            let centre = l.centre[i];
            let half = l.bundle_width[i] / 2.0 + settings.bar_overhang;
            // The side of the street the stop is on, or where it is on the
            // middle of it, the right of the way buses run.
            let offset = (stop.at.0 - centre.0, stop.at.1 - centre.1);
            let lateral = dot(offset, right_of(tangent));
            let side = if lateral.abs() > settings.stop_side {
                Some(lateral.signum())
            } else {
                stop.heading.map(|h| dot(h, tangent).signum())
            };
            drawn_at.entry(line).or_default().push(centre);
            let stop = OnBundle {
                centre,
                tangent,
                half,
                side,
                heading: stop.heading,
            };
            // A bar of the same bundle close enough along it.
            let near = bars.iter_mut().find(|bar| {
                let first = &bar[0];
                let d = (centre.0 - first.centre.0, centre.1 - first.centre.1);
                dot(first.tangent, tangent).abs() > settings.max_angle.cos()
                    && d.0.hypot(d.1) < settings.stop_merge
                    && dot(d, right_of(first.tangent)).abs() < first.half.max(half)
            });
            match near {
                Some(bar) => bar.push(stop),
                None => bars.push(vec![stop]),
            }
        }
    }

    let mean = |points: &mut dyn Iterator<Item = (f64, f64)>| {
        let (mut x, mut y, mut n) = (0.0, 0.0, 0.0);
        for p in points {
            x += p.0;
            y += p.1;
            n += 1.0;
        }
        (x / n, y / n)
    };
    let bars = bars
        .into_iter()
        .map(|stops| {
            let tangent = stops[0].tangent;
            let right = right_of(tangent);
            let centre = mean(&mut stops.iter().map(|s| s.centre));
            let half = stops.iter().map(|s| s.half).fold(0.0, f64::max);
            let mut arrows: Vec<(f64, f64)> = Vec::new();
            for stop in &stops {
                let (Some(side), Some(heading)) = (stop.side, stop.heading) else {
                    continue;
                };
                // Sides and directions as the bar's own tangent has them.
                let side = side * dot(stop.tangent, tangent).signum();
                let forward = dot(heading, tangent).signum();
                if !arrows.contains(&(side, forward)) {
                    arrows.push((side, forward));
                }
            }
            let reach = half + settings.arrow_gap;
            // Both ways on one side — a terminus, where buses arrive and
            // leave from the same stop — side by side rather than one over
            // the other.
            let both = |side: f64| arrows.contains(&(side, 1.0)) && arrows.contains(&(side, -1.0));
            let apart = |side: f64, forward: f64| {
                if both(side) {
                    forward * settings.arrow_length * 0.6
                } else {
                    0.0
                }
            };
            Bar {
                ends: [
                    (centre.0 - half * right.0, centre.1 - half * right.1),
                    (centre.0 + half * right.0, centre.1 + half * right.1),
                ],
                centre,
                arrows: arrows
                    .iter()
                    .map(|&(side, forward)| Arrow {
                        at: (
                            centre.0 + side * reach * right.0 + apart(side, forward) * tangent.0,
                            centre.1 + side * reach * right.1 + apart(side, forward) * tangent.1,
                        ),
                        heading: (forward * tangent.0, forward * tangent.1),
                    })
                    .collect(),
            }
        })
        .collect();
    let circles = loose
        .into_iter()
        .map(|stops| {
            let mut lines: Vec<usize> = stops.iter().map(|(line, _)| *line).collect();
            lines.sort_unstable();
            lines.dedup();
            let mut headings: Vec<(f64, f64)> = Vec::new();
            for heading in stops.iter().filter_map(|(_, s)| s.heading) {
                // Two ways less than a quarter turn apart are one way.
                if headings.iter().all(|&h| dot(h, heading) < 0.7) {
                    headings.push(heading);
                }
            }
            Circle {
                centre: mean(&mut stops.iter().map(|(_, s)| s.at)),
                lines,
                headings,
            }
        })
        .collect();
    Stops {
        bars,
        circles,
        drawn_at,
    }
}

/// `points` with every vertex left out that lies within `tolerance` mm of
/// the line its neighbours left in would draw: Douglas and Peucker's
/// simplification.
fn simplify(points: &[(f64, f64)], tolerance: f64) -> Vec<(f64, f64)> {
    if points.len() <= 2 {
        return points.to_vec();
    }
    let mut keep = vec![false; points.len()];
    keep[0] = true;
    keep[points.len() - 1] = true;
    let mut stack = vec![(0, points.len() - 1)];
    while let Some((a, b)) = stack.pop() {
        let (pa, pb) = (points[a], points[b]);
        let (dx, dy) = (pb.0 - pa.0, pb.1 - pa.1);
        let length = dx.hypot(dy);
        let mut far = (0.0, a);
        for (i, &p) in points.iter().enumerate().take(b).skip(a + 1) {
            let d = if length > 0.0 {
                ((p.0 - pa.0) * dy - (p.1 - pa.1) * dx).abs() / length
            } else {
                (p.0 - pa.0).hypot(p.1 - pa.1)
            };
            if d > far.0 {
                far = (d, i);
            }
        }
        if far.0 > tolerance {
            keep[far.1] = true;
            stack.push((a, far.1));
            stack.push((far.1, b));
        }
    }
    points
        .iter()
        .zip(keep)
        .filter_map(|(&p, k)| k.then_some(p))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> Settings {
        Settings {
            sample: 0.5,
            tolerance: 2.0,
            max_angle: 35f64.to_radians(),
            jump: 1.0,
            smooth: 1.0,
            round: 1.0,
            cap: 3.0,
            stop_reach: 3.0,
            bar_overhang: 0.3,
            stop_merge: 3.0,
            stop_side: 0.2,
            arrow_gap: 0.5,
            arrow_length: 1.0,
        }
    }

    /// A straight east-west street from x = 0 to x = 100 at y = 0, in ten
    /// segments, mapped west to east except for one segment the other way.
    fn street() -> Network {
        let segments = (0..10)
            .map(|i| {
                let (x0, x1) = (f64::from(i) * 10.0, f64::from(i + 1) * 10.0);
                let (a, b, nodes) = if i == 4 {
                    ((x1, 0.0), (x0, 0.0), (i64::from(i) + 1, i64::from(i)))
                } else {
                    ((x0, 0.0), (x1, 0.0), (i64::from(i), i64::from(i) + 1))
                };
                Segment {
                    a,
                    b,
                    nodes,
                    rail: false,
                }
            })
            .collect();
        Network::new(segments, 5.0)
    }

    fn route(line: usize, rank: usize, points: Vec<(f64, f64)>) -> RouteInput {
        RouteInput {
            line,
            rank,
            rail: false,
            width: 1.0,
            points,
        }
    }

    #[test]
    fn two_lines_on_one_street_are_drawn_side_by_side_in_rank_order() {
        // Line 1 a little north of the street, line 0 a little south, both
        // eastwards: snapped onto the street, then a width apart, line 0
        // (lower rank) on the left of the bundle — the north, the paper's y
        // pointing down — whatever side it came from.
        let laid = lay_out(
            &street(),
            &[
                route(1, 1, vec![(5.0, -0.8), (95.0, -0.8)]),
                route(0, 0, vec![(5.0, 0.9), (95.0, 0.9)]),
            ],
            &settings(),
        );
        let mid = |l: &Laid| l.points[l.points.len() / 2];
        // The routes are given line 1 first: `laid[1]` is line 0.
        assert!(
            (mid(&laid[1]).1 - (-0.5)).abs() < 1e-9,
            "{:?}",
            mid(&laid[1])
        );
        assert!((mid(&laid[0]).1 - 0.5).abs() < 1e-9, "{:?}", mid(&laid[0]));
        // Across the segment mapped the other way the order holds too.
        for l in &laid {
            assert!(l
                .points
                .iter()
                .skip(5)
                .take(l.points.len() - 10)
                .all(|p| p.1.abs() > 0.49));
        }
        assert_eq!(laid[0].bundle_width[40], 2.0);
    }

    #[test]
    fn a_line_running_the_other_way_keeps_its_place_in_the_bundle() {
        let laid = lay_out(
            &street(),
            &[
                route(0, 0, vec![(5.0, 0.0), (95.0, 0.0)]),
                route(1, 1, vec![(95.0, 0.0), (5.0, 0.0)]),
            ],
            &settings(),
        );
        let mid = |l: &Laid| l.points[l.points.len() / 2];
        // Line 0 eastwards on its left, the north; line 1 westwards on its
        // left too, which is the south: each on its own side of the bundle,
        // whichever way it runs.
        assert!(mid(&laid[0]).1 < 0.0);
        assert!(mid(&laid[1]).1 > 0.0);
    }

    #[test]
    fn a_bundle_too_wide_for_the_cap_is_drawn_thinner() {
        // Four lines a width of 1 each make 4, over the cap of 3: drawn at
        // two thirds, 2.67 across.
        let routes: Vec<RouteInput> = (0..4)
            .map(|l| route(l, l, vec![(5.0, 0.0), (95.0, 0.0)]))
            .collect();
        let laid = lay_out(&street(), &routes, &settings());
        assert_eq!(laid[0].width_class[40], 2);
        assert!((laid[0].bundle_width[40] - 4.0 * 2.0 / 3.0).abs() < 1e-9);
        let pieces = laid[0].pieces();
        assert_eq!(pieces.len(), 1);
        assert_eq!(pieces[0].0, 2);
    }

    #[test]
    fn a_route_away_from_the_network_is_drawn_where_it_is() {
        let laid = lay_out(
            &street(),
            &[route(0, 0, vec![(5.0, 30.0), (95.0, 30.0)])],
            &settings(),
        );
        assert!(laid[0].segment.iter().all(Option::is_none));
        assert!(laid[0].points.iter().all(|p| p.1 == 30.0));
        assert_eq!(laid[0].width_class[0], 0);
    }

    #[test]
    fn a_route_crossing_the_street_does_not_join_it() {
        let laid = lay_out(
            &street(),
            &[route(0, 0, vec![(50.0, -20.0), (50.0, 20.0)])],
            &settings(),
        );
        assert!(laid[0].segment.iter().all(Option::is_none));
    }

    fn stop(at: (f64, f64), heading: (f64, f64)) -> StopInput {
        StopInput {
            at,
            heading: Some(heading),
        }
    }

    #[test]
    fn the_stops_of_two_lines_at_one_place_are_one_bar_across_both() {
        let routes = vec![
            route(0, 0, vec![(5.0, 0.0), (95.0, 0.0)]),
            route(1, 1, vec![(5.0, 0.0), (95.0, 0.0)]),
        ];
        let laid = lay_out(&street(), &routes, &settings());
        let east = (1.0, 0.0);
        let stops: HashMap<usize, Vec<StopInput>> = [
            (0, vec![stop((50.0, 0.4), east), stop((70.0, 25.0), east)]),
            (1, vec![stop((50.1, 0.4), east)]),
        ]
        .into_iter()
        .collect();
        let laid_out = super::stops(&routes, &laid, &stops, &settings());
        assert_eq!(laid_out.bars.len(), 1);
        // Across both lines, and the overhang either side.
        let bar = &laid_out.bars[0];
        let [a, b] = bar.ends;
        assert!(((a.1 - b.1).abs() - (2.0 + 0.6)).abs() < 1e-9, "{bar:?}");
        assert!((a.0 - b.0).abs() < 1e-9);
        // One arrow, past the end on the stops' side (y down: south of the
        // street, to the right of going east), pointing east.
        assert_eq!(bar.arrows.len(), 1);
        let arrow = bar.arrows[0];
        assert_eq!(arrow.heading, east);
        assert!((arrow.at.1 - (1.3 + 0.5)).abs() < 1e-9, "{arrow:?}");
        // The stop far from the street is a circle.
        assert_eq!(laid_out.circles.len(), 1);
        assert_eq!(laid_out.circles[0].centre, (70.0, 25.0));
        assert_eq!(laid_out.circles[0].lines, vec![0]);
        assert_eq!(laid_out.drawn_at[&1].len(), 1);
    }

    #[test]
    fn the_stops_either_side_of_a_street_are_one_bar_with_two_arrows() {
        let routes = vec![route(0, 0, vec![(5.0, 0.0), (95.0, 0.0)])];
        let laid = lay_out(&street(), &routes, &settings());
        let stops: HashMap<usize, Vec<StopInput>> = [(
            0,
            vec![
                stop((40.0, 0.6), (1.0, 0.0)),
                stop((41.5, -0.6), (-1.0, 0.0)),
                // Too far along to merge.
                stop((60.0, 0.6), (1.0, 0.0)),
            ],
        )]
        .into_iter()
        .collect();
        let laid_out = super::stops(&routes, &laid, &stops, &settings());
        assert_eq!(laid_out.bars.len(), 2);
        let pair = &laid_out.bars[0];
        assert!((pair.centre.0 - 40.75).abs() < 1e-9, "{pair:?}");
        let mut arrows: Vec<((f64, f64), f64)> = pair
            .arrows
            .iter()
            .map(|a| (a.heading, a.at.1.signum()))
            .collect();
        arrows.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert_eq!(arrows, vec![((-1.0, 0.0), -1.0), ((1.0, 0.0), 1.0)]);
    }

    #[test]
    fn both_ways_at_one_stop_are_two_arrows_side_by_side() {
        let routes = vec![route(0, 0, vec![(5.0, 0.0), (95.0, 0.0)])];
        let laid = lay_out(&street(), &routes, &settings());
        let stops: HashMap<usize, Vec<StopInput>> = [(
            0,
            vec![
                stop((50.0, 0.6), (1.0, 0.0)),
                stop((50.0, 0.6), (-1.0, 0.0)),
            ],
        )]
        .into_iter()
        .collect();
        let laid_out = super::stops(&routes, &laid, &stops, &settings());
        let arrows = &laid_out.bars[0].arrows;
        assert_eq!(arrows.len(), 2);
        // Both on the stop's side, each moved the way it points.
        for arrow in arrows {
            assert!(arrow.at.1 > 0.0);
            assert!(((arrow.at.0 - 50.0) - 0.6 * arrow.heading.0).abs() < 1e-9);
        }
    }

    #[test]
    fn a_stop_off_its_own_route_goes_across_another_line_s_bundle() {
        // Line 0 runs along the street; line 1 somewhere else, but stops on
        // the street on its way back.
        let routes = vec![
            route(0, 0, vec![(5.0, 0.0), (95.0, 0.0)]),
            route(1, 1, vec![(5.0, 40.0), (95.0, 40.0)]),
        ];
        let laid = lay_out(&street(), &routes, &settings());
        let stops: HashMap<usize, Vec<StopInput>> = [
            (1, vec![stop((30.0, -0.5), (-1.0, 0.0))]),
            (0, vec![stop((80.0, 0.5), (1.0, 0.0))]),
        ]
        .into_iter()
        .collect();
        let laid_out = super::stops(&routes, &laid, &stops, &settings());
        assert_eq!(laid_out.bars.len(), 2);
        assert!(laid_out.circles.is_empty());
        assert!((laid_out.drawn_at[&1][0].1).abs() < 1e-9);
    }

    #[test]
    fn close_circles_of_several_lines_are_one() {
        let routes = vec![route(0, 0, vec![(5.0, 0.0), (95.0, 0.0)])];
        let laid = lay_out(&street(), &routes, &settings());
        let stops: HashMap<usize, Vec<StopInput>> = [
            (0, vec![stop((50.0, 30.0), (0.0, 1.0))]),
            (
                1,
                vec![
                    stop((50.5, 30.5), (0.0, -1.0)),
                    stop((50.2, 30.0), (0.1, 0.995)),
                ],
            ),
        ]
        .into_iter()
        .collect();
        let laid_out = super::stops(&routes, &laid, &stops, &settings());
        assert_eq!(laid_out.circles.len(), 1);
        let circle = &laid_out.circles[0];
        assert_eq!(circle.lines, vec![0, 1]);
        // Down and up: the third is the first's way again.
        assert_eq!(circle.headings, vec![(0.0, 1.0), (0.0, -1.0)]);
    }

    #[test]
    fn rounding_keeps_the_ends_and_the_straight_and_cuts_the_corner() {
        let points = resample(&[(0.0, 0.0), (10.0, 0.0), (10.0, 10.0)], 0.5);
        let mut along = vec![0.0; points.len()];
        for i in 1..points.len() {
            let (a, b) = (points[i - 1], points[i]);
            along[i] = along[i - 1] + (b.0 - a.0).hypot(b.1 - a.1);
        }
        let round = rounded(&points, &along, 2.0);
        assert_eq!(round[0], points[0]);
        assert_eq!(round[round.len() - 1], points[points.len() - 1]);
        // Well before the corner, still on the line.
        assert!((round[8].1).abs() < 1e-9);
        // At the corner, cut inside it.
        let corner = round[20];
        assert!(corner.0 < 10.0 && corner.1 > 0.0, "{corner:?}");
    }

    #[test]
    fn simplifying_keeps_the_corners_and_drops_the_straight() {
        let points = vec![(0.0, 0.0), (1.0, 0.0), (2.0, 0.0), (2.0, 1.0), (2.0, 2.0)];
        assert_eq!(
            simplify(&points, 0.01),
            vec![(0.0, 0.0), (2.0, 0.0), (2.0, 2.0)]
        );
    }

    #[test]
    fn resampling_keeps_both_ends_and_the_spacing() {
        let samples = resample(&[(0.0, 0.0), (1.0, 0.0), (1.0, 1.05)], 0.5);
        assert_eq!(samples.first(), Some(&(0.0, 0.0)));
        assert_eq!(samples.last(), Some(&(1.0, 1.05)));
        assert_eq!(samples.len(), 5);
    }
}
