//! Generates a folder of random orienteering maps out of one map's symbol
//! set, each of them drawn to an image and labelled pixel by pixel.
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
//! Every cell is filled with one piece of ground cover — an opaque area
//! symbol of the set, drawn uniformly at random — and then drawn over: a
//! line along some of the cell sides, a see-through area over some of the
//! cells, and point symbols scattered into them. Nothing is put where the
//! drawing order would bury it: an overlay is picked out of the symbols
//! which show up against the ground under it. Lettering is the one kind
//! nothing draws with yet.
//!
//! `--just-opaque-areas` stops after the ground: the cells are filled and
//! nothing is drawn over them, which leaves a map of area fills alone — and
//! is the one map whose every pixel has an answer, so it is the one which is
//! labelled.
//!
//! A dataset is three folders and a file naming what the labels mean:
//!
//! ```text
//! generate_maps_dataset --just-opaque-areas maps/ISOM_10k.omap dataset
//!
//! dataset/classes.json       what channel each opaque area owns
//! dataset/maps/map_001.omap  the map, to open in Mapper
//! dataset/images/map_001.png what it looks like: a model's input
//! dataset/gt/map_001.bin     what it is, pixel by pixel: the answer
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
    create_dataset, Generated, Settings, CLASSES_FILE, DEFAULT_CELL_SIZE, DEFAULT_EMPTY_SIDES,
    DEFAULT_LAYOUT_SIZE, DEFAULT_MAPS, DEFAULT_POINT_SYMBOLS, DEFAULT_TRANSPARENT_AREAS,
    GROUND_TRUTH_FOLDER, IMAGES_FOLDER, MAPS_FOLDER,
};
use maur_o::progress::Progress;
use maur_o::render::{DEFAULT_FRAME, DEFAULT_RESOLUTION};

/// Where the maps go when no folder is named.
const DEFAULT_FOLDER: &str = "dataset";

#[derive(Parser)]
#[command(
    name = "generate_maps_dataset",
    version,
    about = "Generates a folder of random orienteering maps, drawn with the symbols of an \
             existing map.\n\n\
             Each map covers a square of ground divided into cells whose boundaries wander \
             rather than run along the grid. Every cell is filled with one of the set's opaque \
             areas, lines run along some of the boundaries, see-through areas cover some of \
             the cells and point symbols are scattered into them -- each of them picked out \
             of the symbols which show up against the ground they land on.\n\n\
             The folder comes out as maps/, images/ and, with --just-opaque-areas, gt/: the \
             map, the picture of it and what each pixel of that picture is, which is a \
             training set. classes.json says what the labels mean.\n\n\
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

    /// The share of cell sides left without a line along them, from 0 for a
    /// line on every side to 1 for none at all.
    #[arg(short = 'e', long, default_value_t = DEFAULT_EMPTY_SIDES, value_name = "SHARE")]
    empty_sides: f64,

    /// The chance of a cell being covered by a transparent area.
    #[arg(short = 't', long, default_value_t = DEFAULT_TRANSPARENT_AREAS, value_name = "CHANCE")]
    transparent_areas: f64,

    /// The chance of a cell holding a point symbol. Two are half as likely
    /// as one, three half as likely again, and so on.
    #[arg(short = 'p', long, default_value_t = DEFAULT_POINT_SYMBOLS, value_name = "CHANCE")]
    point_symbols: f64,

    /// Draw nothing but the ground: the cells are filled with opaque areas
    /// and the lines, the see-through areas and the point symbols are all
    /// skipped, whatever --empty-sides, --transparent-areas and
    /// --point-symbols say. The labels in gt/ are written only for these.
    #[arg(short = 'j', long)]
    just_opaque_areas: bool,

    /// How many pixels of image one meter of ground comes to, which decides
    /// how big the images and their labels are.
    #[arg(short = 'r', long, default_value_t = DEFAULT_RESOLUTION, value_name = "PX_PER_M")]
    resolution: f64,

    /// How much white ground to leave around each map, in meters. It is part
    /// of the image, and the background class of the labels.
    #[arg(short = 'f', long, default_value_t = DEFAULT_FRAME, value_name = "METERS")]
    frame: f64,

    /// Write the maps and nothing else. Drawing them is most of the work, so
    /// this is what somebody after the map files alone wants; the labels go
    /// with the images, having nothing left to be labels of.
    #[arg(long)]
    no_images: bool,

    /// Write the maps and their images, but not the gt/ labels, even where
    /// --just-opaque-areas would otherwise have them written. classes.json
    /// still goes out, since it is what lets a label be computed from a map
    /// in maps/ instead of read from gt/ -- training reads it either way.
    #[arg(long)]
    no_gt: bool,

    /// Keep to the IOF rules for what may be drawn where. Not read yet: what
    /// goes over a piece of ground is picked for being visible on it, not
    /// for being allowed there.
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

    for (share, name) in [
        (args.empty_sides, "--empty-sides"),
        (args.transparent_areas, "--transparent-areas"),
        (args.point_symbols, "--point-symbols"),
    ] {
        if !(0.0..=1.0).contains(&share) {
            return Err((
                ExitCode::from(1),
                format!("Error: Invalid value for {name}: it must be between zero and one."),
            ));
        }
    }

    if args.resolution.is_nan() || args.resolution <= 0.0 {
        return Err((
            ExitCode::from(1),
            "Error: Invalid value for --resolution: it must be greater than zero.".to_string(),
        ));
    }
    if args.frame.is_nan() || args.frame < 0.0 {
        return Err((
            ExitCode::from(1),
            "Error: Invalid value for --frame: it cannot be negative.".to_string(),
        ));
    }

    let settings = Settings {
        layout_size: args.layout_size,
        cell_size: args.background_cell_size,
        maps: args.maps,
        iof_rules: args.iof_rules,
        seed: args.seed,
        empty_sides: args.empty_sides,
        transparent_areas: args.transparent_areas,
        point_symbols: args.point_symbols,
        just_opaque_areas: args.just_opaque_areas,
        images: !args.no_images,
        ground_truth: !args.no_gt,
        resolution: args.resolution,
        frame: args.frame,
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

    // What may go over what: the pairs which show up, of the pairs there are.
    let (shown, tried) = summary.overlays.transparent_pairs(&summary.catalogue);
    println!("  {shown} of {tried} transparent areas over a fill show up");
    let (shown, tried) = summary.overlays.point_pairs(&summary.catalogue);
    println!("  {shown} of {tried} point symbols over a fill show up");

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
        summary
            .catalogue
            .opaque_areas
            .iter()
            .filter(|entry| entry.turns)
            .count(),
    );
    println!(
        "  {} fills, {} lines, {} transparent areas, {} point symbols drawn{}",
        summary.drawn.fills,
        summary.drawn.lines,
        summary.drawn.transparent_areas,
        summary.drawn.points,
        if settings.just_opaque_areas {
            " (just the opaque areas: nothing was drawn over the ground)"
        } else {
            ""
        },
    );

    // The three folders, and what a training set made of them holds.
    let count = |files: fn(&Generated) -> bool| summary.written.iter().filter(|g| files(g)).count();
    println!(
        "  {MAPS_FOLDER}/ {} maps written",
        count(|g| g.map.is_file())
    );
    let images = count(|g| g.image.is_some());
    if images == 0 {
        println!("  {IMAGES_FOLDER}/ nothing: --no-images was asked for");
    } else {
        println!(
            "  {IMAGES_FOLDER}/ {images} images of {} by {} pixels, at {} px/m with a {} m frame",
            summary.image_size, summary.image_size, settings.resolution, settings.frame,
        );
    }
    let labelled = count(|g| g.ground_truth.is_some());
    if labelled == 0 {
        println!(
            "  {GROUND_TRUTH_FOLDER}/ nothing: {}",
            if args.no_gt && images > 0 && settings.just_opaque_areas {
                format!(
                    "--no-gt was asked for; {CLASSES_FILE} still went out, so a label can be \
                     computed from a map in {MAPS_FOLDER}/ instead"
                )
            } else if args.no_gt {
                "--no-gt was asked for".to_string()
            } else {
                "a pixel's label is the one piece of ground cover under it, so only the images \
                 of --just-opaque-areas maps are labelled"
                    .to_string()
            }
        );
    } else {
        println!(
            "  {GROUND_TRUTH_FOLDER}/ {labelled} labels of {} by {} by {} ({} classes, then the \
             sine and the cosine of the pattern angle), and {CLASSES_FILE} says which channel is \
             which",
            summary.image_size,
            summary.image_size,
            fills + 2,
            fills,
        );
    }

    println!(
        "  seed {}, IOF rules {} (not read yet: an overlay is picked for showing up on its \
         ground, not for being allowed there)",
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
