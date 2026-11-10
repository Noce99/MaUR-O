//! Downloads the aerial orthophoto of a map's ground and adds it to the map
//! as a georeferenced template, behind everything else.
//!
//! ```text
//! add_orthophoto [--service ID] [--pixel-size M] [--margin M]
//!                [--image FILE] <map-file> [output-map-file]
//! add_orthophoto --list-services
//! ```
//!
//! The photos come from the national and regional mapping agencies' own
//! WMS services, across much of Europe: `--list-services` lists them. The
//! service is picked by where the map is: the first, in that list's order,
//! with photos of the whole map (regions before their country, finer photos
//! before coarser, newer before older). `--service` picks one by its id
//! instead. A map that no service has photos of is reported with a warning,
//! and nothing is written.
//!
//! The map has to be an OpenOrienteering Mapper `.omap`/`.xmap` file,
//! georeferenced: placed on the earth in a projected CRS, with the latitude
//! and longitude Mapper keeps beside it. The photo is asked for in the
//! map's own CRS where the service offers it; otherwise in Web Mercator
//! (EPSG:3857), or latitude and longitude (EPSG:4326), and Mapper
//! reprojects it onto the map when it opens it. The area downloaded is the
//! box around the map's objects, `--margin` meters (50 by default) wider on
//! every side. The box is north-up in its CRS: Mapper turns the image by the
//! map's grivation itself.
//!
//! The image is a JPEG beside the output map, named after it with the
//! service's id (`forest.omap` → `forest_it-emilia-romagna-2023.jpg`) unless
//! `--image` says otherwise, with the world file Mapper places it by
//! (`.jgw`). It is added as the map's bottom-most template and made visible
//! in the map's saved view. The output map defaults to the input map itself:
//! nothing but its template list and view change. Running it again
//! re-downloads the image and leaves the map's templates as they are.
//!
//! `--pixel-size` trades detail for size: the photos' own resolution is the
//! default, and an image larger than 300 megapixels (about 3.5 × 3.5 km at
//! 0.2 m) needs a larger one.
//!
//! The photos belong to the agency that serves them: credit them (the tool
//! prints how) wherever a map drawn over them is published.
//!
//! Exit codes: 0 success, 1 usage error, 2 the map could not be read,
//! 3 the map is not georeferenced (or no service covering it offers a CRS
//! the photo can be placed in), 4 the image or the map could not be
//! written, 5 no service has photos of the map's ground, 6 the service could
//! not be reached or answered with something other than a photo.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use clap::Parser;
use image::{ImageFormat, RgbImage};

use maur_o::image_template::{add_image_template, Added, ImageTemplate};
use maur_o::orthophoto::{
    capabilities_url, footprint, geographic_anchor, get_map_url, is_blank, pixel_size, request_crs,
    Capabilities, GeographicAnchor, Grid, ProjectedBox, Service,
};
use maur_o::orthophoto_services::SERVICES;
use maur_o::progress::Progress;
use maur_o::stats::coordinate_bounds;
use maur_o::xml_reader::read_xml_map_str;

/// The most pixels the stitched image may have: 900 MB of RGB in memory.
const MAX_PIXELS: u64 = 300_000_000;

/// The longest side of the low-resolution image the coverage is checked on.
const PROBE_SIZE: f64 = 512.0;

/// The share of the map's ground a service must have photos of to be used
/// without looking further. Not all of it: bright roofs read as no photo
/// in the probe (see `is_blank`), a few in a hundred in a city.
const FULL_COVERAGE: f64 = 0.98;

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
    about = "Downloads the aerial orthophoto of a map's ground from a national or regional \
             mapping agency and adds it to the map as a georeferenced template."
)]
struct Args {
    /// The OpenOrienteering Mapper map (.omap or .xmap) to add the photo to.
    #[arg(required_unless_present = "list_services")]
    map_file: Option<PathBuf>,

    /// The map to write. Defaults to the input map, changed in place.
    output_map_file: Option<PathBuf>,

    /// The service to download from, by id (see --list-services), instead of
    /// the best one for where the map is.
    #[arg(long, value_name = "ID")]
    service: Option<String>,

    /// Lists the services, best first, and exits.
    #[arg(long, alias = "list_services")]
    list_services: bool,

    /// The image's ground resolution, in meters per pixel. Defaults to the
    /// service's own.
    #[arg(long, value_name = "METERS", alias = "pixel_size")]
    pixel_size: Option<f64>,

    /// How far beyond the map's objects to download, in meters on the ground.
    #[arg(long, value_name = "METERS", default_value_t = 50.0)]
    margin: f64,

    /// Where to write the image. Defaults to the output map's name with
    /// `_<service id>.jpg`. Its world file goes beside it.
    #[arg(long, value_name = "FILE")]
    image: Option<PathBuf>,
}

/// A failure: the exit code and what to say.
type Failure = (ExitCode, String);

fn fail(code: u8, message: impl std::fmt::Display) -> Failure {
    (ExitCode::from(code), format!("Error: {message}"))
}

/// A service that has photos of the map, and how to ask it for them.
struct Choice<'a> {
    service: &'a Service,
    /// The CRS to ask in, and the image's.
    epsg: i32,
    /// The map's footprint in it.
    footprint: ProjectedBox,
    /// The share of the footprint it has photos of.
    coverage: f64,
}

/// Why a service was passed over.
enum Skip {
    /// It has no photos there.
    NotCovered,
    /// It offers no CRS the photo could be placed in.
    NoCrs(String),
    /// It could not be asked.
    Failed(Failure),
}

fn list_services() {
    println!("{:<34} {:>6}  {:<11} Area", "Id", "Pixel", "Years");
    for service in SERVICES {
        println!(
            "{:<34} {:>5}m  {:<11} {}",
            service.id, service.ground_resolution, service.years, service.area
        );
    }
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
    if args.list_services {
        list_services();
        return Ok(());
    }
    let map_file = args.map_file.clone().expect("clap requires it");
    let chosen = match &args.service {
        Some(id) => Some(SERVICES.iter().find(|s| s.id == id).ok_or_else(|| {
            fail(
                1,
                format!("there is no service {id}; --list-services lists them"),
            )
        })?),
        None => None,
    };
    if let Some(pixel_size) = args.pixel_size {
        if !(pixel_size.is_finite() && pixel_size > 0.0) {
            return Err(fail(1, "--pixel-size must be a positive number of meters"));
        }
    }
    if !(args.margin.is_finite() && args.margin >= 0.0) {
        return Err(fail(
            1,
            "--margin must be zero or a positive number of meters",
        ));
    }
    let output = args.output_map_file.clone().unwrap_or(map_file.clone());

    // The map: read for where it is, and kept as text to add the template to.
    let ext = map_file.extension().unwrap_or_default().to_string_lossy();
    if !ext.eq_ignore_ascii_case("omap") && !ext.eq_ignore_ascii_case("xmap") {
        return Err(fail(
            2,
            format!(
                "{} is not a Mapper .omap/.xmap file (open an OCAD map in Mapper and save it as one first)",
                map_file.display()
            ),
        ));
    }
    let xml = std::fs::read_to_string(&map_file)
        .map_err(|e| fail(2, format!("cannot read {}: {e}", map_file.display())))?;
    let (map, _) = read_xml_map_str(&xml)
        .map_err(|e| fail(2, format!("cannot read {}: {e}", map_file.display())))?;
    let anchor = geographic_anchor(&xml)
        .filter(|_| map.georeferencing.is_some())
        .ok_or_else(|| {
            fail(
                3,
                "the map is not georeferenced: it has no place on the earth (latitude and longitude)",
            )
        })?;
    if coordinate_bounds(&map).is_none() {
        return Err(fail(3, "the map has no objects to download a photo of"));
    }
    let map_epsg = map.georeferencing.as_ref().map_or(0, |g| g.epsg);

    let candidates: Vec<&Service> = match chosen {
        Some(service) => vec![service],
        None => SERVICES
            .iter()
            .filter(|s| s.covers(anchor.lat, anchor.lon))
            .collect(),
    };
    if candidates.is_empty() {
        return Err(not_covered(&map_file, &anchor));
    }

    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(120)))
        .user_agent(concat!("maur-o/", env!("CARGO_PKG_VERSION")))
        .build()
        .into();

    // The first service with photos of the whole map; failing that, the one
    // with photos of most of it.
    let mut best: Option<Choice> = None;
    let mut skipped: Vec<(&Service, Skip)> = Vec::new();
    for &service in &candidates {
        match try_service(&agent, service, &map, &anchor, map_epsg, args.margin) {
            Ok(choice) => {
                let full = choice.coverage >= FULL_COVERAGE;
                if best.as_ref().is_none_or(|b| choice.coverage > b.coverage) {
                    best = Some(choice);
                }
                if full {
                    break;
                }
            }
            Err(skip) => skipped.push((service, skip)),
        }
    }
    let Some(choice) = best else {
        return Err(nothing_usable(&map_file, &anchor, skipped));
    };
    for (service, skip) in &skipped {
        if let Skip::Failed((_, message)) | Skip::NoCrs(message) = skip {
            eprintln!(
                "Warning: skipped {} ({}): {message}",
                service.id, service.area
            );
        }
    }
    let Choice {
        service,
        epsg,
        footprint,
        coverage,
    } = choice;
    if coverage < FULL_COVERAGE {
        eprintln!(
            "Warning: {} has photos of only {:.0}% of the map's ground; the rest of the image is black",
            service.area,
            coverage * 100.0
        );
    }

    // The photo itself, tile by tile.
    let ground_pixel = args.pixel_size.unwrap_or(service.ground_resolution);
    let grid = Grid::covering(
        &footprint,
        pixel_size(epsg, map_epsg, anchor.lat, ground_pixel),
    );
    if grid.pixel_count() > MAX_PIXELS || grid.width > 65_000 || grid.height > 65_000 {
        return Err(fail(
            1,
            format!(
                "{} × {} pixels is too large an image; give a --pixel-size larger than {ground_pixel} m",
                grid.width, grid.height
            ),
        ));
    }
    println!(
        "Downloading {} ({}, {} m a pixel) in EPSG:{epsg}",
        service.area, service.years, ground_pixel
    );
    let photo = download(&agent, service, epsg, &grid)?;

    let image_path = args.image.clone().unwrap_or_else(|| {
        let stem = output.file_stem().unwrap_or_default().to_string_lossy();
        output.with_file_name(format!("{stem}_{}.jpg", service.id))
    });
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
    if added == Added::Added || output != map_file {
        write_atomically(&output, edited.as_bytes())?;
    }

    println!(
        "{} × {} pixels, about {:.0} × {:.0} m of ground",
        grid.width,
        grid.height,
        f64::from(grid.width) * ground_pixel,
        f64::from(grid.height) * ground_pixel,
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
        "Photos: {} ({}) — credit them wherever the map is published.",
        service.attribution, service.licence
    );
    Ok(())
}

/// Asks `service` whether it has photos of the map, and in which CRS.
fn try_service<'a>(
    agent: &ureq::Agent,
    service: &'a Service,
    map: &maur_o::map::Map,
    anchor: &GeographicAnchor,
    map_epsg: i32,
    margin: f64,
) -> Result<Choice<'a>, Skip> {
    // The capabilities say whether the layer is still there, and may name
    // more CRSs than the service is known for. Some servers' are broken,
    // which is no reason not to ask for the photo.
    let capabilities = fetch(agent, &capabilities_url(service))
        .ok()
        .and_then(|(_, body)| Capabilities::parse(&String::from_utf8_lossy(&body)).ok());
    if let Some(capabilities) = &capabilities {
        if !capabilities.has_layers(service.layer) {
            return Err(Skip::Failed(fail(
                6,
                format!("{} no longer serves {}", service.url, service.layer),
            )));
        }
    }
    let offers = |epsg: i32| {
        service.crs.contains(&epsg) || capabilities.as_ref().is_some_and(|c| c.offers_epsg(epsg))
    };
    let epsg = request_crs(offers, map_epsg).ok_or_else(|| {
        let mut offered: Vec<String> = service.crs.iter().map(|c| format!("EPSG:{c}")).collect();
        if let Some(capabilities) = &capabilities {
            offered.extend(
                capabilities
                    .crs
                    .iter()
                    .filter(|c| c.starts_with("EPSG:"))
                    .cloned(),
            );
        }
        offered.dedup();
        Skip::NoCrs(format!(
            "the map is in EPSG:{map_epsg}, and the service offers neither it nor \
             EPSG:3857/4326, only {}",
            offered.join(", ")
        ))
    })?;
    let footprint = footprint(map, anchor, epsg, margin).map_err(|e| Skip::Failed(fail(3, e)))?;

    // Is the map where the photos are? Blank is where they are not.
    let (pw, ph) = pixel_size(epsg, map_epsg, anchor.lat, service.ground_resolution);
    let coarser = ((footprint.width() / pw).max(footprint.height() / ph) / PROBE_SIZE)
        .ceil()
        .max(1.0);
    let probe = Grid::covering(&footprint, (pw * coarser, ph * coarser));
    let url = get_map_url(
        service,
        epsg,
        &probe.bounds(),
        probe.width,
        probe.height,
        "image/png",
        true,
    );
    let probe_image = fetch_image(agent, &url, None)
        .map_err(Skip::Failed)?
        .to_rgba8();
    let photographed = probe_image.pixels().filter(|p| !is_blank(p.0)).count();
    if photographed == 0 {
        return Err(Skip::NotCovered);
    }
    Ok(Choice {
        service,
        epsg,
        footprint,
        coverage: photographed as f64 / (probe_image.width() * probe_image.height()).max(1) as f64,
    })
}

/// No service the tool knows of covers the map: a warning, exit code 5.
fn not_covered(map_file: &Path, anchor: &GeographicAnchor) -> Failure {
    (
        ExitCode::from(5),
        format!(
            "Warning: none of the orthophoto services this tool knows of has photos of {} \
             (at {:.4}, {:.4}); --list-services lists them. Nothing was written.",
            map_file.display(),
            anchor.lat,
            anchor.lon
        ),
    )
}

/// Every service worth asking was passed over: why, and the exit code that
/// says so (5 if none had photos there, else 3 if one had them in no usable
/// CRS, else 6).
fn nothing_usable(
    map_file: &Path,
    anchor: &GeographicAnchor,
    skipped: Vec<(&Service, Skip)>,
) -> Failure {
    if skipped
        .iter()
        .all(|(_, skip)| matches!(skip, Skip::NotCovered))
    {
        return not_covered(map_file, anchor);
    }
    let code = if skipped
        .iter()
        .any(|(_, skip)| matches!(skip, Skip::NoCrs(_)))
    {
        3
    } else {
        6
    };
    let reasons: Vec<String> = skipped
        .into_iter()
        .map(|(service, skip)| {
            let why = match skip {
                Skip::NotCovered => "no photos of the map's ground".to_owned(),
                Skip::NoCrs(message) => message,
                Skip::Failed((_, message)) => message.trim_start_matches("Error: ").to_owned(),
            };
            format!("\n  {} ({}): {why}", service.id, service.area)
        })
        .collect();
    (
        ExitCode::from(code),
        format!("Error: no service could be used:{}", reasons.concat()),
    )
}

/// GETs `url`: its content type and body, asking up to [`ATTEMPTS`] times,
/// since some servers fail a request now and then. A failure to connect,
/// an HTTP error status and a body too large are all exit code 6.
fn fetch(agent: &ureq::Agent, url: &str) -> Result<(String, Vec<u8>), Failure> {
    let mut result = fetch_once(agent, url);
    for _ in 1..ATTEMPTS {
        if result.is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_secs(2));
        result = fetch_once(agent, url);
    }
    result
}

/// One attempt at [`fetch`].
fn fetch_once(agent: &ureq::Agent, url: &str) -> Result<(String, Vec<u8>), Failure> {
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
    format: Option<ImageFormat>,
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
    match format {
        Some(format) => image::load_from_memory_with_format(&body, format),
        // A server that cannot do what was asked may send another format.
        None => image::load_from_memory(&body),
    }
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
                let mut result = fetch_image(agent, &url, Some(ImageFormat::Jpeg));
                for _ in 1..ATTEMPTS {
                    if result.is_ok() {
                        break;
                    }
                    std::thread::sleep(Duration::from_secs(2));
                    result = fetch_image(agent, &url, Some(ImageFormat::Jpeg));
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
