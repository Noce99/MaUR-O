//! Converts a map into MOT tiles and writes each tile's payload to a directory,
//! so the size per tile can be measured (e.g. `zstd -19 -r`).
//!
//! `cargo run --release --example mot_try -- map.omap out_dir [zoom] [grid_cm]`

use maur_o::mot_convert::{convert, Options};
use maur_o::mot_symbols::SymbolTable;
use maur_o::orthophoto::geographic_anchor;
use maur_o::xml_reader::read_xml_map_str;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let text = std::fs::read_to_string(&args[1]).expect("read");
    let anchor = geographic_anchor(&text).expect("map has no ref_point_deg");
    let (map, _) = read_xml_map_str(&text).expect("parse");
    let options = Options {
        zoom: args.get(3).and_then(|v| v.parse().ok()).unwrap_or(14),
        grid_cm: args.get(4).and_then(|v| v.parse().ok()).unwrap_or(5),
        ..Options::default()
    };
    let t = std::time::Instant::now();
    let converted = convert(&map, &anchor, &SymbolTable::isom(), 0, &options).expect("convert");
    eprintln!("converted in {:.1?}", t.elapsed());
    std::fs::create_dir_all(&args[2]).unwrap();
    let mut total = 0usize;
    let mut stored = 0usize;
    for (id, tile) in &converted.tiles {
        let bytes = tile.encode();
        total += bytes.len();
        stored += tile.objects.len();
        std::fs::write(format!("{}/{}_{}_{}.mot", args[2], id.z, id.x, id.y), bytes).unwrap();
    }
    println!(
        "{} objects ({} dropped, {} remapped, {} hidden/helper left out), {} tiles, {} object copies, {} raw bytes",
        converted.objects,
        converted.dropped,
        converted.remapped,
        converted.hidden,
        converted.tiles.len(),
        stored,
        total
    );
    for w in converted.warnings.iter().take(40) {
        println!("warning: {w}");
    }
    if converted.warnings.len() > 40 {
        println!("... {} more warnings", converted.warnings.len() - 40);
    }
}
