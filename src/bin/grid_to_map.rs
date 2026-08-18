//! Turns a grid of symbols back into an OpenOrienteering Mapper map.
//!
//! ```text
//! grid_to_map [OPTIONS] <symbol-set> <labels> [map-file]
//! ```
//!
//! The labels are what `generate_maps_dataset --just-opaque-areas` wrote into
//! `gt/`: one opaque area symbol for each pixel of the image beside it, in
//! the `MAUROGT2` format [`maur_o::ground_truth`] describes. This is the way
//! back from that grid to a map — the regions its cells form, written as
//! path objects drawn with the symbol set of an existing map, with the pixel
//! staircase along their boundaries rounded off. See [`maur_o::vectorize`]
//! for what that rounding is and why.
//!
//! The symbol set is an ordinary map file, and only its symbols and colours
//! are used. It has to be the set the labels were written for: a class is a
//! place in the list of that set's opaque areas and nothing in a labels file
//! says which set it was.
//!
//! ```text
//! generate_maps_dataset --just-opaque-areas maps/ISOM_10k.omap dataset
//! grid_to_map maps/ISOM_10k.omap dataset/gt/map_001.bin back.omap --image back.png
//! ```
//!
//! `back.png` is then drawn over the very ground `dataset/images/map_001.png`
//! was, so the two are the same size and can be compared pixel for pixel.
//!
//! The grid is placed on the ground by the same four numbers a dataset was
//! generated with — `--layout-size`, `--background-cell-size`, `--frame` and
//! `--resolution`, spelt as `generate_maps_dataset` spells them — because
//! that is what says how much ground a cell of it covers. A labels file
//! records how many pixels it holds and nothing about how big they were.
//!
//! `--tolerance` is what to reach for when the map comes out with more nodes
//! than a map should have. At the three pixels per meter a dataset is drawn
//! at, a boundary which wandered smoothly across the ground comes back as a
//! staircase which turns every pixel or two, and none of that is worth
//! keeping; a tolerance of a few cells throws it away, in exchange for
//! moving the boundary by that many cells at worst.
//!
//! Exit codes: 0 success, 1 usage error, 2 the map could not be written.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;

use maur_o::dataset::{Settings, DEFAULT_CELL_SIZE, DEFAULT_LAYOUT_SIZE};
use maur_o::ground_truth::GroundTruth;
use maur_o::render::{render_map_over, save_pixmap, Extent, DEFAULT_FRAME, DEFAULT_RESOLUTION};
use maur_o::vectorize::{write_map, Placement, Simplify, SymbolGrid, SAME_ANGLE};
use maur_o::xml_reader::read_xml_map;

#[derive(Parser)]
#[command(
    name = "grid_to_map",
    version,
    about = "Turns a grid of symbols -- the labels of a rendered map, or what a model said \
             about one -- back into a map file.\n\n\
             The cells are gathered into regions and each region is written as a path object \
             drawn with a symbol of the given set. A boundary between two regions runs along \
             the sides of cells, which would make it a staircase; a bezier node half a cell \
             in from each corner, and a control point on the corner itself, is what rounds it \
             off. --tolerance simplifies the staircase first, which is what a grid the size of \
             a rendered map needs.\n\n\
             How much ground a cell covers is not in a labels file: it comes from the four \
             options a dataset was generated with, which are spelt here as they are there."
)]
struct Args {
    /// The map whose symbol set the regions are drawn with (.omap, .xmap).
    /// It has to be the set the labels were written for.
    symbol_set: PathBuf,

    /// The labels to be turned back into a map: a gt/*.bin of a dataset.
    labels: PathBuf,

    /// The map to be written. Defaults to the labels' name with a .omap
    /// suffix, beside them.
    map_file: Option<PathBuf>,

    /// How far the boundary may be moved to be rid of a node, in cells.
    /// Nought keeps every node the staircase asks for.
    #[arg(short = 't', long, default_value_t = 0.0, value_name = "CELLS")]
    tolerance: f64,

    /// How far apart two neighbouring cells' angles may be and still be one
    /// object's, as a share of a whole turn. A symbol turned two ways is two
    /// objects; `inf` never splits a region on its angle.
    #[arg(short = 'a', long, default_value_t = SAME_ANGLE, value_name = "TURNS")]
    same_angle: f32,

    /// Draw the map which was written, over the very ground the dataset's own
    /// images cover, so the two can be compared pixel for pixel.
    #[arg(short = 'i', long, value_name = "IMAGE")]
    image: Option<PathBuf>,

    /// How many cells across the layout the labels came from was.
    #[arg(short = 'l', long, default_value_t = DEFAULT_LAYOUT_SIZE, value_name = "CELLS")]
    layout_size: usize,

    /// How wide one cell of that layout was, in meters on the ground.
    #[arg(short = 'c', long, default_value_t = DEFAULT_CELL_SIZE, value_name = "METERS")]
    background_cell_size: u32,

    /// How much white ground there was around the map, in meters.
    #[arg(short = 'f', long, default_value_t = DEFAULT_FRAME, value_name = "METERS")]
    frame: f64,

    /// How many pixels of image one meter of ground came to.
    #[arg(short = 'r', long, default_value_t = DEFAULT_RESOLUTION, value_name = "PX_PER_M")]
    resolution: f64,

    /// The scale to write the map at. Defaults to the symbol set's own.
    #[arg(short = 's', long, value_name = "DENOMINATOR")]
    scale: Option<i32>,
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
    let usage = |message: String| (ExitCode::from(1), format!("Error: {message}"));
    let failed = |message: String| (ExitCode::from(2), format!("Error: {message}"));

    for (value, name) in [
        (args.layout_size, "--layout-size"),
        (args.background_cell_size as usize, "--background-cell-size"),
    ] {
        if value == 0 {
            return Err(usage(format!(
                "Invalid value for {name}: it must be greater than zero."
            )));
        }
    }
    if args.resolution.is_nan() || args.resolution <= 0.0 {
        return Err(usage(
            "Invalid value for --resolution: it must be greater than zero.".to_string(),
        ));
    }
    if args.frame.is_nan() || args.frame < 0.0 {
        return Err(usage(
            "Invalid value for --frame: it cannot be negative.".to_string(),
        ));
    }
    if args.tolerance.is_nan() || args.tolerance < 0.0 {
        return Err(usage(
            "Invalid value for --tolerance: it cannot be negative.".to_string(),
        ));
    }
    if args.same_angle.is_nan() || args.same_angle < 0.0 {
        return Err(usage(
            "Invalid value for --same-angle: it cannot be negative.".to_string(),
        ));
    }

    let map_path = args
        .map_file
        .clone()
        .unwrap_or_else(|| args.labels.with_extension("omap"));

    // The scale the objects are written at, which is the source map's own
    // unless something else was asked for: a coordinate is mm on the paper,
    // and only the scale says how much ground that is.
    let scale = match args.scale {
        Some(scale) if scale > 0 => scale,
        Some(scale) => {
            return Err(usage(format!(
                "Invalid value for --scale: {scale} is not a map scale."
            )))
        }
        None => {
            let (map, _) = read_xml_map(&args.symbol_set).map_err(failed)?;
            map.scale_denominator
        }
    };

    let settings = Settings {
        layout_size: args.layout_size,
        cell_size: args.background_cell_size,
        frame: args.frame,
        resolution: args.resolution,
        ..Settings::default()
    };
    let ground = settings.ground();

    let truth = GroundTruth::read(&args.labels).map_err(failed)?;
    // The labels are one cell of the grid per pixel of the image they were
    // written for, so a grid which is not the size the options say it is was
    // written for a dataset generated with other ones.
    let wanted = settings.image_size();
    if truth.width != wanted || truth.height != wanted {
        return Err(usage(format!(
            "{} is {}x{} cells, and {} m of ground at {} px/m is {wanted}x{wanted}: the layout \
             size, the cell size, the frame and the resolution have to be the ones the dataset \
             was generated with, since a labels file does not record them.",
            args.labels.display(),
            truth.width,
            truth.height,
            ground.width(),
            args.resolution,
        )));
    }

    let grid = SymbolGrid::from(&truth);
    let placement = Placement {
        ground,
        scale_denominator: scale,
    };
    let simplify = Simplify {
        tolerance: args.tolerance,
        same_angle: args.same_angle,
    };
    let written =
        write_map(&grid, &args.symbol_set, &map_path, &placement, &simplify).map_err(failed)?;
    for warning in &written.warnings {
        eprintln!("Warning: {warning}");
    }

    println!(
        "{}: {} objects, {} coordinates, from {}x{} cells of {} m at 1:{}",
        map_path.display(),
        written.objects,
        written.coords,
        truth.width,
        truth.height,
        ground.width() / truth.width as f64,
        scale,
    );
    if args.tolerance > 0.0 {
        println!(
            "  simplified at a tolerance of {} cells, which is {} m on the ground",
            args.tolerance,
            args.tolerance * ground.width() / truth.width as f64,
        );
    }

    // Drawn from the file just written rather than from the objects in hand,
    // over the ground the dataset's own images cover: any disagreement
    // between the two is a bug worth tripping over here.
    if let Some(image_path) = &args.image {
        let drawing = render_map_over(&map_path, args.resolution, Extent::Ground(ground))
            .map_err(|e| failed(format!("cannot draw {}: {e}", map_path.display())))?;
        for warning in &drawing.warnings {
            eprintln!("Warning: {warning}");
        }
        save_pixmap(&drawing.pixmap, image_path)
            .map_err(|e| failed(format!("cannot write {}: {e}", image_path.display())))?;
        println!(
            "{}: {}x{} pixels, over the same ground the dataset's images cover",
            image_path.display(),
            drawing.pixmap.width(),
            drawing.pixmap.height(),
        );
    }
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
