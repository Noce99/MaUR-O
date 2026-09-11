//! Turning an object's [`CoordList`] into ground-meter [`geo`] geometry, for
//! `contours_to_raster` (see `Contours-to-Raster.md`).
//!
//! [`bezier_to_linestring`] and [`resample_equal_chords`] are Appendix 1,
//! ported close to verbatim from the doc. [`ls_to_polygon`] is Appendix 2 and
//! [`densify`] is Appendix 3: both are thin wrappers around a `geo` trait the
//! doc calls directly, kept here only so every step reaches them through this
//! module rather than reaching into `geo` with slightly different call sites.
//!
//! [`coords_to_linestrings`] is the one function the doc gives no code for:
//! it walks a `CoordList` the same way [`crate::geometry::flatten`] does
//! (splitting subpaths with `crate::geometry::part_ranges`, and switching
//! on `Coord::is_curve_start` one vertex at a time), but flattens curves
//! with Appendix 1's algorithm at a caller-given step in ground meters
//! instead of `flatten`'s fixed paper-mm tolerance, and finishes with
//! Appendix 1's equal-chord resampling -- two things `flatten` does not do,
//! because it exists for a different purpose (drawing, at display
//! resolution). Reusing `flatten` itself would mean fighting its tolerance
//! and its lack of resampling; reusing just the subpath splitting is the
//! actual overlap between the two.

use geo::algorithm::line_measures::Euclidean;
use geo::algorithm::{Area, Buffer};
use geo::Densify;
use geo::{Coord, LineString, MultiPolygon, Polygon};

use crate::geometry::part_ranges;
use crate::map::CoordList;

/// Ground meters per mm on the paper, from a map's scale denominator (e.g.
/// `15000` for a 1:15000 map). The output is elevation "up to a constant"
/// (see `Contours-to-Raster.md`'s opening line), so this flat, origin-free
/// scale factor is all the conversion needs -- there is no full georeferenced
/// transform anywhere else in this crate either (see `stats::meters_per_mm`,
/// `render.rs`'s `mm_per_meter`, both the same ratio in the opposite
/// direction).
pub fn meters_per_mm(scale_denominator: i32) -> f64 {
    scale_denominator as f64 / 1000.0
}

fn lerp(a: Coord<f64>, b: Coord<f64>, t: f64) -> Coord<f64> {
    Coord {
        x: a.x + (b.x - a.x) * t,
        y: a.y + (b.y - a.y) * t,
    }
}

/// Split a cubic Bezier at t = 0.5 into two cubic Beziers (De Casteljau).
fn subdivide(
    p0: Coord<f64>,
    p1: Coord<f64>,
    p2: Coord<f64>,
    p3: Coord<f64>,
) -> ([Coord<f64>; 4], [Coord<f64>; 4]) {
    let p01 = lerp(p0, p1, 0.5);
    let p12 = lerp(p1, p2, 0.5);
    let p23 = lerp(p2, p3, 0.5);
    let p012 = lerp(p01, p12, 0.5);
    let p123 = lerp(p12, p23, 0.5);
    let mid = lerp(p012, p123, 0.5);
    ([p0, p01, p012, mid], [mid, p123, p23, p3])
}

fn dist(a: Coord<f64>, b: Coord<f64>) -> f64 {
    ((b.x - a.x).powi(2) + (b.y - a.y).powi(2)).sqrt()
}

/// Perpendicular distance from `p` to the infinite line through `a` and `b`.
/// Falls back to the distance to `a` when `a == b` (a degenerate chord).
fn point_line_distance(p: Coord<f64>, a: Coord<f64>, b: Coord<f64>) -> f64 {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len = (dx * dx + dy * dy).sqrt();
    if len == 0.0 {
        return dist(p, a);
    }
    ((p.x - a.x) * dy - (p.y - a.y) * dx).abs() / len
}

/// How far the curve's control points bow away from the p0-p3 chord.
fn flatness(p0: Coord<f64>, p1: Coord<f64>, p2: Coord<f64>, p3: Coord<f64>) -> f64 {
    point_line_distance(p1, p0, p3).max(point_line_distance(p2, p0, p3))
}

fn subdivide_recursive(
    p0: Coord<f64>,
    p1: Coord<f64>,
    p2: Coord<f64>,
    p3: Coord<f64>,
    step: f64,
    out: &mut Vec<Coord<f64>>,
    depth: u32,
) {
    // A short chord alone does not mean the curve is flat: a tight loop or
    // hairpin can have p0 close to p3 while still bowing far away from the
    // p0-p3 line. Require the curve to be flat *and* the chord to be short
    // before treating it as a single straight segment.
    if depth >= 24 || (flatness(p0, p1, p2, p3) <= step && dist(p0, p3) <= step) {
        out.push(p3);
        return;
    }
    let (left, right) = subdivide(p0, p1, p2, p3);
    subdivide_recursive(left[0], left[1], left[2], left[3], step, out, depth + 1);
    subdivide_recursive(right[0], right[1], right[2], right[3], step, out, depth + 1);
}

/// Flattens a cubic Bezier curve into straight line segments no longer than
/// `step`. See Appendix 1 of `Contours-to-Raster.md`.
pub fn bezier_to_linestring(
    p0: Coord<f64>,
    p1: Coord<f64>,
    p2: Coord<f64>,
    p3: Coord<f64>,
    step: f64,
) -> LineString<f64> {
    let mut out = vec![p0];
    subdivide_recursive(p0, p1, p2, p3, step, &mut out, 0);
    LineString::new(out)
}

/// Smallest t in [t_min, 1] with |A + t(B-A) - C| == r.
fn circle_exit(a: Coord<f64>, b: Coord<f64>, c: Coord<f64>, r: f64, t_min: f64) -> Option<f64> {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let (fx, fy) = (a.x - c.x, a.y - c.y);

    let qa = dx * dx + dy * dy;
    if qa == 0.0 {
        return None; // duplicate points
    }
    let qb = 2.0 * (fx * dx + fy * dy);
    let qc = fx * fx + fy * fy - r * r;

    let disc = qb * qb - 4.0 * qa * qc;
    if disc < 0.0 {
        return None;
    }
    let sq = disc.sqrt();
    [(-qb - sq) / (2.0 * qa), (-qb + sq) / (2.0 * qa)]
        .into_iter()
        .find(|&t| t >= t_min && t <= 1.0)
}

/// Resamples a `LineString` so consecutive `Coord`s are `step` apart. An
/// open input's last segment is excepted and may come out shorter, kept as
/// whatever remainder is left. A closed input has no such segment to absorb
/// a remainder into, so it is handled differently in two ways -- otherwise
/// the ring's closing segment could come out anywhere from a full `step`
/// down to a near-zero sliver, which starves it of
/// `sources_per_contour_segment`'s intended spacing in Step 2 and, at the
/// zero extreme, hands `vector_on_side` a zero-length tangent:
/// - `step` is shrunk to the nearest length that divides the perimeter
///   evenly. This is exact on straight stretches, but each hop that crosses
///   a corner covers more arc length than its `step`-long chord, so on a
///   sharply bent contour the walk can still finish a little short.
/// - Whatever closing segment is left is checked against half of that
///   shrunk `step`: too short to stand on its own, it is merged into the
///   preceding segment instead of kept as its own near-zero one.
///
/// See Appendix 1 of `Contours-to-Raster.md`.
pub fn resample_equal_chords(ls: &LineString<f64>, step: f64) -> LineString<f64> {
    assert!(step > 0.0);
    let pts = &ls.0;
    if pts.len() < 2 {
        return ls.clone();
    }

    let closed = pts.len() > 2 && pts[0] == *pts.last().unwrap();
    let step = if closed {
        let perimeter: f64 = pts.windows(2).map(|w| dist(w[0], w[1])).sum();
        let n = (perimeter / step).round().max(1.0);
        perimeter / n
    } else {
        step
    };

    let mut out = vec![pts[0]];
    let mut cur = pts[0]; // circle centre = last emitted point
    let mut seg = 0usize; // segment pts[seg] -> pts[seg+1]
    let mut t = 0.0f64; // parameter of `cur` inside that segment

    'outer: loop {
        let mut i = seg;
        let mut t0 = t;
        loop {
            if i + 1 >= pts.len() {
                break 'outer; // tail shorter than `step`
            }
            let (a, b) = (pts[i], pts[i + 1]);
            if let Some(th) = circle_exit(a, b, cur, step, t0) {
                let p = Coord {
                    x: a.x + th * (b.x - a.x),
                    y: a.y + th * (b.y - a.y),
                };
                out.push(p);
                cur = p;
                seg = i;
                t = th;
                break;
            }
            i += 1;
            t0 = 0.0;
        }
    }

    // The loop above stops once the remaining tail is shorter than `step`,
    // without ever emitting the original last point. For an open input,
    // always add it back: this recovers the (possibly short) final segment,
    // by design (see this function's docs).
    let last = *pts.last().unwrap();
    if closed {
        // `step` was already shrunk above to divide the perimeter evenly,
        // so this is usually an exact (or float-noise-only) landing on
        // `last` already. When corner crossings left a real remainder,
        // merge it into the preceding segment rather than keep it as its
        // own too-short one -- unless there is no preceding resampled point
        // to merge into (a ring shorter than one `step`), in which case
        // there is nothing to do but close it as-is.
        let out_last = *out.last().unwrap();
        if out_last != last {
            if out.len() > 1 && dist(out_last, last) < step / 2.0 {
                *out.last_mut().unwrap() = last;
            } else {
                out.push(last);
            }
        }
    } else if *out.last().unwrap() != last {
        out.push(last);
    }

    LineString::new(out)
}

/// One vertex of a still-curved contour's own raw node sequence, in ground
/// meters, carrying the same [`crate::map::Coord::is_curve_start`] flag
/// [`coords_to_linestrings`] reads to find each Bezier segment. Kept
/// alongside the converted position (rather than converting straight to a
/// `geo::LineString`, which has no notion of curves) so a renderer can draw
/// the actual curve -- an SVG cubic `C` command through the next three
/// vertices -- instead of connecting the anchor and control points with
/// straight lines.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RawVertex {
    /// The vertex's own position, in ground meters.
    pub coord: Coord<f64>,
    /// Whether this vertex is a curve's first anchor, i.e. whether it and
    /// the next three vertices form one cubic Bezier segment (see
    /// [`crate::map::Coord::is_curve_start`]).
    pub is_curve_start: bool,
}

/// The object's coordinates, per subpath, converted straight to ground
/// meters with no curve flattening or resampling at all -- the "before"
/// picture [`coords_to_linestrings`]'s processing is compared against in the
/// `--create_svg` visualization (`Contours-to-Raster.md`'s "##
/// Visualization": the raw, still-curved contour in red, next to the
/// linearized and resampled one in green).
pub fn raw_polylines(coords: &CoordList, meters_per_mm: f64) -> Vec<Vec<RawVertex>> {
    let to_vertex = |c: &crate::map::Coord| RawVertex {
        coord: Coord {
            x: c.x * meters_per_mm,
            y: c.y * meters_per_mm,
        },
        is_curve_start: c.is_curve_start(),
    };
    part_ranges(coords)
        .into_iter()
        .map(|range| {
            coords[range.begin..range.end]
                .iter()
                .map(to_vertex)
                .collect()
        })
        .collect()
}

/// Turns an object's raw [`CoordList`] (mm on paper, curves flagged per
/// `Coord::is_curve_start`) into one ground-meter, equal-chord-spaced
/// `LineString` per subpath (see the module docs for why this is not just a
/// call to [`crate::geometry::flatten`]).
pub fn coords_to_linestrings(
    coords: &CoordList,
    meters_per_mm: f64,
    bezier_linearization_step: f64,
    contours_step: f64,
) -> Vec<LineString<f64>> {
    let to_m = |c: &crate::map::Coord| Coord {
        x: c.x * meters_per_mm,
        y: c.y * meters_per_mm,
    };

    let mut out = Vec::new();
    for range in part_ranges(coords) {
        let mut pts: Vec<Coord<f64>> = vec![to_m(&coords[range.begin])];
        let mut i = range.begin;
        while i + 1 < range.end {
            if coords[i].is_curve_start() && i + 3 < range.end {
                let (p0, p1, p2, p3) = (
                    to_m(&coords[i]),
                    to_m(&coords[i + 1]),
                    to_m(&coords[i + 2]),
                    to_m(&coords[i + 3]),
                );
                let curve = bezier_to_linestring(p0, p1, p2, p3, bezier_linearization_step);
                // `curve`'s first point duplicates the one already pushed.
                pts.extend(curve.0.into_iter().skip(1));
                i += 3;
            } else {
                pts.push(to_m(&coords[i + 1]));
                i += 1;
            }
        }
        let ls = LineString::new(pts);
        out.push(resample_equal_chords(&ls, contours_step));
    }
    out
}

/// The index of `ls`'s closest node to `p`. Shared by Step 0 (locating a
/// Heavy Object intersection along the contour it crosses, for the circle
/// fit) and Step 1 (locating a definer's own local tangent on the contour it
/// gives evidence about).
pub(crate) fn nearest_index(ls: &LineString<f64>, p: Coord<f64>) -> usize {
    ls.0.iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| {
            let da = (a.x - p.x).hypot(a.y - p.y);
            let db = (b.x - p.x).hypot(b.y - p.y);
            da.total_cmp(&db)
        })
        .map(|(i, _)| i)
        .unwrap_or(0)
}

/// The two endpoints of the segment of `ls` at node `idx`: `(ls[idx],
/// ls[idx+1])`, or `(ls[idx-1], ls[idx])` at the very last node. A local
/// tangent to hand [`crate::gravity_model::set_or_check_gravity`] or
/// [`crate::gravity_model::vote_side`] for a reading taken near node `idx`.
pub(crate) fn local_tangent(ls: &LineString<f64>, idx: usize) -> (Coord<f64>, Coord<f64>) {
    if idx + 1 < ls.0.len() {
        (ls.0[idx], ls.0[idx + 1])
    } else if idx > 0 {
        (ls.0[idx - 1], ls.0[idx])
    } else {
        (ls.0[idx], ls.0[idx])
    }
}

/// Buffers a `LineString` into the polygon `Contours-to-Raster.md`'s
/// [`crate::gravity_model::LineGravityDefiners`] carries. See Appendix 2.
pub fn ls_to_polygon(ls: &LineString<f64>, width: f64, extra_growing: f64) -> Polygon<f64> {
    let line_with_width: MultiPolygon<f64> = ls.buffer(width);
    let fat_polygons: MultiPolygon<f64> = line_with_width.buffer(extra_growing);
    // In theory we should never have more than one polygon because we do not have line
    // intersection and we use positive buffer sizes, but buffering is numeric and can still
    // produce extra slivers in degenerate cases (e.g. very tight hairpins). If that happens
    // we keep the largest polygon and emit a warning rather than silently dropping data.
    if fat_polygons.0.len() > 1 {
        eprintln!(
            "warning: buffering a LineString produced {} disjoint polygons instead of 1; keeping only the largest by area",
            fat_polygons.0.len()
        );
    }
    fat_polygons
        .0
        .into_iter()
        .max_by(|a, b| a.unsigned_area().total_cmp(&b.unsigned_area()))
        .expect("buffering a non-empty LineString always yields at least one polygon")
}

/// Densifies a `LineString` for writing to the Contour Raster / checking
/// Heavy Object intersections. See Appendix 3.
pub fn densify(
    ls: &LineString<f64>,
    rasterization_step_factor: f64,
    rasterization_px_size: f64,
) -> LineString<f64> {
    Euclidean.densify(ls, rasterization_step_factor * rasterization_px_size)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map::Coord as MapCoord;

    fn c(x: f64, y: f64) -> Coord<f64> {
        Coord { x, y }
    }

    #[test]
    fn bezier_with_collinear_controls_is_two_points() {
        let ls = bezier_to_linestring(c(0.0, 0.0), c(1.0, 0.0), c(2.0, 0.0), c(3.0, 0.0), 10.0);
        assert_eq!(ls.0, vec![c(0.0, 0.0), c(3.0, 0.0)]);
    }

    #[test]
    fn bezier_chords_stay_under_step() {
        // A sharply curved arc-like Bezier.
        let ls = bezier_to_linestring(c(0.0, 0.0), c(0.0, 10.0), c(10.0, 10.0), c(10.0, 0.0), 0.5);
        for w in ls.0.windows(2) {
            assert!(
                dist(w[0], w[1]) <= 0.5 + 1e-9,
                "chord {:?}-{:?} too long",
                w[0],
                w[1]
            );
        }
    }

    #[test]
    fn resample_keeps_near_equal_spacing_on_a_straight_line() {
        let ls = LineString::new(vec![c(0.0, 0.0), c(10.0, 0.0)]);
        let resampled = resample_equal_chords(&ls, 3.0);
        // 0, 3, 6, 9, 10 (tail).
        assert_eq!(resampled.0.len(), 5);
        for w in resampled.0.windows(2).take(3) {
            assert!((dist(w[0], w[1]) - 3.0).abs() < 1e-9);
        }
    }

    #[test]
    fn resample_preserves_closedness() {
        let ls = LineString::new(vec![
            c(0.0, 0.0),
            c(10.0, 0.0),
            c(10.0, 10.0),
            c(0.0, 10.0),
            c(0.0, 0.0),
        ]);
        let resampled = resample_equal_chords(&ls, 3.0);
        assert_eq!(*resampled.0.first().unwrap(), *resampled.0.last().unwrap());
    }

    #[test]
    fn resample_closed_input_has_no_short_closing_segment() {
        // A 34-unit-perimeter closed rectangle: 34 / 5.0 does not divide
        // evenly, so a naive fixed-step walk would leave a short (4-unit)
        // remainder segment to close the ring. No segment -- including the
        // closing one -- should come out shorter than half the (adapted)
        // step: either the perimeter-dividing step landed it close to a
        // full step already, or the too-short leftover got merged away.
        let ls = LineString::new(vec![
            c(0.0, 0.0),
            c(10.0, 0.0),
            c(10.0, 7.0),
            c(0.0, 7.0),
            c(0.0, 0.0),
        ]);
        let resampled = resample_equal_chords(&ls, 5.0);
        assert_eq!(*resampled.0.first().unwrap(), *resampled.0.last().unwrap());
        let lens: Vec<f64> = resampled.0.windows(2).map(|w| dist(w[0], w[1])).collect();
        let min = lens.iter().cloned().fold(f64::INFINITY, f64::min);
        assert!(min > 2.4, "a segment came out too short: {:?}", lens);
    }

    #[test]
    fn resample_short_input_is_unchanged() {
        let ls = LineString::new(vec![c(0.0, 0.0)]);
        let resampled = resample_equal_chords(&ls, 3.0);
        assert_eq!(resampled, ls);
    }

    #[test]
    fn coords_to_linestrings_scales_mm_to_meters_and_keeps_closedness() {
        // A closed square, 1000mm per side, straight segments only.
        let coords: CoordList = vec![
            MapCoord::new(0.0, 0.0, 0),
            MapCoord::new(1000.0, 0.0, 0),
            MapCoord::new(1000.0, 1000.0, 0),
            MapCoord::new(0.0, 1000.0, 0),
            MapCoord::new(0.0, 0.0, crate::map::coord_flag::CLOSE_POINT),
        ];
        // meters_per_mm=0.001 -> a 1000mm side is 1m on the ground.
        let out = coords_to_linestrings(&coords, 0.001, 0.05, 0.1);
        assert_eq!(out.len(), 1);
        let ls = &out[0];
        assert_eq!(*ls.0.first().unwrap(), *ls.0.last().unwrap());
        assert!((ls.0[0].x - 0.0).abs() < 1e-9 && (ls.0[0].y - 0.0).abs() < 1e-9);
        // Resampling at 0.1m over a 4m perimeter should land close to 41
        // points (40 steps of 0.1m plus the closing tail point), regardless
        // of where the original corners were -- resample_equal_chords is not
        // vertex-preserving, it walks the curve at fixed arc-length steps.
        assert!(
            ls.0.len() >= 38 && ls.0.len() <= 42,
            "got {} points",
            ls.0.len()
        );
        for w in ls.0.windows(2) {
            assert!(dist(w[0], w[1]) <= 0.1 + 1e-9);
        }
    }

    #[test]
    fn coords_to_linestrings_splits_on_curve_start() {
        // One straight segment, then a curve back to the start (closed).
        let coords: CoordList = vec![
            MapCoord::new(0.0, 0.0, 0),
            MapCoord::new(0.0, 10000.0, crate::map::coord_flag::CURVE_START),
            MapCoord::new(10000.0, 10000.0, 0),
            MapCoord::new(10000.0, 0.0, 0),
            MapCoord::new(0.0, 0.0, crate::map::coord_flag::CLOSE_POINT),
        ];
        // A contours_step comfortably smaller than the path's length (tens
        // of meters here) so resampling doesn't collapse it to just the two
        // endpoints.
        let out = coords_to_linestrings(&coords, 0.001, 0.5, 1.0);
        assert_eq!(out.len(), 1);
        let ls = &out[0];
        assert_eq!(*ls.0.first().unwrap(), *ls.0.last().unwrap());
        assert!(ls.0.len() > 5); // the curve should have been subdivided and then resampled
    }
}
