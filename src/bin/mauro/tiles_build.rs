//! Converts maps into a MOT archive: the tiled world-map format of
//! `mds/MOT-Format.md`, in a PMTiles file.
//!
//! ```text
//! mauro tiles build [--standard auto|isom|issprom] [--zoom 14] [--min-zoom 0] [--grid-cm 5]
//!               [--buffer 0.03125] --output world.pmtiles <map-file>...
//! ```
//!
//! Each map must be georeferenced with a geographic reference point (Mapper's
//! `<ref_point_deg>`, which every georeferenced map it saves has) so it can be
//! placed on the earth; one that is not is skipped with a warning. All the
//! maps go into one archive, their objects merged tile by tile.
//!
//! The symbols are those of a table: ISOM 2017-2 or ISSprOM 2019-2, found for each
//! map from what it says it is drawn to and from the symbols its objects use
//! (`--standard auto`), or the one asked for. One archive holds one symbol set:
//! maps of both give two archives, written as `FILE` with `.isom` and `.issprom`
//! before its extension. A symbol of the map that is not in the table is stored
//! as the closest one (same code, the symbol it is a variant of,
//! or the same name) or, where there is none, its objects are dropped; either
//! is reported as a warning, with how many objects it concerns. Maps whose codes
//! are their own (a club's set, an older edition) are matched first by a
//! cross-reference table, `--symbol-map` (see `SymbolMap`). Objects of
//! hidden symbols and drawing aids are left out: they draw nothing.
//!
//! | option | default | |
//! |---|---|---|
//! | `--zoom` | 14 | the finest zoom level, 1 to 20 (14 is about 2 km wide): every object is kept as drawn |
//! | `--min-zoom` | 0 | the coarsest zoom level |
//! | `--grid-cm` | 5 | resolution of the nodes, in centimetres of Mercator |
//! | `--buffer` | 0.03125 | how far past its edge a tile keeps what it holds, as a fraction of its side |
//! | `--symbol-map` | none | a cross-reference table from the maps' own codes to the standard's |
//!
//! The levels from `--zoom` up to `--min-zoom` are all written. The finest has
//! every object as drawn. A coarser level has only what the rules of each symbol
//! (its `min_zoom`, a size under which an object is too small to see, how far
//! its line may be moved to have fewer nodes) keep at that zoom, on a coarser
//! grid, so a zoomed-out view reads a little of the archive instead of all of
//! it. The objects have the same ids at every level.
//!
//! Every level also has the area each map covers (a rectangle in the map's own
//! frame round the objects it draws), as an area of its own in the tiles, so the
//! levels too coarse for any symbol (below zoom 11 for ISOM) still say where
//! there is a map. The archive remembers which file each came from.
//!
//! A tile holds the part of each object inside its box grown by the buffer, so
//! a line or a symbol at the edge is not cut short; an object that crosses the
//! box has its curves flattened and is cut: a line into pieces that say where
//! they were cut, an area into a ring closed along the edge. A line whose symbol
//! spaces dashes or symbols along its whole length is not cut but kept whole in
//! every tile it crosses, as Mapper's spacing depends on that length.
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
use maur_o::mot_symbols::{SymbolMap, SymbolTable};
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
    /// Every map in ISSprOM 2019-2.
    Issprom,
}

#[derive(Parser)]
#[command(about = "Converts Mapper maps into a MOT archive (tiled world map, in PMTiles).")]
pub struct Args {
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
    /// symbols' own rules, and those too coarse for any symbol to appear at have
    /// only the area each map covers.
    #[arg(long, default_value_t = 0, value_parser = clap::value_parser!(u8).range(0..=20), value_name = "ZOOM")]
    min_zoom: u8,

    /// How far past its edge a tile keeps what it holds, as a fraction of the
    /// tile's side (0 to 0.5).
    #[arg(long, default_value_t = 0.03125, value_parser = parse_buffer, value_name = "FRACTION")]
    buffer: f64,

    /// Resolution of the nodes, in centimetres of Mercator.
    #[arg(long, default_value_t = 5, value_parser = clap::value_parser!(u32).range(1..=100), value_name = "CM")]
    grid_cm: u32,

    /// A cross-reference table saying which symbol of the standard each symbol
    /// of the maps is, for maps whose codes are their own: a `[standard]`
    /// section, then lines of a code (and a name in quotes, for the symbols of
    /// that code it starts), the codes it may become (the one of the same kind
    /// and closest in line width is taken), or `-` to leave it out. It comes
    /// before matching by code, for every map not drawn to the standard itself.
    #[arg(long, value_name = "FILE")]
    symbol_map: Option<PathBuf>,
}

fn parse_buffer(text: &str) -> Result<f64, String> {
    match text.parse::<f64>() {
        Ok(v) if (0.0..=0.5).contains(&v) => Ok(v),
        _ => Err(format!("not a fraction between 0 and 0.5: {text:?}")),
    }
}

fn run(args: Args) -> Result<(), (ExitCode, String)> {
    if args.min_zoom > args.zoom {
        return Err((
            ExitCode::from(1),
            "Error: --min-zoom is finer than --zoom".into(),
        ));
    }
    let symbol_map = match &args.symbol_map {
        None => SymbolMap::default(),
        Some(path) => {
            let text = std::fs::read_to_string(path).map_err(|e| {
                (
                    ExitCode::from(1),
                    format!("Error: cannot read {}: {e}", path.display()),
                )
            })?;
            SymbolMap::parse(&text)
                .map_err(|e| (ExitCode::from(1), format!("Error: {}: {e}", path.display())))?
        }
    };
    // Every line of the table must name symbols its standard has, whether or
    // not a map is stored in it.
    for standard in [Standard::Isom, Standard::Issprom] {
        SymbolTable::new(standard)
            .with_symbol_map(&symbol_map)
            .map_err(|e| (ExitCode::from(1), format!("Error: symbol map: {e}")))?;
    }
    let options = Options {
        zoom: args.zoom,
        detail_zoom: args.zoom,
        grid_cm: args.grid_cm,
        buffer: args.buffer,
    };
    // One archive per symbol set: what is to go in it so far.
    struct Group {
        standard: Standard,
        tiles: BTreeMap<TileId, Tile>,
        next_id: u64,
        sources: Vec<(u64, String)>,
    }
    let mut groups: Vec<Group> = Vec::new();
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
        let table = SymbolTable::new(standard)
            .with_symbol_map(&symbol_map)
            .expect("checked above");
        let index = match groups.iter().position(|g| g.standard == standard) {
            Some(i) => i,
            None => {
                groups.push(Group {
                    standard,
                    tiles: BTreeMap::new(),
                    next_id: 0,
                    sources: Vec::new(),
                });
                groups.len() - 1
            }
        };
        let converted = match convert_levels(
            &map,
            &anchor,
            &table,
            groups[index].next_id,
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
        groups[index].next_id = converted.next_id;
        if let Some(id) = converted.coverage_id {
            let name = path.file_name().map_or_else(
                || path.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            );
            groups[index].sources.push((id, name));
        }
        merge_tiles(&mut groups[index].tiles, converted.tiles);
        converted_maps += 1;
    }
    if converted_maps == 0 {
        return Err((
            ExitCode::from(2),
            "Error: none of the maps could be converted".into(),
        ));
    }
    let several = groups.len() > 1;
    for group in &groups {
        let info = ArchiveInfo {
            layer: group.standard.name().to_string(),
            grid_cm: args.grid_cm,
            min_zoom: args.min_zoom,
            max_zoom: args.zoom,
            sources: group.sources.clone(),
        };
        let path = if several {
            suffixed(&args.output, &group.standard)
        } else {
            args.output.clone()
        };
        write(&path, &group.tiles, &info)
            .map_err(|e| (ExitCode::from(4), format!("Error: {e}")))?;
        eprintln!("{} tiles written to {}", group.tiles.len(), path.display());
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

/// What an exit code of this subcommand means, as the module's own
/// documentation lists them.
pub fn exit_meaning(code: i32) -> Option<&'static str> {
    Some(match code {
        0 => "success",
        1 => "usage error",
        2 => "a map could not be read, or none could be converted",
        4 => "the archive could not be written",
        _ => return None,
    })
}

/// Runs `mauro tiles build`.
pub fn main(args: Args) -> ExitCode {
    match run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err((code, message)) => {
            if !message.is_empty() {
                eprintln!("{message}");
            }
            code
        }
    }
}
