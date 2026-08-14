//! A faithful port of Qt's private `QBezier::shifted()` — the curve offset
//! algorithm used to shift a line symbol's border sideways off the main
//! line, preserving it as a curve rather than flattening first.
//!
//! Ported line-for-line from Qt 5.15's
//! `qtbase/src/gui/painting/qbezier.cpp` (`shift()`, `good_offset()`,
//! `addCircle()`, `shifted()`) and `qbezier_p.h` (`pointAt()`,
//! `normalVector()`, `split()`), since this is the one piece of Qt's private
//! rendering internals `geometry.cpp` depended on directly. Kept as its own
//! module, independent of the rest of the geometry code, exactly like the
//! original's dependency on `<private/qbezier_p.h>`.

use crate::map::Point;

/// The pen miter limit used by Mapper for the near-cusp semicircle
/// approximation, following Qt's own constant.
const KAPPA: f64 = 0.5522847498;

fn fuzzy_compare(p1: f64, p2: f64) -> bool {
    (p1 - p2).abs() * 1_000_000_000_000.0 <= p1.abs().min(p2.abs())
}

fn fuzzy_is_null(d: f64) -> bool {
    d.abs() <= 0.000_000_000_001
}

/// A cubic bezier curve, in the same `x1,y1..x4,y4` layout Qt's `QBezier`
/// uses.
#[derive(Clone, Copy, Debug, Default)]
pub struct QBezier {
    pub x1: f64,
    pub y1: f64,
    pub x2: f64,
    pub y2: f64,
    pub x3: f64,
    pub y3: f64,
    pub x4: f64,
    pub y4: f64,
}

impl QBezier {
    pub fn from_points(p1: Point, p2: Point, p3: Point, p4: Point) -> QBezier {
        QBezier {
            x1: p1.x,
            y1: p1.y,
            x2: p2.x,
            y2: p2.y,
            x3: p3.x,
            y3: p3.y,
            x4: p4.x,
            y4: p4.y,
        }
    }

    pub fn pt1(&self) -> Point {
        Point::new(self.x1, self.y1)
    }
    pub fn pt2(&self) -> Point {
        Point::new(self.x2, self.y2)
    }
    pub fn pt3(&self) -> Point {
        Point::new(self.x3, self.y3)
    }
    pub fn pt4(&self) -> Point {
        Point::new(self.x4, self.y4)
    }

    pub fn point_at(&self, t: f64) -> Point {
        let m_t = 1.0 - t;
        let x = {
            let a = self.x1 * m_t + self.x2 * t;
            let b = self.x2 * m_t + self.x3 * t;
            let c = self.x3 * m_t + self.x4 * t;
            let a = a * m_t + b * t;
            let b = b * m_t + c * t;
            a * m_t + b * t
        };
        let y = {
            let a = self.y1 * m_t + self.y2 * t;
            let b = self.y2 * m_t + self.y3 * t;
            let c = self.y3 * m_t + self.y4 * t;
            let a = a * m_t + b * t;
            let b = b * m_t + c * t;
            a * m_t + b * t
        };
        Point::new(x, y)
    }

    pub fn normal_vector(&self, t: f64) -> Point {
        let m_t = 1.0 - t;
        let a = m_t * m_t;
        let b = t * m_t;
        let c = t * t;
        Point::new(
            (self.y2 - self.y1) * a + (self.y3 - self.y2) * b + (self.y4 - self.y3) * c,
            -(self.x2 - self.x1) * a - (self.x3 - self.x2) * b - (self.x4 - self.x3) * c,
        )
    }

    pub fn split(&self) -> (QBezier, QBezier) {
        let mid = |a: Point, b: Point| (a + b) * 0.5;
        let mid_12 = mid(self.pt1(), self.pt2());
        let mid_23 = mid(self.pt2(), self.pt3());
        let mid_34 = mid(self.pt3(), self.pt4());
        let mid_12_23 = mid(mid_12, mid_23);
        let mid_23_34 = mid(mid_23, mid_34);
        let mid_all = mid(mid_12_23, mid_23_34);
        (
            QBezier::from_points(self.pt1(), mid_12, mid_12_23, mid_all),
            QBezier::from_points(mid_all, mid_23_34, mid_34, self.pt4()),
        )
    }

    /// `(width, height)` of the axis-aligned bounding box of the four
    /// control points.
    fn bounds_size(&self) -> (f64, f64) {
        let xs = [self.x1, self.x2, self.x3, self.x4];
        let ys = [self.y1, self.y2, self.y3, self.y4];
        let xmin = xs.iter().cloned().fold(f64::INFINITY, f64::min);
        let xmax = xs.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        let ymin = ys.iter().cloned().fold(f64::INFINITY, f64::min);
        let ymax = ys.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        (xmax - xmin, ymax - ymin)
    }

    /// Returns up to `max_segments` bezier curves approximating this curve
    /// offset sideways by `offset` (to the right of the curve's direction,
    /// with the y axis pointing down, matching `perpRightUnit()` elsewhere
    /// in this crate). `threshold` controls the quality/segment-count
    /// trade-off, as in Qt.
    pub fn shifted(&self, max_segments: usize, offset: f64, threshold_in: f32) -> Vec<QBezier> {
        if fuzzy_compare(self.x1, self.x2)
            && fuzzy_compare(self.x1, self.x3)
            && fuzzy_compare(self.x1, self.x4)
            && fuzzy_compare(self.y1, self.y2)
            && fuzzy_compare(self.y1, self.y3)
            && fuzzy_compare(self.y1, self.y4)
        {
            return Vec::new();
        }

        let max_segments = max_segments.saturating_sub(1);
        let mut threshold = threshold_in;

        'redo: loop {
            let mut stack: Vec<QBezier> = vec![*self];
            let mut output: Vec<QBezier> = Vec::new();

            while let Some(top) = stack.last().copied() {
                let stack_segments = stack.len() as i64;
                let trip = stack_segments == 10
                    || output.len() as i64 == max_segments as i64 - stack_segments;
                if trip {
                    threshold *= 1.5;
                    if threshold > 2.0 {
                        // give_up: keep the current stack/output state and
                        // accept every remaining frame unconditionally.
                        while let Some(top) = stack.last().copied() {
                            match shift(&top, offset, threshold) {
                                ShiftResult::Ok(b) | ShiftResult::Split(b) => output.push(b),
                                _ => {}
                            }
                            stack.pop();
                        }
                        return output;
                    }
                    continue 'redo;
                }

                match shift(&top, offset, threshold) {
                    ShiftResult::Discard => {
                        stack.pop();
                    }
                    ShiftResult::Ok(b) => {
                        output.push(b);
                        stack.pop();
                    }
                    ShiftResult::Circle if max_segments as i64 - output.len() as i64 >= 2 => {
                        if let Some((c1, c2)) = add_circle(&top, offset) {
                            output.push(c1);
                            output.push(c2);
                        }
                        stack.pop();
                    }
                    _ => {
                        // A genuine Split, or a Circle without room: subdivide.
                        let (first, second) = top.split();
                        stack.pop();
                        stack.push(second);
                        stack.push(first);
                    }
                }
            }
            return output;
        }
    }
}

enum ShiftResult {
    Discard,
    Ok(QBezier),
    /// Carries the (possibly low-quality) shifted curve, used only by the
    /// `shifted()` give-up pass.
    Split(QBezier),
    Circle,
}

/// The unit normal of a direction vector, following `QLineF(origin,
/// direction).normalVector().unitVector()`: rotate `(dx, dy)` to `(dy,
/// -dx)`, then normalize.
fn normal_unit(direction: Point) -> Point {
    Point::new(direction.y, -direction.x).normalized()
}

fn good_offset(b1: &QBezier, b2: &QBezier, offset: f64, threshold: f32) -> bool {
    let threshold = threshold as f64;
    let o2 = offset * offset;
    let max_dist_line = threshold * offset * offset;
    let max_dist_normal = threshold * offset;
    let divisions = 4;
    let spacing = 1.0 / divisions as f64;
    let mut t = spacing;
    for _ in 1..divisions {
        let p1 = b1.point_at(t);
        let p2 = b2.point_at(t);
        let d = (p1.x - p2.x) * (p1.x - p2.x) + (p1.y - p2.y) * (p1.y - p2.y);
        if (d - o2).abs() > max_dist_line {
            return false;
        }

        let normal_point = b1.normal_vector(t);
        let l = normal_point.x.abs() + normal_point.y.abs();
        if l != 0.0 {
            let d = (normal_point.x * (p1.y - p2.y) - normal_point.y * (p1.x - p2.x)).abs() / l;
            if d > max_dist_normal {
                return false;
            }
        }
        t += spacing;
    }
    true
}

fn shift(orig: &QBezier, offset: f64, threshold: f32) -> ShiftResult {
    let p1_p2_equal = fuzzy_compare(orig.x1, orig.x2) && fuzzy_compare(orig.y1, orig.y2);
    let p2_p3_equal = fuzzy_compare(orig.x2, orig.x3) && fuzzy_compare(orig.y2, orig.y3);
    let p3_p4_equal = fuzzy_compare(orig.x3, orig.x4) && fuzzy_compare(orig.y3, orig.y4);

    let mut points = [Point::default(); 4];
    let mut map = [0usize; 4];
    let mut np = 0usize;
    points[np] = orig.pt1();
    map[0] = 0;
    np += 1;
    if !p1_p2_equal {
        points[np] = orig.pt2();
        np += 1;
    }
    map[1] = np - 1;
    if !p2_p3_equal {
        points[np] = orig.pt3();
        np += 1;
    }
    map[2] = np - 1;
    if !p3_p4_equal {
        points[np] = orig.pt4();
        np += 1;
    }
    map[3] = np - 1;
    if np == 1 {
        return ShiftResult::Discard;
    }

    let (bw, bh) = orig.bounds_size();
    if np == 4 && bw < 0.1 * offset && bh < 0.1 * offset {
        // Note: reproduces Qt's own expression verbatim, operator
        // precedence included (`a + b*c + d`, not `(a+b)*(c+d)`).
        let l = (orig.x1 - orig.x2) * (orig.x1 - orig.x2)
            + (orig.y1 - orig.y2) * (orig.y1 - orig.y2) * (orig.x3 - orig.x4) * (orig.x3 - orig.x4)
            + (orig.y3 - orig.y4) * (orig.y3 - orig.y4);
        let dot = (orig.x1 - orig.x2) * (orig.x3 - orig.x4) + (orig.y1 - orig.y2) * (orig.y3 - orig.y4);
        if dot < 0.0 && dot * dot < 0.8 * l {
            return ShiftResult::Circle;
        }
    }

    let mut points_shifted = [Point::default(); 4];

    let prev_dir = points[1] - points[0];
    if prev_dir.length() == 0.0 {
        return ShiftResult::Discard;
    }
    let mut prev_normal = normal_unit(prev_dir);

    points_shifted[0] = points[0] + prev_normal * offset;

    for i in 1..np - 1 {
        let next_dir = points[i + 1] - points[i];
        let next_normal = normal_unit(next_dir);

        let normal_sum = prev_normal + next_normal;
        let r = 1.0 + prev_normal.x * next_normal.x + prev_normal.y * next_normal.y;

        if fuzzy_is_null(r) {
            points_shifted[i] = points[i] + prev_normal * offset;
        } else {
            let k = offset / r;
            points_shifted[i] = points[i] + normal_sum * k;
        }
        prev_normal = next_normal;
    }

    points_shifted[np - 1] = points[np - 1] + prev_normal * offset;

    let shifted = QBezier::from_points(
        points_shifted[map[0]],
        points_shifted[map[1]],
        points_shifted[map[2]],
        points_shifted[map[3]],
    );

    if np > 2 {
        if good_offset(orig, &shifted, offset, threshold) {
            ShiftResult::Ok(shifted)
        } else {
            ShiftResult::Split(shifted)
        }
    } else {
        ShiftResult::Ok(shifted)
    }
}

/// Approximates a near-cusp (the curve nearly reverses on itself) with a
/// semicircle of two bezier curves, following Qt's `addCircle()`.
fn add_circle(b: &QBezier, offset: f64) -> Option<(QBezier, QBezier)> {
    let mut normals = [Point::default(); 3];

    normals[0] = Point::new(b.y2 - b.y1, b.x1 - b.x2);
    let dist0 = normals[0].length();
    if fuzzy_is_null(dist0) {
        return None;
    }
    normals[0] = normals[0] / dist0;

    normals[2] = Point::new(b.y4 - b.y3, b.x3 - b.x4);
    let dist2 = normals[2].length();
    if fuzzy_is_null(dist2) {
        return None;
    }
    normals[2] = normals[2] / dist2;

    let mut n1 = Point::new(b.x1 - b.x2 - b.x3 + b.x4, b.y1 - b.y2 - b.y3 + b.y4);
    n1 = n1 / (-1.0 * (n1.x * n1.x + n1.y * n1.y).sqrt());
    normals[1] = n1;

    let mut angles = [0.0f64; 2];
    let mut sign = 1.0f64;
    for i in 0..2 {
        let mut cos_a = normals[i].dot(normals[i + 1]);
        if cos_a > 1.0 {
            cos_a = 1.0;
        }
        if cos_a < -1.0 {
            cos_a = -1.0;
        }
        angles[i] = cos_a.acos() * std::f64::consts::FRAC_1_PI;
    }

    if angles[0] + angles[1] > 1.0 {
        normals[1] = -normals[1];
        angles[0] = 1.0 - angles[0];
        angles[1] = 1.0 - angles[1];
        sign = -1.0;
    }

    let mut circle = [Point::default(); 3];
    circle[0] = b.pt1() + normals[0] * offset;
    circle[1] = (b.pt1() + b.pt4()) * 0.5 + normals[1] * offset;
    circle[2] = b.pt4() + normals[2] * offset;

    let mut out = [QBezier::default(); 2];
    for i in 0..2 {
        let kappa = 2.0 * KAPPA * sign * offset * angles[i];
        let o = &mut out[i];
        o.x1 = circle[i].x;
        o.y1 = circle[i].y;
        o.x2 = circle[i].x - normals[i].y * kappa;
        o.y2 = circle[i].y + normals[i].x * kappa;
        o.x3 = circle[i + 1].x + normals[i + 1].y * kappa;
        o.y3 = circle[i + 1].y - normals[i + 1].x * kappa;
        o.x4 = circle[i + 1].x;
        o.y4 = circle[i + 1].y;
    }
    Some((out[0], out[1]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shifts_a_gentle_curve_outward_by_the_offset() {
        // A gentle curve from (0,0) to (10,0) bowing down (+y) a little.
        let b = QBezier::from_points(
            Point::new(0.0, 0.0),
            Point::new(3.0, 2.0),
            Point::new(7.0, 2.0),
            Point::new(10.0, 0.0),
        );
        let offset = 1.0;
        let segments = b.shifted(16, offset, 0.03);
        assert!(!segments.is_empty());

        // Endpoints of the shifted curve should sit ~offset away from the
        // original endpoints, along the normal direction (down = +y here,
        // since normal_unit((dx,dy)) = unit(dy,-dx); direction at start is
        // (3,2), so normal is (2,-3) normalized -> shifts up-right).
        let first = segments.first().unwrap();
        let start = b.pt1();
        let shifted_start = first.pt1();
        let dist = (shifted_start - start).length();
        assert!((dist - offset).abs() < 0.05, "dist={dist}");

        let last = segments.last().unwrap();
        let end = b.pt4();
        let shifted_end = last.pt4();
        let dist_end = (shifted_end - end).length();
        assert!((dist_end - offset).abs() < 0.05, "dist_end={dist_end}");
    }

    #[test]
    fn degenerate_point_curve_yields_nothing() {
        let p = Point::new(5.0, 5.0);
        let b = QBezier::from_points(p, p, p, p);
        assert!(b.shifted(16, 1.0, 0.03).is_empty());
    }
}
