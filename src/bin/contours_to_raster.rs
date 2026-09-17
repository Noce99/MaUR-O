//! Extracts elevation-gravity information from an .omap's contours --
//! `Contours-to-Raster.md`'s Steps 1 through 4. Step 5 (writing the final
//! elevation TIFF) is still `TO DO` in the doc, so this binary works out,
//! for every contour, which way is downhill and its own relative elevation,
//! and stops there: no TIFF is written yet.
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
//! this interface does not need to change once Step 5 lands.
//!
//! Exit codes: 0 success, 1 usage error, 2 the map could not be read, 3 the
//! config file could not be read or parsed, 4 the run folder or a
//! `--create_svg` file could not be written, or a Step 1 geometry error (a
//! degenerate buffer polygon -- a raster conflict no longer fails the run at
//! all, see `Contours-to-Raster.md`'s Step 1), or 5 a Step 2/Step 3 gravity
//! conflict, or a Step 4 tree invariant violation (a contour found to be its
//! own ancestor -- see `step4_elevation::resolve`).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;

use maur_o::contours_to_raster_config::{Config, DEFAULT_CONFIG_PATH};
use maur_o::contours_to_raster_svg::{
    write_contours_function_svg, write_final_svg, write_step1_close_search_svg,
    write_step1_growing_svg, write_step1_svg, write_step2_svg, write_step3_anti_rain_svg,
    write_step3_rain_svg, write_step4_svg,
};
use maur_o::step1_extract;
use maur_o::step2_obvious_gravity;
use maur_o::step3_rain_drop;
use maur_o::step4_elevation;
use maur_o::xml_reader::read_xml_map;

#[derive(Parser)]
#[command(
    name = "contours_to_raster",
    version,
    about = "Extracts elevation-gravity information from an .omap's contours (Contours-to-Raster.md, \
             Steps 1-4). Step 5 is not yet implemented, so no TIFF is written yet."
)]
struct Args {
    /// The .omap file to read.
    map_file: PathBuf,

    /// The file name the elevation TIFF would be written as, and what the
    /// --create_svg files are named after. Any directory given here is
    /// ignored -- everything this run produces goes inside its own folder
    /// under --results. Defaults to the map file's own name with a .tif
    /// suffix. Unused until Step 5 exists.
    output_file: Option<PathBuf>,

    /// Where this run's own timestamped folder is created.
    #[arg(long, default_value = "Results")]
    results: PathBuf,

    /// The config file with the algorithm's parameters.
    #[arg(long, default_value = DEFAULT_CONFIG_PATH)]
    config: PathBuf,

    /// Write the eight per-step validation SVGs Contours-to-Raster.md's
    /// "Visualization" section describes (Step 1 before its own Growing
    /// Process sub-step, after that sub-step's own Close Search pass, after
    /// its Seeking phase, after its Matching phase, Step 2, Step 3's Rain
    /// and Anti Rain Drop Productions each in their own file, and Step 4), an
    /// unnumbered "final" SVG with
    /// just the algorithm's actual answer once every contour's gravity is
    /// resolved, an unnumbered "contours_function" SVG plotting the
    /// contour-pixel force curve itself (a function of --config alone, not of
    /// the map), and a full-raster, every-pixel-colored PNG (the vector SVGs
    /// only square a contour or high-density pixel, to keep their file size
    /// sane), all inside this run's own folder.
    #[arg(long = "create_svg")]
    create_svg: bool,
}

/// The default output file name for the given input path: the map file's
/// own name, with the suffix replaced by ".tif".
fn default_output_name(map_path: &Path) -> PathBuf {
    Path::new(map_path.file_stem().unwrap_or_default()).with_extension("tif")
}

/// `<prefix>_<output>_<step>.svg`, next to `output_path` -- the eight
/// numbered `--create_svg` files (`00`.."07", see the doc's Visualization
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

    // `growing_enabled = 0.0` skips Step 1's whole Growing Process (Close
    // Search, Seeking and Matching alike): every contour is left exactly as
    // extracted, dangling Flying Ends and all, and none of the three passes'
    // own `01_..._step1_close_search.svg`/`02_..._step1_growing_seeking.svg`/
    // `03_..._step1_growing_matching.svg` are written, since none of them
    // ran for those files to show -- `00_..._step1.svg` above already covers
    // the pre-growing state.
    if config.growing_enabled != 0.0 {
        // Close Search: a cheap, non-iterative pass that resolves whatever
        // Flying Ends are already essentially where they need to be --
        // another Flying End or an out-of-bound pixel directly in front of
        // them -- before Seeking/Matching ever take an integration step.
        // `01_..._step1_close_search.svg` shows the result, plus every
        // search cone it actually used.
        step1_extract::run_growing_close_search(&mut step1, &config);

        if args.create_svg {
            write_step1_close_search_svg(
                &numbered_svg_path(&output_path, "01", "step1_close_search"),
                &step1,
            )
            .map_err(|e| (ExitCode::from(4), format!("Error: {e}")))?;
        }

        // The Growing Process's own two remaining phases (Seeking then
        // Matching -- see `Contours-to-Raster.md`'s Growing Process
        // section), each with its own `--create_svg` file:
        // `02_..._step1_growing_seeking.svg` shows what Phase 1
        // (matching-free, chasing the out-of-bound area on its own) managed
        // alone, `03_..._step1_growing_matching.svg` what Phase 2 (the full
        // process, restored merging) went on to do with whatever Phase 1
        // didn't resolve.
        let (growing_state, seeking_warnings) =
            step1_extract::run_growing_seeking(&mut step1, &config);
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
    }

    // Heavy Object gravity is resolved only now, against each contour's
    // final geometry -- post-Matching's, or, with `growing_enabled = 0.0`,
    // whatever Step 1 extracted it as, dangling ends and all (see
    // `resolve_heavy_object_gravity`'s own doc comment for why post-Matching
    // is preferred) -- so this must run after the Growing Process above and
    // before `03_..._step1_growing_matching.svg` is written, the first file
    // meant to show a Heavy Object's own arrow.
    let heavy_object_warnings = step1_extract::resolve_heavy_object_gravity(&mut step1, &config);
    for warning in &heavy_object_warnings {
        eprintln!("Warning: {warning}");
    }
    step1.warnings.extend(heavy_object_warnings);

    if args.create_svg {
        if config.growing_enabled != 0.0 {
            write_step1_growing_svg(
                &numbered_svg_path(&output_path, "03", "step1_growing_matching"),
                &step1,
                &config,
            )
            .map_err(|e| (ExitCode::from(4), format!("Error: {e}")))?;
        }
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
        &config,
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
        let result = step3_rain_drop::resolve(
            &mut step1.contours,
            &mut step1.raster,
            &step1.line_definers,
            &config,
        );
        for warning in &result.warnings {
            eprintln!("Warning: {warning}");
        }
        if args.create_svg {
            write_step3_svgs(&output_path, &step1, &result)?;
        }
        Some(result)
    };

    if args.create_svg && step3.is_none() {
        // Every contour was already resolved before Step 3 ran: write
        // the same picture Step 2 saw, so every numbered file that depends
        // on it still exists under --create_svg.
        let empty_step3 = step3_rain_drop::Step3Result {
            resolved_by_votes: 0,
            warnings: Vec::new(),
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

    let step4 = step4_elevation::resolve(&mut step1.contours, &mut step1.raster, &config);
    for warning in &step4.warnings {
        eprintln!("Warning: {warning}");
    }

    if args.create_svg {
        // Written after Step 4, against `step1` only once every contour's
        // gravity is fully settled (Step 3 having run above, or having been
        // skipped because Step 2 already resolved everything) -- the
        // algorithm's actual gravity answer, not a per-step snapshot. Step
        // 4's own mutation of `step1.raster` (erasing a dropped contour's own
        // footprint, see `step4_elevation::resolve`) does not affect this
        // file, since it never draws that raster's pixel layer for a contour
        // gravity itself already left undefined.
        write_final_svg(&step_svg_path(&output_path, "final"), &step1)
            .map_err(|e| (ExitCode::from(4), format!("Error: {e}")))?;
        write_step4_svg(&numbered_svg_path(&output_path, "07", "step4"), &step1, &step4)
            .map_err(|e| (ExitCode::from(4), format!("Error: {e}")))?;
        println!(
            "wrote {}, {}, {}, {}, {}, {}, {}, {}, {}, {} and {}",
            step_svg_path(&output_path, "contours_function").display(),
            numbered_svg_path(&output_path, "00", "step1").display(),
            numbered_svg_path(&output_path, "01", "step1_close_search").display(),
            numbered_svg_path(&output_path, "02", "step1_growing_seeking").display(),
            numbered_svg_path(&output_path, "03", "step1_growing_matching").display(),
            raster_png_path(&output_path).display(),
            numbered_svg_path(&output_path, "04", "step2").display(),
            numbered_svg_path(&output_path, "05", "step3_rain").display(),
            numbered_svg_path(&output_path, "06", "step3_anti_rain").display(),
            numbered_svg_path(&output_path, "07", "step4").display(),
            step_svg_path(&output_path, "final").display(),
        );
    }

    let contour_count = step1.contours.len();
    let resolved_by_slope_line = step2.resolved_by_slope_line;
    let resolved_by_hill = step2.resolved_by_hill;
    let resolved_by_vote = step2.resolved_by_vote;
    let resolved_by_rain_drop_votes = step3.as_ref().map(|s| s.resolved_by_votes).unwrap_or(0);

    println!(
        "{}: {contour_count} contours; gravity resolved by {resolved_by_slope_line} slope \
         line(s), {resolved_by_hill} closed hill(s), {resolved_by_vote} heavy-object/jump \
         vote(s), {resolved_by_rain_drop_votes} rain/anti-rain drop vote(s)",
        args.map_file.display(),
    );
    println!(
        "elevation resolved for {}/{contour_count} contours ({} dropped, still undefined)",
        step4.resolved,
        contour_count as u64 - step4.resolved,
    );

    eprintln!(
        "Note: Step 5 (final TIFF) is not yet implemented; {} was not written.",
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
