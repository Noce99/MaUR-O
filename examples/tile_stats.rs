//! Exploratory measurement for the planned tile format: how many objects and
//! nodes a map has, and how big a delta/varint encoding of its geometry would
//! be (ground coordinates on a 1 cm grid, objects sorted by symbol).
//!
//! `cargo run --release --example tile_stats -- map.omap [dump.bin]`
//!
//! With a second argument the estimated stream is written there, so it can be
//! compressed with an external tool (zstd, xz) to see the end-to-end ratio.

use maur_o::map::{coord_flag, ObjectKind};
use maur_o::xml_reader::read_xml_map;
use std::collections::BTreeMap;
use std::path::Path;

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

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = Path::new(&args[1]);
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let t = std::time::Instant::now();
    let (map, _) = read_xml_map(path).expect("read");
    eprintln!("parsed in {:.1?}", t.elapsed());

    // Paper mm -> ground cm.
    let grid_cm: f64 = std::env::var("GRID_CM")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1.0);
    let cm_per_mm = f64::from(map.scale_denominator) / 1000.0 * 100.0 / grid_cm;

    let (mut points, mut paths, mut texts) = (0usize, 0usize, 0usize);
    let (mut nodes, mut curves, mut closes, mut holes, mut dashes, mut gaps) =
        (0usize, 0usize, 0usize, 0usize, 0usize, 0usize);
    let mut per_symbol: BTreeMap<i32, (usize, usize)> = BTreeMap::new();
    let mut order: Vec<usize> = (0..map.objects.len()).collect();
    order.sort_by_key(|&i| map.objects[i].symbol_id);

    let mut buf: Vec<u8> = Vec::new();
    let mut deltas_len = [0usize; 4]; // 1,2,3,4+ byte zigzag deltas
    for &i in &order {
        let o = &map.objects[i];
        let e = per_symbol.entry(o.symbol_id).or_default();
        e.0 += 1;
        e.1 += o.coords.len();
        match &o.kind {
            ObjectKind::Point => points += 1,
            ObjectKind::Path(_) => paths += 1,
            ObjectKind::Text(t) => {
                texts += 1;
                varint(&mut buf, t.text.len() as u64);
                buf.extend_from_slice(t.text.as_bytes());
            }
        }
        varint(&mut buf, o.symbol_id.max(0) as u64);
        varint(&mut buf, o.coords.len() as u64);
        let (mut px, mut py) = (0i64, 0i64);
        for c in &o.coords {
            nodes += 1;
            let f = c.flags;
            curves += (f & coord_flag::CURVE_START != 0) as usize;
            closes += (f & coord_flag::CLOSE_POINT != 0) as usize;
            holes += (f & coord_flag::HOLE_POINT != 0) as usize;
            dashes += (f & coord_flag::DASH_POINT != 0) as usize;
            gaps += (f & coord_flag::GAP_POINT != 0) as usize;
            let (x, y) = (
                (c.x * cm_per_mm).round() as i64,
                (c.y * cm_per_mm).round() as i64,
            );
            buf.push(f as u8);
            for d in [x - px, y - py] {
                let before = buf.len();
                varint(&mut buf, zigzag(d));
                deltas_len[(buf.len() - before).min(4) - 1] += 1;
            }
            px = x;
            py = y;
        }
    }

    println!("file bytes        {size}");
    println!("scale             1:{}", map.scale_denominator);
    println!("symbols defined   {}", map.symbols.len());
    println!("symbols used      {}", per_symbol.len());
    println!(
        "objects           {} (point {points}, path {paths}, text {texts})",
        map.objects.len()
    );
    println!(
        "nodes             {nodes}  ({:.1} per object)",
        nodes as f64 / map.objects.len().max(1) as f64
    );
    println!(
        "  curve starts {curves} ({:.1}%), close {closes}, hole {holes}, dash {dashes}, gap {gaps}",
        100.0 * curves as f64 / nodes.max(1) as f64
    );
    println!(
        "  file bytes/node  {:.1}",
        size as f64 / nodes.max(1) as f64
    );
    println!(
        "encoded bytes     {}  ({:.2} per node)",
        buf.len(),
        buf.len() as f64 / nodes.max(1) as f64
    );
    let total: usize = deltas_len.iter().sum();
    println!(
        "  delta sizes: 1B {:.0}%  2B {:.0}%  3B {:.0}%  4B+ {:.0}%",
        100.0 * deltas_len[0] as f64 / total.max(1) as f64,
        100.0 * deltas_len[1] as f64 / total.max(1) as f64,
        100.0 * deltas_len[2] as f64 / total.max(1) as f64,
        100.0 * deltas_len[3] as f64 / total.max(1) as f64
    );
    let mut top: Vec<_> = per_symbol.iter().collect();
    top.sort_by_key(|(_, v)| std::cmp::Reverse(v.1));
    println!("top symbols by nodes (id: objects, nodes):");
    for (id, (n, c)) in top.iter().take(8) {
        println!("  {id}: {n}, {c}");
    }
    if let Some(p) = args.get(2) {
        std::fs::write(p, &buf).expect("write dump");
    }
}
