//! Renders an OpenOrienteering Mapper map to a raster image, without a
//! graphical user interface. Ported from `main.cpp`.
//!
//! ```text
//! map_to_image [-r px-per-meter] [-f meters] <map-file> [image-file]
//! ```
//!
//! Lengths are given in meters on the ground. They are converted to paper
//! units using the map scale (1:15000 etc.) stored in the map file.
//!
//! Exit codes: 0 success, 1 usage error, 2 the map could not be read, 3 the
//! image geometry is invalid, 4 the image could not be written.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;
use tiny_skia::{Pixmap, Transform};

use mti::renderer::Renderer;
use mti::xml_reader::read_xml_map;

/// The default resolution, in pixels per meter on the ground.
const DEFAULT_RESOLUTION: f64 = 3.0;
/// The default width of the white frame around the map, in meters on the ground.
const DEFAULT_FRAME: f64 = 50.0;

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

    let (map, warnings) = read_xml_map(&args.map_file).map_err(|e| {
        (ExitCode::from(2), format!("Error: Failed to load {}: {}", args.map_file.display(), e))
    })?;
    for warning in &warnings {
        eprintln!("Warning: {}", warning);
    }

    // Map coordinates are given in mm on the paper. The map scale relates
    // these paper units to the ground.
    let scale_denominator = map.scale_denominator;
    let mm_per_meter = 1000.0 / scale_denominator as f64;
    let pixel_per_mm = args.resolution * scale_denominator as f64 / 1000.0;

    let renderer = Renderer::new(&map);

    // The extent of the map objects, enlarged by the white frame. For an
    // empty map, the extent is a null rect at the origin, i.e. the frame
    // alone determines the size of the image.
    let frame_mm = args.frame * mm_per_meter;
    let extent = renderer.extent().adjusted(-frame_mm, -frame_mm, frame_mm, frame_mm);

    let width = (extent.width() * pixel_per_mm).round();
    let height = (extent.height() * pixel_per_mm).round();
    if width <= 0.0 || height <= 0.0 {
        return Err((ExitCode::from(3), "Error: The requested image is empty.".to_string()));
    }
    let (width, height) = (width as u32, height as u32);

    let mut pixmap = Pixmap::new(width, height).ok_or_else(|| {
        (ExitCode::from(3), format!(
            "Error: Failed to allocate an image of {}x{} pixels. Not enough memory?",
            width, height
        ))
    })?;
    pixmap.fill(tiny_skia::Color::WHITE);

    // The image resolution refers to the paper, not to the ground. (Only
    // pixel content is compared by the benchmark harness; the PNG DPI
    // metadata Mapper also writes here is not reproduced.)
    let translate = Transform::from_translate(-extent.left() as f32, -extent.top() as f32);
    let scale = Transform::from_scale(pixel_per_mm as f32, pixel_per_mm as f32);
    let page_transform = translate.post_concat(scale);

    renderer.paint(&mut pixmap.as_mut(), page_transform);

    save_image(&pixmap, &image_path).map_err(|e| {
        (ExitCode::from(4), format!(
            "Error: Failed to save {}. Does the directory exist? Is the image format supported? ({e})",
            image_path.display()
        ))
    })?;

    println!(
        "{}: {}x{} pixels, {}x{} meters, map scale 1:{}",
        image_path.display(),
        width,
        height,
        (extent.width() / mm_per_meter).round() as i64,
        (extent.height() / mm_per_meter).round() as i64,
        scale_denominator,
    );
    Ok(())
}

fn save_image(pixmap: &Pixmap, path: &Path) -> Result<(), String> {
    // tiny-skia's pixel buffer is premultiplied RGBA8, but every pixel in
    // the output is fully opaque (the map is painted over an opaque white
    // background), so it is numerically identical to straight RGBA here.
    let image = image::RgbaImage::from_raw(pixmap.width(), pixmap.height(), pixmap.data().to_vec())
        .ok_or_else(|| "invalid pixel buffer".to_string())?;
    image.save(path).map_err(|e| e.to_string())
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
