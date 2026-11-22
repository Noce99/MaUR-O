//! Reads a picture of a map back into a map file, with a trained network.
//!
//! ```text
//! mauro net image-to-map [OPTIONS] <run-folder> <image> [map-file]
//! ```
//!
//! This is the whole pipeline in one call, and the other end of
//! [`map_to_image`](https://docs.rs/maur-o). That tool turns a map into a
//! picture; this turns a picture back into a map: the network of a training
//! run says which of the symbol set's opaque areas each pixel is, and
//! [`maur_o::vectorize`] turns that grid of symbols into the regions and
//! bezier paths of an `.omap`.
//!
//! ```text
//! mauro net image-to-map trainings/UNet_2026_08_18__09_26_59 dataset/images/map_001.png back.omap
//! ```
//!
//! A run of the tree network (`mauro net train --model tree`, recognised by its
//! `tree.json`) is read the same way, with a difference in the middle: the
//! tree answers with the curves which cut the picture and a symbol for each
//! leaf, which is already a vector drawing, so there is no grid and no
//! vectorizer. The areas the cuts make are worked out from the curves
//! themselves (see [`maur_o::tree_vector`]) and written as they are, their
//! boundaries the curves the network drew. `--crop`, `--overlap` and
//! `--same-angle` are the U-Net's and do not apply to it, and it has no angle
//! to give.
//!
//! A run whose network has a lines layer (trained on a dataset made with
//! `mauro net dataset --line-labels`) gives the lines as well: where the
//! network says a line of a symbol runs, the band is thinned to its middle,
//! traced into paths, and each path written as a line object of that symbol
//! — see [`maur_o::line_vector`]. `--line-threshold` is how sure the network
//! has to be of a pixel, and `--min-line` how long a path has to be; the
//! lines are simplified by `--tolerance`, like the areas' boundaries.
//!
//! A run whose network has a points layer (trained on a dataset made with
//! `mauro net dataset --content areas-lines-points`) gives the point symbols
//! too: every peak of a symbol's heatmap at least `--point-threshold` high,
//! and no lower than its neighbours, is one point object of that symbol, at
//! the peak refined to a fraction of a pixel — see
//! [`maur_o::point_vector`].
//!
//! # How dense the picture is
//!
//! The map is only the right size if it is known how many pixels of the
//! picture a meter of ground is, and a picture does not say. It is taken,
//! first to last, from:
//!
//! 1. `--resolution`, in pixels per meter of ground at the scale the map is
//!    written at;
//! 2. `--dpi`, for a scan or an export: so many pixels to an inch of paper,
//!    which `--scale` turns into meters of ground;
//! 3. the network itself, where it has a scale head (a run trained on a
//!    dataset made with `mauro net dataset --min-resolution/--max-resolution`):
//!    it is asked about the tiles of the picture which have anything on them,
//!    and the median of what they say is taken. It is held to the range the
//!    dataset was drawn over, and says so where that clips it;
//! 4. the density the run's dataset was drawn at, from its `classes.json`.
//!
//! Whichever it is, a picture at a density the network was trained over is
//! read as it is, every pixel of it; one outside is resampled to the nearer
//! end of that range before the network reads it (to the one density, for a
//! run trained at one), and the map drawn back at the picture's own density
//! to score it. A density is a
//! number of pixels to a millimeter of paper, which is what a symbol set fixes
//! its symbols' sizes in: a map written at another `--scale` than the symbol
//! set's covers another amount of ground with the same paper, and its
//! `--resolution` is in meters of that ground.
//!
//! The run folder is all it is told, because a run folder holds everything:
//! `training.json` says how big the network was, `best.bpk` is its weights,
//! and `classes.json` and the symbol set beside them — copied out of the
//! dataset when the run started — say which symbol a class is and how much
//! ground a pixel covers. A run from before those were copied in is a run
//! whose answers cannot be read, and this says so rather than guessing.
//!
//! # The number it prints
//!
//! A map read off a picture is scored by drawing it again and comparing it
//! with the picture it was read off. That is a test of the whole chain at
//! once — what the network got wrong, and what the vectorizer rounded off —
//! and it needs no labels, so it works on any picture of a map and not only
//! on one a dataset generated.
//!
//! The comparison is [`maur_o::differences`], the one the benchmark harness
//! scores a renderer with, so it separates a real disagreement from the sort
//! two rasterizers have about an edge they both drew. That distinction earns
//! its keep here: rounding the staircase off a boundary moves it by half a
//! pixel, and half a pixel of boundary is exactly an edge effect.
//!
//! Exit codes: 0 success, 1 usage error, 2 the map could not be made.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;

use maur_o::dataset::{Classes, CLASSES_FILE};
use maur_o::differences::open_image;
use maur_o::symbol_kinds::Catalogue;
use maur_o::xml_reader::read_xml_map;

use maur_o::line_vector::LineSettings;
use maur_o::point_vector::DEFAULT_POINT_THRESHOLD;
use maur_o::vectorize::Simplify;

use maur_o::net::predict::{
    at_scale, estimate_scale, load, read_back, read_back_tree, ReadBackSettings,
    DEFAULT_LINE_THRESHOLD, OVERLAP, PREDICTED_TOLERANCE, RESAMPLE_TOLERANCE, TREE_TOLERANCE,
};
use maur_o::net::tree_training::load_tree;

#[derive(Parser)]
#[command(
    name = "image_to_map",
    version,
    about = "Reads a picture of an orienteering map back into a map file, with the network of a \
             training run.\n\n\
             The network says which of the symbol set's opaque areas each pixel is; the regions \
             those pixels form become the path objects of an .omap, with the pixel staircase \
             along their boundaries rounded off. The run folder holds all of it -- the weights, \
             the symbol set and what a class means -- so nothing else has to be given.\n\n\
             The map it writes is then drawn again and compared with the picture it was read \
             off, which scores the network and the vectorizer together."
)]
pub struct Args {
    /// A run folder under trainings/: the weights, and the notes copied out
    /// of the dataset the run trained on.
    run: PathBuf,

    /// The picture of a map to read.
    image: PathBuf,

    /// The map to be written. Defaults to the image's name with a .omap
    /// suffix, beside it.
    map_file: Option<PathBuf>,

    /// The weights to use. Defaults to best.bpk in the run folder, which is
    /// the epoch which validated best; a checkpoint/007/model.bpk may be
    /// named instead.
    #[arg(short = 'w', long, value_name = "FILE")]
    weights: Option<PathBuf>,

    /// How far the boundary may be moved to be rid of a node, in cells --
    /// one cell being one pixel of the picture. Nought keeps every node the
    /// staircase asks for, which on a picture this size is a great many.
    /// Defaults to 1 for a U-Net's staircase and to 0.05 for a tree's
    /// boundaries, which are curves already and have none.
    #[arg(short = 't', long, value_name = "PIXELS")]
    tolerance: Option<f64>,

    /// How far apart two neighbouring pixels' angles may be and still be one
    /// object's, as a share of a whole turn. `inf` never splits a region on
    /// its angle.
    #[arg(short = 'a', long, value_name = "TURNS")]
    same_angle: f32,

    /// How many pixels square a tile of the picture is. Defaults to the crop
    /// the run trained at, and has to divide by 16.
    #[arg(short = 'c', long, value_name = "PIXELS")]
    crop: Option<usize>,

    /// How much of each tile is shared with the one before it, in pixels.
    /// Defaults to a quarter of the tile.
    #[arg(short = 'o', long, value_name = "PIXELS")]
    overlap: Option<usize>,

    /// The probability a pixel has to be given for a line to run through it,
    /// for a run whose network has a lines layer (a dataset made with
    /// --line-labels). Lower finds more of the lines and more that are not.
    #[arg(long, value_name = "PROBABILITY")]
    line_threshold: f32,

    /// How long a line has to be to be written, in pixels. A shorter one is a
    /// speck, or the spur a thinning leaves at the end of a band.
    #[arg(long, value_name = "PIXELS")]
    min_line: f64,

    /// How high a point symbol's heatmap has to peak for a point to be
    /// written there, for a run whose network has a points layer (a dataset
    /// made with --content areas-lines-points). Lower finds more of the
    /// points and more that are not.
    #[arg(long, value_name = "PROBABILITY")]
    point_threshold: f32,

    /// Draw the map which was written, over the very ground the picture
    /// covers, and keep it. It is drawn either way, to score the result.
    // No short flag: `-i` next to the `<IMAGE>` this reads would be a
    // reasonable thing to expect to mean the input.
    #[arg(long, value_name = "IMAGE")]
    rendered: Option<PathBuf>,

    /// How many pixels of picture one meter of ground is, at the scale the
    /// map is written at. Defaults to --dpi where that is given; else to what
    /// the network says, where it has a scale head (a run trained on maps
    /// drawn over a range of densities); else to what the run's classes.json
    /// records for the dataset it trained on. The picture is resampled to the
    /// density the network trained at before it is read.
    #[arg(short = 'r', long, value_name = "PX_PER_M", conflicts_with = "dpi")]
    resolution: Option<f64>,

    /// How many pixels of picture an inch of the paper map is: the
    /// resolution a scan or an export was made at. Turned into pixels per
    /// meter of ground by the scale.
    #[arg(long, value_name = "DPI")]
    dpi: Option<f64>,

    /// The scale to write the map at. Defaults to the symbol set's own.
    #[arg(short = 's', long, value_name = "DENOMINATOR")]
    scale: Option<i32>,
}

fn run(args: Args) -> Result<(), (ExitCode, String)> {
    let usage = |message: String| (ExitCode::from(1), format!("Error: {message}"));
    let failed = |message: String| (ExitCode::from(2), format!("Error: {message}"));

    let tree = args.run.join("tree.json").is_file();
    let tolerance = args.tolerance.unwrap_or(if tree {
        TREE_TOLERANCE
    } else {
        PREDICTED_TOLERANCE
    });
    if tolerance.is_nan() || tolerance < 0.0 {
        return Err(usage(
            "Invalid value for --tolerance: it cannot be negative.".to_string(),
        ));
    }
    if args.same_angle.is_nan() || args.same_angle < 0.0 {
        return Err(usage(
            "Invalid value for --same-angle: it cannot be negative.".to_string(),
        ));
    }

    // What the run trained on, which is what its answers are in terms of.
    let notes_file = args.run.join(CLASSES_FILE);
    let notes = Classes::read(&notes_file).map_err(|e| {
        failed(format!(
            "{e}\nA run folder carries the {CLASSES_FILE} of the dataset it trained on, so that \
             what it learned can be read back; a run started before that was so does not, and \
             training again is what puts it there."
        ))
    })?;
    let symbol_set = notes.symbol_set.as_ref().ok_or_else(|| {
        failed(format!(
            "{} names no symbol set, so there is no telling which symbol a class is",
            notes_file.display(),
        ))
    })?;
    let symbol_set = args.run.join(symbol_set);

    // Named, because `read_xml_map` reports what went wrong and not what it
    // went wrong on, and a run folder missing half of itself is the likely
    // way to get here.
    let (mut map, warnings) = read_xml_map(&symbol_set).map_err(|e| {
        failed(format!(
            "cannot read the symbol set {}: {e}",
            symbol_set.display()
        ))
    })?;
    map.resolve_references();
    for warning in &warnings {
        eprintln!("Warning: {warning}");
    }
    let catalogue = Catalogue::of(&map);
    if catalogue.opaque_areas.len() != notes.classes {
        return Err(failed(format!(
            "{} holds {} opaque areas and {} was written for {}: the symbol set beside a run is \
             not the one it trained on",
            symbol_set.display(),
            catalogue.opaque_areas.len(),
            notes_file.display(),
            notes.classes,
        )));
    }
    let scale = match args.scale {
        Some(scale) if scale > 0 => scale,
        Some(scale) => {
            return Err(usage(format!(
                "Invalid value for --scale: {scale} is not a map scale."
            )))
        }
        None => map.scale_denominator,
    };
    // The density the network reads best, at the scale the map is written
    // at: the same pixels to a millimeter of paper as its dataset had.
    let normal = at_scale(notes.resolution, map.scale_denominator, scale);
    // What the picture was said to be, where it was.
    let given = match (args.resolution, args.dpi) {
        (Some(resolution), _) if !(resolution.is_finite() && resolution > 0.0) => {
            return Err(usage(format!(
                "Invalid value for --resolution: {resolution} is not pixels per meter."
            )))
        }
        (_, Some(dpi)) if !(dpi.is_finite() && dpi > 0.0) => {
            return Err(usage(format!(
                "Invalid value for --dpi: {dpi} is not pixels per inch."
            )))
        }
        (Some(resolution), _) => Some((resolution, "--resolution".to_string())),
        // Pixels to the mm of paper, then the paper of a meter of ground.
        (None, Some(dpi)) => Some((dpi / 25.4 * 1000.0 / scale as f64, format!("--dpi {dpi}"))),
        (None, None) => None,
    };

    // `open_image` rather than `image::open`: it lifts the decoder's default
    // refusal to read past 512MiB, which a map at a fine resolution clears.
    let picture = open_image(&args.image).map_err(failed)?;
    let (width, height) = (picture.width(), picture.height());

    let map_path = args
        .map_file
        .clone()
        .unwrap_or_else(|| args.image.with_extension("omap"));
    let simplify = Simplify {
        tolerance,
        same_angle: args.same_angle,
    };

    let (done, resolution) = if tree {
        // A tree of curves: its answer is a cut of the whole picture and a
        // symbol for each leaf, so there are no tiles to cut it into, and
        // the areas it makes are worked out from the curves themselves.
        let (model, _) =
            load_tree(&args.run, args.weights.as_deref(), &device()).map_err(failed)?;
        if model.classes() != notes.classes {
            return Err(failed(format!(
                "the network tells {} symbols apart and {} was written for {}",
                model.classes(),
                notes_file.display(),
                notes.classes,
            )));
        }
        println!(
            "{BACKEND}: {width}x{height} pixels through a tree of {} leaves of {} symbols, its \
             cuts drawn at the picture's own size",
            model.leaves(),
            model.classes(),
        );
        let (resolution, from) = given.unwrap_or((normal, CLASSES_FILE.to_string()));
        println!("  the picture is {resolution:.3} px/m, from {from}");
        let settings = ReadBackSettings {
            tile: 0,
            overlap: 0,
            resolution,
            // The tree reduces the whole picture to the size it trained at,
            // whatever its density, so nothing is resampled for it.
            network_resolutions: (resolution, resolution),
            scale_denominator: scale,
            simplify,
            line_threshold: DEFAULT_LINE_THRESHOLD,
            lines: LineSettings::default(),
            point_threshold: DEFAULT_POINT_THRESHOLD,
        };
        let done = read_back_tree(
            &model,
            &picture,
            &symbol_set,
            &catalogue.opaque_areas,
            &map_path,
            &settings,
            &device(),
        )
        .map_err(failed)?;
        (done, resolution)
    } else {
        let (model, config) =
            load(&args.run, args.weights.as_deref(), &device()).map_err(failed)?;
        if model.classes() != notes.classes {
            return Err(failed(format!(
                "the network tells {} symbols apart and {} was written for {}",
                model.classes(),
                notes_file.display(),
                notes.classes,
            )));
        }

        if model.lines() != catalogue.lines.len() && model.lines() > 0 {
            return Err(failed(format!(
                "the network answers for {} line symbols and {} holds {}: the symbol set beside \
                 a run is not the one it trained on",
                model.lines(),
                symbol_set.display(),
                catalogue.lines.len(),
            )));
        }

        if model.points() != catalogue.points.len() && model.points() > 0 {
            return Err(failed(format!(
                "the network answers for {} point symbols and {} holds {}: the symbol set beside \
                 a run is not the one it trained on",
                model.points(),
                symbol_set.display(),
                catalogue.points.len(),
            )));
        }

        let tile = args.crop.unwrap_or(config.crop);
        let overlap = args.overlap.unwrap_or((tile as f64 * OVERLAP) as usize);
        println!(
            "{BACKEND}: {}x{} pixels through a U-Net of {} symbols, in tiles of {tile} \
             overlapping by {overlap}",
            width,
            height,
            model.classes(),
        );

        // The densities the network was trained over, at this scale: the
        // one, for a run trained at one.
        let (low, high) = notes
            .resolution_range
            .map(|(low, high)| {
                (
                    at_scale(low, map.scale_denominator, scale),
                    at_scale(high, map.scale_denominator, scale),
                )
            })
            .unwrap_or((normal, normal));
        let (resolution, from) = match given {
            Some(given) => given,
            None => match estimate_scale(&model, &picture, tile, &device()) {
                Some(estimate) => {
                    let said = normal * estimate.log_scale.exp();
                    // Held to what the network was trained on: past it, the
                    // head is guessing.
                    let held = said.clamp(low, high);
                    if held != said {
                        eprintln!(
                            "Warning: the network says {said:.3} px/m, outside the {low:.3} to \
                             {high:.3} it was trained on; taking {held:.3}. Give --resolution or \
                             --dpi if that is wrong."
                        );
                    }
                    let spread = 100.0 * estimate.spread.exp_m1();
                    (
                        held,
                        format!(
                            "the network's scale head, the median of {} tiles, give or take \
                             {spread:.1}%",
                            estimate.tiles
                        ),
                    )
                }
                None => (normal, CLASSES_FILE.to_string()),
            },
        };
        println!(
            "  the picture is {resolution:.3} px/m ({:.2} px to a mm of paper), from {from}",
            resolution * scale as f64 / 1000.0,
        );
        let settings = ReadBackSettings {
            tile,
            overlap,
            resolution,
            network_resolutions: (low, high),
            scale_denominator: scale,
            simplify,
            line_threshold: args.line_threshold,
            // Simplified as far as the areas' boundaries are: both are a
            // line of pixels, and a pixel is a pixel.
            lines: LineSettings {
                tolerance,
                min_length: args.min_line,
            },
            point_threshold: args.point_threshold,
        };
        let reading = settings.reading_resolution();
        let factor = reading / resolution;
        if (factor - 1.0).abs() > RESAMPLE_TOLERANCE {
            let trained = if low < high {
                format!("{low:.3} to {high:.3} px/m")
            } else {
                format!("{low:.3} px/m")
            };
            println!(
                "  resampled to {}x{} pixels, {reading:.3} px/m, before it is read: the network \
                 was trained at {trained}",
                (width as f64 * factor).round(),
                (height as f64 * factor).round(),
            );
        } else if low < high {
            println!("  read as it is: the network was trained at {low:.3} to {high:.3} px/m",);
        }
        let done = read_back(
            &model,
            &picture,
            &symbol_set,
            &catalogue.opaque_areas,
            &map_path,
            &settings,
            &device(),
        )
        .map_err(failed)?;
        (done, resolution)
    };
    for warning in &done.warnings {
        eprintln!("Warning: {warning}");
    }

    // What the network actually said, before any of it becomes geometry. A
    // map which comes out empty is a network which called the whole picture
    // the frame, and that is worth being told rather than left to work out
    // from an .omap of no objects.
    println!(
        "  it called {:.1}% of the picture the frame and used {} of the {} symbols on the rest",
        100.0 * done.frame_share(),
        done.symbols_used(),
        catalogue.opaque_areas.len(),
    );
    if done.line_symbols > 0 || done.lines > 0 {
        println!(
            "  it found {} lines, of {} of the set's {} line symbols",
            done.lines,
            done.line_symbols,
            catalogue.lines.len(),
        );
    }
    if done.point_symbols > 0 || done.points > 0 {
        println!(
            "  it found {} point symbols, of {} of the set's {} point symbols",
            done.points,
            done.point_symbols,
            catalogue.points.len(),
        );
    }
    println!(
        "{}: {} objects, {} coordinates, over {} by {} meters at 1:{scale}",
        map_path.display(),
        done.objects,
        done.coords,
        (width as f64 / resolution).round(),
        (height as f64 / resolution).round(),
    );

    if let Some(path) = &args.rendered {
        done.drawn
            .save(path)
            .map_err(|e| failed(format!("cannot write {}: {e}", path.display())))?;
        println!(
            "{}: {}x{} pixels",
            path.display(),
            done.drawn.width(),
            done.drawn.height(),
        );
    }

    let (agree, edge, real) = done.shares();
    println!(
        "drawn again against {}:",
        args.image
            .file_name()
            .unwrap_or(args.image.as_os_str())
            .to_string_lossy(),
    );
    println!(
        "  {:.3}% of pixels agree, {:.3}% differ as two renderers differ about an edge, {:.3}% \
         really differ",
        100.0 * agree,
        100.0 * edge,
        100.0 * real,
    );
    if let Some((mean, deviation)) = done.comparison.error {
        println!(
            "  the wrong pixels are out by {mean:.1} of 765 on average, give or take \
             {deviation:.1}, and by {} at the worst",
            done.comparison.largest,
        );
    }
    Ok(())
}

/// What an exit code of this subcommand means, as the module's own
/// documentation lists them.
pub fn exit_meaning(code: i32) -> Option<&'static str> {
    Some(match code {
        0 => "success",
        1 => "usage error",
        2 => "the map could not be made",
        _ => return None,
    })
}

/// Runs `mauro net image-to-map`.
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

// The backend, picked at build time as burn takes it as a type parameter.
// The same three as `train`, without the `Autodiff` wrapper: a record is
// tensor data and loads into either, and nothing here needs a gradient.
use backend::{device, BACKEND};

mod backend {
    use burn::tensor::Device;

    /// What the tool prints for the backend it was built against.
    #[cfg(feature = "cuda")]
    pub const BACKEND: &str = "CUDA";
    /// What the tool prints for the backend it was built against.
    #[cfg(all(feature = "wgpu", not(feature = "cuda")))]
    pub const BACKEND: &str = "wgpu";
    /// What the tool prints for the backend it was built against.
    #[cfg(not(any(feature = "cuda", feature = "wgpu")))]
    pub const BACKEND: &str = "flex";

    /// The one device, whichever it is. No gradient tape over it: the
    /// trainers ask for that themselves.
    #[cfg(feature = "cuda")]
    pub fn device() -> Device {
        Device::cuda(burn::tensor::DeviceIndex::Default)
    }

    /// The one device, whichever it is.
    #[cfg(all(feature = "wgpu", not(feature = "cuda")))]
    pub fn device() -> Device {
        Device::wgpu(burn::tensor::DeviceKind::DefaultDevice)
    }

    /// The one device, whichever it is.
    #[cfg(not(any(feature = "cuda", feature = "wgpu")))]
    pub fn device() -> Device {
        Device::flex()
    }
}
