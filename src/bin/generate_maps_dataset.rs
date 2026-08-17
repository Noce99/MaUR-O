//! Generates a folder of random orienteering maps out of one map's symbol
//! set.
//!
//! ```text
//! generate_maps_dataset [OPTIONS] <symbol-set> [folder]
//! ```
//!
//! The symbol set is an ordinary map file, and only its symbols and colours
//! are used: nothing that was drawn on it ends up in the generated maps. Each
//! map is a square of ground, `layout size` by `layout size` cells of
//! `background cell size` meters, whose cells are pieces of terrain with
//! wandering boundaries rather than squares.
//!
//! Every cell is filled with one piece of ground cover: an opaque area symbol
//! of the set, drawn uniformly at random. What a real map carries *over* its
//! ground — lines, point symbols, lettering — is not written yet.
//!
//! ```text
//! generate_maps_dataset maps/ISOM_10k.omap dataset
//! map_to_image dataset/map_001.omap
//! ```
//!
//! The whole dataset follows from the seed: the same options give the same
//! maps, down to the coordinate, and the n-th map is the same map whatever
//! number of maps was asked for.
//!
//! Exit codes: 0 success, 1 usage error, 2 the dataset could not be
//! generated.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;

use maur_o::dataset::{
    create_dataset, Settings, DEFAULT_CELL_SIZE, DEFAULT_LAYOUT_SIZE, DEFAULT_MAPS,
};
use maur_o::progress::Progress;

/// Where the maps go when no folder is named.
const DEFAULT_FOLDER: &str = "dataset";

#[derive(Parser)]
#[command(
    name = "generate_maps_dataset",
    version,
    about = "Generates a folder of random orienteering maps, drawn with the symbols of an \
             existing map.\n\n\
             Each map covers a square of ground divided into cells whose boundaries wander \
             rather than run along the grid, every cell filled with one of the set's opaque \
             area symbols. What a map carries over its ground -- lines, point symbols, \
             lettering -- is not written yet.\n\n\
             The same options give the same maps: everything random comes from the seed."
)]
struct Args {
    /// The map whose symbol set the generated maps are drawn with (.omap,
    /// .xmap). Nothing drawn on it is used, only its symbols and colours.
    symbol_set: PathBuf,

    /// The folder the maps are written into. Made if it is not there.
    #[arg(default_value = DEFAULT_FOLDER)]
    folder: PathBuf,

    /// How many cells a map is across: a map holds this squared.
    #[arg(short = 'l', long, default_value_t = DEFAULT_LAYOUT_SIZE, value_name = "CELLS")]
    layout_size: usize,

    /// How wide one cell is, in meters on the ground.
    #[arg(short = 'c', long, default_value_t = DEFAULT_CELL_SIZE, value_name = "METERS")]
    background_cell_size: u32,

    /// How many maps to generate.
    #[arg(short = 'n', long, default_value_t = DEFAULT_MAPS, value_name = "COUNT")]
    maps: usize,

    /// Keep to the IOF rules for what may be drawn where. Read by the step
    /// which draws over the cells, which is not written yet.
    #[arg(long)]
    iof_rules: bool,

    /// What the randomness is seeded with. The same seed gives the same
    /// dataset.
    #[arg(short = 's', long, default_value_t = 0)]
    seed: u64,
}

fn run() -> Result<(), (ExitCode, String)> {
    let args = match Args::try_parse() {
        Ok(a) => a,
        Err(e) => {
            // clap prints its own usage/help text; forward its exit behavior.
            e.print().ok();
            return Err((
                ExitCode::from(if e.exit_code() == 0 { 0 } else { 1 }),
                String::new(),
            ));
        }
    };

    for (value, name) in [
        (args.layout_size, "--layout-size"),
        (args.background_cell_size as usize, "--background-cell-size"),
        (args.maps, "--maps"),
    ] {
        if value == 0 {
            return Err((
                ExitCode::from(1),
                format!("Error: Invalid value for {name}: it must be greater than zero."),
            ));
        }
    }

    let settings = Settings {
        layout_size: args.layout_size,
        cell_size: args.background_cell_size,
        maps: args.maps,
        iof_rules: args.iof_rules,
        seed: args.seed,
    };

    let mut progress = Progress::new("Maps", settings.maps);
    let summary = create_dataset(&args.symbol_set, &args.folder, &settings, |_, _| {
        progress.tick()
    })
    .map_err(|message| (ExitCode::from(2), format!("Error: {message}")))?;
    progress.finish();

    for warning in &summary.warnings {
        eprintln!("Warning: {warning}");
    }

    println!(
        "{}: {} symbols, map scale 1:{}",
        args.symbol_set.display(),
        summary.catalogue.len(),
        summary.scale_denominator,
    );
    for (kind, entries) in summary.catalogue.by_kind() {
        println!("  {:<17} {:>4}", kind.name(), entries.len());
    }

    let side = settings.layout_size as u32 * settings.cell_size;
    let fills = summary.catalogue.opaque_areas.len();
    println!(
        "{}: {} maps of {} by {} cells, {} by {} meters, filled from {} opaque area{}",
        args.folder.display(),
        summary.written.len(),
        settings.layout_size,
        settings.layout_size,
        side,
        side,
        fills,
        if fills == 1 { "" } else { "s" },
    );
    println!(
        "  {} of them fill with a pattern which turns, and were turned at random",
        summary.turning_fills,
    );
    println!(
        "  seed {}, IOF rules {} (not read yet: the step which draws over the cells is not written)",
        settings.seed,
        if settings.iof_rules { "on" } else { "off" },
    );
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::from(0),
        Err((code, message)) => {
            if !message.is_empty() {
                eprintln!("{message}");
            }
            code
        }
    }
}
