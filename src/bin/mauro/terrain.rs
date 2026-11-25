//! Downloads the terrain model (DTM) of a map's ground as an elevation
//! GeoTIFF, in the map's own CRS.
//!
//! ```text
//! mauro terrain [--service ID] [--pixel-size M] [--margin M]
//!               [--output FILE] <map-file>
//! mauro terrain --list-services
//! ```
//!
//! The elevations come from open servers that need no key: the region of
//! Emilia-Romagna's LiDAR surveys (0.5 m), its 5 m DTM of the whole region,
//! and INGV's TINITALY (10 m, all of Italy). `--list-services` lists them.
//! The service is picked by where the map is: the first, in that list's
//! order, with elevations of the whole map (finer before coarser, newer
//! before older); failing that, the one with elevations of most of it, with
//! a warning. `--service` picks one by its id instead. A map that no
//! service has elevations of is reported with a warning, and nothing is
//! written.
//!
//! The map may be an OpenOrienteering Mapper `.omap`/`.xmap` file or an
//! OCAD `.ocd` one, georeferenced in a projected CRS with an EPSG code. The
//! area downloaded is the box around the map's objects, `--margin` meters
//! (50 by default) wider on every side, north-up in the map's CRS: the
//! servers reproject to it themselves, so the GeoTIFF lies on the map as it
//! is, and `mauro dem-eval` can score `mauro dem`'s output against it.
//!
//! The GeoTIFF is one band of 32-bit float elevations in meters, -9999 (its
//! `GDAL_NODATA`) where the service has none, deflate-compressed, written
//! beside the map and named after it with the service's id
//! (`forest.omap` → `forest_it-er-lidar-2023-24.tif`) unless `--output`
//! says otherwise.
//!
//! `--pixel-size` trades detail for size: the model's own resolution is the
//! default. The whole raster is held in memory, so it may have at most 80
//! megapixels (about 4.5 × 4.5 km at 0.5 m); a larger one is turned away
//! with the pixel size that would fit.
//!
//! The elevations belong to whoever serves them: credit them (the tool
//! prints how) wherever something made from them is published.
//!
//! Exit codes: 0 success, 1 usage error (an area too large to download at
//! that pixel size included), 2 the map could not be read, 3 the map is
//! not georeferenced in a projected CRS with an EPSG code, or has no
//! objects, 4 the GeoTIFF could not be written, 5 no service has elevations
//! of the map's ground, 6 the service could not be reached or answered
//! with something other than elevations.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use clap::Parser;

use maur_o::dem_geo_tiff_evaluator::read_bytes;
use maur_o::ocd::{is_ocd_file, ocd_to_omap_xml};
use maur_o::orthophoto::{geographic_anchor, map_footprint, Grid, ProjectedBox, Tile};
use maur_o::progress::Progress;
use maur_o::terrain::{
    coverage_url, error_text, is_axis_label_error, is_outside_error, request_area, tile_size,
    ElevationGrid, Protocol, TerrainService, WCS_AXES,
};
use maur_o::terrain_services::TERRAIN_SERVICES;
use maur_o::xml_reader::read_xml_map_str;

/// The most pixels the raster may have: 320 MB of floats in memory.
const MAX_PIXELS: u64 = 80_000_000;

/// The longest side of the low-resolution raster coverage is checked on.
const PROBE_SIZE: f64 = 128.0;

/// The share of the map's ground a service must have elevations of to be
/// used without looking further: not all of it, since a DTM may have none
/// over water.
const FULL_COVERAGE: f64 = 0.98;

/// The share of the map's ground a finer service passed over must have
/// elevations of to be pointed out: less is mostly the coarse probe
/// blurring a survey's edge over the map.
const NOTEWORTHY_COVERAGE: f64 = 0.1;

/// How many tiles are downloaded at once.
const WORKERS: usize = 4;

/// How many times a request is sent before giving up, where the server
/// failed rather than refused.
const ATTEMPTS: usize = 3;

/// How long to wait before asking again.
const RETRY_DELAY: Duration = Duration::from_secs(2);

/// The largest response read: far more than a 2000-pixel float tile.
const MAX_RESPONSE: u64 = 64 << 20;

#[derive(Parser)]
#[command(
    about = "Downloads the terrain model (DTM) of a map's ground as an elevation GeoTIFF",
    long_about = "Downloads the terrain model (DTM) of a map's ground from an open elevation \
             server (LiDAR surveys, a national DTM) as an elevation GeoTIFF in the map's own CRS."
)]
pub struct Args {
    /// The map (.omap, .xmap or .ocd) whose ground to download.
    #[arg(required_unless_present = "list_services")]
    map_file: Option<PathBuf>,

    /// Where to write the GeoTIFF. Defaults to the map's name with
    /// `_<service id>.tif`, beside it.
    #[arg(long, short, value_name = "FILE")]
    output: Option<PathBuf>,

    /// The service to download from, by id (see --list-services), instead of
    /// the best one for where the map is.
    #[arg(long, value_name = "ID")]
    service: Option<String>,

    /// Lists the services, best first, and exits.
    #[arg(long, alias = "list_services")]
    list_services: bool,

    /// The raster's ground resolution, in meters per pixel. Defaults to the
    /// model's own.
    #[arg(long, value_name = "METERS", alias = "pixel_size")]
    pixel_size: Option<f64>,

    /// How far beyond the map's objects to download, in meters on the ground.
    #[arg(long, value_name = "METERS", default_value_t = 50.0)]
    margin: f64,
}

/// A failure: the exit code and what to say.
type Failure = (ExitCode, String);

fn fail(code: u8, message: impl std::fmt::Display) -> Failure {
    (ExitCode::from(code), format!("Error: {message}"))
}

/// A service that has elevations of the map, and how to ask it for them.
struct Choice<'a> {
    service: &'a TerrainService,
    /// The axis labels its subsets are named by (a WCS's; ignored by ArcGIS).
    axes: (&'static str, &'static str),
    /// The share of the footprint it has elevations of.
    coverage: f64,
}

/// Why a service was passed over.
enum Skip {
    /// It has no elevations there.
    NotCovered,
    /// It could not be asked.
    Failed(Failure),
}

/// What a server answered.
enum Answer {
    /// A raster: its bytes.
    Tiff(Vec<u8>),
    /// Something else: what its error says.
    Refused(String),
}

fn list_services() {
    println!("{:<22} {:>6}  {:<12} Area", "Id", "Pixel", "Years");
    for service in TERRAIN_SERVICES {
        println!(
            "{:<22} {:>5}m  {:<12} {}",
            service.id, service.ground_resolution, service.years, service.area
        );
    }
}

fn run(args: Args) -> Result<(), Failure> {
    if args.list_services {
        list_services();
        return Ok(());
    }
    let map_file = args.map_file.clone().expect("clap requires it");
    let chosen = match args.service.as_deref() {
        Some(id) => Some(
            TERRAIN_SERVICES
                .iter()
                .find(|s| s.id == id)
                .ok_or_else(|| {
                    fail(
                        1,
                        format!("there is no service {id}; --list-services lists them"),
                    )
                })?,
        ),
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

    // The map: where it is, and in which CRS.
    let cannot_read =
        |e: &dyn std::fmt::Display| fail(2, format!("cannot read {}: {e}", map_file.display()));
    let bytes = std::fs::read(&map_file).map_err(|e| cannot_read(&e))?;
    let xml = if is_ocd_file(&bytes) {
        ocd_to_omap_xml(&bytes).map_err(|e| cannot_read(&e))?.xml
    } else {
        String::from_utf8(bytes).map_err(|e| cannot_read(&e))?
    };
    let (map, _) = read_xml_map_str(&xml).map_err(|e| cannot_read(&e))?;
    let epsg = map.georeferencing.as_ref().map_or(0, |g| g.epsg);
    if epsg == 0 {
        return Err(fail(
            3,
            "the map is not georeferenced in a projected CRS with an EPSG code",
        ));
    }
    let epsg_tag = u16::try_from(epsg)
        .map_err(|_| fail(3, format!("EPSG:{epsg} cannot be written in a GeoTIFF key")))?;
    let footprint = map_footprint(&map, args.margin).map_err(|e| fail(3, e))?;
    let anchor = geographic_anchor(&xml);
    // Too large at a pixel size already known is turned away before any
    // server is asked.
    if let Some(pixel) = args.pixel_size.or(chosen.map(|s| s.ground_resolution)) {
        sized_grid(&footprint, pixel)?;
    }

    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(180)))
        .http_status_as_error(false)
        .user_agent(concat!("maur-o/", env!("CARGO_PKG_VERSION")))
        .build()
        .into();

    // The services worth asking: where the map's place on the earth is
    // known, those whose box holds it; otherwise every one, the probe
    // telling them apart.
    let candidates: Vec<&TerrainService> = match chosen {
        Some(service) => vec![service],
        None => TERRAIN_SERVICES
            .iter()
            .filter(|s| anchor.is_none_or(|a| s.covers(a.lat, a.lon)))
            .collect(),
    };
    if candidates.is_empty() {
        return Err(not_covered(&map_file, chosen));
    }
    let mut best: Option<Choice> = None;
    let mut skipped: Vec<(&TerrainService, Skip)> = Vec::new();
    // The services with elevations of part of the map, and how much.
    let mut partial: Vec<(&TerrainService, f64)> = Vec::new();
    for &service in &candidates {
        match probe(&agent, service, epsg, &footprint) {
            Ok(choice) => {
                let full = choice.coverage >= FULL_COVERAGE;
                if !full {
                    partial.push((service, choice.coverage));
                }
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
    let Some(Choice {
        service,
        axes,
        coverage,
    }) = best
    else {
        return Err(nothing_usable(&map_file, chosen, skipped));
    };
    for (service, skip) in &skipped {
        if let Skip::Failed((_, message)) = skip {
            eprintln!(
                "Warning: skipped {} ({}): {}",
                service.id,
                service.area,
                message.trim_start_matches("Error: ")
            );
        }
    }
    // Finer models passed over for having only part of the map: worth
    // knowing of, to pick one with --service where that part is the one
    // that matters.
    for (other, share) in &partial {
        if other.id != service.id
            && other.ground_resolution < service.ground_resolution
            && *share >= NOTEWORTHY_COVERAGE
        {
            println!(
                "Note: {} has {} m elevations of only about {:.0}% of the map's ground; \
                 --service {} downloads them anyway",
                other.id,
                other.ground_resolution,
                share * 100.0,
                other.id
            );
        }
    }
    if coverage < FULL_COVERAGE {
        eprintln!(
            "Warning: {} has elevations of only about {:.0}% of the map's ground; the rest is no data",
            service.id,
            coverage * 100.0
        );
    }

    // The raster itself, tile by tile.
    let pixel = args.pixel_size.unwrap_or(service.ground_resolution);
    let grid = sized_grid(&footprint, pixel)?;
    println!(
        "Downloading {} ({}, {pixel} m a pixel) in EPSG:{epsg}",
        service.area, service.years
    );
    let dem = download(&agent, service, axes, epsg, grid)?;
    let Some((low, high)) = dem.range() else {
        // The coarse probe saw some, the full raster none: a sliver of an
        // edge, blurred over in the probe.
        return Err(not_covered(&map_file, Some(service)));
    };

    let output = args.output.clone().unwrap_or_else(|| {
        let stem = map_file.file_stem().unwrap_or_default().to_string_lossy();
        map_file.with_file_name(format!("{stem}_{}.tif", service.id))
    });
    write_atomically(&output, |file| dem.write_geotiff(file, epsg_tag))?;
    println!("Wrote {}", output.display());
    println!(
        "{} × {} pixels, about {:.0} × {:.0} m of ground",
        grid.width,
        grid.height,
        f64::from(grid.width) * pixel,
        f64::from(grid.height) * pixel,
    );
    println!(
        "Elevations from {low:.1} to {high:.1} m, of {:.1}% of the pixels",
        dem.coverage() * 100.0
    );
    println!(
        "Elevations: {} ({}) — credit them wherever what is made from them is published.",
        service.attribution, service.licence
    );
    Ok(())
}

/// The grid of `pixel`-meter pixels covering `footprint`, or, where it
/// would have more than [`MAX_PIXELS`], a usage error naming the pixel size
/// that would fit.
fn sized_grid(footprint: &ProjectedBox, pixel: f64) -> Result<Grid, Failure> {
    let grid = Grid::covering(footprint, (pixel, pixel));
    if grid.pixel_count() <= MAX_PIXELS {
        return Ok(grid);
    }
    let fits = pixel * (grid.pixel_count() as f64 / MAX_PIXELS as f64).sqrt();
    Err(fail(
        1,
        format!(
            "{} × {} pixels at {pixel} m is more than one raster may have; \
             give --pixel-size {} or more",
            grid.width,
            grid.height,
            (fits * 100.0).ceil() / 100.0
        ),
    ))
}

/// Asks `service` for a coarse raster of the map's ground: whether it has
/// elevations there, of how much of it, and (a WCS) which axis labels it
/// wants.
fn probe<'a>(
    agent: &ureq::Agent,
    service: &'a TerrainService,
    epsg: i32,
    footprint: &ProjectedBox,
) -> Result<Choice<'a>, Skip> {
    let pixel =
        (footprint.width().max(footprint.height()) / PROBE_SIZE).max(service.ground_resolution);
    let grid = Grid::covering(footprint, (pixel, pixel));
    let whole = Tile {
        x: 0,
        y: 0,
        width: grid.width,
        height: grid.height,
    };
    let axes_to_try: &[(&'static str, &'static str)] = match service.protocol {
        Protocol::ArcGisImageServer => &WCS_AXES[..1],
        Protocol::Wcs { .. } => WCS_AXES,
    };
    let mut last = String::new();
    for &axes in axes_to_try {
        let (area, width, height) = request_area(service, &grid.bounds(), grid.width, grid.height);
        let url = coverage_url(service, epsg, &area, width, height, axes);
        match fetch(agent, &url).map_err(Skip::Failed)? {
            Answer::Tiff(body) => {
                let raster = read_bytes(&body, &url)
                    .map_err(|e| Skip::Failed(fail(6, format!("unreadable elevations: {e}"))))?;
                let mut dem = ElevationGrid::new(grid);
                dem.paste(&whole, &raster);
                if dem.coverage() == 0.0 {
                    return Err(Skip::NotCovered);
                }
                return Ok(Choice {
                    service,
                    axes,
                    coverage: dem.coverage(),
                });
            }
            Answer::Refused(text) if is_outside_error(&text) => return Err(Skip::NotCovered),
            Answer::Refused(text) if is_axis_label_error(&text) => last = text,
            Answer::Refused(text) => {
                return Err(Skip::Failed(fail(
                    6,
                    format!("the service answered with an error: {text}"),
                )))
            }
        }
    }
    Err(Skip::Failed(fail(
        6,
        format!("the service names EPSG:{epsg}'s axes by none of the labels tried: {last}"),
    )))
}

/// The whole grid's elevations from `service`, tile by tile,
/// [`WORKERS`] at a time.
fn download(
    agent: &ureq::Agent,
    service: &TerrainService,
    axes: (&'static str, &'static str),
    epsg: i32,
    grid: Grid,
) -> Result<ElevationGrid, Failure> {
    let tiles = grid.tiles(tile_size(service, grid.pixel_width));
    let dem = Mutex::new(ElevationGrid::new(grid));
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
                let (area, width, height) =
                    request_area(service, &grid.tile_box(tile), tile.width, tile.height);
                let url = coverage_url(service, epsg, &area, width, height, axes);
                let raster = match fetch(agent, &url) {
                    Ok(Answer::Tiff(body)) => read_bytes(&body, &url)
                        .map_err(|e| fail(6, format!("unreadable elevations: {e}"))),
                    // A tile wholly outside a WCS's coverage: no data there.
                    Ok(Answer::Refused(text)) if is_outside_error(&text) => {
                        progress.lock().unwrap().tick();
                        continue;
                    }
                    Ok(Answer::Refused(text)) => Err(fail(
                        6,
                        format!("the service answered with an error: {text}"),
                    )),
                    Err(e) => Err(e),
                };
                match raster {
                    Ok(raster) => {
                        dem.lock().unwrap().paste(tile, &raster);
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
    Ok(dem.into_inner().unwrap())
}

/// No service covers the map (or `only`, the one picked, does not): a
/// warning, exit code 5.
fn not_covered(map_file: &Path, only: Option<&TerrainService>) -> Failure {
    let message = match only {
        Some(service) => format!(
            "Warning: {} ({}) has no elevations of {}. Nothing was written.",
            service.id,
            service.area,
            map_file.display()
        ),
        None => format!(
            "Warning: none of the elevation services this tool knows of has elevations of {}; \
             --list-services lists them. Nothing was written.",
            map_file.display()
        ),
    };
    (ExitCode::from(5), message)
}

/// Every service worth asking was passed over: why, and the exit code that
/// says so (5 if none had elevations there, else 6).
fn nothing_usable(
    map_file: &Path,
    only: Option<&TerrainService>,
    skipped: Vec<(&TerrainService, Skip)>,
) -> Failure {
    if skipped
        .iter()
        .all(|(_, skip)| matches!(skip, Skip::NotCovered))
    {
        return not_covered(map_file, only);
    }
    let reasons: Vec<String> = skipped
        .into_iter()
        .map(|(service, skip)| {
            let why = match skip {
                Skip::NotCovered => "no elevations of the map's ground".to_owned(),
                Skip::Failed((_, message)) => message.trim_start_matches("Error: ").to_owned(),
            };
            format!("\n  {} ({}): {why}", service.id, service.area)
        })
        .collect();
    (
        ExitCode::from(6),
        format!("Error: no service could be used:{}", reasons.concat()),
    )
}

/// GETs `url`: a TIFF, or what the server's error says. A failure to
/// connect, or a server error (5xx), is asked again up to [`ATTEMPTS`]
/// times, [`RETRY_DELAY`] apart, since some servers fail a request now and
/// then; one that still fails is exit code 6. A refusal is not asked again:
/// it would only be refused again.
fn fetch(agent: &ureq::Agent, url: &str) -> Result<Answer, Failure> {
    let mut result = fetch_once(agent, url);
    for _ in 1..ATTEMPTS {
        if result.is_ok() {
            break;
        }
        std::thread::sleep(RETRY_DELAY);
        result = fetch_once(agent, url);
    }
    result
}

/// One attempt at [`fetch`].
fn fetch_once(agent: &ureq::Agent, url: &str) -> Result<Answer, Failure> {
    let mut response = agent
        .get(url)
        .call()
        .map_err(|e| fail(6, format!("{url}: {e}")))?;
    let status = response.status();
    if status.is_server_error() {
        return Err(fail(6, format!("{url}: the server answered {status}")));
    }
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
    let is_tiff = body.starts_with(b"II*\0") || body.starts_with(b"MM\0*");
    Ok(
        if status.is_success() && (is_tiff || content_type.contains("tiff")) {
            Answer::Tiff(body)
        } else {
            Answer::Refused(error_text(&body))
        },
    )
}

/// Writes `path` all at once, by `write`: to a temporary file beside it,
/// then renamed over it, so a failure halfway never leaves half a raster.
fn write_atomically(
    path: &Path,
    write: impl FnOnce(&mut std::io::BufWriter<std::fs::File>) -> Result<(), String>,
) -> Result<(), Failure> {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let temporary = path.with_file_name(format!(".{name}.terrain.tmp"));
    let written = std::fs::File::create(&temporary)
        .map_err(|e| e.to_string())
        .and_then(|file| {
            let mut out = std::io::BufWriter::new(file);
            write(&mut out)?;
            out.into_inner()
                .map_err(|e| e.to_string())?
                .sync_all()
                .map_err(|e| e.to_string())
        })
        .and_then(|()| std::fs::rename(&temporary, path).map_err(|e| e.to_string()));
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
        1 => "usage error, or an area too large to download at that pixel size",
        2 => "the map could not be read",
        3 => "the map is not georeferenced in a projected CRS with an EPSG code, or has no objects",
        4 => "the GeoTIFF could not be written",
        5 => "no service has elevations of the map's ground",
        6 => "the service could not be reached, or answered with something other than elevations",
        _ => return None,
    })
}

/// Runs `mauro terrain`.
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
