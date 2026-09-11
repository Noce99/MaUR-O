//! Extracts elevation-gravity information from an .omap's contours --
//! `Contours-to-Raster.md`'s Steps 0, 1 and 2. Step 3 (actually assigning an
//! elevation number to each contour) is still `TO DO` in the doc, so this
//! binary works out, for every contour, which way is downhill, and stops
//! there: no TIFF is written yet, since there is no real elevation to put in
//! one.
//!
//! ```text
//! contours_to_raster <map.omap> [output.tif] [--results <dir>] [--config <path>] [--create_svg]
//! ```
//!
//! Every run gets its own timestamped folder, so nothing from an earlier run
//! is silently overwritten:
//!
//! ```text
//! <results>/contours_to_raster_YYYY_MM_DD__HH_mm_ss/
//! ```
//!
//! `--results` (default `Results`) says where that folder is created.
//! `[output.tif]` names the files written inside it -- only its file name is
//! used, since the directory is always the run folder -- and is used to name
//! the `--create_svg` files even though nothing is written under it yet, so
//! this interface does not need to change once Step 3 lands.
//!
//! Exit codes: 0 success, 1 usage error, 2 the map could not be read, 3 the
//! config file could not be read or parsed, 4 the run folder or a
//! `--create_svg` file could not be written, or a Step 0 geometry/raster
//! error (a conflicting Contour Raster pixel two contours could not be
//! unified across -- see `step0_extract::ExtractError::Conflict`, which
//! still gets its own `<stem>_conflict.svg` under `--create_svg` -- or a
//! degenerate buffer polygon), 5 a Step 1/Step 2 gravity conflict, or a
//! contour left undefined after both rain-drop passes.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;

use maur_o::contours_to_raster_config::{Config, DEFAULT_CONFIG_PATH};
use maur_o::contours_to_raster_svg::{
    write_conflict_svg, write_final_svg, write_step0_svg, write_step1_svg,
    write_step2_anti_rain_svg, write_step2_rain_svg,
};
use maur_o::step0_extract::{self, ExtractError};
use maur_o::step1_obvious_gravity;
use maur_o::step2_rain_drop;
use maur_o::xml_reader::read_xml_map;

#[derive(Parser)]
#[command(
    name = "contours_to_raster",
    version,
    about = "Extracts elevation-gravity information from an .omap's contours (Contours-to-Raster.md, \
             Steps 0-2). Step 3 is not yet implemented, so no TIFF is written yet."
)]
struct Args {
    /// The .omap file to read.
    map_file: PathBuf,

    /// The file name the elevation TIFF would be written as, and what the
    /// --create_svg files are named after. Any directory given here is
    /// ignored -- everything this run produces goes inside its own folder
    /// under --results. Defaults to the map file's own name with a .tif
    /// suffix. Unused until Step 3 exists.
    output_file: Option<PathBuf>,

    /// Where this run's own timestamped folder is created.
    #[arg(long, default_value = "Results")]
    results: PathBuf,

    /// The config file with the algorithm's parameters.
    #[arg(long, default_value = DEFAULT_CONFIG_PATH)]
    config: PathBuf,

    /// Write the four per-step validation SVGs Contours-to-Raster.md's
    /// "Visualization" section describes (Step 0, Step 1, and Step 2's Rain
    /// and Anti Rain Drop Productions each in their own file), plus a fifth
    /// "final" SVG with just the algorithm's actual answer once every
    /// contour is resolved, inside this run's own folder.
    #[arg(long = "create_svg")]
    create_svg: bool,
}

/// The default output file name for the given input path: the map file's
/// own name, with the suffix replaced by ".tif".
fn default_output_name(map_path: &Path) -> PathBuf {
    Path::new(map_path.file_stem().unwrap_or_default()).with_extension("tif")
}

/// `<output>_step{0,1,2_rain,2_anti_rain}.svg`, next to `output_path`.
fn step_svg_path(output_path: &Path, step: &str) -> PathBuf {
    let stem = output_path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy();
    output_path.with_file_name(format!("{stem}_{step}.svg"))
}

/// Step 2's two files: one with every Rain Drop Production drop's path, one
/// with every Anti Rain Drop Production drop's -- kept apart so a drop's
/// path stays legible where the two passes cross, rather than overlaying
/// both colors on one picture.
fn write_step2_svgs(
    output_path: &Path,
    step0: &step0_extract::Step0Result,
    step2: &step2_rain_drop::Step2Result,
) -> Result<(), (ExitCode, String)> {
    write_step2_rain_svg(&step_svg_path(output_path, "step2_rain"), step0, step2)
        .map_err(|e| (ExitCode::from(4), format!("Error: {e}")))?;
    write_step2_anti_rain_svg(&step_svg_path(output_path, "step2_anti_rain"), step0, step2)
        .map_err(|e| (ExitCode::from(4), format!("Error: {e}")))?;
    Ok(())
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

    let config =
        Config::load(&args.config).map_err(|e| (ExitCode::from(3), format!("Error: {e}")))?;

    // Every run gets its own timestamped folder under --results, so nothing
    // from an earlier run is silently overwritten. The directory is only
    // actually created once something is about to be written into it (below,
    // under --create_svg) -- a run that produces nothing leaves no folder.
    let started = chrono::Local::now();
    let run_dir = args.results.join(format!(
        "contours_to_raster_{}",
        started.format("%Y_%m_%d__%H_%M_%S")
    ));
    let output_name = args
        .output_file
        .as_deref()
        .and_then(Path::file_name)
        .map(PathBuf::from)
        .unwrap_or_else(|| default_output_name(&args.map_file));
    let output_path = run_dir.join(&output_name);

    let (map, read_warnings) =
        read_xml_map(&args.map_file).map_err(|e| (ExitCode::from(2), format!("Error: {e}")))?;
    for warning in &read_warnings {
        eprintln!("Warning: {warning}");
    }

    // Created before Step 0 even runs (rather than only once it succeeds),
    // so a Contour Raster conflict it cannot recover from can still write
    // its own diagnostic picture into this run's folder before the whole
    // run fails.
    if args.create_svg {
        std::fs::create_dir_all(&run_dir).map_err(|e| {
            (
                ExitCode::from(4),
                format!("cannot make {}: {e}", run_dir.display()),
            )
        })?;
    }

    let mut step0 = match step0_extract::extract(&map, &config) {
        Ok(step0) => step0,
        Err(ExtractError::Conflict {
            message,
            diagnostics,
        }) => {
            if args.create_svg {
                let conflict_path = step_svg_path(&output_path, "conflict");
                match write_conflict_svg(&conflict_path, &diagnostics) {
                    Ok(()) => eprintln!(
                        "Note: wrote {} showing the two colliding contours before failing.",
                        conflict_path.display()
                    ),
                    Err(e) => {
                        eprintln!("Warning: could not write {}: {e}", conflict_path.display())
                    }
                }
            }
            return Err((ExitCode::from(4), format!("Error: {message}")));
        }
        Err(ExtractError::Message(message)) => {
            return Err((ExitCode::from(4), format!("Error: {message}")));
        }
    };
    for warning in &step0.warnings {
        eprintln!("Warning: {warning}");
    }

    // Each SVG is written right after its own step, before the next step
    // mutates `step0.contours` further -- writing all four only at the end
    // (against the same, by-then fully-resolved `step0`) would make the
    // "step0" and "step1" files silently show the final state instead of
    // their own step's.
    if args.create_svg {
        write_step0_svg(&step_svg_path(&output_path, "step0"), &step0)
            .map_err(|e| (ExitCode::from(4), format!("Error: {e}")))?;
    }

    let step1 = step1_obvious_gravity::resolve(
        &mut step0.contours,
        &step0.point_definers,
        &step0.line_definers,
        &step0.raster,
    )
    .map_err(|e| (ExitCode::from(5), format!("Error: {e}")))?;
    for warning in &step1.warnings {
        eprintln!("Warning: {warning}");
    }

    if args.create_svg {
        write_step1_svg(&step_svg_path(&output_path, "step1"), &step0)
            .map_err(|e| (ExitCode::from(4), format!("Error: {e}")))?;
    }

    let step2 = if step1.still_undefined.is_empty() {
        None
    } else {
        let (result, outcome) =
            step2_rain_drop::resolve(&mut step0.contours, &step0.raster, &config);
        for warning in &result.ambiguous_warnings {
            eprintln!("Warning: {warning}");
        }
        // Written from the partial result even on failure below (`result`
        // holds every simulated path regardless of `outcome`), so a
        // still-undefined contour can actually be inspected rather than
        // just reported.
        if args.create_svg {
            write_step2_svgs(&output_path, &step0, &result)?;
            if outcome.is_err() {
                eprintln!(
                    "Note: wrote {} and {} for inspection despite the failure below.",
                    step_svg_path(&output_path, "step2_rain").display(),
                    step_svg_path(&output_path, "step2_anti_rain").display(),
                );
            }
        }
        outcome.map_err(|e| (ExitCode::from(5), format!("Error: {e}")))?;
        Some(result)
    };

    if args.create_svg {
        if step2.is_none() {
            // Every contour was already resolved before Step 2 ran: write
            // the same picture Step 1 saw, so all four files always exist
            // together under --create_svg.
            let empty_step2 = step2_rain_drop::Step2Result {
                resolved_by_rain: 0,
                resolved_by_anti_rain: 0,
                ambiguous_warnings: Vec::new(),
                defined_after_rain: vec![true; step0.contours.len()],
                rain_paths: Vec::new(),
                anti_rain_paths: Vec::new(),
                rain_hysteresis_points: Vec::new(),
                anti_rain_hysteresis_points: Vec::new(),
                rain_vote_segments: Vec::new(),
                anti_rain_vote_segments: Vec::new(),
            };
            write_step2_svgs(&output_path, &step0, &empty_step2)?;
        }
        // Written last, against `step0` only after every contour's gravity
        // is fully settled (Step 2 having returned `Ok` above, or having
        // been skipped because Step 1 already resolved everything) -- the
        // algorithm's actual answer, not a per-step snapshot.
        write_final_svg(&step_svg_path(&output_path, "final"), &step0)
            .map_err(|e| (ExitCode::from(4), format!("Error: {e}")))?;
        println!(
            "wrote {}, {}, {}, {} and {}",
            step_svg_path(&output_path, "step0").display(),
            step_svg_path(&output_path, "step1").display(),
            step_svg_path(&output_path, "step2_rain").display(),
            step_svg_path(&output_path, "step2_anti_rain").display(),
            step_svg_path(&output_path, "final").display(),
        );
    }

    let contour_count = step0.contours.len();
    let resolved_by_points = step1.resolved_by_points;
    let resolved_by_lines = step1.resolved_by_lines;
    let resolved_by_hill = step1.resolved_by_hill;
    let (resolved_by_rain, resolved_by_anti_rain) = step2
        .as_ref()
        .map(|s| (s.resolved_by_rain, s.resolved_by_anti_rain))
        .unwrap_or((0, 0));

    println!(
        "{}: {contour_count} contours; gravity resolved by {resolved_by_points} slope \
         line/heavy-object reading(s), {resolved_by_lines} jump(s), {resolved_by_hill} closed \
         hill(s), {resolved_by_rain} rain drop(s), {resolved_by_anti_rain} anti rain drop(s)",
        args.map_file.display(),
    );

    eprintln!(
        "Note: Step 3 (elevation assignment) is not yet implemented; {} was not written.",
        output_path.display()
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
