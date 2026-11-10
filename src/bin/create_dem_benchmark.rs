//! Builds a DEM benchmark: random places in Sweden, each with an
//! orienteering contour map made from its lidar and the ground model the
//! same lidar gives, for `dem_benchmark` to run `contours_to_raster` on
//! and score.
//!
//! ```text
//! create_dem_benchmark --pullauta <path> [--samples N] [--seed S] [--size M]
//!     [--cache <dir>] [--env-file <path>] [--min-contours N] [--min-relief M]
//!     [--min-density D] [--max-year-gap Y] [--max-attempts N]
//!     [--at LAT,LON]... <output-dir>
//! ```
//!
//! For each sample it draws a point uniformly over Sweden's mainland (from
//! `--seed`, so the same seed draws the same places), and from
//! Lantmäteriet's STAC API:
//!
//! 1. finds the point cloud tile (Laserdata Skog, 2.5 km, COPC `.laz`)
//!    there, and places a `--size`-meter box (1000 by default) inside it;
//! 2. finds the ground model tile (Markhöjdmodell, 1 m GeoTIFF) holding the
//!    box, and checks both were measured within `--max-year-gap` years;
//! 3. downloads the point cloud and runs Karttapullautin (`--pullauta`) on
//!    it, for the contours a mapper would draw;
//! 4. keeps the place only if the box holds at least `--min-contours`
//!    contour lines spanning `--min-relief` meters, so flat farmland and
//!    lakes are drawn again;
//! 5. downloads the ground model and cuts the box out of it;
//! 6. imports the contours twice, with and without slope lines on
//!    depressions.
//!
//! `--at` (repeatable) gives the places instead of drawing them: one try
//! each, in order, for reproducing a sample or a place of interest.
//!
//! Places that fail a check are drawn again, up to `--max-attempts` draws in
//! all (ten per sample by default); why each was rejected is counted in
//! `info.txt`.
//!
//! The output folder (which must not exist yet, or be empty) gets:
//!
//! ```text
//! <output-dir>/
//!   info.txt                     the run's parameters, and what was rejected why
//!   sample_0001/
//!     map_depressions.omap       contours, with slope lines on depressions
//!     map_none.omap              the same contours, without slope lines
//!     levels.txt                 every line's true height (object index, meters)
//!     ground_truth.tif           the ground model, cut to the box
//!     ground_truth_smoothed.tif  the same, Gaussian-smoothed
//!     water_mask.tif             1 where the ground model is a lake or the sea
//!     sample.txt                 where it is, which tiles, what was imported
//!     preview.png                Karttapullautin's own render of the tile
//! ```
//!
//! (`maur_o::dem_archive` documents the layout, and reads it back.)
//!
//! Downloads and Karttapullautin's output are kept in `--cache`
//! (`dem_cache` by default), so a rerun or a resampled place never fetches a
//! tile twice. A ground model tile is about 250 MB.
//!
//! It needs a GeoTorget account with access to Lantmäteriet's elevation
//! data: `LANTMATERIET_USERNAME` and `LANTMATERIET_PASSWORD` from the
//! environment, or from `--env-file` (`KEY=VALUE` lines). They are only ever
//! sent to `https://*.lantmateriet.se`.
//!
//! The point clouds are not CC licensed (the ground model is CC BY 4.0):
//! keep the archive to yourself unless you have checked you may share it.
//!
//! Exit codes: 0 success, 1 usage error, 2 no credentials, 3 Karttapullautin
//! could not be run, 4 the output folder or the cache could not be written
//! (or the output folder is not empty), 5 Lantmäteriet could not be reached
//! or refused the credentials, 6 fewer samples found than asked for within
//! `--max-attempts`.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Duration;

use clap::Parser;

use maur_o::dem_archive::{
    gaussian_smooth, water_mask, FORMAT, SMOOTHING_SIGMA_M, WATER_FLATNESS_M, WATER_MIN_AREA_M2,
};
use maur_o::dem_geo_tiff_evaluator::{read_window, write, GeoRaster};
use maur_o::dxf::{self, Entity};
use maur_o::lantmateriet::{
    basic_auth, box_in_tile, covers, is_lantmateriet_url, parse_search, sample_point, search_url,
    sweref99tm, sweref99tm_inverse, Rng, StacItem, DTM_COLLECTION, LAZ_COLLECTION, SWEREF99_TM,
};
use maur_o::pullauta_import::{import, GroundBox, ImportOptions, ImportedMap};

/// How many times a request is tried before giving up: Lantmäteriet's
/// download server refuses the first requests of a session now and then.
const ATTEMPTS: u32 = 4;

/// How far (meters) the box is kept from its point cloud tile's edge, where
/// Karttapullautin's contours are least reliable.
const TILE_MARGIN: f64 = 100.0;

/// The most of the box the ground model may have no data for.
const MAX_MISSING: f64 = 0.01;

/// The map scale the samples are drawn at.
const SCALE: i32 = 10_000;

#[derive(Parser)]
#[command(
    name = "create_dem_benchmark",
    version,
    about = "Builds a DEM benchmark from Lantmäteriet lidar: random places in Sweden, each \
             with a Karttapullautin contour map and the ground model it should give back."
)]
struct Args {
    /// The folder to create the benchmark in. Must not exist yet, or be
    /// empty.
    output: PathBuf,

    /// The Karttapullautin executable (`pullauta`).
    #[arg(long)]
    pullauta: PathBuf,

    /// How many places to sample.
    #[arg(long, default_value_t = 10)]
    samples: usize,

    /// The seed of the random draw: the same seed draws the same places.
    #[arg(long, default_value_t = 1)]
    seed: u64,

    /// The side of each sample's square box, in meters.
    #[arg(long, default_value_t = 1000.0)]
    size: f64,

    /// Where downloads and Karttapullautin's output are kept between runs.
    #[arg(long, default_value = "dem_cache")]
    cache: PathBuf,

    /// A file of `KEY=VALUE` lines to read the credentials from, when they
    /// are not in the environment.
    #[arg(long)]
    env_file: Option<PathBuf>,

    /// The fewest contour lines (contours and index contours) the box must
    /// hold.
    #[arg(long, default_value_t = 20)]
    min_contours: usize,

    /// The least height range (meters) the box's contour lines must span.
    #[arg(long, default_value_t = 15.0)]
    min_relief: f64,

    /// The fewest lidar points per square meter a point cloud tile must have.
    #[arg(long, default_value_t = 0.5)]
    min_density: f64,

    /// The most years apart the point cloud and the ground model may have
    /// been measured.
    #[arg(long, default_value_t = 1)]
    max_year_gap: i32,

    /// The most places to draw in all; ten per sample by default.
    #[arg(long)]
    max_attempts: Option<usize>,

    /// A place to use instead of a random draw, as `LAT,LON` in degrees.
    /// Repeatable; each is tried once, in order.
    #[arg(long, value_parser = parse_lat_lon)]
    at: Vec<(f64, f64)>,
}

/// `LAT,LON` in degrees.
fn parse_lat_lon(text: &str) -> Result<(f64, f64), String> {
    let (lat, lon) = text
        .split_once(',')
        .ok_or_else(|| format!("{text:?} is not LAT,LON"))?;
    let parse = |v: &str| {
        v.trim()
            .parse::<f64>()
            .map_err(|_| format!("{text:?} is not LAT,LON"))
    };
    Ok((parse(lat)?, parse(lon)?))
}

/// A failure that ends the run, with its exit code.
struct Failure {
    code: u8,
    message: String,
}

fn fail(code: u8, message: impl Into<String>) -> Failure {
    Failure {
        code,
        message: message.into(),
    }
}

/// Why one drawn place was not used: counted, and the next one drawn.
enum Reject {
    NoPointCloud,
    SparsePointCloud,
    NoGroundModel,
    YearGap,
    TooFlat,
    GroundModelGaps,
    Karttapullautin(String),
    Download(String),
}

impl Reject {
    fn label(&self) -> &'static str {
        match self {
            Reject::NoPointCloud => "no point cloud there",
            Reject::SparsePointCloud => "point cloud too sparse",
            Reject::NoGroundModel => "no ground model tile holding the box",
            Reject::YearGap => "point cloud and ground model measured too far apart",
            Reject::TooFlat => "too flat",
            Reject::GroundModelGaps => "ground model has gaps in the box",
            Reject::Karttapullautin(_) => "Karttapullautin failed",
            Reject::Download(_) => "a tile could not be downloaded",
        }
    }
}

/// An attempt that either found a sample, was rejected, or must stop the run.
enum Outcome {
    Sample(Box<Sample>),
    Rejected(Reject),
}

/// Everything one accepted place gives.
struct Sample {
    bounds: GroundBox,
    laz: StacItem,
    dtm: StacItem,
    lines_in_box: usize,
    relief: f64,
    with_slopes: ImportedMap,
    without_slopes: ImportedMap,
    ground_truth: maur_o::dem_geo_tiff_evaluator::GeoRaster,
    preview: Option<PathBuf>,
}

/// The HTTP side: an agent, and the credentials it sends Lantmäteriet.
struct Client {
    agent: ureq::Agent,
    authorization: String,
}

impl Client {
    /// GETs `url` and hands the open body to `consume`, trying again after a
    /// failure. A 401 is not retried: the credentials are wrong.
    fn get<T>(
        &self,
        url: &str,
        mut consume: impl FnMut(&mut dyn Read) -> Result<T, String>,
    ) -> Result<T, Failure> {
        let mut last = String::new();
        for attempt in 0..ATTEMPTS {
            if attempt > 0 {
                std::thread::sleep(Duration::from_secs(2 << attempt));
            }
            let mut request = self.agent.get(url);
            if is_lantmateriet_url(url) {
                request = request.header("Authorization", &self.authorization);
            }
            match request.call() {
                Ok(response) => {
                    let mut reader = response.into_body().into_reader();
                    match consume(&mut reader) {
                        Ok(value) => return Ok(value),
                        Err(e) => last = e,
                    }
                }
                Err(ureq::Error::StatusCode(401)) => {
                    return Err(fail(
                        5,
                        format!("{url}: 401 Unauthorized: check LANTMATERIET_USERNAME/PASSWORD"),
                    ))
                }
                Err(e) => last = e.to_string(),
            }
        }
        Err(fail(5, format!("{url}: {last}")))
    }

    /// The items of one collection meeting `ground`.
    fn search(&self, collection: &str, ground: &GroundBox) -> Result<Vec<StacItem>, Failure> {
        let url = search_url(collection, ground, 20);
        let text = self.get(&url, |r| {
            let mut text = String::new();
            r.read_to_string(&mut text).map_err(|e| e.to_string())?;
            Ok(text)
        })?;
        parse_search(&text).map_err(|e| fail(5, format!("{url}: {e}")))
    }

    /// Downloads `url` to `path` unless it is already there, through a
    /// `.part` file so an interrupted download is never mistaken for a
    /// whole one.
    fn download(&self, url: &str, path: &Path) -> Result<(), Failure> {
        if path.exists() {
            return Ok(());
        }
        let dir = path.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir)
            .map_err(|e| fail(4, format!("cannot create {}: {e}", dir.display())))?;
        let part = path.with_extension("part");
        eprintln!("  downloading {url}");
        self.get(url, |r| {
            let mut file = std::fs::File::create(&part)
                .map_err(|e| format!("cannot write {}: {e}", part.display()))?;
            std::io::copy(r, &mut file).map_err(|e| e.to_string())?;
            Ok(())
        })?;
        std::fs::rename(&part, path)
            .map_err(|e| fail(4, format!("cannot write {}: {e}", path.display())))
    }
}

/// The last path segment of a URL, for a cache file name.
fn file_name(url: &str) -> &str {
    url.rsplit('/').next().unwrap_or(url)
}

/// `KEY=VALUE` lines, `#` comments and blank lines skipped, quotes around a
/// value dropped.
fn read_env_file(path: &Path) -> Result<BTreeMap<String, String>, Failure> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| fail(2, format!("cannot read {}: {e}", path.display())))?;
    Ok(text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.strip_prefix("export ").unwrap_or(l).split_once('='))
        .map(|(k, v)| {
            let v = v.trim();
            let v = v
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .unwrap_or(v);
            (k.trim().to_string(), v.to_string())
        })
        .collect())
}

fn credentials(env_file: Option<&Path>) -> Result<(String, String), Failure> {
    let file = match env_file {
        Some(path) => read_env_file(path)?,
        None => BTreeMap::new(),
    };
    let get = |key: &str| {
        std::env::var(key)
            .ok()
            .filter(|v| !v.is_empty())
            .or_else(|| file.get(key).cloned())
    };
    match (get("LANTMATERIET_USERNAME"), get("LANTMATERIET_PASSWORD")) {
        (Some(user), Some(password)) => Ok((user, password)),
        _ => Err(fail(
            2,
            "no Lantmäteriet credentials: set LANTMATERIET_USERNAME and LANTMATERIET_PASSWORD \
             (a GeoTorget account), in the environment or in --env-file",
        )),
    }
}

/// Runs Karttapullautin on `laz` in `work` (once: its output is kept), and
/// returns the folder its DXF went to.
fn run_pullauta(pullauta: &Path, laz: &Path, work: &Path) -> Result<PathBuf, Reject> {
    let temp = work.join("temp");
    if temp.join("out2.dxf").exists() && temp.join("formlines.dxf").exists() {
        return Ok(temp);
    }
    std::fs::create_dir_all(work)
        .map_err(|e| Reject::Karttapullautin(format!("cannot create {}: {e}", work.display())))?;
    let laz = std::fs::canonicalize(laz).unwrap_or_else(|_| laz.to_path_buf());
    eprintln!("  running Karttapullautin on {}", laz.display());
    let output = Command::new(pullauta)
        .arg(&laz)
        .current_dir(work)
        .output()
        .map_err(|e| Reject::Karttapullautin(e.to_string()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let last = stderr.lines().last().unwrap_or("").to_string();
        return Err(Reject::Karttapullautin(format!(
            "{}: {last}",
            output.status
        )));
    }
    if !temp.join("out2.dxf").exists() || !temp.join("formlines.dxf").exists() {
        return Err(Reject::Karttapullautin(
            "no temp/out2.dxf or temp/formlines.dxf: is output_dxf=1 in pullauta.ini?".to_string(),
        ));
    }
    Ok(temp)
}

fn read_dxf(path: &Path) -> Result<Vec<Entity>, Reject> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| Reject::Karttapullautin(format!("cannot read {}: {e}", path.display())))?;
    dxf::parse(&text).map_err(|e| Reject::Karttapullautin(format!("{}: {e}", path.display())))
}

/// How many contour and index contour lines reach into `bounds`, and the
/// height range they span.
fn relief(out2: &[Entity], bounds: &GroundBox) -> (usize, f64) {
    let mut count = 0;
    let (mut lo, mut hi) = (f64::MAX, f64::MIN);
    for e in out2 {
        let contour = matches!(
            e.layer.as_str(),
            "contour" | "contour_index" | "depression" | "depression_index"
        );
        if !contour || !e.points.iter().any(|&p| bounds.contains(p)) {
            continue;
        }
        count += 1;
        if let Some(z) = e.elevation {
            lo = lo.min(z);
            hi = hi.max(z);
        }
    }
    (count, if hi >= lo { hi - lo } else { 0.0 })
}

/// One draw: a place, and everything that decides whether it becomes a
/// sample.
fn attempt(client: &Client, args: &Args, point: (f64, f64)) -> Result<Outcome, Failure> {
    let (lon, lat) = sweref99tm_inverse(point);
    eprintln!("drawing ({lat:.5}, {lon:.5})");
    let around = GroundBox {
        min_x: point.0 - 1.0,
        min_y: point.1 - 1.0,
        max_x: point.0 + 1.0,
        max_y: point.1 + 1.0,
    };
    let Some(laz) = client
        .search(LAZ_COLLECTION, &around)?
        .into_iter()
        .filter(|i| i.footprint.contains(point))
        .max_by_key(|i| i.measured.clone())
    else {
        return Ok(Outcome::Rejected(Reject::NoPointCloud));
    };
    if laz.density().is_none_or(|d| d < args.min_density) {
        return Ok(Outcome::Rejected(Reject::SparsePointCloud));
    }
    let Some(bounds) = box_in_tile(point, args.size, TILE_MARGIN, &laz.footprint) else {
        return Ok(Outcome::Rejected(Reject::NoPointCloud));
    };
    // The ground model footprints come from latitude/longitude, a few
    // centimeters off: a meter of slack.
    let shrunk = GroundBox {
        min_x: bounds.min_x + 1.0,
        min_y: bounds.min_y + 1.0,
        max_x: bounds.max_x - 1.0,
        max_y: bounds.max_y - 1.0,
    };
    let Some(dtm) = client
        .search(DTM_COLLECTION, &bounds)?
        .into_iter()
        .find(|i| covers(&i.footprint, &shrunk))
    else {
        return Ok(Outcome::Rejected(Reject::NoGroundModel));
    };
    match (laz.year(), dtm.year()) {
        (Some(a), Some(b)) if (a - b).abs() <= args.max_year_gap => {}
        _ => return Ok(Outcome::Rejected(Reject::YearGap)),
    }

    let laz_path = args.cache.join("laz").join(file_name(&laz.data_href));
    if let Err(e) = client.download(&laz.data_href, &laz_path) {
        if e.code == 4 {
            return Err(e);
        }
        return Ok(Outcome::Rejected(Reject::Download(e.message)));
    }
    let work = args.cache.join("pullauta").join(&laz.id);
    let temp = match run_pullauta(&args.pullauta, &laz_path, &work) {
        Ok(temp) => temp,
        Err(reject) => return Ok(Outcome::Rejected(reject)),
    };
    let out2 = match read_dxf(&temp.join("out2.dxf")) {
        Ok(e) => e,
        Err(reject) => return Ok(Outcome::Rejected(reject)),
    };
    let formlines = match read_dxf(&temp.join("formlines.dxf")) {
        Ok(e) => e,
        Err(reject) => return Ok(Outcome::Rejected(reject)),
    };
    let (lines_in_box, relief) = relief(&out2, &bounds);
    if lines_in_box < args.min_contours || relief < args.min_relief {
        eprintln!("  too flat: {lines_in_box} contour line(s), {relief:.1} m");
        return Ok(Outcome::Rejected(Reject::TooFlat));
    }

    let dtm_path = args.cache.join("dtm").join(file_name(&dtm.data_href));
    if let Err(e) = client.download(&dtm.data_href, &dtm_path) {
        if e.code == 4 {
            return Err(e);
        }
        return Ok(Outcome::Rejected(Reject::Download(e.message)));
    }
    let ground_truth = match read_window(
        &dtm_path,
        (bounds.min_x, bounds.min_y),
        (bounds.max_x, bounds.max_y),
    ) {
        Ok(g) => g,
        Err(e) => return Ok(Outcome::Rejected(Reject::Download(e))),
    };
    let missing = ground_truth.values.iter().filter(|v| v.is_nan()).count() as f64
        / ground_truth.values.len().max(1) as f64;
    if missing > MAX_MISSING {
        return Ok(Outcome::Rejected(Reject::GroundModelGaps));
    }

    let center = bounds.center();
    let (lon, lat) = sweref99tm_inverse(center);
    let options = |slope_lines| ImportOptions {
        bounds,
        scale: SCALE,
        epsg: i32::from(SWEREF99_TM),
        center_lat_lon: (lat, lon),
        slope_lines,
    };
    let with_slopes = import(&out2, &formlines, &options(true));
    let without_slopes = import(&out2, &formlines, &options(false));
    let preview = Some(work.join("pullautus_depr.png")).filter(|p| p.exists());
    Ok(Outcome::Sample(Box::new(Sample {
        bounds,
        laz,
        dtm,
        lines_in_box,
        relief,
        with_slopes,
        without_slopes,
        ground_truth,
        preview,
    })))
}

/// Writes one sample's folder.
fn write_sample(dir: &Path, sample: &Sample, seed: u64) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    sample
        .with_slopes
        .write(&dir.join("map_depressions.omap"))?;
    sample.without_slopes.write(&dir.join("map_none.omap"))?;
    // Both maps hold the same lines in the same order (slope line points
    // come after them), so one levels file serves both.
    let write_text = |name: &str, text: String| {
        let path = dir.join(name);
        std::fs::write(&path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))
    };
    write_text("levels.txt", sample.with_slopes.levels_text())?;
    let truth = &sample.ground_truth;
    write(truth, SWEREF99_TM, &dir.join("ground_truth.tif"))?;
    let px = truth.transform.col_x.abs();
    let smoothed = gaussian_smooth(truth, SMOOTHING_SIGMA_M / px);
    write(
        &smoothed,
        SWEREF99_TM,
        &dir.join("ground_truth_smoothed.tif"),
    )?;
    let water = water_mask(
        truth,
        WATER_FLATNESS_M,
        (WATER_MIN_AREA_M2 / (px * px)).ceil() as usize,
    );
    let water_share = water.iter().filter(|&&w| w).count() as f64 / water.len().max(1) as f64;
    let mask = GeoRaster {
        values: water.iter().map(|&w| if w { 1.0 } else { 0.0 }).collect(),
        ..truth.clone()
    };
    write(&mask, SWEREF99_TM, &dir.join("water_mask.tif"))?;
    if let Some(preview) = &sample.preview {
        let to = dir.join("preview.png");
        std::fs::copy(preview, &to).map_err(|e| format!("cannot write {}: {e}", to.display()))?;
    }

    let b = sample.bounds;
    let (lon, lat) = sweref99tm_inverse(b.center());
    let stats = &sample.with_slopes.stats;
    let mut text = String::new();
    let _ = writeln!(text, "seed = {seed}");
    let _ = writeln!(text, "center_lat_lon = {lat:.6} {lon:.6}");
    let _ = writeln!(text, "epsg = {SWEREF99_TM}");
    let _ = writeln!(
        text,
        "box = {} {} {} {}",
        b.min_x, b.min_y, b.max_x, b.max_y
    );
    let _ = writeln!(text, "point_cloud = {}", sample.laz.id);
    let _ = writeln!(
        text,
        "point_cloud_measured = {}",
        sample.laz.measured.as_deref().unwrap_or("?")
    );
    let _ = writeln!(
        text,
        "point_cloud_density = {:.3}",
        sample.laz.density().unwrap_or(0.0)
    );
    let _ = writeln!(text, "ground_model = {}", sample.dtm.id);
    let _ = writeln!(
        text,
        "ground_model_measured = {}",
        sample.dtm.measured.as_deref().unwrap_or("?")
    );
    let _ = writeln!(text, "contour_lines_in_box = {}", sample.lines_in_box);
    let _ = writeln!(text, "relief_m = {:.1}", sample.relief);
    let _ = writeln!(text, "water_share = {water_share:.4}");
    let _ = writeln!(text, "contours = {}", stats.contours);
    let _ = writeln!(text, "index_contours = {}", stats.index_contours);
    let _ = writeln!(text, "form_lines = {}", stats.form_lines);
    let _ = writeln!(text, "form_line_rings = {}", stats.form_line_rings);
    let _ = writeln!(text, "slope_lines = {}", stats.slope_lines);
    let _ = writeln!(text, "slope_lines_dropped = {}", stats.slope_lines_dropped);
    write_text("sample.txt", text)
}

fn run(args: &Args) -> Result<(), Failure> {
    let (user, password) = credentials(args.env_file.as_deref())?;
    if !args.pullauta.is_file() {
        return Err(fail(
            3,
            format!("Karttapullautin not found at {}", args.pullauta.display()),
        ));
    }
    if args.output.exists()
        && std::fs::read_dir(&args.output)
            .map(|mut d| d.next().is_some())
            .unwrap_or(true)
    {
        return Err(fail(
            4,
            format!("{} exists and is not empty", args.output.display()),
        ));
    }
    std::fs::create_dir_all(&args.output)
        .map_err(|e| fail(4, format!("cannot create {}: {e}", args.output.display())))?;

    let client = Client {
        agent: ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(1800)))
            .user_agent(concat!("maur-o/", env!("CARGO_PKG_VERSION")))
            .build()
            .into(),
        authorization: basic_auth(&user, &password),
    };

    let max_attempts = if args.at.is_empty() {
        args.max_attempts.unwrap_or(args.samples * 10)
    } else {
        args.at.len()
    };
    let mut rng = Rng::new(args.seed);
    let mut rejected: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut found = 0;
    let mut attempts = 0;
    while found < args.samples && attempts < max_attempts {
        let point = match args.at.get(attempts) {
            Some(&(lat, lon)) => sweref99tm((lon, lat)),
            None => sample_point(&mut rng),
        };
        attempts += 1;
        match attempt(&client, args, point)? {
            Outcome::Rejected(reject) => {
                let detail = match &reject {
                    Reject::Karttapullautin(d) | Reject::Download(d) => format!(": {d}"),
                    _ => String::new(),
                };
                eprintln!("  rejected: {}{detail}", reject.label());
                *rejected.entry(reject.label()).or_default() += 1;
            }
            Outcome::Sample(sample) => {
                found += 1;
                let dir = args.output.join(format!("sample_{found:04}"));
                write_sample(&dir, &sample, args.seed).map_err(|e| fail(4, e))?;
                eprintln!(
                    "  sample {found}/{}: {} contour line(s), {:.0} m of relief",
                    args.samples, sample.lines_in_box, sample.relief
                );
            }
        }
    }

    let mut info = String::new();
    let _ = writeln!(info, "format = {FORMAT}");
    let _ = writeln!(info, "seed = {}", args.seed);
    let _ = writeln!(info, "samples = {found}");
    let _ = writeln!(info, "samples_asked = {}", args.samples);
    let _ = writeln!(info, "attempts = {attempts}");
    let _ = writeln!(info, "size_m = {}", args.size);
    let _ = writeln!(info, "equidistance_m = 5");
    let _ = writeln!(info, "scale = {SCALE}");
    let _ = writeln!(info, "smoothing_sigma_m = {SMOOTHING_SIGMA_M}");
    let _ = writeln!(info, "min_contours = {}", args.min_contours);
    let _ = writeln!(info, "min_relief_m = {}", args.min_relief);
    let _ = writeln!(info, "min_density = {}", args.min_density);
    let _ = writeln!(info, "max_year_gap = {}", args.max_year_gap);
    let _ = writeln!(
        info,
        "created = {}",
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
    );
    for (label, count) in &rejected {
        let _ = writeln!(info, "rejected: {label} = {count}");
    }
    let path = args.output.join("info.txt");
    std::fs::write(&path, info)
        .map_err(|e| fail(4, format!("cannot write {}: {e}", path.display())))?;

    eprintln!(
        "wrote {found} sample(s) to {} ({attempts} place(s) drawn)",
        args.output.display()
    );
    if found < args.samples {
        return Err(fail(
            6,
            format!(
                "only {found} of {} sample(s) found in {attempts} draw(s); raise --max-attempts \
                 or relax --min-contours/--min-relief",
                args.samples
            ),
        ));
    }
    Ok(())
}

fn main() -> ExitCode {
    let args = match Args::try_parse() {
        Ok(a) => a,
        Err(e) => {
            e.print().ok();
            return ExitCode::from(if e.exit_code() == 0 { 0 } else { 1 });
        }
    };
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(f) => {
            eprintln!("Error: {}", f.message);
            ExitCode::from(f.code)
        }
    }
}
