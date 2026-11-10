//! Downloads the aerial orthophoto of a map's ground and adds it to the map
//! as a georeferenced template, behind everything else.
//!
//! ```text
//! add_orthophoto [--year 2023|2020] [--pixel-size M] [--margin M]
//!                [--image FILE] <map-file> [output-map-file]
//! ```
//!
//! The photos are AGEA's (Agenzia per le Erogazioni in Agricoltura), as
//! Regione Emilia-Romagna serves them, at 20 cm a pixel: `--year` picks the
//! flight, 2023 by default. Only Emilia-Romagna is covered for now. A map
//! elsewhere is reported with a warning, and nothing is written.
//!
//! The map has to be an OpenOrienteering Mapper `.omap`/`.xmap` file,
//! georeferenced in a projected CRS the service offers: WGS84 / UTM zone 32N
//! or 33N (Mapper's "UTM" CRS, or EPSG:32632/32633), ETRS89 / UTM 32N or 33N
//! (EPSG:25832/25833), Monte Mario (EPSG:3003/3004), and a few others. The
//! area downloaded is the box around the map's objects, `--margin` meters
//! (50 by default) wider on every side. The box is north-up in the CRS:
//! Mapper turns the image by the map's grivation itself.
//!
//! The image is a JPEG beside the output map, named after it with the
//! service's year (`forest.omap` → `forest_agea2023.jpg`) unless `--image`
//! says otherwise, with the world file Mapper places it by (`.jgw`). It is
//! added as the map's bottom-most template and made visible in the map's
//! saved view. The output map defaults to the input map itself: nothing but
//! its template list and view change. Running it again re-downloads the
//! image and leaves the map's templates as they are.
//!
//! `--pixel-size` trades detail for size: the photos' own 0.2 m is the
//! default, and a map larger than 300 megapixels at it (about 3.5 × 3.5 km)
//! needs a larger one.
//!
//! The photos are Regione Emilia-Romagna's and AGEA's: credit them wherever
//! a map drawn over them is published.
//!
//! Exit codes: 0 success, 1 usage error, 2 the map could not be read,
//! 3 the map is not georeferenced in a CRS the service offers, 4 the image or
//! the map could not be written, 5 the map is not in Emilia-Romagna,
//! 6 the service could not be reached or answered with something other than
//! a photo.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use clap::Parser;
use image::{ImageFormat, RgbImage};

use maur_o::image_template::{add_image_template, Added, ImageTemplate};
use maur_o::orthophoto::{
    capabilities_url, geographic_ref_point, get_map_url, map_footprint, Capabilities, Grid,
    Service, EMILIA_ROMAGNA,
};
use maur_o::progress::Progress;
use maur_o::xml_reader::read_xml_map_str;

/// The most pixels the stitched image may have: 900 MB of RGB in memory.
const MAX_PIXELS: u64 = 300_000_000;

/// The longest side of the low-resolution image the coverage is checked on.
const PROBE_SIZE: f64 = 512.0;

/// How many tiles are downloaded at once.
const WORKERS: usize = 4;

/// How many times a tile is asked for before giving up.
const ATTEMPTS: usize = 3;

/// The largest response read: far more than a 2000-pixel tile.
const MAX_RESPONSE: u64 = 64 << 20;

#[derive(Parser)]
#[command(
    name = "add_orthophoto",
    version,
    about = "Downloads the AGEA orthophoto of a map's ground (Emilia-Romagna only, for now) \
             and adds it to the map as a georeferenced template."
)]
struct Args {
    /// The OpenOrienteering Mapper map (.omap or .xmap) to add the photo to.
    map_file: PathBuf,

    /// The map to write. Defaults to the input map, changed in place.
    output_map_file: Option<PathBuf>,

    /// Which flight's photos to use.
    #[arg(long, default_value_t = 2023, value_parser = parse_year)]
    year: u16,

    /// The image's ground resolution, in meters per pixel. Defaults to the
    /// photos' own, 0.2.
    #[arg(long, value_name = "METERS", alias = "pixel_size")]
    pixel_size: Option<f64>,

    /// How far beyond the map's objects to download, in meters on the ground.
    #[arg(long, value_name = "METERS", default_value_t = 50.0)]
    margin: f64,

    /// Where to write the image. Defaults to the output map's name with
    /// `_agea<year>.jpg`. Its world file goes beside it.
    #[arg(long, value_name = "FILE")]
    image: Option<PathBuf>,
}

fn parse_year(text: &str) -> Result<u16, String> {
    let years: Vec<String> = EMILIA_ROMAGNA.iter().map(|s| s.year.to_string()).collect();
    text.parse::<u16>()
        .ok()
        .filter(|year| EMILIA_ROMAGNA.iter().any(|s| s.year == *year))
        .ok_or_else(|| format!("the photos are of {}", years.join(" or ")))
}

/// A failure: the exit code and what to say.
type Failure = (ExitCode, String);

fn fail(code: u8, message: impl std::fmt::Display) -> Failure {
    (ExitCode::from(code), format!("Error: {message}"))
}

fn run() -> Result<(), Failure> {
    let args = match Args::try_parse() {
        Ok(a) => a,
        Err(e) => {
            e.print().ok();
            return Err((
                ExitCode::from(if e.exit_code() == 0 { 0 } else { 1 }),
                String::new(),
            ));
        }
    };
    let service = EMILIA_ROMAGNA
        .iter()
        .find(|s| s.year == args.year)
        .expect("parse_year only lets listed years through");
    let pixel_size = args.pixel_size.unwrap_or(service.ground_resolution);
    if !(pixel_size.is_finite() && pixel_size > 0.0) {
        return Err(fail(1, "--pixel-size must be a positive number of meters"));
    }
    if !(args.margin.is_finite() && args.margin >= 0.0) {
        return Err(fail(
            1,
            "--margin must be zero or a positive number of meters",
        ));
    }
    let output = args
        .output_map_file
        .clone()
        .unwrap_or(args.map_file.clone());
    let image_path = args.image.clone().unwrap_or_else(|| {
        let stem = output.file_stem().unwrap_or_default().to_string_lossy();
        output.with_file_name(format!("{stem}_agea{}.jpg", service.year))
    });

    // The map: read for where it is, and kept as text to add the template to.
    let ext = args
        .map_file
        .extension()
        .unwrap_or_default()
        .to_string_lossy();
    if !ext.eq_ignore_ascii_case("omap") && !ext.eq_ignore_ascii_case("xmap") {
        return Err(fail(
            2,
            format!(
                "{} is not a Mapper .omap/.xmap file (open an OCAD map in Mapper and save it as one first)",
                args.map_file.display()
            ),
        ));
    }
    let xml = std::fs::read_to_string(&args.map_file)
        .map_err(|e| fail(2, format!("cannot read {}: {e}", args.map_file.display())))?;
    let (map, _) = read_xml_map_str(&xml)
        .map_err(|e| fail(2, format!("cannot read {}: {e}", args.map_file.display())))?;
    let footprint = map_footprint(&map, args.margin).map_err(|e| fail(3, e))?;
    let epsg = map.georeferencing.as_ref().map_or(0, |g| g.epsg);

    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(120)))
        .user_agent(concat!("maur-o/", env!("CARGO_PKG_VERSION")))
        .build()
        .into();

    // Does the service have this layer, in this CRS?
    let (_, body) = fetch(&agent, &capabilities_url(service))?;
    let capabilities = Capabilities::parse(&String::from_utf8_lossy(&body))
        .map_err(|e| fail(6, format!("{}: {e}", service.url)))?;
    if !capabilities.layers.iter().any(|l| l == service.layer) {
        return Err(fail(
            6,
            format!("{} no longer serves {}", service.url, service.layer),
        ));
    }
    // Far away -- another region, another country -- is plain from the map's
    // own latitude and longitude, whatever its CRS.
    if let (Some((lat, lon)), Some(area)) = (geographic_ref_point(&xml), capabilities.lat_lon_box) {
        if !area.contains(lat, lon) {
            return Err(not_covered(&args.map_file, service));
        }
    }
    if !capabilities.offers_epsg(epsg) {
        let offered: Vec<&str> = capabilities
            .crs
            .iter()
            .map(String::as_str)
            .filter(|c| c.starts_with("EPSG:"))
            .collect();
        return Err(fail(
            3,
            format!(
                "the map is in EPSG:{epsg}, which {} does not serve; it serves {}",
                service.region,
                offered.join(", ")
            ),
        ));
    }

    // Is the map where the photos are? Transparent is where they are not.
    let probe_pixel = (footprint.width().max(footprint.height()) / PROBE_SIZE).max(pixel_size);
    let probe = Grid::covering(&footprint, probe_pixel);
    let url = get_map_url(
        service,
        epsg,
        &probe.bounds(),
        probe.width,
        probe.height,
        "image/png",
        true,
    );
    let probe_image = fetch_image(&agent, &url, ImageFormat::Png)?.to_rgba8();
    let photographed = probe_image.pixels().filter(|p| p[3] > 0).count();
    let coverage = photographed as f64 / (probe_image.width() * probe_image.height()).max(1) as f64;
    if photographed == 0 {
        return Err(not_covered(&args.map_file, service));
    }
    if coverage < 0.995 {
        eprintln!(
            "Warning: only {:.0}% of the map's ground is in {}; the rest of the image is black",
            coverage * 100.0,
            service.region
        );
    }

    // The photo itself, tile by tile.
    let grid = Grid::covering(&footprint, pixel_size);
    if grid.pixel_count() > MAX_PIXELS || grid.width > 65_000 || grid.height > 65_000 {
        return Err(fail(
            1,
            format!(
                "{} × {} pixels is too large an image; give a --pixel-size larger than {pixel_size} m",
                grid.width, grid.height
            ),
        ));
    }
    let photo = download(&agent, service, epsg, &grid)?;

    image_writer(&photo, &image_path)?;
    let world_file = world_file_path(&image_path);
    std::fs::write(&world_file, grid.world_file())
        .map_err(|e| fail(4, format!("cannot write {}: {e}", world_file.display())))?;

    // The map.
    let absolute = std::path::absolute(&image_path)
        .map_err(|e| fail(4, format!("cannot place {}: {e}", image_path.display())))?;
    let name = image_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let relpath = relative_to_map(&absolute, &output);
    let template = ImageTemplate {
        name: &name,
        path: &absolute.to_string_lossy(),
        relpath: &relpath,
        epsg,
    };
    let (edited, added) = add_image_template(&xml, &template).map_err(|e| fail(2, e))?;
    if added == Added::Added || output != args.map_file {
        write_atomically(&output, edited.as_bytes())?;
    }

    println!(
        "{} × {} pixels, {:.0} × {:.0} m at {} m a pixel, EPSG:{epsg}",
        grid.width,
        grid.height,
        f64::from(grid.width) * pixel_size,
        f64::from(grid.height) * pixel_size,
        pixel_size
    );
    println!(
        "Wrote {} and {}",
        image_path.display(),
        world_file.display()
    );
    match added {
        Added::Added => println!("Added it to {} as a template", output.display()),
        Added::AlreadyThere => println!(
            "{} already had it as a template; only the image was replaced",
            output.display()
        ),
    }
    println!(
        "Photos: {} — credit them wherever the map is published.",
        service.attribution
    );
    Ok(())
}

/// The map is somewhere the service has no photo of: a warning, exit code 5.
fn not_covered(map_file: &Path, service: &Service) -> Failure {
    (
        ExitCode::from(5),
        format!(
            "Warning: {} is not in {}: its orthophotos ({}) have nothing of the map's ground. \
             Only Emilia-Romagna is supported for now; nothing was written.",
            map_file.display(),
            service.region,
            service.attribution
        ),
    )
}

/// GETs `url`: its content type and body. A failure to connect, an HTTP
/// error status and a body too large are all exit code 6.
fn fetch(agent: &ureq::Agent, url: &str) -> Result<(String, Vec<u8>), Failure> {
    let mut response = agent
        .get(url)
        .call()
        .map_err(|e| fail(6, format!("{url}: {e}")))?;
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let body = response
        .body_mut()
        .with_config()
        .limit(MAX_RESPONSE)
        .read_to_vec()
        .map_err(|e| fail(6, format!("{url}: {e}")))?;
    Ok((content_type, body))
}

/// GETs an image. A WMS reports an error as a `200 OK` XML document, so
/// anything that is not an image is an error, with what it says.
fn fetch_image(
    agent: &ureq::Agent,
    url: &str,
    format: ImageFormat,
) -> Result<image::DynamicImage, Failure> {
    let (content_type, body) = fetch(agent, url)?;
    if !content_type.starts_with("image/") {
        let text = String::from_utf8_lossy(&body);
        let text: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
        let excerpt: String = text.chars().take(300).collect();
        return Err(fail(
            6,
            format!("the service answered with {content_type} instead of a photo: {excerpt}"),
        ));
    }
    image::load_from_memory_with_format(&body, format)
        .map_err(|e| fail(6, format!("the service sent an unreadable photo: {e}")))
}

/// The whole grid's photo, its tiles downloaded [`WORKERS`] at a time.
fn download(
    agent: &ureq::Agent,
    service: &Service,
    epsg: i32,
    grid: &Grid,
) -> Result<RgbImage, Failure> {
    let tiles = grid.tiles(service.max_tile_size);
    let photo = Mutex::new(RgbImage::new(grid.width, grid.height));
    let progress = Mutex::new(Progress::new("Downloading", tiles.len()));
    let next = AtomicUsize::new(0);
    let failure: Mutex<Option<Failure>> = Mutex::new(None);

    std::thread::scope(|scope| {
        for _ in 0..WORKERS.min(tiles.len()) {
            scope.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= tiles.len() || failure.lock().unwrap().is_some() {
                    return;
                }
                let tile = &tiles[i];
                let url = get_map_url(
                    service,
                    epsg,
                    &grid.tile_box(tile),
                    tile.width,
                    tile.height,
                    "image/jpeg",
                    false,
                );
                let mut result = fetch_image(agent, &url, ImageFormat::Jpeg);
                for _ in 1..ATTEMPTS {
                    if result.is_ok() {
                        break;
                    }
                    std::thread::sleep(Duration::from_secs(2));
                    result = fetch_image(agent, &url, ImageFormat::Jpeg);
                }
                match result {
                    Ok(piece) => {
                        let piece = piece.to_rgb8();
                        if (piece.width(), piece.height()) != (tile.width, tile.height) {
                            *failure.lock().unwrap() = Some(fail(
                                6,
                                format!(
                                    "asked for a {} × {} tile, the service sent {} × {}",
                                    tile.width,
                                    tile.height,
                                    piece.width(),
                                    piece.height()
                                ),
                            ));
                            return;
                        }
                        image::imageops::replace(
                            &mut *photo.lock().unwrap(),
                            &piece,
                            i64::from(tile.x),
                            i64::from(tile.y),
                        );
                        progress.lock().unwrap().tick();
                    }
                    Err(e) => {
                        *failure.lock().unwrap() = Some(e);
                        return;
                    }
                }
            });
        }
    });

    if let Some(failure) = failure.into_inner().unwrap() {
        return Err(failure);
    }
    progress.into_inner().unwrap().finish();
    Ok(photo.into_inner().unwrap())
}

/// Writes `photo` to `path`: JPEG at quality 90 for `.jpg`/`.jpeg`, else
/// whatever the extension says.
fn image_writer(photo: &RgbImage, path: &Path) -> Result<(), Failure> {
    let cannot =
        |e: &dyn std::fmt::Display| fail(4, format!("cannot write {}: {e}", path.display()));
    let ext = path
        .extension()
        .unwrap_or_default()
        .to_string_lossy()
        .to_ascii_lowercase();
    if ext == "jpg" || ext == "jpeg" {
        let file = std::fs::File::create(path).map_err(|e| cannot(&e))?;
        let mut out = std::io::BufWriter::new(file);
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 90)
            .encode_image(photo)
            .map_err(|e| cannot(&e))
    } else {
        photo.save(path).map_err(|e| cannot(&e))
    }
}

/// Where Mapper looks for an image's world file first
/// (`WorldFile::tryToLoadForImage`): the extension's first and last letters
/// and a `w`, `.jpg` → `.jgw`; `.wld` for an extension too short for that.
fn world_file_path(image: &Path) -> PathBuf {
    let ext: Vec<char> = image
        .extension()
        .unwrap_or_default()
        .to_string_lossy()
        .chars()
        .collect();
    if ext.len() > 2 {
        image.with_extension(format!("{}{}w", ext[0], ext[ext.len() - 1]))
    } else {
        image.with_extension("wld")
    }
}

/// The image's path as the map's template list keeps it relative to the map:
/// its file name when the two are side by side, its absolute path otherwise.
fn relative_to_map(image: &Path, map: &Path) -> String {
    let map_dir = std::path::absolute(map)
        .ok()
        .and_then(|m| m.parent().map(Path::to_path_buf));
    match (map_dir, image.parent(), image.file_name()) {
        (Some(dir), Some(parent), Some(name)) if dir == parent => {
            name.to_string_lossy().into_owned()
        }
        _ => image.to_string_lossy().into_owned(),
    }
}

/// Writes `path` all at once: to a temporary file beside it, then renamed
/// over it, so a failure halfway never leaves half a map. The file keeps the
/// permissions it had; a new one gets the usual ones.
fn write_atomically(path: &Path, content: &[u8]) -> Result<(), Failure> {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let temporary = path.with_file_name(format!(".{name}.add_orthophoto.tmp"));
    let permissions = std::fs::metadata(path).ok().map(|m| m.permissions());
    let written = std::fs::write(&temporary, content)
        .and_then(|()| match permissions {
            Some(permissions) => std::fs::set_permissions(&temporary, permissions),
            None => Ok(()),
        })
        .and_then(|()| std::fs::rename(&temporary, path));
    written.map_err(|e| {
        std::fs::remove_file(&temporary).ok();
        fail(4, format!("cannot write {}: {e}", path.display()))
    })
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err((code, message)) => {
            if !message.is_empty() {
                eprintln!("{message}");
            }
            code
        }
    }
}
