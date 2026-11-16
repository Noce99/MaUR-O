//! Exploratory measurement: how much smaller does a map's geometry get if runs
//! of straight segments that are really smooth are replaced by cubic beziers?
//!
//! `TOL_CM=3 CORNER_DEG=40 cargo run --release --example curve_fit -- map.omap [fitted.bin]`
//!
//! Corners, flagged vertices (dash, close, hole, gap) and existing curves are
//! kept; only runs of plain line segments between them are refitted, and only
//! when that takes fewer coordinates and stays within the tolerance of the
//! original polyline everywhere.

use maur_o::map::{coord_flag, Coord, ObjectKind};
use maur_o::xml_reader::read_xml_map;
use std::path::Path;

type P = [f64; 2];

fn sub(a: P, b: P) -> P {
    [a[0] - b[0], a[1] - b[1]]
}
fn add(a: P, b: P) -> P {
    [a[0] + b[0], a[1] + b[1]]
}
fn mul(a: P, s: f64) -> P {
    [a[0] * s, a[1] * s]
}
fn dot(a: P, b: P) -> f64 {
    a[0] * b[0] + a[1] * b[1]
}
fn len(a: P) -> f64 {
    dot(a, a).sqrt()
}
fn norm(a: P) -> P {
    let l = len(a);
    if l > 0.0 {
        mul(a, 1.0 / l)
    } else {
        a
    }
}

fn bez(b: &[P; 4], t: f64) -> P {
    let s = 1.0 - t;
    let (b0, b1, b2, b3) = (s * s * s, 3.0 * s * s * t, 3.0 * s * t * t, t * t * t);
    [
        b0 * b[0][0] + b1 * b[1][0] + b2 * b[2][0] + b3 * b[3][0],
        b0 * b[0][1] + b1 * b[1][1] + b2 * b[2][1] + b3 * b[3][1],
    ]
}

fn bez_d1(b: &[P; 4], t: f64) -> P {
    let s = 1.0 - t;
    let q = [sub(b[1], b[0]), sub(b[2], b[1]), sub(b[3], b[2])];
    [
        3.0 * (s * s * q[0][0] + 2.0 * s * t * q[1][0] + t * t * q[2][0]),
        3.0 * (s * s * q[0][1] + 2.0 * s * t * q[1][1] + t * t * q[2][1]),
    ]
}

fn bez_d2(b: &[P; 4], t: f64) -> P {
    let s = 1.0 - t;
    let q = [
        add(sub(b[2], mul(b[1], 2.0)), b[0]),
        add(sub(b[3], mul(b[2], 2.0)), b[1]),
    ];
    [
        6.0 * (s * q[0][0] + t * q[1][0]),
        6.0 * (s * q[0][1] + t * q[1][1]),
    ]
}

// ---- Schneider's curve fitting (Graphics Gems) ----

fn chord_params(pts: &[P]) -> Vec<f64> {
    let mut u = vec![0.0];
    for i in 1..pts.len() {
        u.push(u[i - 1] + len(sub(pts[i], pts[i - 1])));
    }
    let total = *u.last().unwrap();
    for v in u.iter_mut() {
        *v /= total.max(1e-12);
    }
    u
}

fn generate(pts: &[P], u: &[f64], t1: P, t2: P) -> [P; 4] {
    let n = pts.len();
    let (first, last) = (pts[0], pts[n - 1]);
    let (mut c00, mut c01, mut c11, mut x0, mut x1) = (0.0, 0.0, 0.0, 0.0, 0.0);
    for i in 0..n {
        let t = u[i];
        let s = 1.0 - t;
        let a1 = mul(t1, 3.0 * s * s * t);
        let a2 = mul(t2, 3.0 * s * t * t);
        c00 += dot(a1, a1);
        c01 += dot(a1, a2);
        c11 += dot(a2, a2);
        let b = [s * s * s, 3.0 * s * s * t, 3.0 * s * t * t, t * t * t];
        let base = add(mul(first, b[0] + b[1]), mul(last, b[2] + b[3]));
        let tmp = sub(pts[i], base);
        x0 += dot(a1, tmp);
        x1 += dot(a2, tmp);
    }
    let det = c00 * c11 - c01 * c01;
    let (mut al, mut ar) = if det.abs() > 1e-12 {
        ((x0 * c11 - x1 * c01) / det, (c00 * x1 - c01 * x0) / det)
    } else {
        (0.0, 0.0)
    };
    let seg = len(sub(last, first));
    let eps = 1e-6 * seg;
    if al < eps || ar < eps {
        al = seg / 3.0;
        ar = seg / 3.0;
    }
    [first, add(first, mul(t1, al)), add(last, mul(t2, ar)), last]
}

fn newton(b: &[P; 4], p: P, u: f64) -> f64 {
    let d = sub(bez(b, u), p);
    let (d1, d2) = (bez_d1(b, u), bez_d2(b, u));
    let num = dot(d, d1);
    let den = dot(d1, d1) + dot(d, d2);
    if den.abs() < 1e-12 {
        u
    } else {
        (u - num / den).clamp(0.0, 1.0)
    }
}

fn max_error(pts: &[P], b: &[P; 4], u: &[f64]) -> (f64, usize) {
    let mut worst = 0.0;
    let mut split = pts.len() / 2;
    for i in 1..pts.len() - 1 {
        let d = sub(bez(b, u[i]), pts[i]);
        let e = dot(d, d);
        if e >= worst {
            worst = e;
            split = i;
        }
    }
    (worst, split)
}

fn fit(pts: &[P], t1: P, t2: P, err: f64, out: &mut Vec<[P; 4]>) {
    let n = pts.len();
    if n == 2 {
        let d = len(sub(pts[1], pts[0])) / 3.0;
        out.push([
            pts[0],
            add(pts[0], mul(t1, d)),
            add(pts[1], mul(t2, d)),
            pts[1],
        ]);
        return;
    }
    let mut u = chord_params(pts);
    let mut b = generate(pts, &u, t1, t2);
    let (mut e, mut split) = max_error(pts, &b, &u);
    if e < err * err {
        out.push(b);
        return;
    }
    if e < err * err * 16.0 {
        for _ in 0..6 {
            for i in 0..n {
                u[i] = newton(&b, pts[i], u[i]);
            }
            b = generate(pts, &u, t1, t2);
            let r = max_error(pts, &b, &u);
            e = r.0;
            split = r.1;
            if e < err * err {
                out.push(b);
                return;
            }
        }
    }
    let center = norm(sub(pts[split - 1], pts[split + 1]));
    fit(&pts[..=split], t1, center, err, out);
    fit(&pts[split..], mul(center, -1.0), t2, err, out);
}

fn dist_to_polyline(p: P, poly: &[P]) -> f64 {
    let mut best = f64::MAX;
    for w in poly.windows(2) {
        let (a, b) = (w[0], w[1]);
        let ab = sub(b, a);
        let l2 = dot(ab, ab);
        let t = if l2 > 0.0 {
            (dot(sub(p, a), ab) / l2).clamp(0.0, 1.0)
        } else {
            0.0
        };
        best = best.min(len(sub(p, add(a, mul(ab, t)))));
    }
    best
}

// ---- encoding estimate, same stream as tile_stats ----

fn varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}
fn zigzag(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

fn encode(buf: &mut Vec<u8>, symbol: i32, coords: &[(i64, i64, i32)]) {
    varint(buf, symbol.max(0) as u64);
    varint(buf, coords.len() as u64);
    let (mut px, mut py) = (0i64, 0i64);
    for &(x, y, f) in coords {
        buf.push(f as u8);
        varint(buf, zigzag(x - px));
        varint(buf, zigzag(y - py));
        px = x;
        py = y;
    }
}

/// One plain run: vertices `[a..=b]` of line segments, in ground cm.
fn try_fit_run(run: &[P], tol: f64) -> Option<Vec<[P; 4]>> {
    let n = run.len();
    let t1 = norm(sub(run[1], run[0]));
    let t2 = norm(sub(run[n - 2], run[n - 1]));
    let mut out = Vec::new();
    fit(run, t1, t2, tol, &mut out);
    if out.len() * 3 >= n - 1 {
        return None;
    }
    // The curve must stay near the polyline between the samples too.
    for b in &out {
        for k in 1..16 {
            if dist_to_polyline(bez(b, k as f64 / 16.0), run) > tol * 1.5 {
                return None;
            }
        }
    }
    Some(out)
}

fn main() {
    let arg = std::env::args().nth(1).expect("map");
    let tol: f64 = std::env::var("TOL_CM")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3.0);
    let corner_deg: f64 = std::env::var("CORNER_DEG")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(40.0);
    let corner_cos = corner_deg.to_radians().cos();
    let (map, _) = read_xml_map(Path::new(&arg)).expect("read");
    let cm_per_mm = f64::from(map.scale_denominator) / 1000.0 * 100.0;

    let (mut before, mut after) = (Vec::new(), Vec::new());
    let (mut nodes_before, mut nodes_after) = (0usize, 0usize);
    let (mut runs_tried, mut runs_fitted) = (0usize, 0usize);
    for o in &map.objects {
        if matches!(o.kind, ObjectKind::Text(_)) {
            continue;
        }
        let g: Vec<(i64, i64, i32)> = o
            .coords
            .iter()
            .map(|c: &Coord| {
                (
                    (c.x * cm_per_mm).round() as i64,
                    (c.y * cm_per_mm).round() as i64,
                    c.flags,
                )
            })
            .collect();
        nodes_before += g.len();
        encode(&mut before, o.symbol_id, &g);

        // Walk the coordinates; collect plain-line runs.
        let n = g.len();
        let mut result: Vec<(i64, i64, i32)> = Vec::with_capacity(n);
        let pos = |i: usize| -> P { [g[i].0 as f64, g[i].1 as f64] };
        let terminator = |f: i32| {
            f & (coord_flag::HOLE_POINT | coord_flag::GAP_POINT | coord_flag::CLOSE_POINT) != 0
        };
        let mut i = 0;
        let mut already = false;
        while i < n {
            // Emit vertex i as is, then decide on what follows.
            if !already {
                result.push(g[i]);
            }
            already = false;
            if g[i].2 & coord_flag::CURVE_START != 0 && i + 3 < n {
                result.push(g[i + 1]);
                result.push(g[i + 2]);
                i += 3;
                continue;
            }
            if terminator(g[i].2) || i + 1 >= n {
                i += 1;
                continue;
            }
            // Try to extend a run of line segments starting at vertex i.
            let mut j = i;
            while j + 1 < n {
                // segment j -> j+1 is a line if vertex j is not a curve start
                if g[j].2 & coord_flag::CURVE_START != 0 {
                    break;
                }
                // vertex j+1 is part of the run; stop *at* it if it is special
                let special = g[j + 1].2 & (coord_flag::DASH_POINT | coord_flag::CURVE_START) != 0
                    || terminator(g[j + 1].2);
                // corner test at j+1 (needs j+2 and a plain continuation)
                let mut corner = false;
                if !special && j + 2 < n && g[j + 1].2 & coord_flag::CURVE_START == 0 {
                    let a = norm(sub(pos(j + 1), pos(j)));
                    let b = norm(sub(pos(j + 2), pos(j + 1)));
                    corner = dot(a, b) < corner_cos;
                }
                j += 1;
                if special || corner {
                    break;
                }
            }
            // run is vertices i..=j
            if j - i >= 3 {
                runs_tried += 1;
                let run: Vec<P> = (i..=j).map(pos).collect();
                if let Some(curves) = try_fit_run(&run, tol) {
                    runs_fitted += 1;
                    for (k, c) in curves.iter().enumerate() {
                        if k > 0 {
                            // joint vertex: starts the next curve
                            let last = result.last_mut().unwrap();
                            let _ = last;
                        }
                        let start = if k == 0 { result.len() - 1 } else { usize::MAX };
                        let start_flag = |r: &mut Vec<(i64, i64, i32)>, idx: usize| {
                            r[idx].2 |= coord_flag::CURVE_START;
                        };
                        if k == 0 {
                            start_flag(&mut result, start);
                        } else {
                            result.push((
                                c[0][0].round() as i64,
                                c[0][1].round() as i64,
                                coord_flag::CURVE_START,
                            ));
                        }
                        result.push((c[1][0].round() as i64, c[1][1].round() as i64, 0));
                        result.push((c[2][0].round() as i64, c[2][1].round() as i64, 0));
                        if k + 1 == curves.len() {
                            // end vertex keeps the original coordinate and flags
                            result.push(g[j]);
                        }
                    }
                    i = j;
                    already = true;
                    continue;
                }
            }
            i += 1;
        }
        nodes_after += result.len();
        encode(&mut after, o.symbol_id, &result);
    }
    println!("tolerance {tol} cm, corner {corner_deg} deg");
    println!("runs tried {runs_tried}, fitted {runs_fitted}");
    println!(
        "coords {nodes_before} -> {nodes_after} ({:.1}%)",
        100.0 * nodes_after as f64 / nodes_before as f64
    );
    println!(
        "raw bytes {} -> {} ({:.1}%)",
        before.len(),
        after.len(),
        100.0 * after.len() as f64 / before.len() as f64
    );
    if let Some(p) = std::env::args().nth(2) {
        std::fs::write(&p, &after).unwrap();
        std::fs::write(format!("{p}.orig"), &before).unwrap();
    }
}
