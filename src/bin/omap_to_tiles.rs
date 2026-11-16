//! Converts maps into a MOT archive: the tiled world-map format of
//! `mds/MOT-Format.md`, in a PMTiles file.
//!
//! ```text
//! omap_to_tiles [--zoom 14] [--min-zoom 11] [--grid-cm 5] [--buffer 0.03125] --output world.pmtiles <map-file>...
//! ```
//!
//! Each map must be georeferenced with a geographic reference point (Mapper's
//! `<ref_point_deg>`, which every georeferenced map it saves has) so it can be
//! placed on the earth; one that is not is skipped with a warning. All the
//! maps go into one archive, their objects merged tile by tile.
//!
//! The symbols are those of a table: ISOM 2017-2 or ISSprOM 2019, found for each
//! map from what it says it is drawn to and from the symbols its objects use
//! (`--standard auto`), or the one asked for. One archive holds one symbol set:
//! maps of both give two archives, written as `FILE` with `.isom` and `.issprom`
//! before its extension. A symbol of the map that is not in the table is stored
//! as the closest one (same code, the symbol it is a variant of,
//! or the same name) or, where there is none, its objects are dropped; either
//! is reported as a warning, with how many objects it concerns. Objects of
//! hidden symbols and drawing aids are left out: they draw nothing.
//!
//! | option | default | |
//! |---|---|---|
//! | `--zoom` | 14 | the finest zoom level, 1 to 20 (14 is about 2 km wide): every object is kept as drawn |
//! | `--min-zoom` | 11 | the coarsest zoom level |
//! | `--grid-cm` | 5 | resolution of the nodes, in centimetres of Mercator |
//! | `--buffer` | 0.03125 | how far past its edge a tile keeps what it holds, as a fraction of its side |
//!
//! The levels from `--zoom` up to `--min-zoom` are all written. The finest has
//! every object as drawn. A coarser level has only what the rules of each symbol
//! (its `min_zoom`, a size under which an object is too small to see, how far
//! its line may be moved to have fewer nodes) keep at that zoom, on a coarser
//! grid, so a zoomed-out view reads a little of the archive instead of all of
//! it. The objects have the same ids at every level.
//!
//! A tile holds the part of each object inside its box grown by the buffer, so
//! a line or a symbol at the edge is not cut short; an object that crosses the
//! box has its curves flattened, and its pieces say where they were cut and,
//! for a line, how far along it they start, so its dashes stay in step from
//! tile to tile.
//!
//! Exit codes: 0 success, 1 bad usage, 2 a map cannot be read or none can be
//! converted, 4 the archive cannot be written.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, ValueEnum};
use maur_o::mot::{Tile, TileId};
use maur_o::mot_archive::{write, ArchiveInfo};
use maur_o::mot_convert::{convert_levels, merge_tiles, Options};
use maur_o::mot_symbols::SymbolTable;
use maur_o::orthophoto::geographic_anchor;
use maur_o::osm_orienteering::Standard;
use maur_o::xml_reader::read_xml_map_str;

/// Which symbol set the maps are stored in.
#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum StandardArg {
    /// Each map in the standard it is drawn to.
    Auto,
    /// Every map in ISOM 2017-2.
    Isom,
    /// Every map in ISSprOM 2019.
    Issprom,
}

#[derive(Parser)]
#[command(
    version,
    about = "Converts Mapper maps into a MOT archive (tiled world map, in PMTiles)."
)]
struct Args {
    /// The maps to convert (.omap or .xmap).
    #[arg(value_name = "MAP_FILE", required = true)]
    maps: Vec<PathBuf>,

    /// The archive to write.
    #[arg(short, long, value_name = "FILE")]
    output: PathBuf,

    /// The symbol set to store the maps in: each in its own standard, found from
    /// what it says and what its symbols are (`auto`), or all in one. One
    /// archive holds one symbol set: with several, the archives are written as
    /// FILE with `.isom` or `.issprom` before its extension.
    #[arg(long, value_enum, default_value = "auto")]
    standard: StandardArg,

    /// The finest zoom level, where every object is kept as drawn.
    #[arg(long, default_value_t = 14, value_parser = clap::value_parser!(u8).range(1..=20))]
    zoom: u8,

    /// The coarsest zoom level. The levels between are generalised by the
    /// symbols' own rules.
    #[arg(long, default_value_t = 11, value_parser = clap::value_parser!(u8).range(1..=20), value_name = "ZOOM")]
    min_zoom: u8,

    /// How far past its edge a tile keeps what it holds, as a fraction of the
    /// tile's side (0 to 0.5).
    #[arg(long, default_value_t = 0.03125, value_parser = parse_buffer, value_name = "FRACTION")]
    buffer: f64,

    /// Resolution of the nodes, in centimetres of Mercator.
    #[arg(long, default_value_t = 5, value_parser = clap::value_parser!(u32).range(1..=100), value_name = "CM")]
    grid_cm: u32,
}

fn parse_buffer(text: &str) -> Result<f64, String> {
    match text.parse::<f64>() {
        Ok(v) if (0.0..=0.5).contains(&v) => Ok(v),
        _ => Err(format!("not a fraction between 0 and 0.5: {text:?}")),
    }
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
    if args.min_zoom > args.zoom {
        return Err((
            ExitCode::from(1),
            "Error: --min-zoom is finer than --zoom".into(),
        ));
    }
    let options = Options {
        zoom: args.zoom,
        detail_zoom: args.zoom,
        grid_cm: args.grid_cm,
        buffer: args.buffer,
    };
    // One archive per symbol set: the tiles and the next id of each so far.
    let mut groups: Vec<(Standard, BTreeMap<TileId, Tile>, u64)> = Vec::new();
    let mut converted_maps = 0usize;
    for path in &args.maps {
        let text = std::fs::read_to_string(path).map_err(|e| {
            (
                ExitCode::from(2),
                format!("Error: cannot read {}: {e}", path.display()),
            )
        })?;
        let Some(anchor) = geographic_anchor(&text) else {
            eprintln!(
                "Warning: {} has no geographic reference point; skipped",
                path.display()
            );
            continue;
        };
        let (map, _) = read_xml_map_str(&text)
            .map_err(|e| (ExitCode::from(2), format!("Error: {}: {e}", path.display())))?;
        let standard = match args.standard {
            StandardArg::Isom => Standard::Isom,
            StandardArg::Issprom => Standard::Issprom,
            StandardArg::Auto => SymbolTable::detect(&map),
        };
        let table = SymbolTable::new(standard);
        let index = match groups.iter().position(|g| g.0 == standard) {
            Some(i) => i,
            None => {
                groups.push((standard, BTreeMap::new(), 0));
                groups.len() - 1
            }
        };
        let converted = match convert_levels(
            &map,
            &anchor,
            &table,
            groups[index].2,
            args.min_zoom,
            &options,
        ) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("Warning: {}: {e}; skipped", path.display());
                continue;
            }
        };
        for warning in &converted.warnings {
            eprintln!("Warning: {}: {warning}", path.display());
        }
        eprintln!(
            "{} ({}): {} objects stored ({} dropped, {} under another symbol, {} hidden or helper left out)",
            path.display(),
            table.standard,
            converted.objects,
            converted.dropped,
            converted.remapped,
            converted.hidden
        );
        for level in &converted.levels {
            eprintln!(
                "  zoom {}: {} objects ({} left out by the symbols' rules), {} tiles",
                level.zoom, level.objects, level.generalized, level.tiles
            );
        }
        groups[index].2 = converted.next_id;
        merge_tiles(&mut groups[index].1, converted.tiles);
        converted_maps += 1;
    }
    if converted_maps == 0 {
        return Err((
            ExitCode::from(2),
            "Error: none of the maps could be converted".into(),
        ));
    }
    let several = groups.len() > 1;
    for (standard, tiles, _) in &groups {
        let info = ArchiveInfo {
            layer: standard.name().to_string(),
            grid_cm: args.grid_cm,
            min_zoom: args.min_zoom,
            max_zoom: args.zoom,
        };
        let path = if several {
            suffixed(&args.output, standard)
        } else {
            args.output.clone()
        };
        write(&path, tiles, &info).map_err(|e| (ExitCode::from(4), format!("Error: {e}")))?;
        eprintln!("{} tiles written to {}", tiles.len(), path.display());
    }
    Ok(())
}

/// `world.pmtiles` as `world.isom.pmtiles`.
fn suffixed(path: &Path, standard: &Standard) -> PathBuf {
    let tag = match standard {
        Standard::Isom => "isom",
        Standard::Issprom => "issprom",
    };
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("out");
    let name = match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => format!("{stem}.{tag}.{ext}"),
        None => format!("{stem}.{tag}"),
    };
    path.with_file_name(name)
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
