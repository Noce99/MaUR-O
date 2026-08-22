//! Laying out a course over a map.
//!
//! A course is printed over the map in purple: a triangle at the start, a
//! circle at every control, two circles at the finish, a straight line
//! between each pair, and a number beside every circle. Where each of those
//! goes is not obvious -- the line has to stop at the edge of the circle
//! rather than run through it, it has to break where it passes some other
//! control, and the number has to sit where it does not land on anything --
//! and this works it out.
//!
//! ```no_run
//! use maur_o::course::{layout, Control, Kind, Options};
//!
//! let controls = vec![
//!     Control::new(10.0, 10.0, Kind::Start, ""),
//!     Control::new(40.0, 20.0, Kind::Control, "1"),
//!     Control::new(70.0, 10.0, Kind::Finish, ""),
//! ];
//! let course = layout(&controls, &Options::default());
//! println!("{} legs", course.legs.len());
//! ```
//!
//! # Sizes
//!
//! Everything here is in millimetres **on the printed page**, and none of it
//! scales with the map: a control circle is 5.35 mm across on a 1:15000 map
//! and on a 1:4000 one, because it is the runner's eye it is sized for and
//! not the ground. That happens to be the unit the rest of this crate works
//! in, so the numbers here are the standard's own.
//!
//! Ported from Purple Pen's `CourseFormatter` and `Appearance`.

/// The sizes a course is drawn at, in mm on the page, as ISOM 2017-2 has
/// them.
pub mod size {
    /// How thick every line of the overprint is.
    pub const LINE_WIDTH: f64 = 0.35;
    /// The radius of a control circle.
    pub const CONTROL_CIRCLE_RADIUS: f64 = 5.35 / 2.0;
    /// The radius of the outer of the finish's two circles.
    pub const FINISH_OUTER_RADIUS: f64 = 6.35 / 2.0;
    /// The radius of the inner one.
    pub const FINISH_INNER_RADIUS: f64 = 4.35 / 2.0;
    /// The distance from the middle of the start triangle to a corner.
    pub const START_TRIANGLE_RADIUS: f64 = 3.46;
    /// The font size which gives a control number the 4 mm digits the
    /// standard asks for.
    pub const NUMBER_EM: f64 = 5.57;
    /// How tall those digits are.
    pub const DIGIT_HEIGHT: f64 = 4.0;
    /// How wide a digit is, as a fraction of the em, for a sans face.
    pub const DIGIT_WIDTH_FRACTION: f64 = 0.556;
    /// How far a number sits from the edge of its circle.
    pub const NUMBER_CIRCLE_DISTANCE: f64 = 1.825;
    /// How wide a gap to leave where a leg passes over another control.
    pub const AUTO_LEG_GAP: f64 = 3.5;
}

use size::*;

/// Where a control number is put before anything is known about what is in
/// the way: up and to the right, in the page's own coordinates.
const DEFAULT_NUMBER_ANGLE: f64 = -std::f64::consts::FRAC_PI_6;

/// How many angles round the circle are tried when placing a number.
const NUMBER_ANGLE_TRIES: usize = 32;

/// What a control is: the three kinds of thing a course is made of.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// Where the runner begins, drawn as a triangle pointing at the first
    /// control.
    Start,
    /// A control, drawn as a circle with its number beside it.
    Control,
    /// Where the runner ends, drawn as two circles.
    Finish,
}

/// One point of a course.
#[derive(Clone, Debug)]
pub struct Control {
    /// Where it is across the page, in mm.
    pub x: f64,
    /// And down it.
    pub y: f64,
    /// Which of the three kinds it is.
    pub kind: Kind,
    /// What is printed beside it: the sequence number, usually.
    pub label: String,
}

impl Control {
    /// A control at a point.
    pub fn new(x: f64, y: f64, kind: Kind, label: &str) -> Control {
        Control {
            x,
            y,
            kind,
            label: label.to_string(),
        }
    }
}

/// A point on the page.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    /// Across the page, in mm.
    pub x: f64,
    /// Down it.
    pub y: f64,
}

/// A straight piece of a leg.
#[derive(Clone, Copy, Debug)]
pub struct Segment {
    /// Where it starts.
    pub x1: f64,
    /// Where it starts.
    pub y1: f64,
    /// Where it ends.
    pub x2: f64,
    /// Where it ends.
    pub y2: f64,
}

/// The line from one control to the next, in the pieces it is actually drawn
/// in.
#[derive(Clone, Debug)]
pub struct Leg {
    /// Which control it leaves; it arrives at the next one.
    pub from_index: usize,
    /// The visible pieces, in order along the leg. Empty where the two
    /// controls are so close that nothing of the line is left.
    pub segments: Vec<Segment>,
}

/// A circle of the overprint, and which control it belongs to.
#[derive(Clone, Copy, Debug)]
pub struct Circle {
    /// Which control it is drawn for.
    pub control_index: usize,
    /// Its middle, across the page.
    pub x: f64,
    /// Its middle, down the page.
    pub y: f64,
}

/// The start triangle, and which control it belongs to.
#[derive(Clone, Debug)]
pub struct Triangle {
    /// Which control it is drawn for.
    pub control_index: usize,
    /// Its three corners, the first being the one it points with.
    pub points: Vec<Point>,
}

/// A control number, placed.
#[derive(Clone, Debug)]
pub struct Number {
    /// The middle of the label, across the page.
    pub x: f64,
    /// The middle of the label, down the page.
    pub y: f64,
    /// What it says.
    pub text: String,
    /// Which control it belongs to.
    pub control_index: usize,
}

/// A course, laid out.
#[derive(Clone, Debug, Default)]
pub struct Layout {
    /// The lines between the controls, in course order.
    pub legs: Vec<Leg>,
    /// The start triangles -- usually one.
    pub start_triangles: Vec<Triangle>,
    /// The control circles.
    pub control_circles: Vec<Circle>,
    /// The finish circles.
    pub finish_circles: Vec<Circle>,
    /// The control numbers, placed clear of everything else.
    pub numbers: Vec<Number>,
}

/// How to lay a course out.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Whether to draw the lines between controls.
    ///
    /// Off for a view of every control on the map at once, which has no
    /// order to draw lines along.
    pub legs: bool,
}

impl Default for Options {
    fn default() -> Options {
        Options { legs: true }
    }
}

// ---------------------------------------------------------------------------
// Geometry

fn point_segment_distance(px: f64, py: f64, s: &Segment) -> f64 {
    let dx = s.x2 - s.x1;
    let dy = s.y2 - s.y1;
    let len_sq = dx * dx + dy * dy;
    let t = if len_sq == 0.0 {
        0.0
    } else {
        (((px - s.x1) * dx + (py - s.y1) * dy) / len_sq).clamp(0.0, 1.0)
    };
    (px - (s.x1 + t * dx)).hypot(py - (s.y1 + t * dy))
}

/// `x - y * round(x / y)`, rounding halves to even -- IEEE's remainder, and
/// what the reference implementation used.
fn ieee_remainder(x: f64, y: f64) -> f64 {
    let n = (x / y).round_ties_even();
    x - n * y
}

/// Which way the start triangle points: at the next control, or straight up
/// the page when it is the only thing there is.
pub fn start_angle_out(controls: &[Control], start_index: usize) -> f64 {
    match controls.get(start_index + 1) {
        None => -std::f64::consts::FRAC_PI_2,
        Some(next) => {
            let s = &controls[start_index];
            (next.y - s.y).atan2(next.x - s.x)
        }
    }
}

/// The three corners of a start triangle pointing at `angle_out`.
pub fn start_triangle_points(cx: f64, cy: f64, angle_out: f64) -> Vec<Point> {
    let third = 2.0 * std::f64::consts::PI / 3.0;
    [0.0, third, -third]
        .iter()
        .map(|d| Point {
            x: cx + START_TRIANGLE_RADIUS * (angle_out + d).cos(),
            y: cy + START_TRIANGLE_RADIUS * (angle_out + d).sin(),
        })
        .collect()
}

/// How far the edge of a start triangle is from its middle, along a ray --
/// as a fraction of the distance to a corner.
///
/// A leg leaving the start has to stop at the triangle's edge, and where that
/// edge is depends on which way the leg goes: a half of the way to a corner
/// through the middle of a side, all of it through a corner.
pub fn start_boundary_factor(angle_leg: f64, angle_out: f64) -> f64 {
    // A triangle looks the same from three directions, so the ray can be
    // brought within a third of a turn of a corner and measured there.
    let third = 2.0 * std::f64::consts::PI / 3.0;
    let net = ieee_remainder(angle_leg - angle_out, third).abs();
    0.5 / (net - std::f64::consts::FRAC_PI_3).cos()
}

/// How far back from a control a leg stops, so that it meets the edge of what
/// is drawn there rather than running into it.
fn leg_radius(control: &Control, angle_leg: f64, angle_out: f64) -> f64 {
    match control.kind {
        Kind::Start => START_TRIANGLE_RADIUS * start_boundary_factor(angle_leg, angle_out),
        Kind::Finish => FINISH_OUTER_RADIUS - LINE_WIDTH / 2.0,
        Kind::Control => CONTROL_CIRCLE_RADIUS - LINE_WIDTH / 2.0,
    }
}

/// How much room a control takes up, for a leg which merely passes it.
fn apparent_radius(control: &Control) -> f64 {
    match control.kind {
        Kind::Start => START_TRIANGLE_RADIUS,
        Kind::Finish => FINISH_OUTER_RADIUS - LINE_WIDTH / 2.0,
        Kind::Control => CONTROL_CIRCLE_RADIUS - LINE_WIDTH / 2.0,
    }
}

/// The line between two controls, stopped at each end where its circle
/// begins. `None` where they are too close together for any of it to show.
fn build_leg(controls: &[Control], from_index: usize, angle_out_of_start: f64) -> Option<Segment> {
    let a = &controls[from_index];
    let b = &controls[from_index + 1];
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    let len = dx.hypot(dy);
    if len == 0.0 {
        return None;
    }
    let angle = dy.atan2(dx);

    // A leg leaves the first control at `angle` and arrives at the second
    // from the opposite direction, which is what its own trim is measured by.
    let r1 = leg_radius(a, angle, angle_out_of_start);
    let r2 = leg_radius(b, angle + std::f64::consts::PI, angle_out_of_start);
    if len <= r1 + r2 {
        return None;
    }
    let ux = dx / len;
    let uy = dy / len;
    Some(Segment {
        x1: a.x + ux * r1,
        y1: a.y + uy * r1,
        x2: b.x - ux * r2,
        y2: b.y - uy * r2,
    })
}

/// Breaks a leg wherever it passes over another control, so that the line
/// does not run through a circle it has nothing to do with.
fn cut_leg(seg: &Segment, controls: &[Control], from_index: usize) -> Vec<Segment> {
    let dx = seg.x2 - seg.x1;
    let dy = seg.y2 - seg.y1;
    let len = dx.hypot(dy);
    if len == 0.0 {
        return Vec::new();
    }
    let ux = dx / len;
    let uy = dy / len;

    let mut cuts: Vec<(f64, f64)> = Vec::new();
    for (i, c) in controls.iter().enumerate() {
        if i == from_index || i == from_index + 1 {
            continue;
        }
        let r_other = apparent_radius(c) + LINE_WIDTH * 2.0;
        let t = ((c.x - seg.x1) * ux + (c.y - seg.y1) * uy).clamp(0.0, len);
        let d = (c.x - (seg.x1 + t * ux)).hypot(c.y - (seg.y1 + t * uy));
        // A control right at the end of a leg is the leg's own business, and
        // half a millimetre of slack keeps it from being cut twice.
        if d < r_other && t > 0.5 && t < len - 0.5 {
            let gap_radius = (r_other * r_other - d * d).sqrt() + AUTO_LEG_GAP / 2.0;
            cuts.push((t - gap_radius, t + gap_radius));
        }
    }
    if cuts.is_empty() {
        return vec![*seg];
    }
    cuts.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

    let mut segments = Vec::new();
    let mut pos = 0f64;
    for (cut_start, cut_end) in cuts {
        if cut_start > pos {
            segments.push(Segment {
                x1: seg.x1 + ux * pos,
                y1: seg.y1 + uy * pos,
                x2: seg.x1 + ux * cut_start,
                y2: seg.y1 + uy * cut_start,
            });
        }
        pos = pos.max(cut_end);
    }
    if pos < len {
        segments.push(Segment {
            x1: seg.x1 + ux * pos,
            y1: seg.y1 + uy * pos,
            x2: seg.x2,
            y2: seg.y2,
        });
    }
    segments
}

// ---------------------------------------------------------------------------
// Putting the numbers somewhere

/// Something a control number should not be printed on top of.
enum Avoid {
    /// A piece of a leg.
    Segment(Segment),
    /// The line of a circle -- not the disc: a number may sit inside one.
    Circle { cx: f64, cy: f64, r: f64 },
    /// A number already placed.
    Rect { cx: f64, cy: f64, hw: f64, hh: f64 },
}

impl Avoid {
    fn distance_from(&self, px: f64, py: f64) -> f64 {
        match self {
            Avoid::Segment(s) => point_segment_distance(px, py, s),
            Avoid::Circle { cx, cy, r } => ((px - cx).hypot(py - cy) - r).abs(),
            Avoid::Rect { cx, cy, hw, hh } => {
                let dx = ((px - cx).abs() - hw).max(0.0);
                let dy = ((py - cy).abs() - hh).max(0.0);
                dx.hypot(dy)
            }
        }
    }
}

/// Roughly how big a number is when printed.
pub fn number_text_size(text: &str) -> (f64, f64) {
    let chars = text.chars().count().max(1) as f64;
    (chars * NUMBER_EM * DIGIT_WIDTH_FRACTION, DIGIT_HEIGHT)
}

/// Where to put the middle of a `width` by `height` label so that it just
/// touches a circle of radius `r` about the origin, in the direction `angle`.
///
/// Which part of the label touches depends on the direction: a side, an end,
/// or a corner, and each is a different piece of arithmetic.
pub fn rectangle_center(cx: f64, cy: f64, r: f64, angle: f64, width: f64, height: f64) -> Point {
    let w2 = width / 2.0;
    let h2 = height / 2.0;
    let tangent = angle.tan();
    let (mut x, mut y);

    if w2 == 0.0 && h2 == 0.0 {
        x = angle.cos() * r;
        y = angle.sin() * r;
    } else if w2 > 0.0 && tangent.abs() >= (h2 + r) / w2 {
        // Its top or bottom edge touches.
        x = (h2 + r) / tangent;
        y = h2 + r;
        if angle.sin() < 0.0 {
            x = -x;
            y = -y;
        }
    } else if tangent.abs() <= h2 / (w2 + r) {
        // One of its ends touches.
        x = r + w2;
        y = (r + w2) * tangent;
        if angle.cos() < 0.0 {
            x = -x;
            y = -y;
        }
    } else {
        // A corner touches.
        let normal_angle = angle.sin().abs().atan2(angle.cos().abs());
        let angle_rect = h2.atan2(w2);
        let radius_rect = h2.hypot(w2);
        let beta = ((normal_angle - angle_rect).sin() / r * radius_rect).asin();
        let angle_to_touch = normal_angle + beta;
        x = angle_to_touch.cos() * r + w2;
        y = angle_to_touch.sin() * r + h2;
        if angle.cos() < 0.0 {
            x = -x;
        }
        if angle.sin() < 0.0 {
            y = -y;
        }
    }

    Point {
        x: cx + x,
        y: cy + y,
    }
}

/// Finds somewhere to put one control's number.
///
/// Thirty-two positions round the circle are tried and the one furthest from
/// everything nearby wins -- which is how a number ends up on the far side of
/// a control from the legs running into it.
fn place_number(control: &Control, text: &str, objects: &[Avoid]) -> Point {
    let distance = CONTROL_CIRCLE_RADIUS + NUMBER_CIRCLE_DISTANCE;
    let (width, height) = number_text_size(text);

    let nearby: Vec<&Avoid> = objects
        .iter()
        .filter(|o| {
            // Its own circle is not something to avoid: the number is placed
            // against it.
            if let Avoid::Circle { cx, cy, .. } = o {
                if *cx == control.x && *cy == control.y {
                    return false;
                }
            }
            o.distance_from(control.x, control.y) <= distance * 4.0
        })
        .collect();

    let mut best = rectangle_center(
        control.x,
        control.y,
        distance,
        DEFAULT_NUMBER_ANGLE,
        width,
        height,
    );
    if nearby.is_empty() {
        // Nothing in the way: the default position is as good as any.
        return best;
    }
    let mut best_distance = -1f64;
    for i in 0..NUMBER_ANGLE_TRIES {
        let angle = DEFAULT_NUMBER_ANGLE + (i as f64 * std::f64::consts::PI) / 16.0;
        let pt = rectangle_center(control.x, control.y, distance, angle, width, height);
        let d = nearby
            .iter()
            .map(|o| o.distance_from(pt.x, pt.y))
            .fold(f64::INFINITY, f64::min);
        if d > best_distance {
            best = pt;
            best_distance = d;
        }
    }
    best
}

// ---------------------------------------------------------------------------
// The whole course

/// Lays out a course: where every circle, line and number goes.
pub fn layout(controls: &[Control], options: &Options) -> Layout {
    let mut out = Layout::default();
    if controls.is_empty() {
        return out;
    }

    let first_start = controls.iter().position(|c| c.kind == Kind::Start);
    let angle_out_of_start = match (options.legs, first_start) {
        (true, Some(i)) => Some(start_angle_out(controls, i)),
        _ => None,
    };

    if options.legs {
        for i in 0..controls.len() - 1 {
            let segments = match build_leg(controls, i, angle_out_of_start.unwrap_or(f64::NAN)) {
                Some(seg) => cut_leg(&seg, controls, i),
                None => Vec::new(),
            };
            out.legs.push(Leg {
                from_index: i,
                segments,
            });
        }
    }

    for (i, c) in controls.iter().enumerate() {
        match c.kind {
            Kind::Start => {
                // With no order to the controls there is nothing for the
                // triangle to point at, so it points up the page.
                let angle = if !options.legs {
                    -std::f64::consts::FRAC_PI_2
                } else if Some(i) == first_start {
                    angle_out_of_start.unwrap_or_else(|| start_angle_out(controls, i))
                } else {
                    start_angle_out(controls, i)
                };
                out.start_triangles.push(Triangle {
                    control_index: i,
                    points: start_triangle_points(c.x, c.y, angle),
                });
            }
            Kind::Finish => out.finish_circles.push(Circle {
                control_index: i,
                x: c.x,
                y: c.y,
            }),
            Kind::Control => out.control_circles.push(Circle {
                control_index: i,
                x: c.x,
                y: c.y,
            }),
        }
    }

    // A number is placed clear of the legs, the circles, and the numbers
    // already placed -- so two of them never land on each other.
    let mut avoid: Vec<Avoid> = Vec::new();
    for leg in &out.legs {
        for s in &leg.segments {
            avoid.push(Avoid::Segment(*s));
        }
    }
    for c in &out.control_circles {
        avoid.push(Avoid::Circle {
            cx: c.x,
            cy: c.y,
            r: CONTROL_CIRCLE_RADIUS,
        });
    }
    for f in &out.finish_circles {
        avoid.push(Avoid::Circle {
            cx: f.x,
            cy: f.y,
            r: FINISH_OUTER_RADIUS,
        });
    }
    for t in &out.start_triangles {
        let c = &controls[t.control_index];
        avoid.push(Avoid::Circle {
            cx: c.x,
            cy: c.y,
            r: START_TRIANGLE_RADIUS,
        });
    }

    let circles: Vec<usize> = out
        .control_circles
        .iter()
        .map(|c| c.control_index)
        .collect();
    for control_index in circles {
        let c = &controls[control_index];
        let pt = place_number(c, &c.label, &avoid);
        let (width, height) = number_text_size(&c.label);
        out.numbers.push(Number {
            x: pt.x,
            y: pt.y,
            text: c.label.clone(),
            control_index,
        });
        avoid.push(Avoid::Rect {
            cx: pt.x,
            cy: pt.y,
            hw: width / 2.0,
            hh: height / 2.0,
        });
    }

    out
}
