//! Renders an OpenOrienteering Mapper map to a raster image — no Mapper, no
//! Qt, no graphical environment needed.
//!
//! ```text
//! map_to_image [-r px-per-meter] [-f meters | -w left,top,right,bottom] <map-file> [image-file]
//! ```
//!
//! This is the tool to reach for to just look at a map. The suffix of the
//! output file picks the format (`.png`, `.bmp`, `.tif`, `.jpg`, or `.svg`
//! for vector output); with no output file given, the map's own name with
//! `.png` is used.
//!
//! Both options are given in meters on the ground, not on the paper, since
//! that is the way a map is talked about — how many pixels per meter of
//! forest, how wide a margin around it. They are converted to paper units
//! with the map scale (1:15000 and so on) stored in the map file, so the same
//! resolution gives comparable detail across maps drawn at different scales.
//!
//! The one exception is `--window`, which picks a part of the map to draw in
//! the map's own coordinates — mm on the paper, as Mapper stores them — so
//! that where a point of the map lands in the image is known exactly.
//!
//! `.svg` writes the drawing itself rather than a picture of it, so it stays
//! sharp at any size. `-r` does not apply to it — there are no pixels — and
//! the frame is empty rather than white, an SVG being a drawing and not a
//! page. Nor does `--window`: an SVG is always the whole map.
//!
//! Exit codes: 0 success, 1 usage error, 2 the map could not be read, 3 the
//! image geometry is invalid, 4 the image could not be written.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;

use maur_o::geometry::Rect;
use maur_o::render::{
    render_map_svg, render_map_window, save_pixmap, DEFAULT_FRAME, DEFAULT_RESOLUTION,
};

#[derive(Parser)]
#[command(
    name = "map_to_image",
    version,
    about = "Renders an OpenOrienteering Mapper or OCAD map to a raster image, without a \
             graphical user interface.\n\n\
             Lengths are given in meters on the ground. They are converted to paper \
             units using the map scale (1:15000 etc.) stored in the map file."
)]
struct Args {
    /// The map to be rendered (.omap, .xmap, .ocd).
    map_file: PathBuf,

    /// The image to be written. The file name suffix selects the format
    /// (.png, .bmp, .tif, .jpg, or .svg for vector output). Defaults to the
    /// map file name with a .png suffix.
    image_file: Option<PathBuf>,

    /// Resolution of the image, in pixels per meter on the ground.
    #[arg(short = 'r', long, default_value_t = DEFAULT_RESOLUTION)]
    resolution: f64,

    /// Width of the white frame which is added on each side of the map, in
    /// meters on the ground.
    #[arg(short = 'f', long, default_value_t = DEFAULT_FRAME)]
    frame: f64,

    /// Draw only this rectangle of the map instead of all of it, given as
    /// `left,top,right,bottom` in map coordinates (mm on the paper, y
    /// pointing down, as Mapper stores them). Cannot be combined with
    /// `--frame`.
    #[arg(
        short = 'w',
        long,
        value_name = "LEFT,TOP,RIGHT,BOTTOM",
        allow_hyphen_values = true,
        conflicts_with = "frame"
    )]
    window: Option<String>,
}

/// The default output path for the given input path: next to the map file,
/// with the suffix replaced by ".png".
fn default_output_path(map_path: &Path) -> PathBuf {
    map_path.with_extension("png")
}

/// Writes the map as SVG, reporting it the way the raster path reports an
/// image — the ground it covers rather than the pixels it took.
fn write_svg(args: &Args, svg_path: &Path) -> Result<(), (ExitCode, String)> {
    let rendering = render_map_svg(&args.map_file, args.frame).map_err(|e| match e {
        maur_o::render::Error::Read(message) => (ExitCode::from(2), format!("Error: {message}")),
        maur_o::render::Error::Geometry(message) => {
            (ExitCode::from(3), format!("Error: {message}"))
        }
    })?;
    for warning in &rendering.warnings {
        eprintln!("Warning: {}", warning);
    }
    std::fs::write(svg_path, &rendering.svg).map_err(|e| {
        (
            ExitCode::from(4),
            format!(
                "Error: Failed to save {}. Does the directory exist? ({e})",
                svg_path.display()
            ),
        )
    })?;
    println!(
        "{}: vector, {}x{} meters, map scale 1:{}",
        svg_path.display(),
        rendering.ground_width,
        rendering.ground_height,
        rendering.scale_denominator,
    );
    Ok(())
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

    if !args.resolution.is_finite() || args.resolution < 0.0 {
        return Err((
            ExitCode::from(1),
            format!("Error: Invalid value for --resolution: {}", args.resolution),
        ));
    }
    if !args.frame.is_finite() || args.frame < 0.0 {
        return Err((
            ExitCode::from(1),
            format!("Error: Invalid value for --frame: {}", args.frame),
        ));
    }
    if args.resolution == 0.0 {
        return Err((
            ExitCode::from(1),
            "Error: The resolution must be greater than zero.".to_string(),
        ));
    }

    let window = match &args.window {
        None => None,
        Some(text) => {
            let invalid = || {
                (
                    ExitCode::from(1),
                    format!(
                        "Error: Invalid value for --window: {text} (expected \
                         left,top,right,bottom with right > left and bottom > top)"
                    ),
                )
            };
            let w: Vec<f64> = text
                .split(',')
                .map(|v| v.trim().parse::<f64>())
                .collect::<Result<_, _>>()
                .map_err(|_| invalid())?;
            if w.len() != 4 || !w.iter().all(|v| v.is_finite()) || w[2] <= w[0] || w[3] <= w[1] {
                return Err(invalid());
            }
            Some(Rect::new(w[0], w[1], w[2] - w[0], w[3] - w[1]))
        }
    };

    let image_path = args
        .image_file
        .clone()
        .unwrap_or_else(|| default_output_path(&args.map_file));

    let is_svg = image_path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("svg"));
    if is_svg {
        if window.is_some() {
            return Err((
                ExitCode::from(1),
                "Error: --window does not apply to an SVG, which is always the whole map."
                    .to_string(),
            ));
        }
        return write_svg(&args, &image_path);
    }

    let rendering = render_map_window(&args.map_file, args.resolution, args.frame, window)
        .map_err(|e| match e {
            maur_o::render::Error::Read(message) => {
                (ExitCode::from(2), format!("Error: {message}"))
            }
            maur_o::render::Error::Geometry(message) => {
                (ExitCode::from(3), format!("Error: {message}"))
            }
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
