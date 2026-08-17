//! Renders an OpenOrienteering Mapper map to a raster image — no Mapper, no
//! Qt, no graphical environment needed.
//!
//! ```text
//! map_to_image [-r px-per-meter] [-f meters] <map-file> [image-file]
//! ```
//!
//! This is the tool to reach for to just look at a map. The suffix of the
//! output file picks the format (`.png`, `.bmp`, `.tif`, `.jpg`); with no
//! output file given, the map's own name with `.png` is used.
//!
//! Both options are given in meters on the ground, not on the paper, since
//! that is the way a map is talked about — how many pixels per meter of
//! forest, how wide a margin around it. They are converted to paper units
//! with the map scale (1:15000 and so on) stored in the map file, so the same
//! resolution gives comparable detail across maps drawn at different scales.
//!
//! Exit codes: 0 success, 1 usage error, 2 the map could not be read, 3 the
//! image geometry is invalid, 4 the image could not be written.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;

use maur_o::render::{render_map, save_pixmap, DEFAULT_FRAME, DEFAULT_RESOLUTION};

#[derive(Parser)]
#[command(
    name = "map_to_image",
    version,
    about = "Renders an OpenOrienteering Mapper map to a raster image, without a \
             graphical user interface.\n\n\
             Lengths are given in meters on the ground. They are converted to paper \
             units using the map scale (1:15000 etc.) stored in the map file."
)]
struct Args {
    /// The map to be rendered (.omap, .xmap).
    map_file: PathBuf,

    /// The image to be written. The file name suffix selects the format
    /// (.png, .bmp, .tif, .jpg). Defaults to the map file name with a .png
    /// suffix.
    image_file: Option<PathBuf>,

    /// Resolution of the image, in pixels per meter on the ground.
    #[arg(short = 'r', long, default_value_t = DEFAULT_RESOLUTION)]
    resolution: f64,

    /// Width of the white frame which is added on each side of the map, in
    /// meters on the ground.
    #[arg(short = 'f', long, default_value_t = DEFAULT_FRAME)]
    frame: f64,
}

/// The default output path for the given input path: next to the map file,
/// with the suffix replaced by ".png".
fn default_output_path(map_path: &Path) -> PathBuf {
    map_path.with_extension("png")
}

fn run() -> Result<(), (ExitCode, String)> {
    let args = match Args::try_parse() {
        Ok(a) => a,
        Err(e) => {
            // clap prints its own usage/help text; forward its exit behavior.
            e.print().ok();
            return Err((ExitCode::from(if e.exit_code() == 0 { 0 } else { 1 }), String::new()));
        }
    };

    if !args.resolution.is_finite() || args.resolution < 0.0 {
        return Err((ExitCode::from(1), format!("Error: Invalid value for --resolution: {}", args.resolution)));
    }
    if !args.frame.is_finite() || args.frame < 0.0 {
        return Err((ExitCode::from(1), format!("Error: Invalid value for --frame: {}", args.frame)));
    }
    if args.resolution == 0.0 {
        return Err((ExitCode::from(1), "Error: The resolution must be greater than zero.".to_string()));
    }

    let image_path = args.image_file.clone().unwrap_or_else(|| default_output_path(&args.map_file));

    let rendering = render_map(&args.map_file, args.resolution, args.frame).map_err(|e| match e {
        maur_o::render::Error::Read(message) => (ExitCode::from(2), format!("Error: {message}")),
        maur_o::render::Error::Geometry(message) => (ExitCode::from(3), format!("Error: {message}")),
    })?;
    for warning in &rendering.warnings {
        eprintln!("Warning: {}", warning);
    }

    save_pixmap(&rendering.pixmap, &image_path).map_err(|e| {
        (ExitCode::from(4), format!(
            "Error: Failed to save {}. Does the directory exist? Is the image format supported? ({e})",
            image_path.display()
        ))
    })?;

    println!(
        "{}: {}x{} pixels, {}x{} meters, map scale 1:{}",
        image_path.display(),
        rendering.pixmap.width(),
        rendering.pixmap.height(),
        rendering.ground_width,
        rendering.ground_height,
        rendering.scale_denominator,
    );
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::from(0),
        Err((code, message)) => {
            if !message.is_empty() {
                eprintln!("{}", message);
            }
            code
        }
    }
}
