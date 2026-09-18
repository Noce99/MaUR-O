//! Scores a predicted DEM GeoTIFF against a ground-truth one over their
//! shared ground, correcting for the arbitrary additive constant a DEM is
//! only ever meaningful up to, and reports it as MSE/RMSE/MAE plus a
//! couple of colormap images of the error.
//!
//! ```text
//! dem_geo_tiff_evaluator <ground_truth.tif> <prediction.tif> [--results <dir>]
//! ```
//!
//! Every run gets its own timestamped folder, so nothing from an earlier run
//! is silently overwritten:
//!
//! ```text
//! <results>/dem_evaluation_YYYY_MM_DD__HH_mm_ss/
//! ```
//!
//! Exit codes: 0 success, 1 usage error, 2 the ground truth file could not
//! be read, 3 the prediction file could not be read, 4 the two files could
//! not be compared (different/missing CRS, or no pixel with a value on both
//! sides), 5 the run folder or one of its files could not be written.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;

use maur_o::dem_geo_tiff_evaluator::{
    evaluate, read, write_abs_error_png, write_histogram_png, write_report, write_signed_error_png,
};

#[derive(Parser)]
#[command(
    name = "dem_geo_tiff_evaluator",
    version,
    about = "Scores a predicted DEM GeoTIFF against a ground-truth one over their shared, \
             georeferenced ground."
)]
struct Args {
    /// The ground-truth DEM GeoTIFF -- the output grid's own resolution and
    /// origin.
    ground_truth: PathBuf,

    /// The predicted DEM GeoTIFF, resampled onto the ground truth's own
    /// grid before scoring.
    prediction: PathBuf,

    /// Where this run's own timestamped folder is created.
    #[arg(long, default_value = "Results")]
    results: PathBuf,
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

    let ground_truth =
        read(&args.ground_truth).map_err(|e| (ExitCode::from(2), format!("Error: {e}")))?;
    let prediction =
        read(&args.prediction).map_err(|e| (ExitCode::from(3), format!("Error: {e}")))?;

    let evaluation = evaluate(&ground_truth, &prediction)
        .map_err(|e| (ExitCode::from(4), format!("Error: {e}")))?;

    let started = chrono::Local::now();
    let run_dir = args.results.join(format!(
        "dem_evaluation_{}",
        started.format("%Y_%m_%d__%H_%M_%S")
    ));
    std::fs::create_dir_all(&run_dir).map_err(|e| {
        (
            ExitCode::from(5),
            format!("cannot make {}: {e}", run_dir.display()),
        )
    })?;

    write_report(
        &evaluation,
        &args.ground_truth,
        &args.prediction,
        &run_dir.join("metrics.txt"),
    )
    .map_err(|e| (ExitCode::from(5), format!("Error: {e}")))?;
    write_abs_error_png(&evaluation, &run_dir.join("error_abs.png"))
        .map_err(|e| (ExitCode::from(5), format!("Error: {e}")))?;
    write_signed_error_png(&evaluation, &run_dir.join("error_signed.png"))
        .map_err(|e| (ExitCode::from(5), format!("Error: {e}")))?;
    write_histogram_png(&evaluation, &run_dir.join("error_histogram.png"))
        .map_err(|e| (ExitCode::from(5), format!("Error: {e}")))?;

    println!(
        "wrote {}: {} valid pixel(s), best constant {:.4}, MSE {:.4}, RMSE {:.4}, MAE {:.4}, max \
         absolute error {:.4}",
        run_dir.display(),
        evaluation.valid_pixel_count,
        evaluation.best_constant,
        evaluation.mse,
        evaluation.rmse,
        evaluation.mae,
        evaluation.max_abs_error,
    );

    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::from(0),
        Err((code, message)) => {
            if !message.is_empty() {
                eprintln!("{message}");
            }
            code
        }
    }
}
