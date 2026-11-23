//! Generates a folder of random orienteering maps out of one map's symbol
//! set, each of them drawn to an image and labelled pixel by pixel.
//!
//! ```text
//! mauro net dataset [OPTIONS] <symbol-set> [folder]
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
//! cells, and point symbols scattered into them — and, with
//! `--interior-lines`, lines through the inside of the cells too, which
//! cross the ground rather than follow where it changes. No line is drawn
//! along a side of a cell, nor through one, whose ground draws a border
//! round itself (a building, a lake): it would lie on that border and be
//! labelled as both. Nothing is put where the
//! drawing order would bury it: an overlay is picked out of the symbols
//! which show up against the ground under it. Lettering is the one kind
//! nothing draws with yet.
//!
//! `--content` says how much of that is drawn: `all` of it (the default,
//! which nothing labels); `areas`, which stops after the ground — the cells
//! are filled and nothing is drawn over them, which leaves a map of area
//! fills alone, the one map whose every pixel has an answer, so the one
//! which is labelled; `areas-and-lines`, the ground and the lines along
//! the cell sides, labelled as two layers; or `areas-lines-points`, those and
//! the point symbols, labelled as a third layer — a heatmap per point
//! symbol, peaking where each point is (see [`maur_o::point_truth`]). A point
//! symbol is never put on a line or on another point, which would leave a
//! pixel with two answers, and half of them are put just beside a line,
//! where a surveyor puts a great many. `--just-opaque-areas` (`-j`) and
//! `--line-labels` (`-L`) are the older names of `areas` and
//! `areas-and-lines`.
//!
//! A dataset is three folders and a file naming what the labels mean:
//!
//! ```text
//! mauro net dataset --content areas maps/ISOM_10k.omap dataset
//!
//! dataset/classes.json       what channel each opaque area owns
//! dataset/ISOM_10k.omap      the symbol set the maps were drawn with
//! dataset/maps/map_001.omap  the map, to open in Mapper
//! dataset/images/map_001.png what it looks like: a model's input
//! dataset/gt/map_001.bin     what it is, pixel by pixel: the answer
//! ```
//!
//! `--min-resolution` and `--max-resolution` draw each map at a density of
//! its own, log-uniform between the two, so that a network trained on them
//! learns to tell how dense a picture is. The images stay the size
//! `--resolution` makes them, and cover as much ground as that many pixels
//! do at the map's density; the cells and the frame stretch with it, so the
//! symbols are the only thing which shows the density. Each map's is written
//! to `dataset/resolutions.csv`, and the range to `classes.json`.
//!
//! A symbol set with a table of labels beside it — `almost_ISSprOM.omap`
//! and `almost_ISSprOM.labels.json` — is labelled by the table rather than
//! a class per symbol: the maps are drawn with the set's own symbols, and
//! what the labels say is the fewer things those draw, a building as its
//! fill and its outline, a road as a strip of paved area and its borders,
//! two symbols which look alike as one (see [`maur_o::label_set`]). The
//! table is copied into the dataset beside the set, and `classes.json`
//! names it. A symbol the table keeps for parts of others is not drawn on
//! its own, and a line is only drawn along a side where it shows up over
//! the ground on both sides of it.
//!
//! The whole dataset follows from the seed: the same options give the same
//! maps, down to the coordinate, and the n-th map is the same map whatever
//! number of maps was asked for.
//!
//! Exit codes: 0 success, 1 usage error, 2 the dataset could not be
//! generated.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, ValueEnum};

use maur_o::dataset::{
    create_dataset, Generated, Settings, CLASSES_FILE, GROUND_TRUTH_FOLDER, IMAGES_FOLDER,
    MAPS_FOLDER, RESOLUTIONS_FILE,
};
use maur_o::progress::Progress;

/// How much of a map is drawn, and so what its labels say.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Content {
    /// Everything: the ground, the lines, the see-through areas and the
    /// point symbols. Not labelled: a pixel drawn over has no one answer.
    All,
    /// The ground alone, labelled pixel by pixel.
    Areas,
    /// The ground and the lines along the cell sides, labelled as two
    /// layers; no see-through areas or point symbols, which nothing would
    /// label.
    AreasAndLines,
    /// The ground, the lines and the point symbols, labelled as three
    /// layers; no see-through areas, which nothing would label.
    AreasLinesPoints,
}

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
             The folder comes out as maps/, images/ and, with --content areas, gt/: the \
             map, the picture of it and what each pixel of that picture is, which is a \
             training set. classes.json says what the labels mean.\n\n\
             The same options give the same maps: everything random comes from the seed."
)]
pub struct Args {
    /// The map whose symbol set the generated maps are drawn with (.omap,
    /// .xmap). Nothing drawn on it is used, only its symbols and colours.
    symbol_set: PathBuf,

    /// The folder the maps are written into. Made if it is not there.
    folder: PathBuf,

    /// How many cells a map is across: a map holds this squared.
    #[arg(short = 'l', long, value_name = "CELLS")]
    layout_size: usize,

    /// How wide one cell is, in meters on the ground.
    #[arg(short = 'c', long, value_name = "METERS")]
    background_cell_size: u32,

    /// How many maps to generate.
    #[arg(short = 'n', long, value_name = "COUNT")]
    maps: usize,

    /// The share of cell sides left without a line along them, from 0 for a
    /// line on every side to 1 for none at all.
    #[arg(short = 'e', long, value_name = "SHARE")]
    empty_sides: f64,

    /// The chance of a line through the inside of the cells starting in a
    /// cell: from near its middle, through near the middle of a side into
    /// the next cell and on, until it stops, runs off the map or comes to a
    /// cell it may not go into. Unlike the lines along the sides, it does
    /// not follow where the ground changes. It keeps to cells it shows up
    /// over, whose ground draws no border, and which no other such line has
    /// been through.
    #[arg(long, value_name = "CHANCE")]
    interior_lines: f64,

    /// The chance of a cell being covered by a transparent area.
    #[arg(short = 't', long, value_name = "CHANCE")]
    transparent_areas: f64,

    /// The chance of a cell holding a point symbol. Two are half as likely
    /// as one, three half as likely again, and so on.
    #[arg(short = 'p', long, value_name = "CHANCE")]
    point_symbols: f64,

    /// What the maps hold, and so what their labels say. all: everything,
    /// and no labels, since a pixel drawn over has no one answer. areas:
    /// nothing but the ground -- the cells filled with opaque areas, and the
    /// lines, the see-through areas and the point symbols all skipped,
    /// whatever --empty-sides, --transparent-areas and --point-symbols say;
    /// labelled pixel by pixel in gt/. areas-and-lines: the ground and the
    /// lines along the cell sides, the lines labelled as a second layer over
    /// the ground, which keeps its label under them -- yes-or-no per line
    /// symbol, rebuilt from maps/ where wanted; no see-through areas or point
    /// symbols, whatever --transparent-areas and --point-symbols say, since
    /// they would go unlabelled. areas-lines-points: those and the point
    /// symbols, labelled as a third layer, a heatmap per point symbol
    /// peaking where each point is, rebuilt from maps/ like the lines; no
    /// see-through areas, whatever --transparent-areas says.
    #[arg(long, value_enum, value_name = "WHAT")]
    content: Content,

    /// The same as --content areas.
    #[arg(short = 'j', long, conflicts_with_all = ["content", "line_labels"])]
    just_opaque_areas: bool,

    /// The same as --content areas-and-lines.
    #[arg(short = 'L', long, conflicts_with = "content")]
    line_labels: bool,

    /// How many pixels of image one meter of ground comes to, which decides
    /// how big the images and their labels are.
    #[arg(short = 'r', long, value_name = "PX_PER_M")]
    resolution: f64,

    /// The lowest density a map is drawn at. With --max-resolution, each map
    /// is drawn at a density of its own, log-uniform between the two, over
    /// as much ground as the image's pixels cover at it: the images stay the
    /// size --resolution makes them and the layout stretches with the
    /// ground, so only the symbols show how dense a picture is. Each map's
    /// is written to resolutions.csv. Defaults to --resolution.
    #[arg(long, value_name = "PX_PER_M")]
    min_resolution: Option<f64>,

    /// The highest density a map is drawn at: see --min-resolution. Defaults
    /// to --resolution.
    #[arg(long, value_name = "PX_PER_M")]
    max_resolution: Option<f64>,

    /// How much white ground to leave around each map, in meters. It is part
    /// of the image, and the background class of the labels.
    #[arg(short = 'f', long, value_name = "METERS")]
    frame: f64,

    /// Write the maps and nothing else. Drawing them is most of the work, so
    /// this is what somebody after the map files alone wants; the labels go
    /// with the images, having nothing left to be labels of.
    #[arg(long)]
    no_images: bool,

    /// Write the maps and their images, but not the gt/ labels, even where
    /// --content areas would otherwise have them written. classes.json
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
    #[arg(short = 's', long)]
    seed: u64,
}

fn run(args: Args) -> Result<(), (ExitCode, String)> {
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
        (args.interior_lines, "--interior-lines"),
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
    let (low, high) = (
        args.min_resolution.unwrap_or(args.resolution),
        args.max_resolution.unwrap_or(args.resolution),
    );
    for (value, name) in [(low, "--min-resolution"), (high, "--max-resolution")] {
        if !value.is_finite() || value <= 0.0 {
            return Err((
                ExitCode::from(1),
                format!("Error: Invalid value for {name}: it must be greater than zero."),
            ));
        }
    }
    if !(low <= args.resolution && args.resolution <= high) {
        return Err((
            ExitCode::from(1),
            format!(
                "Error: Invalid resolutions: {} px/m is what the images are sized at, and has to \
                 lie between --min-resolution ({low}) and --max-resolution ({high}).",
                args.resolution
            ),
        ));
    }

    let content = if args.just_opaque_areas {
        Content::Areas
    } else if args.line_labels {
        Content::AreasAndLines
    } else {
        args.content
    };
    // Nothing would label a see-through area, nor a point but where the
    // points are labelled.
    let overlays = content == Content::All;
    let points = overlays || content == Content::AreasLinesPoints;
    let settings = Settings {
        layout_size: args.layout_size,
        cell_size: args.background_cell_size,
        maps: args.maps,
        iof_rules: args.iof_rules,
        seed: args.seed,
        empty_sides: args.empty_sides,
        interior_lines: args.interior_lines,
        transparent_areas: if overlays {
            args.transparent_areas
        } else {
            0.0
        },
        point_symbols: if points { args.point_symbols } else { 0.0 },
        just_opaque_areas: content == Content::Areas,
        line_labels: matches!(content, Content::AreasAndLines | Content::AreasLinesPoints),
        point_labels: content == Content::AreasLinesPoints,
        images: !args.no_images,
        ground_truth: !args.no_gt,
        resolution: args.resolution,
        resolution_range: (low < high).then_some((low, high)),
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

    // What the labels are in, where a table says rather than the symbols.
    if let Some(table) = &summary.labels.table {
        println!(
            "  labelled by {}: {} classes of ground, {} of lines, {} of points",
            table.display(),
            summary.labels.areas.len(),
            summary.labels.lines.len(),
            summary.labels.points.len(),
        );
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
        "  {} fills, {} lines along the sides and {} through the cells, {} transparent areas, \
         {} point symbols drawn{}",
        summary.drawn.fills,
        summary.drawn.lines,
        summary.drawn.interior_lines,
        summary.drawn.transparent_areas,
        summary.drawn.points,
        if settings.just_opaque_areas {
            " (just the opaque areas: nothing was drawn over the ground)"
        } else {
            ""
        },
    );
    if summary.drawn.crowded > 0 {
        println!(
            "  {} more point symbols found no room in their cell, clear of the lines and of the \
             other points, and were left out",
            summary.drawn.crowded,
        );
    }

    // The three folders, and what a training set made of them holds.
    let count = |files: fn(&Generated) -> bool| summary.written.iter().filter(|g| files(g)).count();
    println!(
        "  {MAPS_FOLDER}/ {} maps written",
        count(|g| g.map.is_file())
    );
    // The set travels with the answers: a class of a label is a place in its
    // list of opaque areas, and nothing else says which place.
    println!(
        "  {} the symbol set the maps were drawn with, copied in",
        summary
            .symbol_set
            .file_name()
            .unwrap_or(summary.symbol_set.as_os_str())
            .to_string_lossy(),
    );
    if let Some(name) = summary.labels.table.as_ref().and_then(|t| t.file_name()) {
        println!(
            "  {} the table the labels' classes come out of, copied in",
            name.to_string_lossy()
        );
    }
    let images = count(|g| g.image.is_some());
    if images == 0 {
        println!("  {IMAGES_FOLDER}/ nothing: --no-images was asked for");
    } else {
        println!(
            "  {IMAGES_FOLDER}/ {images} images of {} by {} pixels, at {} px/m with a {} m frame",
            summary.image_size, summary.image_size, settings.resolution, settings.frame,
        );
        if settings.varies() {
            println!(
                "  each map drawn at its own density, {low} to {high} px/m (the ground, the cells \
                 and the frame stretched to match); {RESOLUTIONS_FILE} says which",
            );
        }
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
                 of --content areas maps are labelled, and of --content areas-and-lines and \
                 areas-lines-points maps, whose lines and points are layers of their own"
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
            summary.labels.areas.len() + 2,
            summary.labels.areas.len(),
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

/// What an exit code of this subcommand means, as the module's own
/// documentation lists them.
pub fn exit_meaning(code: i32) -> Option<&'static str> {
    Some(match code {
        0 => "success",
        1 => "usage error",
        2 => "the dataset could not be generated",
        _ => return None,
    })
}

/// Runs `mauro net dataset`.
pub fn main(args: Args) -> ExitCode {
    match run(args) {
        Ok(()) => ExitCode::from(0),
        Err((code, message)) => {
            if !message.is_empty() {
                eprintln!("{message}");
            }
            code
        }
    }
}
