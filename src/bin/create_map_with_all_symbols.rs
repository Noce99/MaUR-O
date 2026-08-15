//! Creates a folder with one map file per symbol of the given map.
//!
//! ```text
//! create_map_with_all_symbols <map-file> [output-dir]
//! ```
//!
//! Each generated map contains a single symbol, applied to a grid of test
//! objects: several shapes, at several sizes, and — where the symbol supports
//! it — at several rotations. A companion `.txt` file describes the grid.
//! See [`mti::all_symbols`], which does the work, for what is on a sheet.
//!
//! Sizes are given in meters on the ground. They are converted to paper units
//! using the map scale (1:15000 etc.) stored in the map file.
//!
//! A port of the C++ tool of the same name, and the source of the maps
//! `create_benchmark` builds a suite out of.
//!
//! Exit codes: 0 success, 1 usage error, 2 the map could not be read, 4 a
//! file could not be written.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;

use mti::progress::Progress;

#[derive(Parser)]
#[command(
    name = "create_map_with_all_symbols",
    version,
    about = "Creates a directory with one map file per symbol of the given map.\n\n\
             Each generated map contains a single symbol, applied to a grid of test \
             objects: several shapes, at several sizes, and - where the symbol supports \
             it - at several rotations. A companion .txt file describes the grid.\n\n\
             Sizes are given in meters on the ground. They are converted to paper units \
             using the map scale (1:15000 etc.) stored in the map file."
)]
struct Args {
    /// The map providing the symbols (.omap, .xmap).
    map_file: PathBuf,

    /// The directory to be filled with the generated files. It is created if
    /// it does not exist. Defaults to the map file name with a "_symbols"
    /// suffix.
    output_dir: Option<PathBuf>,
}

/// The default output directory for the given input path: next to the map
/// file, named after it.
fn default_output_path(map_path: &Path) -> PathBuf {
    let stem = map_path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
    map_path.with_file_name(format!("{stem}_symbols"))
}

fn run() -> Result<(), (ExitCode, String)> {
    let args = match Args::try_parse() {
        Ok(a) => a,
        Err(e) => {
            e.print().ok();
            return Err((ExitCode::from(if e.exit_code() == 0 { 0 } else { 1 }), String::new()));
        }
    };

    let output = args.output_dir.clone().unwrap_or_else(|| default_output_path(&args.map_file));
    if output.is_file() {
        return Err((ExitCode::from(1), format!("Error: {} is a file.", output.display())));
    }
    // Files of a previous run are overwritten, but files belonging to symbols
    // which no longer exist are left behind.
    if output.is_dir() && std::fs::read_dir(&output).map(|d| d.count() > 0).unwrap_or(false) {
        eprintln!(
            "Warning: The directory {} is not empty. Existing files are not removed.",
            output.display()
        );
    }

    let mut progress: Option<Progress> = None;
    let summary = mti::all_symbols::create_maps(&args.map_file, &output, |done, total| {
        let bar = progress.get_or_insert_with(|| Progress::new("Symbols", total));
        let _ = done;
        bar.tick();
    })
    .map_err(|e| (ExitCode::from(2), format!("Error: {e}")))?;
    if let Some(bar) = progress {
        bar.finish();
    }

    for warning in &summary.warnings {
        eprintln!("Warning: {warning}");
    }
    for skipped in &summary.skipped {
        println!("Note: Skipped symbol {skipped} (no objects could be generated).");
    }

    print!("{}: {} symbol(s) written", output.display(), summary.written);
    if !summary.skipped.is_empty() {
        print!(", {} skipped", summary.skipped.len());
    }
    println!(", map scale 1:{}", summary.scale_denominator);
    Ok(())
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
