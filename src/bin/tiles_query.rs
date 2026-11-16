//! Says what a MOT archive holds around a place: the objects in an area, by
//! symbol. This is what a viewer asks, as the command line.
//!
//! ```text
//! tiles_query --around lat,lon [--size 1km] [--zoom Z] [--objects] <archive>
//! tiles_query --bbox min_lon,min_lat,max_lon,max_lat [--zoom Z] [--objects] <archive>
//! ```
//!
//! `--around` takes a point (latitude first, as a map app copies one) and the
//! side of the square to look at, 1 km by default; `--bbox` takes the box
//! directly. Only the tiles the area touches are read. The output is one line
//! per symbol in the area: its code, its name and how many objects; with
//! `--objects` each object is listed too, with its id and where it starts.
//!
//! `--zoom` picks the level to read, the archive's finest by default: a coarser
//! one has only what its symbols' rules keep at that zoom, so it is smaller and
//! shows a larger area at a glance.
//!
//! An object counts when the box around its nodes in a tile touches the area. A
//! tile holds the part of an object inside it and a buffer around it, so an
//! object cut at a tile border is counted once, by its id, whichever tiles the
//! area touches.
//!
//! Exit codes: 0 success, 1 bad usage, 2 the archive cannot be read.

use std::collections::BTreeMap;
use std::collections::HashSet;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use maur_o::mot::{lat_lon, Tile, TileId};
use maur_o::mot_archive::Archive;
use maur_o::mot_symbols::SymbolTable;
use maur_o::orthophoto::web_mercator;

#[derive(Parser)]
#[command(
    version,
    about = "Lists the objects of a MOT archive around a place, by symbol."
)]
struct Args {
    /// The MOT archive (a PMTiles file written by omap_to_tiles).
    #[arg(value_name = "ARCHIVE")]
    archive: PathBuf,

    /// The centre of the area, as lat,lon in degrees.
    #[arg(long, value_name = "LAT,LON", conflicts_with = "bbox")]
    around: Option<String>,

    /// The side of the square around --around: 2km, 500m or metres.
    #[arg(long, default_value = "1km", value_name = "SIZE")]
    size: String,

    /// The area as min_lon,min_lat,max_lon,max_lat in degrees.
    #[arg(long, value_name = "BOX")]
    bbox: Option<String>,

    /// The zoom level to read; the archive's finest by default. A coarser
    /// level has only what its symbols' rules keep at that zoom.
    #[arg(long, value_name = "ZOOM")]
    zoom: Option<u8>,

    /// List every object, not only the count per symbol.
    #[arg(long)]
    objects: bool,
}

fn usage(message: &str) -> (ExitCode, String) {
    (ExitCode::from(1), format!("Error: {message}"))
}

/// The area as `[west, south, east, north]` in Mercator meters.
fn area(args: &Args) -> Result<[f64; 4], (ExitCode, String)> {
    if let Some(bbox) = &args.bbox {
        let v: Vec<f64> = bbox
            .split(',')
            .map(|s| s.trim().parse::<f64>())
            .collect::<Result<_, _>>()
            .map_err(|_| usage("--bbox wants min_lon,min_lat,max_lon,max_lat in degrees"))?;
        let [w, s, e, n] = v[..] else {
            return Err(usage(
                "--bbox wants min_lon,min_lat,max_lon,max_lat in degrees",
            ));
        };
        let (x0, y0) = web_mercator(s, w);
        let (x1, y1) = web_mercator(n, e);
        return Ok([x0, y0, x1, y1]);
    }
    let around = args
        .around
        .as_ref()
        .ok_or_else(|| usage("give --around or --bbox"))?;
    let (lon, lat) = maur_o::osm_download::parse_center(around).map_err(|e| usage(&e))?;
    let side = maur_o::osm_download::parse_size(&args.size).map_err(|e| usage(&e))?;
    // The side is on the ground; a Mercator meter is longer by this much here.
    let half = side / 2.0 / lat.to_radians().cos().max(0.05);
    let (x, y) = web_mercator(lat, lon);
    Ok([x - half, y - half, x + half, y + half])
}

fn run() -> Result<(), (ExitCode, String)> {
    let args = match Args::try_parse() {
        Ok(args) => args,
        Err(e) => {
            let _ = e.print();
            return Err((
                ExitCode::from(if e.exit_code() == 0 { 0 } else { 1 }),
                String::new(),
            ));
        }
    };
    let bounds = area(&args)?;
    let archive =
        Archive::open(&args.archive).map_err(|e| (ExitCode::from(2), format!("Error: {e}")))?;
    let table = SymbolTable::by_name(&archive.info().layer).ok_or_else(|| {
        (
            ExitCode::from(2),
            format!(
                "Error: the archive's symbols are {}, which this tool does not know",
                archive.info().layer
            ),
        )
    })?;
    let info = archive.info();
    let zoom = args.zoom.unwrap_or(info.max_zoom);
    if !(info.min_zoom..=info.max_zoom).contains(&zoom) {
        return Err(usage(&format!(
            "the archive has zoom levels {} to {}, not {zoom}",
            info.min_zoom, info.max_zoom
        )));
    }
    let tiles = archive
        .query(zoom, bounds)
        .map_err(|e| (ExitCode::from(2), format!("Error: {e}")))?;

    // Objects are stored whole in every tile they touch, so one id is counted once.
    let mut seen: HashSet<u64> = HashSet::new();
    let mut per_symbol: BTreeMap<u16, u64> = BTreeMap::new();
    let mut listed: Vec<(u64, u16, f64, f64, usize)> = Vec::new();
    for (id, tile) in &tiles {
        for object in &tile.objects {
            if !touches(tile, *id, object, bounds) || !seen.insert(object.id) {
                continue;
            }
            *per_symbol.entry(object.symbol).or_default() += 1;
            if let Some(first) = object.nodes.first() {
                let (x, y) = tile.node_mercator(*id, first);
                let (lat, lon) = lat_lon(x, y);
                listed.push((object.id, object.symbol, lat, lon, object.nodes.len()));
            }
        }
    }

    println!(
        "{} tiles read at zoom {zoom}, {} objects in the area",
        tiles.len(),
        seen.len()
    );
    let mut rows: Vec<(u16, u64)> = per_symbol.into_iter().collect();
    rows.sort_by_key(|&(_, n)| std::cmp::Reverse(n));
    for (symbol, n) in rows {
        let (code, name) = table
            .get(symbol)
            .map_or(("?", ""), |e| (e.code.as_str(), e.name.as_str()));
        println!("{n:>8}  {code:<8} {name}");
    }
    if args.objects {
        listed.sort_by_key(|o| o.0);
        for (id, symbol, lat, lon, nodes) in listed {
            let code = table.get(symbol).map_or("?", |e| e.code.as_str());
            println!("object {id}: symbol {code}, {nodes} nodes, starts at {lat:.6},{lon:.6}");
        }
    }
    Ok(())
}

/// Whether the box around an object's nodes touches `bounds`.
fn touches(tile: &Tile, id: TileId, object: &maur_o::mot::TileObject, bounds: [f64; 4]) -> bool {
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for node in &object.nodes {
        let (x, y) = tile.node_mercator(id, node);
        (x0, x1) = (x0.min(x), x1.max(x));
        (y0, y1) = (y0.min(y), y1.max(y));
    }
    x0 <= bounds[2] && x1 >= bounds[0] && y0 <= bounds[3] && y1 >= bounds[1]
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err((code, message)) => {
            if !message.is_empty() {
                eprintln!("{message}");
            }
            code
        }
    }
}
