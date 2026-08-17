//! Rendering a map file to a raster image.
//!
//! The geometry here — how the map extent, the white frame and the
//! resolution turn into a pixel buffer — is what `map_to_image` does, kept
//! in one place so that the `benchmark` tool renders a map the same way the
//! command line tool does rather than by a second, drifting copy of it.

use std::path::Path;

use tiny_skia::{Pixmap, Transform};

use crate::renderer::Renderer;
use crate::xml_reader::read_xml_map;

/// The default resolution, in pixels per meter on the ground.
pub const DEFAULT_RESOLUTION: f64 = 3.0;
/// The default width of the white frame around the map, in meters on the ground.
pub const DEFAULT_FRAME: f64 = 50.0;

/// Why a map could not be turned into an image. The variants are the two
/// failures `map_to_image` reports with distinct exit codes.
pub enum Error {
    /// The map file could not be read or parsed.
    Read(String),
    /// The map is there, but the image it asks for cannot be made.
    Geometry(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Read(message) | Error::Geometry(message) => f.write_str(message),
        }
    }
}

/// A drawn map, and what it turned out to be.
pub struct Rendering {
    /// The image itself, opaque RGBA, white wherever the map drew nothing.
    pub pixmap: Pixmap,
    /// The extent covered by the image, in meters on the ground, the white
    /// frame included, and rounded the way `map_to_image` reports it.
    pub ground_width: i64,
    /// The same, vertically.
    pub ground_height: i64,
    /// The map scale the file asked for: 15000 for a 1:15000 map.
    pub scale_denominator: i32,
    /// Non-fatal complaints from reading the map file. A map that produced
    /// some was still drawn, but perhaps not in full.
    pub warnings: Vec<String>,
}

/// Renders `map_file` at `resolution` pixels per meter on the ground, with a
/// white frame of `frame` meters on each side.
pub fn render_map(map_file: &Path, resolution: f64, frame: f64) -> Result<Rendering, Error> {
    let (map, warnings) = read_xml_map(map_file).map_err(|e| {
        Error::Read(format!("Failed to load {}: {}", map_file.display(), e))
    })?;

    // Map coordinates are given in mm on the paper. The map scale relates
    // these paper units to the ground.
    let scale_denominator = map.scale_denominator;
    let mm_per_meter = 1000.0 / scale_denominator as f64;
    let pixel_per_mm = resolution * scale_denominator as f64 / 1000.0;

    let renderer = Renderer::new(&map);

    // The extent of the map objects, enlarged by the white frame. For an
    // empty map, the extent is a null rect at the origin, i.e. the frame
    // alone determines the size of the image.
    let frame_mm = frame * mm_per_meter;
    let extent = renderer.extent().adjusted(-frame_mm, -frame_mm, frame_mm, frame_mm);

    let width = (extent.width() * pixel_per_mm).round();
    let height = (extent.height() * pixel_per_mm).round();
    if width <= 0.0 || height <= 0.0 {
        return Err(Error::Geometry("The requested image is empty.".to_string()));
    }
    let (width, height) = (width as u32, height as u32);

    let mut pixmap = Pixmap::new(width, height).ok_or_else(|| {
        Error::Geometry(format!(
            "Failed to allocate an image of {}x{} pixels. Not enough memory?",
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

    Ok(Rendering {
        pixmap,
        ground_width: (extent.width() / mm_per_meter).round() as i64,
        ground_height: (extent.height() / mm_per_meter).round() as i64,
        scale_denominator,
        warnings,
    })
}

/// Writes a rendered pixmap. The file name suffix selects the format.
pub fn save_pixmap(pixmap: &Pixmap, path: &Path) -> Result<(), String> {
    // tiny-skia's pixel buffer is premultiplied RGBA8, but every pixel in
    // the output is fully opaque (the map is painted over an opaque white
    // background), so it is numerically identical to straight RGBA here.
    let image = image::RgbaImage::from_raw(pixmap.width(), pixmap.height(), pixmap.data().to_vec())
        .ok_or_else(|| "invalid pixel buffer".to_string())?;
    image.save(path).map_err(|e| e.to_string())
}
