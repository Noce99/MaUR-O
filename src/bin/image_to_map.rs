//! Reads a picture of a map back into a map file, with a trained network.
//!
//! ```text
//! image_to_map [OPTIONS] <run-folder> <image> [map-file]
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
//! image_to_map trainings/UNet_2026_08_18__09_26_59 dataset/images/map_001.png back.omap
//! ```
//!
//! The run folder is all it is told, because a run folder holds everything:
//! `training.json` says how big the network was, `best.mpk` is its weights,
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

use maur_o::vectorize::Simplify;

use maur_o::net::predict::{
    load, read_back, ReadBackSettings, OVERLAP, PREDICTED_SAME_ANGLE, PREDICTED_TOLERANCE,
};

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
struct Args {
    /// A run folder under trainings/: the weights, and the notes copied out
    /// of the dataset the run trained on.
    run: PathBuf,

    /// The picture of a map to read.
    image: PathBuf,

    /// The map to be written. Defaults to the image's name with a .omap
    /// suffix, beside it.
    map_file: Option<PathBuf>,

    /// The weights to use. Defaults to best.mpk in the run folder, which is
    /// the epoch which validated best; a checkpoint/007/model.mpk may be
    /// named instead.
    #[arg(short = 'w', long, value_name = "FILE")]
    weights: Option<PathBuf>,

    /// How far the boundary may be moved to be rid of a node, in cells --
    /// one cell being one pixel of the picture. Nought keeps every node the
    /// staircase asks for, which on a picture this size is a great many.
    #[arg(short = 't', long, default_value_t = PREDICTED_TOLERANCE, value_name = "PIXELS")]
    tolerance: f64,

    /// How far apart two neighbouring pixels' angles may be and still be one
    /// object's, as a share of a whole turn. `inf` never splits a region on
    /// its angle.
    #[arg(short = 'a', long, default_value_t = PREDICTED_SAME_ANGLE, value_name = "TURNS")]
    same_angle: f32,

    /// How many pixels square a tile of the picture is. Defaults to the crop
    /// the run trained at, and has to divide by 16.
    #[arg(short = 'c', long, value_name = "PIXELS")]
    crop: Option<usize>,

    /// How much of each tile is shared with the one before it, in pixels.
    /// Defaults to a quarter of the tile.
    #[arg(short = 'o', long, value_name = "PIXELS")]
    overlap: Option<usize>,

    /// Draw the map which was written, over the very ground the picture
    /// covers, and keep it. It is drawn either way, to score the result.
    // No short flag: `-i` next to the `<IMAGE>` this reads would be a
    // reasonable thing to expect to mean the input.
    #[arg(long, value_name = "IMAGE")]
    rendered: Option<PathBuf>,

    /// How many pixels of picture one meter of ground is. Defaults to what
    /// the run's classes.json records for the dataset it trained on.
    #[arg(short = 'r', long, value_name = "PX_PER_M")]
    resolution: Option<f64>,

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
    let resolution = match args.resolution.unwrap_or(notes.resolution) {
        resolution if resolution.is_finite() && resolution > 0.0 => resolution,
        resolution => {
            return Err(usage(format!(
                "Invalid value for --resolution: {resolution} is not pixels per meter."
            )))
        }
    };

    // `open_image` rather than `image::open`: it lifts the decoder's default
    // refusal to read past 512MiB, which a map at a fine resolution clears.
    let picture = open_image(&args.image).map_err(failed)?;
    let (width, height) = (picture.width(), picture.height());

    let (model, config) =
        load::<Backend>(&args.run, args.weights.as_deref(), &device()).map_err(failed)?;
    if model.classes() != notes.classes {
        return Err(failed(format!(
            "the network tells {} symbols apart and {} was written for {}",
            model.classes(),
            notes_file.display(),
            notes.classes,
        )));
    }

    let tile = args.crop.unwrap_or(config.crop);
    let overlap = args.overlap.unwrap_or((tile as f64 * OVERLAP) as usize);
    println!(
        "{BACKEND}: {}x{} pixels through a U-Net of {} symbols, in tiles of {tile} overlapping \
         by {overlap}",
        width,
        height,
        model.classes(),
    );

    let settings = ReadBackSettings {
        tile,
        overlap,
        resolution,
        scale_denominator: scale,
        simplify: Simplify {
            tolerance: args.tolerance,
            same_angle: args.same_angle,
        },
    };
    let map_path = args
        .map_file
        .clone()
        .unwrap_or_else(|| args.image.with_extension("omap"));
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

// The backend, picked at build time as burn takes it as a type parameter.
// The same three as `train`, without the `Autodiff` wrapper: a record is
// tensor data and loads into either, and nothing here needs a gradient.
use backend::{device, Backend, BACKEND};

#[cfg(feature = "cuda")]
mod backend {
    /// What to call it when the tool says which one it used.
    pub const BACKEND: &str = "CUDA";
    /// NVIDIA, through CUDA.
    pub type Backend = burn::backend::Cuda;
    /// The one device, whichever it is.
    pub fn device() -> burn::backend::cuda::CudaDevice {
        Default::default()
    }
}

#[cfg(all(feature = "wgpu", not(feature = "cuda")))]
mod backend {
    /// What to call it when the tool says which one it used.
    pub const BACKEND: &str = "wgpu";
    /// Any GPU with a Vulkan, Metal or DX12 driver.
    pub type Backend = burn::backend::Wgpu;
    /// The one device, whichever it is.
    pub fn device() -> burn::backend::wgpu::WgpuDevice {
        Default::default()
    }
}

#[cfg(not(any(feature = "cuda", feature = "wgpu")))]
mod backend {
    /// What to call it when the tool says which one it used.
    pub const BACKEND: &str = "ndarray";
    /// The pure Rust backend, which runs anywhere the renderer does.
    pub type Backend = burn::backend::NdArray;
    /// The one device, whichever it is.
    pub fn device() -> burn::backend::ndarray::NdArrayDevice {
        Default::default()
    }
}
