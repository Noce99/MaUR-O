//! Cutting paths to the box of a tile, for [`crate::mot_convert`].
//!
//! Pure geometry on points in a tile's own grid units, as `f64`: no maps, no
//! tiles, no symbols. A tile does not store an object whole once it leaves the
//! tile's box (grown by a buffer, so a stroke or a symbol at the edge is not cut
//! short): it stores the part of it inside, and says where the cuts are.
//!
//! * A **line** (an open path) is cut into pieces by [`clip_line`]. Each piece
//!   says whether its first and last vertex are real ends of the line or cuts,
//!   and, for a piece that starts at a cut, how far along the original line it
//!   starts: that is the dash phase a renderer has to start the piece at, or
//!   the dashes would restart at every tile.
//! * An **area** (a closed path) is cut by [`clip_ring`], which keeps it closed
//!   along the edge of the box. Those new edges are not in the map, so the
//!   vertex an edge of that kind leaves from carries
//!   [`node_flag::CLIPPED`]: a renderer fills
//!   them and strokes everything else.
//!
//! Neither handles curves: [`flatten`] turns them into straight segments first,
//! which is only done to an object that has to be cut.

use crate::mot::node_flag;

/// A point of a path and what the path does there.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Vertex {
    /// Grid units east.
    pub x: f64,
    /// Grid units south.
    pub y: f64,
    /// A set of [`node_flag`] bits.
    pub flags: u8,
}

impl Vertex {
    fn at(x: f64, y: f64) -> Vertex {
        Vertex { x, y, flags: 0 }
    }
}

/// The box paths are cut to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    /// West edge.
    pub x0: f64,
    /// North edge.
    pub y0: f64,
    /// East edge.
    pub x1: f64,
    /// South edge.
    pub y1: f64,
}

impl Rect {
    /// Whether the point is in the box, its edges included.
    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x0 && x <= self.x1 && y >= self.y0 && y <= self.y1
    }
}

/// A part of a line, inside the box.
#[derive(Clone, Debug, PartialEq)]
pub struct LinePiece {
    /// The vertices, at least two.
    pub vertices: Vec<Vertex>,
    /// Whether the first vertex is a cut (the line starts outside the box,
    /// or is cut by it) rather than the line's own start.
    pub start_cut: bool,
    /// Whether the last vertex is a cut rather than the line's own end.
    pub end_cut: bool,
    /// The length of the original line before the first vertex, in grid
    /// units: 0 for a piece that starts where the line does.
    pub offset: f64,
}

/// The parameters `[t0, t1]` of the part of the segment `p -> q` inside the
/// box (Liang and Barsky), or `None` where it is all outside.
fn segment_interval(p: &Vertex, q: &Vertex, r: &Rect) -> Option<(f64, f64)> {
    let (dx, dy) = (q.x - p.x, q.y - p.y);
    let (mut t0, mut t1) = (0.0f64, 1.0f64);
    for (pp, qq) in [
        (-dx, p.x - r.x0),
        (dx, r.x1 - p.x),
        (-dy, p.y - r.y0),
        (dy, r.y1 - p.y),
    ] {
        if pp == 0.0 {
            if qq < 0.0 {
                return None;
            }
        } else {
            let t = qq / pp;
            if pp < 0.0 {
                if t > t1 {
                    return None;
                }
                t0 = t0.max(t);
            } else {
                if t < t0 {
                    return None;
                }
                t1 = t1.min(t);
            }
        }
    }
    Some((t0, t1))
}

/// The parts of the polyline inside `rect`, in order.
///
/// A piece of a single point (the line only touches the box) is dropped.
pub fn clip_line(vertices: &[Vertex], rect: &Rect) -> Vec<LinePiece> {
    let mut pieces: Vec<LinePiece> = Vec::new();
    let Some(first) = vertices.first() else {
        return pieces;
    };
    let mut current: Option<LinePiece> = rect.contains(first.x, first.y).then(|| LinePiece {
        vertices: vec![*first],
        start_cut: false,
        end_cut: false,
        offset: 0.0,
    });
    let mut travelled = 0.0;
    for (i, pair) in vertices.windows(2).enumerate() {
        let (p, q) = (&pair[0], &pair[1]);
        let length = (q.x - p.x).hypot(q.y - p.y);
        match segment_interval(p, q, rect) {
            None => {
                if let Some(mut piece) = current.take() {
                    piece.end_cut = true;
                    pieces.push(piece);
                }
            }
            Some((t0, t1)) => {
                if current.is_none() {
                    let entry = if t0 == 0.0 {
                        *p
                    } else {
                        Vertex::at(p.x + t0 * (q.x - p.x), p.y + t0 * (q.y - p.y))
                    };
                    current = Some(LinePiece {
                        vertices: vec![entry],
                        start_cut: i > 0 || t0 > 0.0,
                        end_cut: false,
                        offset: travelled + t0 * length,
                    });
                }
                let piece = current.as_mut().expect("a piece was just started");
                if t1 < 1.0 {
                    piece
                        .vertices
                        .push(Vertex::at(p.x + t1 * (q.x - p.x), p.y + t1 * (q.y - p.y)));
                    piece.end_cut = true;
                    pieces.push(current.take().expect("there is a piece"));
                } else {
                    piece.vertices.push(*q);
                }
            }
        }
        travelled += length;
    }
    if let Some(piece) = current {
        pieces.push(piece);
    }
    pieces.retain(|p| p.vertices.len() >= 2);
    pieces
}

/// A ring cut to the box (Sutherland and Hodgman), as its vertices without
/// the repeat of the first at the end; empty where nothing of it is inside.
///
/// Where the ring leaves the box and comes back, the cut ring runs along the
/// edge of the box between the two, so one ring can come out as several shapes
/// joined by edges on the border. They are not in the map: the vertex each
/// leaves from has [`node_flag::CLIPPED`]. The fill is right, being the same
/// area inside the box.
pub fn clip_ring(ring: &[Vertex], rect: &Rect) -> Vec<Vertex> {
    let mut out: Vec<Vertex> = ring.to_vec();
    if out.len() > 1 {
        let (a, b) = (out[0], out[out.len() - 1]);
        if a.x == b.x && a.y == b.y {
            out.pop();
        }
    }
    for side in 0..4 {
        if out.is_empty() {
            break;
        }
        let input = std::mem::take(&mut out);
        let inside = |v: &Vertex| match side {
            0 => v.x >= rect.x0,
            1 => v.x <= rect.x1,
            2 => v.y >= rect.y0,
            _ => v.y <= rect.y1,
        };
        // Where the edge `a -> b` crosses the side, on it exactly.
        let cross = |a: &Vertex, b: &Vertex| match side {
            0 | 1 => {
                let x = if side == 0 { rect.x0 } else { rect.x1 };
                Vertex::at(x, a.y + (x - a.x) / (b.x - a.x) * (b.y - a.y))
            }
            _ => {
                let y = if side == 2 { rect.y0 } else { rect.y1 };
                Vertex::at(a.x + (y - a.y) / (b.y - a.y) * (b.x - a.x), y)
            }
        };
        for (i, cur) in input.iter().enumerate() {
            let prev = &input[(i + input.len() - 1) % input.len()];
            match (inside(cur), inside(prev)) {
                (true, true) => out.push(*cur),
                (true, false) => {
                    out.push(cross(prev, cur));
                    out.push(*cur);
                }
                (false, true) => out.push(cross(prev, cur)),
                (false, false) => {}
            }
        }
    }
    out.dedup_by(|b, a| a.x == b.x && a.y == b.y);
    if out.len() > 1 && out[0].x == out[out.len() - 1].x && out[0].y == out[out.len() - 1].y {
        out.pop();
    }
    if out.len() < 3 {
        return Vec::new();
    }
    let n = out.len();
    for i in 0..n {
        let (a, b) = (out[i], out[(i + 1) % n]);
        let on_a_side = (a.x == rect.x0 && b.x == rect.x0)
            || (a.x == rect.x1 && b.x == rect.x1)
            || (a.y == rect.y0 && b.y == rect.y0)
            || (a.y == rect.y1 && b.y == rect.y1);
        if on_a_side {
            out[i].flags |= node_flag::CLIPPED as u8;
        }
    }
    out
}

/// The vertices of one part of a path, curves turned into straight segments.
///
/// `coords` are `(x, y, flags)` as a map stores them: a curve is a vertex
/// flagged [`node_flag::CURVE_START`], then its two control points and its end.
/// Each curve becomes enough segments to be within `tolerance` grid units of
/// it; the vertices that were in the path keep their flags (minus the curve
/// flag), the new ones have none.
pub fn flatten(coords: &[(f64, f64, i32)], tolerance: f64) -> Vec<Vertex> {
    let mut out: Vec<Vertex> = Vec::with_capacity(coords.len());
    let mut i = 0;
    while i < coords.len() {
        let (x, y, flags) = coords[i];
        out.push(Vertex {
            x,
            y,
            flags: (flags & !node_flag::CURVE_START) as u8,
        });
        if flags & node_flag::CURVE_START != 0 && i + 3 < coords.len() {
            let [p0, p1, p2, p3] = [0, 1, 2, 3].map(|k| (coords[i + k].0, coords[i + k].1));
            // |B''| <= 6 * the larger second difference, and the distance
            // between a curve and its chords is at most |B''| / (8 n^2).
            let second = |a: (f64, f64), b: (f64, f64), c: (f64, f64)| {
                (a.0 - 2.0 * b.0 + c.0).hypot(a.1 - 2.0 * b.1 + c.1)
            };
            let bound = 6.0 * second(p0, p1, p2).max(second(p1, p2, p3));
            let n = ((bound / (8.0 * tolerance.max(1e-9))).sqrt().ceil() as usize).clamp(1, 256);
            for k in 1..n {
                let t = k as f64 / n as f64;
                let s = 1.0 - t;
                let (a, b, c, d) = (s * s * s, 3.0 * s * s * t, 3.0 * s * t * t, t * t * t);
                out.push(Vertex::at(
                    a * p0.0 + b * p1.0 + c * p2.0 + d * p3.0,
                    a * p0.1 + b * p1.1 + c * p2.1 + d * p3.1,
                ));
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    out
}

/// Fewer vertices for a path, none of them more than `tolerance` from it
/// (Douglas and Peucker). A vertex with any flag is kept, as are the first
/// and the last: a dash point, a gap, the end of a hole or a close point is
/// what the path means there, not just a bend.
pub fn simplify(vertices: &[Vertex], tolerance: f64) -> Vec<Vertex> {
    let n = vertices.len();
    if n < 3 {
        return vertices.to_vec();
    }
    let anchors: Vec<usize> = (0..n)
        .filter(|&i| i == 0 || i == n - 1 || vertices[i].flags != 0)
        .collect();
    let mut keep = vec![false; n];
    for &a in &anchors {
        keep[a] = true;
    }
    let mut stack: Vec<(usize, usize)> = anchors.windows(2).map(|w| (w[0], w[1])).collect();
    while let Some((lo, hi)) = stack.pop() {
        if hi <= lo + 1 {
            continue;
        }
        let (p, q) = (&vertices[lo], &vertices[hi]);
        let (dx, dy) = (q.x - p.x, q.y - p.y);
        let l2 = dx * dx + dy * dy;
        let (mut worst, mut at) = (0.0f64, lo);
        for (i, v) in vertices.iter().enumerate().take(hi).skip(lo + 1) {
            let t = if l2 > 0.0 {
                (((v.x - p.x) * dx + (v.y - p.y) * dy) / l2).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let d = (v.x - (p.x + t * dx)).hypot(v.y - (p.y + t * dy));
            if d > worst {
                (worst, at) = (d, i);
            }
        }
        if worst > tolerance {
            keep[at] = true;
            stack.push((lo, at));
            stack.push((at, hi));
        }
    }
    vertices
        .iter()
        .zip(&keep)
        .filter(|(_, &k)| k)
        .map(|(v, _)| *v)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(x: f64, y: f64) -> Vertex {
        Vertex::at(x, y)
    }

    fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Rect {
        Rect { x0, y0, x1, y1 }
    }

    fn length(vs: &[Vertex]) -> f64 {
        vs.windows(2)
            .map(|w| (w[1].x - w[0].x).hypot(w[1].y - w[0].y))
            .sum()
    }

    fn area(ring: &[Vertex]) -> f64 {
        let n = ring.len();
        (0..n)
            .map(|i| ring[i].x * ring[(i + 1) % n].y - ring[(i + 1) % n].x * ring[i].y)
            .sum::<f64>()
            / 2.0
    }

    /// A deterministic stream of numbers in `[0, 1)`.
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> f64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (self.0 >> 11) as f64 / (1u64 << 53) as f64
        }
    }

    #[test]
    fn a_line_across_the_box_is_one_piece_cut_at_both_ends() {
        let pieces = clip_line(&[v(-10.0, 5.0), v(30.0, 5.0)], &rect(0.0, 0.0, 20.0, 10.0));
        assert_eq!(pieces.len(), 1);
        let p = &pieces[0];
        assert!(p.start_cut && p.end_cut);
        assert_eq!((p.vertices[0].x, p.vertices[1].x), (0.0, 20.0));
        assert_eq!(p.offset, 10.0);
    }

    #[test]
    fn a_line_inside_is_whole_and_has_real_ends() {
        let line = [v(1.0, 1.0), v(5.0, 5.0), v(9.0, 1.0)];
        let pieces = clip_line(&line, &rect(0.0, 0.0, 20.0, 10.0));
        assert_eq!(pieces.len(), 1);
        assert!(!pieces[0].start_cut && !pieces[0].end_cut);
        assert_eq!(pieces[0].offset, 0.0);
        assert_eq!(pieces[0].vertices.len(), 3);
    }

    #[test]
    fn a_line_outside_is_nothing() {
        assert!(clip_line(&[v(30.0, 0.0), v(40.0, 9.0)], &rect(0.0, 0.0, 20.0, 10.0)).is_empty());
    }

    #[test]
    fn a_line_that_leaves_and_returns_is_two_pieces_and_the_second_knows_its_phase() {
        // In at the left, out at the top, back in at the top, out at the right.
        let line = [v(5.0, 5.0), v(5.0, 15.0), v(15.0, 15.0), v(15.0, 5.0)];
        let pieces = clip_line(&line, &rect(0.0, 0.0, 20.0, 10.0));
        assert_eq!(pieces.len(), 2);
        assert!(!pieces[0].start_cut && pieces[0].end_cut);
        assert!(pieces[1].start_cut && !pieces[1].end_cut);
        // 10 up, 10 across, then 5 down to the box again.
        assert!(
            (pieces[1].offset - 25.0).abs() < 1e-9,
            "{}",
            pieces[1].offset
        );
    }

    #[test]
    fn the_pieces_over_a_partition_add_up_to_the_line() {
        let mut rng = Lcg(7);
        let tiles = [
            rect(0.0, 0.0, 50.0, 50.0),
            rect(50.0, 0.0, 100.0, 50.0),
            rect(0.0, 50.0, 50.0, 100.0),
            rect(50.0, 50.0, 100.0, 100.0),
        ];
        let world = rect(0.0, 0.0, 100.0, 100.0);
        for _ in 0..200 {
            let line: Vec<Vertex> = (0..12)
                .map(|_| v(rng.next() * 160.0 - 30.0, rng.next() * 160.0 - 30.0))
                .collect();
            let inside: f64 = clip_line(&line, &world)
                .iter()
                .map(|p| length(&p.vertices))
                .sum();
            let parts: f64 = tiles
                .iter()
                .flat_map(|t| clip_line(&line, t))
                .map(|p| length(&p.vertices))
                .sum();
            assert!((inside - parts).abs() < 1e-6, "{inside} vs {parts}");
        }
    }

    #[test]
    fn a_ring_inside_is_untouched_and_has_no_cut_edges() {
        let ring = [v(1.0, 1.0), v(9.0, 1.0), v(9.0, 9.0), v(1.0, 9.0)];
        let clipped = clip_ring(&ring, &rect(0.0, 0.0, 20.0, 20.0));
        assert_eq!(clipped.len(), 4);
        assert!(clipped.iter().all(|c| c.flags == 0));
    }

    #[test]
    fn a_ring_across_the_edge_has_one_cut_edge_on_it() {
        let ring = [v(5.0, 2.0), v(15.0, 2.0), v(15.0, 8.0), v(5.0, 8.0)];
        let clipped = clip_ring(&ring, &rect(0.0, 0.0, 10.0, 10.0));
        assert!((area(&clipped).abs() - 5.0 * 6.0).abs() < 1e-9);
        let cut: Vec<&Vertex> = clipped
            .iter()
            .filter(|c| c.flags & node_flag::CLIPPED as u8 != 0)
            .collect();
        assert_eq!(cut.len(), 1);
        assert_eq!(cut[0].x, 10.0);
    }

    #[test]
    fn a_ring_around_the_box_fills_it_and_a_ring_beside_it_is_nothing() {
        let around = [v(-5.0, -5.0), v(25.0, -5.0), v(25.0, 25.0), v(-5.0, 25.0)];
        let clipped = clip_ring(&around, &rect(0.0, 0.0, 10.0, 10.0));
        assert!((area(&clipped).abs() - 100.0).abs() < 1e-9);
        assert!(clipped
            .iter()
            .all(|c| c.flags & node_flag::CLIPPED as u8 != 0));
        let beside = [v(20.0, 0.0), v(30.0, 0.0), v(30.0, 10.0)];
        assert!(clip_ring(&beside, &rect(0.0, 0.0, 10.0, 10.0)).is_empty());
    }

    #[test]
    fn the_rings_over_a_partition_add_up_to_the_area() {
        let mut rng = Lcg(11);
        let tiles = [
            rect(0.0, 0.0, 50.0, 50.0),
            rect(50.0, 0.0, 100.0, 50.0),
            rect(0.0, 50.0, 50.0, 100.0),
            rect(50.0, 50.0, 100.0, 100.0),
        ];
        for _ in 0..200 {
            // A star-shaped polygon around a random centre, with concave dents.
            let (cx, cy) = (30.0 + rng.next() * 40.0, 30.0 + rng.next() * 40.0);
            let n = 5 + (rng.next() * 12.0) as usize;
            let ring: Vec<Vertex> = (0..n)
                .map(|k| {
                    let angle = k as f64 / n as f64 * std::f64::consts::TAU;
                    let radius = 5.0 + rng.next() * 40.0;
                    v(cx + radius * angle.cos(), cy + radius * angle.sin())
                })
                .collect();
            let whole = area(&ring).abs();
            let world = rect(-1000.0, -1000.0, 1000.0, 1000.0);
            let in_world: f64 = area(&clip_ring(&ring, &world)).abs();
            assert!((whole - in_world).abs() < 1e-6);
            let parts: f64 = tiles
                .iter()
                .map(|t| area(&clip_ring(&ring, t)))
                .sum::<f64>()
                .abs();
            let clipped_to_all = area(&clip_ring(&ring, &rect(0.0, 0.0, 100.0, 100.0))).abs();
            assert!(
                (clipped_to_all - parts).abs() < 1e-6,
                "{clipped_to_all} vs {parts}"
            );
        }
    }

    #[test]
    fn a_curve_becomes_segments_and_keeps_its_ends_and_flags() {
        let coords = [
            (0.0, 0.0, node_flag::CURVE_START | node_flag::DASH_POINT),
            (0.0, 100.0, 0),
            (100.0, 100.0, 0),
            (100.0, 0.0, node_flag::DASH_POINT),
            (200.0, 0.0, 0),
        ];
        let flat = flatten(&coords, 0.5);
        assert!(flat.len() > 5);
        assert_eq!((flat[0].x, flat[0].y), (0.0, 0.0));
        assert_eq!(flat[0].flags, node_flag::DASH_POINT as u8);
        let end = flat
            .iter()
            .position(|f| f.x == 100.0 && f.y == 0.0)
            .unwrap();
        assert_eq!(flat[end].flags, node_flag::DASH_POINT as u8);
        assert_eq!(flat.last().unwrap().x, 200.0);
        // Every vertex is on the curve to within the tolerance: x(t), y(t).
        for f in &flat[1..end] {
            let near = (0..=2000).any(|k| {
                let t = f64::from(k) / 2000.0;
                let s = 1.0 - t;
                let x = 3.0 * s * t * t * 100.0 + t * t * t * 100.0;
                let y = 3.0 * s * s * t * 100.0 + 3.0 * s * t * t * 100.0;
                (x - f.x).hypot(y - f.y) < 0.2
            });
            assert!(near, "{f:?}");
        }
    }

    #[test]
    fn simplifying_drops_what_is_within_the_tolerance_and_keeps_the_flagged() {
        let mut line: Vec<Vertex> = (0..11).map(|i| v(f64::from(i), 0.0)).collect();
        line[5].y = 0.4; // a small bump
        line[7].flags = node_flag::DASH_POINT as u8; // on the line, but meaningful
        let out = simplify(&line, 0.5);
        assert_eq!(out.len(), 3, "{out:?}");
        assert_eq!((out[0].x, out[1].x, out[2].x), (0.0, 7.0, 10.0));
        let out = simplify(&line, 0.1);
        assert!(out.iter().any(|p| p.x == 5.0), "the bump is kept at 0.1");
    }

    #[test]
    fn nothing_is_further_from_the_simplified_line_than_the_tolerance() {
        let mut rng = Lcg(5);
        for _ in 0..100 {
            let line: Vec<Vertex> = (0..60)
                .map(|i| v(f64::from(i) * 3.0, rng.next() * 8.0))
                .collect();
            let out = simplify(&line, 2.0);
            assert!(out.len() < line.len());
            assert_eq!((out[0], out[out.len() - 1]), (line[0], line[59]));
            for p in &line {
                let near = out.windows(2).any(|w| {
                    let (dx, dy) = (w[1].x - w[0].x, w[1].y - w[0].y);
                    let t = (((p.x - w[0].x) * dx + (p.y - w[0].y) * dy) / (dx * dx + dy * dy))
                        .clamp(0.0, 1.0);
                    (p.x - (w[0].x + t * dx)).hypot(p.y - (w[0].y + t * dy)) <= 2.0 + 1e-9
                });
                assert!(near, "{p:?}");
            }
        }
    }

    #[test]
    fn a_straight_path_is_not_changed_by_flattening() {
        let coords = [(0.0, 0.0, 0), (5.0, 5.0, 0), (9.0, 1.0, 0)];
        assert_eq!(flatten(&coords, 0.5).len(), 3);
    }
}
