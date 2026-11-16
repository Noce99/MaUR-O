//! Exploratory measurement: how much of a map's solid area fill is hidden
//! under opaque fills of higher colour priority.
//!
//! `cargo run --release --example hidden_areas -- map.omap`

use i_overlay::core::fill_rule::FillRule;
use i_overlay::core::overlay_rule::OverlayRule;
use i_overlay::float::single::SingleFloatOverlay;
use maur_o::geometry::flatten;
use maur_o::map::Symbol;
use maur_o::xml_reader::read_xml_map;
use std::path::Path;

type Contour = Vec<[f64; 2]>;

struct Entry {
    color: i32,
    rings: Vec<Contour>,
    bbox: [f64; 4],
    patterns: bool,
    nodes: usize,
}

fn signed_area(c: &Contour) -> f64 {
    let mut a = 0.0;
    for i in 0..c.len() {
        let (p, q) = (c[i], c[(i + 1) % c.len()]);
        a += p[0] * q[1] - q[0] * p[1];
    }
    a / 2.0
}

fn shapes_area(shapes: &[Vec<Contour>]) -> f64 {
    shapes
        .iter()
        .map(|s| {
            s.iter()
                .enumerate()
                .map(|(i, c)| {
                    if i == 0 {
                        signed_area(c).abs()
                    } else {
                        -signed_area(c).abs()
                    }
                })
                .sum::<f64>()
        })
        .sum()
}

fn perimeter(c: &Contour) -> f64 {
    (0..c.len())
        .map(|i| {
            let (p, q) = (c[i], c[(i + 1) % c.len()]);
            ((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2)).sqrt()
        })
        .sum()
}

fn main() {
    let arg = std::env::args().nth(1).expect("map");
    let path = Path::new(&arg);
    let (map, _) = read_xml_map(path).expect("read");
    let mut entries: Vec<Entry> = Vec::new();
    for o in &map.objects {
        let Some(Symbol::Area(a)) = o.symbol_index.and_then(|s| map.symbols.get(s)) else {
            continue;
        };
        if a.is_hidden || a.color < 0 {
            continue;
        }
        let parts = flatten(&o.coords);
        let rings: Vec<Contour> = parts
            .iter()
            .filter(|p| p.points.len() >= 3)
            .map(|p| p.points.iter().map(|q| [q.x, q.y]).collect())
            .collect();
        if rings.is_empty() {
            continue;
        }
        let mut bbox = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
        for r in &rings {
            for p in r {
                bbox = [
                    bbox[0].min(p[0]),
                    bbox[1].min(p[1]),
                    bbox[2].max(p[0]),
                    bbox[3].max(p[1]),
                ];
            }
        }
        entries.push(Entry {
            color: a.color,
            rings,
            bbox,
            patterns: !a.patterns.is_empty(),
            nodes: o.coords.len(),
        });
    }
    println!("solid-fill area objects: {}", entries.len());

    // Opaque ones are the ones that can hide something.
    let opaque = |c: i32| map.color(c).map(|c| c.opacity >= 0.999).unwrap_or(false);
    let occluders: Vec<usize> = (0..entries.len())
        .filter(|&i| opaque(entries[i].color))
        .collect();

    // Uniform grid over the occluders.
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for e in &entries {
        x0 = x0.min(e.bbox[0]);
        y0 = y0.min(e.bbox[1]);
        x1 = x1.max(e.bbox[2]);
        y1 = y1.max(e.bbox[3]);
    }
    let cell = ((x1 - x0).max(y1 - y0) / 128.0).max(1.0);
    let (gw, gh) = (
        ((x1 - x0) / cell) as usize + 1,
        ((y1 - y0) / cell) as usize + 1,
    );
    let mut grid: Vec<Vec<u32>> = vec![Vec::new(); gw * gh];
    for &i in &occluders {
        let b = entries[i].bbox;
        for gy in (((b[1] - y0) / cell) as usize)..=(((b[3] - y0) / cell) as usize) {
            for gx in (((b[0] - x0) / cell) as usize)..=(((b[2] - x0) / cell) as usize) {
                grid[gy * gw + gx].push(i as u32);
            }
        }
    }

    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    let chunk = entries.len().div_ceil(threads);
    let visible_contours: std::sync::Mutex<Vec<Contour>> = std::sync::Mutex::new(Vec::new());
    let t = std::time::Instant::now();
    // (total area, visible area, fully hidden, removable, removable nodes, partly hidden, partly nodes)
    let mut totals = (0.0f64, 0.0f64, 0usize, 0usize, 0usize, 0usize, 0usize);
    std::thread::scope(|s| {
        let handles: Vec<_> = (0..threads)
            .map(|k| {
                let (entries, grid, vc) = (&entries, &grid, &visible_contours);
                s.spawn(move || {
                    let mut tot = (0.0f64, 0.0f64, 0usize, 0usize, 0usize, 0usize, 0usize);
                    for ei in (k * chunk)..((k + 1) * chunk).min(entries.len()) {
                        let e = &entries[ei];
                        let mut cands: Vec<u32> = Vec::new();
                        let b = e.bbox;
                        for gy in (((b[1] - y0) / cell) as usize)..=(((b[3] - y0) / cell) as usize)
                        {
                            for gx in
                                (((b[0] - x0) / cell) as usize)..=(((b[2] - x0) / cell) as usize)
                            {
                                cands.extend_from_slice(&grid[gy * gw + gx]);
                            }
                        }
                        cands.sort_unstable();
                        cands.dedup();
                        let cands: Vec<&Entry> = cands
                            .iter()
                            .map(|&c| &entries[c as usize])
                            .filter(|c| {
                                c.color < e.color
                                    && c.bbox[0] <= b[2]
                                    && c.bbox[2] >= b[0]
                                    && c.bbox[1] <= b[3]
                                    && c.bbox[3] >= b[1]
                            })
                            .collect();
                        let empty: Vec<Contour> = Vec::new();
                        let mut cur: Vec<Vec<Contour>> = e.rings.clone().overlay(
                            &empty,
                            OverlayRule::Subject,
                            FillRule::EvenOdd,
                        );
                        let total = shapes_area(&cur);
                        for c in &cands {
                            if cur.is_empty() {
                                break;
                            }
                            let flat: Vec<Contour> = cur.iter().flatten().cloned().collect();
                            cur =
                                flat.overlay(&c.rings, OverlayRule::Difference, FillRule::EvenOdd);
                        }
                        let visible = shapes_area(&cur);
                        vc.lock().unwrap().extend(cur.iter().flatten().cloned());
                        tot.0 += total;
                        tot.1 += visible;
                        if visible <= total * 0.001 {
                            tot.2 += 1;
                            if !e.patterns {
                                tot.3 += 1;
                                tot.4 += e.nodes;
                            }
                        } else if visible < total * 0.999 {
                            tot.5 += 1;
                            tot.6 += e.nodes;
                        }
                    }
                    tot
                })
            })
            .collect();
        for h in handles {
            let t = h.join().unwrap();
            totals.0 += t.0;
            totals.1 += t.1;
            totals.2 += t.2;
            totals.3 += t.3;
            totals.4 += t.4;
            totals.5 += t.5;
            totals.6 += t.6;
        }
    });
    let all_nodes: usize = entries.iter().map(|e| e.nodes).sum();
    println!("computed in {:.1?}", t.elapsed());
    println!("opaque occluders: {}", occluders.len());
    println!(
        "fill area (sum over objects) hidden: {:.1}%",
        100.0 * (1.0 - totals.1 / totals.0)
    );
    println!(
        "fully hidden objects: {}  ({} without patterns -> removable, {} nodes)",
        totals.2, totals.3, totals.4
    );
    println!("partly hidden objects: {} ({} nodes)", totals.5, totals.6);
    println!("nodes in all solid-fill area objects: {}", all_nodes);
    let contours = visible_contours.into_inner().unwrap();
    let sum_perim: f64 = contours.iter().map(perimeter).sum();
    let empty: Vec<Contour> = Vec::new();
    let united = contours.overlay(&empty, OverlayRule::Subject, FillRule::NonZero);
    let union_perim: f64 = united.iter().flatten().map(perimeter).sum();
    let unique = (sum_perim + union_perim) / 2.0;
    println!(
        "sum of visible perimeters {:.0} mm, perimeter of their union {:.0} mm",
        sum_perim, union_perim
    );
    println!("shared boundary: {:.1}% of the area boundaries would be stored once instead of twice -> unique edge length is {:.1}% of the sum", 100.0 * (sum_perim - union_perim) / sum_perim, 100.0 * unique / sum_perim);
}
