//! Runs `contours_to_raster` over a DEM benchmark archive (made by
//! `create_dem_benchmark`) and scores every result against the ground model
//! the contours were drawn from.
//!
//! ```text
//! dem_benchmark <archive> [--results <dir>] [--config <path>] [--margin M]
//!     [--variant NAME]... [--min-level-accuracy PERCENT] [--no-bundle]
//! ```
//!
//! Every sample's map is run once per variant (`depressions`: slope lines
//! on depressions, as a real map has them; `none`: no slope lines at all;
//! both by default, or those `--variant` names), with the algorithm's
//! parameters from `--config` (`config/contours_to_raster.conf` by
//! default). Each result is scored (`maur_o::dem_benchmark` defines every
//! number) against both the ground model and its smoothed copy, over the
//! pixels that are not water and at least `--margin` meters (50 by default)
//! inside the sample's box:
//!
//! - RMSE and MAE, after removing the mean difference and, separately, the
//!   median one;
//! - per-contour level accuracy: the share of lines at exactly the right
//!   level, one band off, more, and the share of line length at a wrong one;
//! - the inverted area: the share of sloping ground whose predicted slope
//!   points the other way;
//! - the RMSE of a Laplace interpolation of the same lines at their *true*
//!   levels, the best these contours can do.
//!
//! Every run gets its own timestamped folder:
//!
//! ```text
//! <results>/dem_benchmark_YYYY_MM_DD__HH_mm_ss/
//!   summary.txt                         one row per sample and variant, then the means per variant
//!   bundle.zip                          the whole run for a viewer (unless --no-bundle)
//!   sample_0001/<variant>/
//!     prediction.tif                    contours_to_raster's elevation model, in the map's CRS
//!     metrics.txt                       every number, against both ground truths
//!     signed_error.png, abs_error.png, error_histogram.png
//! ```
//!
//! `bundle.zip` is what a viewer reads (o-mnia's DEM benchmark extension):
//! every metric as JSON, and per sample the ground truth, each prediction
//! and the water mask on one 2 m grid, and every contour with its true and
//! predicted level (`maur_o::dem_bundle` documents the layout).
//!
//! A sample the algorithm fails on (a gravity conflict, say) is reported as
//! a failure and the run goes on.
//!
//! Exit codes: 0 success, 1 usage error, 2 the archive could not be read,
//! 3 the config file could not be read or parsed, 4 the results could not
//! be written, 5 a variant's mean level accuracy is below
//! `--min-level-accuracy`.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use clap::Parser;

use maur_o::contours_to_raster_config::{Config, DEFAULT_CONFIG_PATH};
use maur_o::dem_archive::{Archive, Sample, VARIANTS};
use maur_o::dem_benchmark::{map_lines, prediction_raster, score, Scores};
use maur_o::dem_bundle::{
    array, block_mean, f32_bytes, lines_json, mask_onto, sample_onto, scores_json, string,
    BundleWriter, Grid, Obj, BUNDLE_CELL_M, BUNDLE_FORMAT,
};
use maur_o::dem_from_contours::compute;
use maur_o::dem_geo_tiff_evaluator::CrsKey;
use maur_o::dem_geo_tiff_evaluator::{
    evaluate, read, write, write_abs_error_png, write_histogram_png, write_signed_error_png,
};
use maur_o::pullauta_import::parse_levels;
use maur_o::render::read_map_file;

#[derive(Parser)]
#[command(
    name = "dem_benchmark",
    version,
    about = "Runs contours_to_raster over a DEM benchmark archive and scores it against the \
             ground models."
)]
struct Args {
    /// The archive `create_dem_benchmark` made.
    archive: PathBuf,

    /// Where this run's own timestamped folder is created.
    #[arg(long, default_value = "Results")]
    results: PathBuf,

    /// The config file with the algorithm's parameters.
    #[arg(long, default_value = DEFAULT_CONFIG_PATH)]
    config: PathBuf,

    /// How far inside the sample's box (meters) a pixel must be to be
    /// scored.
    #[arg(long, default_value_t = 50.0)]
    margin: f64,

    /// A map variant to run (`depressions` or `none`). Repeatable; both by
    /// default.
    #[arg(long)]
    variant: Vec<String>,

    /// Fail (exit code 5) if any variant's mean share of lines at exactly
    /// the right level is below this, in percent.
    #[arg(long)]
    min_level_accuracy: Option<f64>,

    /// Do not write `bundle.zip`, the whole run for a viewer.
    #[arg(long)]
    no_bundle: bool,
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

/// One sample run in one variant.
struct Row {
    sample: String,
    variant: String,
    outcome: Result<Scored, String>,
}

struct Scored {
    raw: Scores,
    smoothed: Option<Scores>,
    seconds: f64,
    equidistance: f64,
    /// The prediction on the bundle's grid and `lines.json`, when bundling.
    bundle: Option<(Vec<f32>, String)>,
}

/// Runs and scores one sample in one variant, writing its folder.
fn run_one(
    sample: &Sample,
    variant: &str,
    config: &Config,
    equidistance: f64,
    margin: f64,
    out: &Path,
    grid: Option<&Grid>,
) -> Result<Scored, String> {
    let (map, _warnings) = read_map_file(&sample.map(variant))?;
    let levels_text = std::fs::read_to_string(sample.levels())
        .map_err(|e| format!("cannot read {}: {e}", sample.levels().display()))?;
    let levels = parse_levels(&levels_text)?;
    let lines = map_lines(&map, &levels)?;

    let started = Instant::now();
    let result = compute(&map, config, equidistance, false, &mut |_, _| {})?;
    let seconds = started.elapsed().as_secs_f64();
    let prediction = prediction_raster(&result, &map)?;

    let truth = read(&sample.ground_truth())?;
    let smoothed = read(&sample.ground_truth_smoothed()).ok();
    let water: Option<Vec<bool>> = read(&sample.water_mask())
        .ok()
        .filter(|m| m.values.len() == truth.values.len())
        .map(|m| m.values.iter().map(|&v| v > 0.5).collect());

    let raw = score(
        &truth,
        &prediction,
        water.as_deref(),
        &lines,
        equidistance,
        margin,
    )?;
    let smoothed_scores = smoothed
        .as_ref()
        .map(|s| {
            score(
                s,
                &prediction,
                water.as_deref(),
                &lines,
                equidistance,
                margin,
            )
        })
        .transpose()?;

    std::fs::create_dir_all(out).map_err(|e| format!("cannot create {}: {e}", out.display()))?;
    let epsg = map
        .georeferencing
        .as_ref()
        .and_then(|g| u16::try_from(g.epsg).ok())
        .ok_or("the map's CRS has no EPSG code")?;
    write(&prediction, epsg, &out.join("prediction.tif"))?;
    let evaluation = evaluate(&truth, &prediction)?;
    write_signed_error_png(&evaluation, &out.join("signed_error.png"))?;
    write_abs_error_png(&evaluation, &out.join("abs_error.png"))?;
    write_histogram_png(&evaluation, &out.join("error_histogram.png"))?;

    let mut text = String::new();
    let _ = writeln!(text, "sample = {}", sample.name);
    let _ = writeln!(text, "variant = {variant}");
    let _ = writeln!(text, "equidistance_m = {equidistance}");
    let _ = writeln!(text, "seconds = {seconds:.2}");
    let _ = writeln!(text, "contours = {}", result.contour_count);
    let _ = writeln!(text, "still_undefined_px = {}", result.still_undefined);
    write_scores(&mut text, "", &raw, equidistance);
    if let Some(s) = &smoothed_scores {
        write_scores(&mut text, "smoothed.", s, equidistance);
    }
    let path = out.join("metrics.txt");
    std::fs::write(&path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))?;

    // On the truth's datum: the constant the scores remove.
    let offset = raw.mean_offset.offset;
    let bundle = grid.map(|g| {
        (
            sample_onto(&prediction, g, offset),
            lines_json(&lines, &raw.levels, equidistance, offset),
        )
    });
    Ok(Scored {
        raw,
        smoothed: smoothed_scores,
        seconds,
        equidistance,
        bundle,
    })
}

/// `key = value` lines for every number of `s`, keys prefixed by `prefix`.
fn write_scores(text: &mut String, prefix: &str, s: &Scores, equidistance: f64) {
    let mut line = |key: &str, value: String| {
        let _ = writeln!(text, "{prefix}{key} = {value}");
    };
    line("pixels", s.pixels.to_string());
    line("offset_mean_m", format!("{:.3}", s.mean_offset.offset));
    line("rmse_m", format!("{:.3}", s.mean_offset.rmse));
    line("mae_m", format!("{:.3}", s.mean_offset.mae));
    line(
        "rmse_equidistances",
        format!("{:.3}", s.mean_offset.rmse / equidistance),
    );
    line("offset_median_m", format!("{:.3}", s.median_offset.offset));
    line(
        "rmse_median_offset_m",
        format!("{:.3}", s.median_offset.rmse),
    );
    line("mae_median_offset_m", format!("{:.3}", s.median_offset.mae));
    line("lines_scored", s.levels.lines.to_string());
    line("band_offset", s.levels.band_offset.to_string());
    line("levels_exact", format!("{:.4}", s.levels.exact));
    line("levels_off_by_one", format!("{:.4}", s.levels.off_by_one));
    line("levels_off_by_more", format!("{:.4}", s.levels.off_by_more));
    line(
        "levels_wrong_length",
        format!("{:.4}", s.levels.wrong_length),
    );
    line("inverted", format!("{:.4}", s.inverted));
    line("baseline_rmse_m", format!("{:.3}", s.baseline.rmse));
    line("baseline_mae_m", format!("{:.3}", s.baseline.mae));
}

/// The summary table, one row per sample and variant, then each variant's
/// means.
fn summary(rows: &[Row], archive: &Path) -> String {
    let mut text = String::new();
    let _ = writeln!(text, "DEM benchmark of {}", archive.display());
    let _ = writeln!(
        text,
        "RMSE/MAE after removing the mean difference; levels = share of lines at the exact level; \
         wrong len = share of line length at a wrong level; inverted = share of sloping ground \
         sloping the wrong way; baseline = Laplace fill of the true levels. Against the raw \
         ground model; 'smooth' columns against its smoothed copy."
    );
    let _ = writeln!(text);
    let _ = writeln!(
        text,
        "{:<12} {:<12} {:>8} {:>8} {:>8} {:>8} {:>8} {:>9} {:>9} {:>10} {:>8}",
        "sample",
        "variant",
        "RMSE m",
        "MAE m",
        "levels",
        "wrong len",
        "inverted",
        "baseline",
        "RMSE sm.",
        "levels sm.",
        "seconds"
    );
    for row in rows {
        match &row.outcome {
            Ok(s) => {
                let smooth = s.smoothed.as_ref();
                let _ = writeln!(
                    text,
                    "{:<12} {:<12} {:>8.2} {:>8.2} {:>7.1}% {:>7.1}% {:>7.1}% {:>9.2} {:>9} {:>10} {:>8.1}",
                    row.sample,
                    row.variant,
                    s.raw.mean_offset.rmse,
                    s.raw.mean_offset.mae,
                    100.0 * s.raw.levels.exact,
                    100.0 * s.raw.levels.wrong_length,
                    100.0 * s.raw.inverted,
                    s.raw.baseline.rmse,
                    smooth.map_or("-".to_string(), |m| format!("{:.2}", m.mean_offset.rmse)),
                    smooth.map_or("-".to_string(), |m| format!("{:.1}%", 100.0 * m.levels.exact)),
                    s.seconds
                );
            }
            Err(e) => {
                let _ = writeln!(text, "{:<12} {:<12} FAILED: {e}", row.sample, row.variant);
            }
        }
    }
    let _ = writeln!(text);
    let _ = writeln!(text, "Means per variant (failures not included):");
    for (variant, stats) in variant_means(rows) {
        let _ = writeln!(
            text,
            "{variant:<12} samples {:>3}  failed {:>3}  RMSE {:>6.2} m ({:.2} equidistances)  \
             MAE {:>6.2} m  levels {:>5.1}%  wrong length {:>5.1}%  inverted {:>5.1}%  \
             baseline RMSE {:>6.2} m",
            stats.count,
            stats.failed,
            stats.rmse,
            stats.rmse_equidistances,
            stats.mae,
            100.0 * stats.levels,
            100.0 * stats.wrong_length,
            100.0 * stats.inverted,
            stats.baseline
        );
    }
    text
}

/// One variant's means over its successful samples.
struct Means {
    count: usize,
    failed: usize,
    rmse: f64,
    rmse_equidistances: f64,
    mae: f64,
    levels: f64,
    wrong_length: f64,
    inverted: f64,
    baseline: f64,
}

fn variant_means(rows: &[Row]) -> Vec<(String, Means)> {
    let mut variants: Vec<String> = Vec::new();
    for row in rows {
        if !variants.contains(&row.variant) {
            variants.push(row.variant.clone());
        }
    }
    variants
        .into_iter()
        .map(|variant| {
            let of: Vec<&Scored> = rows
                .iter()
                .filter(|r| r.variant == variant)
                .filter_map(|r| r.outcome.as_ref().ok())
                .collect();
            let failed = rows
                .iter()
                .filter(|r| r.variant == variant && r.outcome.is_err())
                .count();
            let n = of.len().max(1) as f64;
            let mean = |f: &dyn Fn(&Scored) -> f64| of.iter().map(|s| f(s)).sum::<f64>() / n;
            let means = Means {
                count: of.len(),
                failed,
                rmse: mean(&|s| s.raw.mean_offset.rmse),
                rmse_equidistances: mean(&|s| s.raw.mean_offset.rmse / s.equidistance),
                mae: mean(&|s| s.raw.mean_offset.mae),
                levels: mean(&|s| s.raw.levels.exact),
                wrong_length: mean(&|s| s.raw.levels.wrong_length),
                inverted: mean(&|s| s.raw.inverted),
                baseline: mean(&|s| s.raw.baseline.rmse),
            };
            (variant, means)
        })
        .collect()
}

/// `sample`'s bundle grid, with its ground truth and water mask on it.
fn bundle_grid(sample: &Sample) -> Result<(Grid, Vec<f32>, Vec<u8>), String> {
    let truth = read(&sample.ground_truth())?;
    let CrsKey::Projected(epsg) = truth.crs else {
        return Err("the ground truth is not in a projected CRS".to_string());
    };
    if truth.transform.row_x != 0.0 || truth.transform.col_y != 0.0 {
        return Err("the ground truth is not north-up".to_string());
    }
    let grid = Grid::covering(&truth, BUNDLE_CELL_M, epsg);
    let mask: Vec<bool> = read(&sample.water_mask())
        .ok()
        .filter(|m| m.values.len() == truth.values.len())
        .map(|m| m.values.iter().map(|&v| v > 0.5).collect())
        .unwrap_or_else(|| vec![false; truth.values.len()]);
    let water = mask_onto(&mask, &truth, &grid);
    Ok((grid, block_mean(&truth, &grid), water))
}

/// Adds one sample's predictions, lines and `sample.json` (its `rows`, one
/// per variant) to the bundle, taking the bundle parts out of the rows.
fn add_sample(
    writer: &mut BundleWriter,
    sample: &Sample,
    grid: &Grid,
    rows: &mut [Row],
) -> Result<(), String> {
    let mut variants = Obj::new();
    for row in rows.iter_mut() {
        let dir = format!("{}/{}", sample.name, row.variant);
        variants = match &mut row.outcome {
            Ok(scored) => {
                if let Some((prediction, lines)) = scored.bundle.take() {
                    writer.add(&format!("{dir}/prediction.f32"), &f32_bytes(&prediction))?;
                    writer.add(&format!("{dir}/lines.json"), lines.as_bytes())?;
                }
                variants.raw(&row.variant, variant_json(scored))
            }
            Err(e) => variants.raw(&row.variant, Obj::new().str("failed", e).build()),
        };
    }
    let mut info = Obj::new();
    for (key, value) in &sample.info {
        info = info.str(key, value);
    }
    let json = Obj::new()
        .str("name", &sample.name)
        .raw("info", info.build())
        .raw("grid", grid.to_json())
        .raw("variants", variants.build())
        .build();
    writer.add(&format!("{}/sample.json", sample.name), json.as_bytes())
}

/// One variant's scores on one sample, against the raw and the smoothed
/// ground truth.
fn variant_json(scored: &Scored) -> String {
    Obj::new()
        .num("seconds", scored.seconds)
        .raw("raw", scores_json(&scored.raw, scored.equidistance))
        .raw(
            "smoothed",
            scored
                .smoothed
                .as_ref()
                .map_or("null".to_string(), |s| scores_json(s, scored.equidistance)),
        )
        .build()
}

/// `results.json`: the run, its means per variant, and per sample what
/// `summary.txt` has.
fn results_json(
    rows: &[Row],
    variants: &[String],
    args: &Args,
    archive: &Archive,
    equidistance: f64,
) -> String {
    let archive_info = archive
        .info
        .iter()
        .fold(Obj::new(), |obj, (k, v)| obj.str(k, v))
        .build();
    let means = variant_means(rows)
        .into_iter()
        .fold(Obj::new(), |obj, (variant, m)| {
            obj.raw(
                &variant,
                Obj::new()
                    .int("samples", m.count as i64)
                    .int("failed", m.failed as i64)
                    .num("rmse_m", m.rmse)
                    .num("rmse_equidistances", m.rmse_equidistances)
                    .num("mae_m", m.mae)
                    .num("levels_exact", m.levels)
                    .num("levels_wrong_length", m.wrong_length)
                    .num("inverted", m.inverted)
                    .num("baseline_rmse_m", m.baseline)
                    .build(),
            )
        })
        .build();
    let mut names: Vec<&str> = Vec::new();
    for row in rows {
        if !names.contains(&row.sample.as_str()) {
            names.push(&row.sample);
        }
    }
    let samples = array(names.iter().map(|name| {
        let per_variant = rows
            .iter()
            .filter(|r| r.sample == *name)
            .fold(Obj::new(), |obj, row| match &row.outcome {
                Ok(s) => obj.raw(&row.variant, variant_json(s)),
                Err(e) => obj.raw(&row.variant, Obj::new().str("failed", e).build()),
            })
            .build();
        Obj::new()
            .str("name", name)
            .raw("variants", per_variant)
            .build()
    }));
    Obj::new()
        .str("format", BUNDLE_FORMAT)
        .str("archive", &args.archive.display().to_string())
        .raw("archive_info", archive_info)
        .str("config", &args.config.display().to_string())
        .str(
            "created",
            &chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        )
        .num("equidistance_m", equidistance)
        .num("margin_m", args.margin)
        .num("cell_m", BUNDLE_CELL_M)
        .raw("variants", array(variants.iter().map(|v| string(v))))
        .raw("means", means)
        .raw("samples", samples)
        .build()
}

fn run(args: &Args) -> Result<(), Failure> {
    let variants: Vec<String> = if args.variant.is_empty() {
        VARIANTS.iter().map(|v| v.to_string()).collect()
    } else {
        for v in &args.variant {
            if !VARIANTS.contains(&v.as_str()) {
                return Err(fail(
                    1,
                    format!("unknown variant {v:?}: expected one of {VARIANTS:?}"),
                ));
            }
        }
        args.variant.clone()
    };
    let archive = Archive::open(&args.archive).map_err(|e| fail(2, e))?;
    let equidistance = archive.equidistance().map_err(|e| fail(2, e))?;
    if archive.samples.is_empty() {
        return Err(fail(
            2,
            format!("{} holds no samples", args.archive.display()),
        ));
    }
    let config = Config::load(&args.config).map_err(|e| fail(3, e))?;

    let run_dir = args.results.join(format!(
        "dem_benchmark_{}",
        chrono::Local::now().format("%Y_%m_%d__%H_%M_%S")
    ));
    std::fs::create_dir_all(&run_dir)
        .map_err(|e| fail(4, format!("cannot create {}: {e}", run_dir.display())))?;

    let bundle_failure = |e: String| fail(4, e);
    let mut bundle = if args.no_bundle {
        None
    } else {
        Some(BundleWriter::create(&run_dir.join("bundle.zip")).map_err(bundle_failure)?)
    };

    let mut rows = Vec::new();
    for sample in &archive.samples {
        // The sample's grid, with the truth and the water already on it.
        let grid = match &mut bundle {
            Some(writer) => match bundle_grid(sample) {
                Ok((grid, truth, water)) => {
                    let dir = &sample.name;
                    writer
                        .add(&format!("{dir}/truth.f32"), &f32_bytes(&truth))
                        .map_err(bundle_failure)?;
                    writer
                        .add(&format!("{dir}/water.u8"), &water)
                        .map_err(bundle_failure)?;
                    Some(grid)
                }
                Err(e) => {
                    eprintln!("  {}: left out of the bundle: {e}", sample.name);
                    None
                }
            },
            None => None,
        };
        let first_row = rows.len();
        for variant in &variants {
            eprintln!("{} / {variant}", sample.name);
            let out = run_dir.join(&sample.name).join(variant);
            let outcome = run_one(
                sample,
                variant,
                &config,
                equidistance,
                args.margin,
                &out,
                grid.as_ref(),
            );
            match &outcome {
                Ok(s) => eprintln!(
                    "  RMSE {:.2} m, levels {:.1}% exact, inverted {:.1}%, baseline RMSE {:.2} m",
                    s.raw.mean_offset.rmse,
                    100.0 * s.raw.levels.exact,
                    100.0 * s.raw.inverted,
                    s.raw.baseline.rmse
                ),
                Err(e) => eprintln!("  failed: {e}"),
            }
            rows.push(Row {
                sample: sample.name.clone(),
                variant: variant.clone(),
                outcome,
            });
        }
        if let (Some(writer), Some(grid)) = (&mut bundle, &grid) {
            add_sample(writer, sample, grid, &mut rows[first_row..]).map_err(bundle_failure)?;
        }
    }
    if let Some(mut writer) = bundle {
        let json = results_json(&rows, &variants, args, &archive, equidistance);
        writer
            .add("results.json", json.as_bytes())
            .map_err(bundle_failure)?;
        writer.finish().map_err(bundle_failure)?;
    }

    let text = summary(&rows, &args.archive);
    let path = run_dir.join("summary.txt");
    std::fs::write(&path, &text)
        .map_err(|e| fail(4, format!("cannot write {}: {e}", path.display())))?;
    print!("{text}");
    eprintln!("wrote {}", run_dir.display());

    if let Some(threshold) = args.min_level_accuracy {
        for (variant, means) in variant_means(&rows) {
            if means.count == 0 || 100.0 * means.levels < threshold {
                return Err(fail(
                    5,
                    format!(
                        "variant {variant}: {:.1}% of lines at the exact level, below {threshold}%",
                        100.0 * means.levels
                    ),
                ));
            }
        }
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
