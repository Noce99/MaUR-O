//! Downloads the aerial orthophoto of a map's ground and adds it to the map
//! as a georeferenced template, behind everything else.
//!
//! ```text
//! mauro ortho [--service ID] [--pixel-size M] [--margin M]
//!                [--image FILE] <map-file> [output-map-file]
//! mauro ortho --service google [--zoom LEVEL] [--margin M]
//!                [--image FILE] <map-file> [output-map-file]
//! mauro ortho --list-services
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
//! `--service google` downloads Google's satellite tiles instead, which
//! cover every map, wherever it is. It is never picked by itself: Google's
//! terms do not license its imagery for download, so it is for whoever
//! takes that on. Its detail is a zoom level, `--zoom` (19 by default,
//! about 0.3 m a pixel at the equator, finer towards the poles), not a
//! pixel size, and its image is always in Web Mercator (EPSG:3857), whole
//! 256-pixel tiles, so a little more than the map's box.
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
//! default. An image may have at most 300 megapixels (about 3.5 × 3.5 km at
//! 0.2 m) and 65 000 pixels a side, so a larger area is downloaded as
//! several images, each under those limits, in as few as it takes, cut on
//! whole tiles: `forest_google_1.jpg`, `forest_google_2.jpg`, ... row by row
//! from the north-west corner, each with its world file and each its own
//! template. Only one is ever held in memory.
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
    xyz_ground_resolution, Capabilities, GeographicAnchor, Grid, ProjectedBox, Service, Tile,
    TileRange, WEB_MERCATOR, XYZ_TILE_SIZE,
};
use maur_o::orthophoto_services::{GOOGLE, SERVICES};
use maur_o::progress::Progress;
use maur_o::stats::coordinate_bounds;
use maur_o::xml_reader::read_xml_map_str;

/// The most pixels one stitched image may have: 900 MB of RGB in memory.
/// A larger area is downloaded as several images.
const MAX_PIXELS: u64 = 300_000_000;

/// The longest side one image may have: under JPEG's 65 535.
const MAX_SIDE: u32 = 65_000;

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

/// How long to wait before asking a WMS again.
const RETRY_DELAY: Duration = Duration::from_secs(2);

/// Who Google's tiles are asked for as: a browser, as Google's own maps
/// ask for them, which it turns away less often than a tool by name. The
/// WMS services are asked as `maur-o/<version>`.
const BROWSER_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36";

/// How long to wait before asking Google again: it turns away a burst of
/// requests for a while, and asking again at once only prolongs it.
const GOOGLE_RETRY_DELAY: Duration = Duration::from_secs(4);

/// The largest response read: far more than a 2000-pixel tile.
const MAX_RESPONSE: u64 = 64 << 20;

#[derive(Parser)]
#[command(
    about = "Adds a map's aerial orthophoto to it as a georeferenced template",
    long_about = "Downloads the aerial orthophoto of a map's ground from a national or regional \
             mapping agency and adds it to the map as a georeferenced template."
)]
pub struct Args {
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

    /// The zoom level of Google's tiles, with `--service google` only: each
    /// level up halves the pixel, 19 is about 0.3 m. Defaults to 19.
    #[arg(long, value_name = "LEVEL")]
    zoom: Option<u8>,

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
    println!(
        "{:<34} {:>6}  {:<11} {} (only with --service {}, --zoom for detail)",
        GOOGLE.id, "zoom", "varies", GOOGLE.area, GOOGLE.id
    );
}

/// A photo to download, and what to say about it.
struct Plan {
    /// Where the whole photo is, in `epsg`.
    grid: Grid,
    /// Its tiles: each where it goes in `grid`, and its URL.
    tiles: Vec<(Tile, String)>,
    /// The tiles' side: an image of `grid` is only ever cut on multiples
    /// of it, so every tile falls in one image.
    unit: u32,
    /// What the tiles are, where the service is sure to send that.
    format: Option<ImageFormat>,
    /// How long to wait before asking for a tile again.
    delay: Duration,
    epsg: i32,
    /// The service's id, for the image's name.
    id: &'static str,
    attribution: &'static str,
    licence: &'static str,
    /// Its pixel's side on the ground, in meters.
    ground_pixel: f64,
}

fn run(args: Args) -> Result<(), Failure> {
    if args.list_services {
        list_services();
        return Ok(());
    }
    let map_file = args.map_file.clone().expect("clap requires it");
    let google = args.service.as_deref() == Some(GOOGLE.id);
    if let Some(zoom) = args.zoom {
        if !google {
            return Err(fail(1, "--zoom is for --service google only"));
        }
        if zoom > GOOGLE.max_zoom {
            return Err(fail(
                1,
                format!("--zoom must be between 0 and {}", GOOGLE.max_zoom),
            ));
        }
    }
    if google && args.pixel_size.is_some() {
        return Err(fail(
            1,
            "Google's tiles come in zoom levels: give --zoom, not --pixel-size",
        ));
    }
    let chosen = match args.service.as_deref().filter(|_| !google) {
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

    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(120)))
        .user_agent(if google {
            BROWSER_USER_AGENT
        } else {
            concat!("maur-o/", env!("CARGO_PKG_VERSION"))
        })
        .build()
        .into();
    let plan = if google {
        plan_google(
            &map,
            &anchor,
            args.zoom.unwrap_or(GOOGLE.default_zoom),
            args.margin,
        )?
    } else {
        plan_wms(&agent, chosen, &map_file, &map, &anchor, map_epsg, &args)?
    };
    let Plan {
        grid,
        epsg,
        id,
        attribution,
        licence,
        ground_pixel,
        ..
    } = plan;

    // The photo, as one image or, too large for one, as several: each
    // downloaded, written and dropped before the next.
    let image_path = args.image.clone().unwrap_or_else(|| {
        let stem = output.file_stem().unwrap_or_default().to_string_lossy();
        output.with_file_name(format!("{stem}_{id}.jpg"))
    });
    let pieces = grid.split(MAX_PIXELS, MAX_SIDE, plan.unit);
    if pieces.len() > 1 {
        println!(
            "{} × {} pixels is too large for one image: downloading it as {} images",
            grid.width,
            grid.height,
            pieces.len()
        );
    }
    let mut edited = xml.clone();
    let (mut added, mut already_there) = (0, 0);
    for (n, piece) in pieces.iter().enumerate() {
        let piece_grid = grid.sub_grid(piece);
        let tiles = plan
            .tiles
            .iter()
            .filter(|(tile, _)| {
                (piece.x..piece.x + piece.width).contains(&tile.x)
                    && (piece.y..piece.y + piece.height).contains(&tile.y)
            })
            .map(|(tile, url)| {
                let tile = Tile {
                    x: tile.x - piece.x,
                    y: tile.y - piece.y,
                    ..*tile
                };
                (tile, url.clone())
            })
            .collect();
        let label = if pieces.len() > 1 {
            format!("Image {}/{}", n + 1, pieces.len())
        } else {
            "Downloading".to_owned()
        };
        let photo = download(&agent, &piece_grid, tiles, plan.format, plan.delay, &label)?;
        let path = if pieces.len() > 1 {
            numbered(&image_path, n + 1, pieces.len())
        } else {
            image_path.clone()
        };
        image_writer(&photo, &path)?;
        drop(photo);
        let world_file = world_file_path(&path);
        std::fs::write(&world_file, piece_grid.world_file())
            .map_err(|e| fail(4, format!("cannot write {}: {e}", world_file.display())))?;
        println!("Wrote {} and {}", path.display(), world_file.display());

        let absolute = std::path::absolute(&path)
            .map_err(|e| fail(4, format!("cannot place {}: {e}", path.display())))?;
        let name = path
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
        let outcome;
        (edited, outcome) = add_image_template(&edited, &template).map_err(|e| fail(2, e))?;
        match outcome {
            Added::Added => added += 1,
            Added::AlreadyThere => already_there += 1,
        }
    }

    // The map.
    if added > 0 || output != map_file {
        write_atomically(&output, edited.as_bytes())?;
    }
    println!(
        "{} × {} pixels, about {:.0} × {:.0} m of ground",
        grid.width,
        grid.height,
        f64::from(grid.width) * ground_pixel,
        f64::from(grid.height) * ground_pixel,
    );
    let (them, templates, images) = if pieces.len() > 1 {
        ("them", "templates", "images were")
    } else {
        ("it", "a template", "image was")
    };
    match (added, already_there) {
        (_, 0) => println!("Added {them} to {} as {templates}", output.display()),
        (0, _) => println!(
            "{} already had {them} as {templates}; only the {images} replaced",
            output.display()
        ),
        (added, _) => println!(
            "Added {added} of them to {} as templates; it already had the other {already_there}",
            output.display()
        ),
    }
    println!("Photos: {attribution} ({licence}) — credit them wherever the map is published.");
    Ok(())
}

/// `path` with `_<n>` before its extension, `n` zero-padded to as many
/// digits as `count` has: `forest_google.jpg` → `forest_google_07.jpg`.
fn numbered(path: &Path, n: usize, count: usize) -> PathBuf {
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let digits = count.to_string().len();
    let name = match path.extension() {
        Some(ext) => format!("{stem}_{n:0digits$}.{}", ext.to_string_lossy()),
        None => format!("{stem}_{n:0digits$}"),
    };
    path.with_file_name(name)
}

/// The photo from the first WMS with photos of the whole map (or most of
/// it): `chosen`, where one was picked.
fn plan_wms(
    agent: &ureq::Agent,
    chosen: Option<&'static Service>,
    map_file: &Path,
    map: &maur_o::map::Map,
    anchor: &GeographicAnchor,
    map_epsg: i32,
    args: &Args,
) -> Result<Plan, Failure> {
    let (margin, pixel_size_arg) = (args.margin, args.pixel_size);
    let candidates: Vec<&Service> = match chosen {
        Some(service) => vec![service],
        None => SERVICES
            .iter()
            .filter(|s| s.covers(anchor.lat, anchor.lon))
            .collect(),
    };
    if candidates.is_empty() {
        return Err(not_covered(map_file, anchor));
    }

    // The first service with photos of the whole map; failing that, the one
    // with photos of most of it.
    let mut best: Option<Choice> = None;
    let mut skipped: Vec<(&Service, Skip)> = Vec::new();
    for &service in &candidates {
        match try_service(agent, service, map, anchor, map_epsg, margin) {
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
        return Err(nothing_usable(map_file, anchor, skipped));
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
    let ground_pixel = pixel_size_arg.unwrap_or(service.ground_resolution);
    let grid = Grid::covering(
        &footprint,
        pixel_size(epsg, map_epsg, anchor.lat, ground_pixel),
    );
    println!(
        "Downloading {} ({}, {} m a pixel) in EPSG:{epsg}",
        service.area, service.years, ground_pixel
    );
    let tiles = grid
        .tiles(service.max_tile_size)
        .into_iter()
        .map(|tile| {
            let url = get_map_url(
                service,
                epsg,
                &grid.tile_box(&tile),
                tile.width,
                tile.height,
                "image/jpeg",
                false,
            );
            (tile, url)
        })
        .collect();
    Ok(Plan {
        grid,
        tiles,
        unit: service.max_tile_size,
        format: Some(ImageFormat::Jpeg),
        delay: RETRY_DELAY,
        epsg,
        id: service.id,
        attribution: service.attribution,
        licence: service.licence,
        ground_pixel,
    })
}

/// The photo from Google's satellite tiles at `zoom`, in Web Mercator.
fn plan_google(
    map: &maur_o::map::Map,
    anchor: &GeographicAnchor,
    zoom: u8,
    margin: f64,
) -> Result<Plan, Failure> {
    let area = footprint(map, anchor, WEB_MERCATOR, margin).map_err(|e| fail(3, e))?;
    let range = TileRange::covering(&area, zoom);
    let grid = range.grid();
    let ground_pixel = xyz_ground_resolution(zoom, anchor.lat);
    println!(
        "Downloading {} tiles of {} at zoom {zoom} (about {ground_pixel:.2} m a pixel) in EPSG:{WEB_MERCATOR}",
        range.count(),
        GOOGLE.area
    );
    let tiles: Vec<(Tile, String)> = range
        .tiles()
        .into_iter()
        .map(|(x, y, tile)| (tile, GOOGLE.tile_url(x, y, zoom)))
        .collect();
    Ok(Plan {
        grid,
        tiles,
        unit: XYZ_TILE_SIZE,
        format: None,
        delay: GOOGLE_RETRY_DELAY,
        epsg: WEB_MERCATOR,
        id: GOOGLE.id,
        attribution: GOOGLE.attribution,
        licence: GOOGLE.licence,
        ground_pixel,
    })
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
    let capabilities = fetch(agent, &capabilities_url(service), RETRY_DELAY)
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
    let probe_image = fetch_image(agent, &url, None, RETRY_DELAY)
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
/// `delay` apart, since some servers fail a request now and then. A failure
/// to connect, an HTTP error status and a body too large are all exit code 6.
fn fetch(agent: &ureq::Agent, url: &str, delay: Duration) -> Result<(String, Vec<u8>), Failure> {
    let mut result = fetch_once(agent, url);
    for _ in 1..ATTEMPTS {
        if result.is_ok() {
            break;
        }
        std::thread::sleep(delay);
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
    delay: Duration,
) -> Result<image::DynamicImage, Failure> {
    let (content_type, body) = fetch(agent, url, delay)?;
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

/// The whole grid's photo from `tiles` (each where it goes in the grid, and
/// its URL), downloaded [`WORKERS`] at a time, `delay` between attempts,
/// its progress bar labelled `label`.
fn download(
    agent: &ureq::Agent,
    grid: &Grid,
    tiles: Vec<(Tile, String)>,
    format: Option<ImageFormat>,
    delay: Duration,
    label: &str,
) -> Result<RgbImage, Failure> {
    let photo = Mutex::new(RgbImage::new(grid.width, grid.height));
    let progress = Mutex::new(Progress::new(label, tiles.len()));
    let next = AtomicUsize::new(0);
    let failure: Mutex<Option<Failure>> = Mutex::new(None);

    std::thread::scope(|scope| {
        for _ in 0..WORKERS.min(tiles.len()) {
            scope.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= tiles.len() || failure.lock().unwrap().is_some() {
                    return;
                }
                let (tile, url) = &tiles[i];
                let mut result = fetch_image(agent, url, format, delay);
                for _ in 1..ATTEMPTS {
                    if result.is_ok() {
                        break;
                    }
                    std::thread::sleep(delay);
                    result = fetch_image(agent, url, format, delay);
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

/// What an exit code of this subcommand means, as the module's own
/// documentation lists them.
pub fn exit_meaning(code: i32) -> Option<&'static str> {
    Some(match code {
        0 => "success",
        1 => "usage error",
        2 => "the map could not be read",
        3 => "the map is not georeferenced, or no service covering it offers a CRS to place the photo in",
        4 => "the image or the map could not be written",
        5 => "no service has photos of the map's ground",
        6 => "the service could not be reached, or answered with something other than a photo",
        _ => return None,
    })
}

/// Runs `mauro ortho`.
pub fn main(args: Args) -> ExitCode {
    match run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err((code, message)) => {
            if !message.is_empty() {
                eprintln!("{message}");
            }
            code
        }
    }
}
