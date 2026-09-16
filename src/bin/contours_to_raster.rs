//! Extracts elevation-gravity information from an .omap's contours --
//! `Contours-to-Raster.md`'s Steps 1, 2 and 3. Step 4 (actually assigning an
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
//! this interface does not need to change once Step 4 lands.
//!
//! Exit codes: 0 success, 1 usage error, 2 the map could not be read, 3 the
//! config file could not be read or parsed, 4 the run folder or a
//! `--create_svg` file could not be written, or a Step 1 geometry error (a
//! degenerate buffer polygon -- a raster conflict no longer fails the run at
//! all, see `Contours-to-Raster.md`'s Step 1), or 5 a Step 2/Step 3 gravity
//! conflict, or a contour left undefined after both rain-drop passes.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;

use maur_o::contours_to_raster_config::{Config, DEFAULT_CONFIG_PATH};
use maur_o::contours_to_raster_svg::{
    write_contours_function_svg, write_final_svg, write_step1_growing_svg, write_step1_svg,
    write_step2_svg, write_step3_anti_rain_svg, write_step3_rain_svg,
};
use maur_o::step1_extract;
use maur_o::step2_obvious_gravity;
use maur_o::step3_rain_drop;
use maur_o::xml_reader::read_xml_map;

#[derive(Parser)]
#[command(
    name = "contours_to_raster",
    version,
    about = "Extracts elevation-gravity information from an .omap's contours (Contours-to-Raster.md, \
             Steps 1-3). Step 4 is not yet implemented, so no TIFF is written yet."
)]
struct Args {
    /// The .omap file to read.
    map_file: PathBuf,

    /// The file name the elevation TIFF would be written as, and what the
    /// --create_svg files are named after. Any directory given here is
    /// ignored -- everything this run produces goes inside its own folder
    /// under --results. Defaults to the map file's own name with a .tif
    /// suffix. Unused until Step 4 exists.
    output_file: Option<PathBuf>,

    /// Where this run's own timestamped folder is created.
    #[arg(long, default_value = "Results")]
    results: PathBuf,

    /// The config file with the algorithm's parameters.
    #[arg(long, default_value = DEFAULT_CONFIG_PATH)]
    config: PathBuf,

    /// Write the seven per-step validation SVGs Contours-to-Raster.md's
    /// "Visualization" section describes (Step 1 before its own Growing
    /// Process sub-step, after that sub-step's own Close Search pass, after
    /// its Seeking phase, after its Matching phase, Step 2, and Step 3's
    /// Rain and Anti Rain Drop Productions each in their own file), an
    /// unnumbered "final" SVG with
    /// just the algorithm's actual answer once every contour is resolved, an
    /// unnumbered "contours_function" SVG plotting the contour-pixel force
    /// curve itself (a function of --config alone, not of the map), and a
    /// full-raster, every-pixel-colored PNG (the vector SVGs only square a
    /// contour or high-density pixel, to keep their file size sane), all
    /// inside this run's own folder.
    #[arg(long = "create_svg")]
    create_svg: bool,
}

/// The default output file name for the given input path: the map file's
/// own name, with the suffix replaced by ".tif".
fn default_output_name(map_path: &Path) -> PathBuf {
    Path::new(map_path.file_stem().unwrap_or_default()).with_extension("tif")
}

/// `<prefix>_<output>_<step>.svg`, next to `output_path` -- the seven
/// numbered `--create_svg` files (`00`.."06", see the doc's Visualization
/// section).
fn numbered_svg_path(output_path: &Path, prefix: &str, step: &str) -> PathBuf {
    let stem = output_path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy();
    output_path.with_file_name(format!("{prefix}_{stem}_{step}.svg"))
}

/// `<output>_<step>.svg`, next to `output_path` -- for the unnumbered "final"
/// file, which the rename to `00`.."06" doesn't touch.
fn step_svg_path(output_path: &Path, step: &str) -> PathBuf {
    let stem = output_path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy();
    output_path.with_file_name(format!("{stem}_{step}.svg"))
}

/// `<output>_raster.png`, next to `output_path` -- the full-raster,
/// every-pixel-colored companion to the (deliberately sparser) vector SVGs.
fn raster_png_path(output_path: &Path) -> PathBuf {
    let stem = output_path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy();
    output_path.with_file_name(format!("{stem}_raster.png"))
}

/// Step 3's two files: one with every Rain Drop Production drop's path, one
/// with every Anti Rain Drop Production drop's -- kept apart so a drop's
/// path stays legible where the two passes cross, rather than overlaying
/// both colors on one picture.
fn write_step3_svgs(
    output_path: &Path,
    step1: &step1_extract::Step1Result,
    step3: &step3_rain_drop::Step3Result,
) -> Result<(), (ExitCode, String)> {
    write_step3_rain_svg(
        &numbered_svg_path(output_path, "05", "step3_rain"),
        step1,
        step3,
    )
    .map_err(|e| (ExitCode::from(4), format!("Error: {e}")))?;
    write_step3_anti_rain_svg(
        &numbered_svg_path(output_path, "06", "step3_anti_rain"),
        step1,
        step3,
    )
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

    if args.create_svg {
        std::fs::create_dir_all(&run_dir).map_err(|e| {
            (
                ExitCode::from(4),
                format!("cannot make {}: {e}", run_dir.display()),
            )
        })?;
        // A plot of the contour-pixel force curve itself, depending only on
        // `config` -- unlike every other --create_svg file, so it is written
        // here rather than alongside the map-derived step it would
        // otherwise belong to.
        write_contours_function_svg(
            &step_svg_path(&output_path, "contours_function"),
            &config,
        )
        .map_err(|e| (ExitCode::from(4), format!("Error: {e}")))?;
    }

    let mut step1 = step1_extract::extract(&map, &config)
        .map_err(|e| (ExitCode::from(4), format!("Error: {e}")))?;
    for warning in &step1.warnings {
        eprintln!("Warning: {warning}");
    }

    // Written before Step 1's own Growing Process mutates `step1.contours`
    // and `step1.raster` further, so `00_..._step1.svg` shows the pre-growing
    // state -- Flying-End rings on contours that haven't grown yet.
    if args.create_svg {
        write_step1_svg(&numbered_svg_path(&output_path, "00", "step1"), &step1)
            .map_err(|e| (ExitCode::from(4), format!("Error: {e}")))?;
    }

    // Close Search: a cheap, non-iterative pass that resolves whatever
    // Flying Ends are already essentially where they need to be -- another
    // Flying End or an out-of-bound pixel directly in front of them -- before
    // Seeking/Matching ever take an integration step. `01_..._step1_close_search.svg`
    // shows the result; it needs no dedicated writer, since Close Search
    // takes no integration steps of its own to draw push/pull vectors or
    // dots for (see `write_step1_svg`'s own doc comment).
    step1_extract::run_growing_close_search(&mut step1, &config);

    if args.create_svg {
        write_step1_svg(
            &numbered_svg_path(&output_path, "01", "step1_close_search"),
            &step1,
        )
        .map_err(|e| (ExitCode::from(4), format!("Error: {e}")))?;
    }

    // The Growing Process's own two remaining phases (Seeking then Matching
    // -- see `Contours-to-Raster.md`'s Growing Process section), each with
    // its own `--create_svg` file: `02_..._step1_growing_seeking.svg` shows
    // what Phase 1 (matching-free, chasing the out-of-bound area on its own)
    // managed alone, `03_..._step1_growing_matching.svg` what Phase 2 (the
    // full process, restored merging) went on to do with whatever Phase 1
    // didn't resolve.
    let (growing_state, seeking_warnings) = step1_extract::run_growing_seeking(&mut step1, &config);
    for warning in &seeking_warnings {
        eprintln!("Warning: {warning}");
    }
    step1.warnings.extend(seeking_warnings);

    if args.create_svg {
        write_step1_growing_svg(
            &numbered_svg_path(&output_path, "02", "step1_growing_seeking"),
            &step1,
            &config,
        )
        .map_err(|e| (ExitCode::from(4), format!("Error: {e}")))?;
    }

    let matching_warnings =
        step1_extract::run_growing_matching(&mut step1, &config, growing_state);
    for warning in &matching_warnings {
        eprintln!("Warning: {warning}");
    }
    step1.warnings.extend(matching_warnings);

    if args.create_svg {
        write_step1_growing_svg(
            &numbered_svg_path(&output_path, "03", "step1_growing_matching"),
            &step1,
            &config,
        )
        .map_err(|e| (ExitCode::from(4), format!("Error: {e}")))?;
        // The Contour Raster itself never changes again past this point
        // (only contours' own gravity does, in Steps 2/3), so this is the
        // one point a full-raster, every-pixel-colored PNG companion to the
        // (deliberately sparser) vector SVGs is worth writing.
        step1
            .raster
            .write_png(&raster_png_path(&output_path))
            .map_err(|e| (ExitCode::from(4), format!("Error: {e}")))?;
    }

    let step2 = step2_obvious_gravity::resolve(
        &mut step1.contours,
        &step1.point_definers,
        &step1.line_definers,
    )
    .map_err(|e| (ExitCode::from(5), format!("Error: {e}")))?;
    for warning in &step2.warnings {
        eprintln!("Warning: {warning}");
    }

    if args.create_svg {
        write_step2_svg(&numbered_svg_path(&output_path, "04", "step2"), &step1)
            .map_err(|e| (ExitCode::from(4), format!("Error: {e}")))?;
    }

    let step3 = if step2.still_undefined.is_empty() {
        None
    } else {
        let (result, outcome) =
            step3_rain_drop::resolve(&mut step1.contours, &mut step1.raster, &config);
        for warning in &result.ambiguous_warnings {
            eprintln!("Warning: {warning}");
        }
        // Written from the partial result even on failure below (`result`
        // holds every simulated path regardless of `outcome`), so a
        // still-undefined contour can actually be inspected rather than
        // just reported.
        if args.create_svg {
            write_step3_svgs(&output_path, &step1, &result)?;
            if outcome.is_err() {
                eprintln!(
                    "Note: wrote {} and {} for inspection despite the failure below.",
                    numbered_svg_path(&output_path, "05", "step3_rain").display(),
                    numbered_svg_path(&output_path, "06", "step3_anti_rain").display(),
                );
            }
        }
        outcome.map_err(|e| (ExitCode::from(5), format!("Error: {e}")))?;
        Some(result)
    };

    if args.create_svg {
        if step3.is_none() {
            // Every contour was already resolved before Step 3 ran: write
            // the same picture Step 2 saw, so all seven numbered files
            // always exist together under --create_svg.
            let empty_step3 = step3_rain_drop::Step3Result {
                resolved_by_rain: 0,
                resolved_by_anti_rain: 0,
                ambiguous_warnings: Vec::new(),
                defined_after_rain: vec![true; step1.contours.len()],
                rain_paths: Vec::new(),
                anti_rain_paths: Vec::new(),
                rain_hysteresis_points: Vec::new(),
                anti_rain_hysteresis_points: Vec::new(),
                rain_vote_segments: Vec::new(),
                anti_rain_vote_segments: Vec::new(),
            };
            write_step3_svgs(&output_path, &step1, &empty_step3)?;
        }
        // Written last, against `step1` only after every contour's gravity
        // is fully settled (Step 3 having returned `Ok` above, or having
        // been skipped because Step 2 already resolved everything) -- the
        // algorithm's actual answer, not a per-step snapshot.
        write_final_svg(&step_svg_path(&output_path, "final"), &step1)
            .map_err(|e| (ExitCode::from(4), format!("Error: {e}")))?;
        println!(
            "wrote {}, {}, {}, {}, {}, {}, {}, {}, {} and {}",
            step_svg_path(&output_path, "contours_function").display(),
            numbered_svg_path(&output_path, "00", "step1").display(),
            numbered_svg_path(&output_path, "01", "step1_close_search").display(),
            numbered_svg_path(&output_path, "02", "step1_growing_seeking").display(),
            numbered_svg_path(&output_path, "03", "step1_growing_matching").display(),
            raster_png_path(&output_path).display(),
            numbered_svg_path(&output_path, "04", "step2").display(),
            numbered_svg_path(&output_path, "05", "step3_rain").display(),
            numbered_svg_path(&output_path, "06", "step3_anti_rain").display(),
            step_svg_path(&output_path, "final").display(),
        );
    }

    let contour_count = step1.contours.len();
    let resolved_by_points = step2.resolved_by_points;
    let resolved_by_lines = step2.resolved_by_lines;
    let resolved_by_hill = step2.resolved_by_hill;
    let (resolved_by_rain, resolved_by_anti_rain) = step3
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
        "Note: Step 4 (elevation assignment) is not yet implemented; {} was not written.",
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
