//! In-memory, no-disk-I/O variant of the `contours_to_raster` binary
//! (`src/bin/contours_to_raster.rs`): runs the same Steps 1 through 5
//! (`Contours-to-Raster.md`) against an already-parsed [`Map`], reporting
//! progress through a callback instead of `--create_svg` files, and
//! returning the final elevation grid in memory instead of writing a TIFF.
//! Exists so a caller with no filesystem (wasm) can drive the exact same
//! algorithm the CLI binary does.

use crate::contours_to_raster_config::Config;
use crate::contours_to_raster_svg;
use crate::map::Map;
use crate::{step1_extract, step2_obvious_gravity, step3_rain_drop, step4_elevation};
use crate::{step5_elevation_raster, step5_gravity_raster};

/// The final answer: one elevation value per Contour Raster pixel, plus the
/// grid's own placement and the run's summary counts.
pub struct DemFromContoursResult {
    /// Row-major elevation values, `width * height` long, row 0 = smallest
    /// y (the map's/`ContourRaster`'s own "up" -- see
    /// [`crate::contour_raster::ContourRaster::origin`]'s own doc comment).
    /// Already multiplied by `equidistance` (an elevation in meters, up to
    /// an unknown absolute baseline -- see [`step5_elevation_raster::write_tiff`]'s
    /// own doc comment), `NaN` where a pixel never got a value at all
    /// (`still_undefined`).
    pub elev: Vec<f32>,
    /// Grid width, in pixels.
    pub width: usize,
    /// Grid height, in pixels.
    pub height: usize,
    /// Pixel size, in ground meters -- `config.rasterization_px_size`,
    /// passed straight through.
    pub px_size_m: f64,
    /// World-space (ground meters) position of pixel `(0, 0)`'s low corner
    /// -- the Contour Raster's own `origin`, passed straight through. Ground
    /// meters here means this crate's own flat, origin-free
    /// `scale_denominator`-derived scale (see
    /// [`crate::contour_geometry::meters_per_mm`]'s own doc comment), not a
    /// real-world CRS -- converting back to the map's native units is left
    /// to the caller, which is expected to already know that map's own
    /// `scale_denominator`.
    pub origin_ground: (f64, f64),
    /// How many contours were extracted in total.
    pub contour_count: usize,
    /// How many contours had their gravity resolved by a Slope Line
    /// reading (Step 2).
    pub resolved_by_slope_line: u64,
    /// How many contours had their gravity resolved by the closed-hill
    /// heuristic (Step 2).
    pub resolved_by_hill: u64,
    /// How many contours had their gravity resolved by a Heavy
    /// Object/Jump confidence-weighted vote (Step 2).
    pub resolved_by_vote: u64,
    /// How many contours had their gravity resolved by a Rain/Anti Rain
    /// Drop Production vote (Step 3) -- `0` if Step 3 never ran because
    /// Step 2 already resolved everything.
    pub resolved_by_rain_drop_votes: u64,
    /// How many contours got an `elevation_height` at all (Step 4), out of
    /// `contour_count`.
    pub step4_resolved: u64,
    /// How many in-bound pixels got a value seeded directly from a
    /// contour's own `elevation_height` or a Gravity-Guided Elevation
    /// Fill drop's own track, before gap-filling ran (Step 5).
    pub filled_by_rain: u64,
    /// How many more in-bound pixels got a value from Step 5's
    /// gap-filling pass instead.
    pub filled_by_gap_fill: u64,
    /// How many in-bound pixels still have no value at all once
    /// gap-filling ran to completion.
    pub still_undefined: u64,
    /// `(name, svg)` pairs, one per `--create_svg` diagnostic this run built
    /// (see [`compute`]'s own `debug_svg` argument) -- empty when that
    /// argument was `false`. Names match `src/bin/contours_to_raster.rs`'s
    /// own step names (`"contours_function"`, `"step1"`,
    /// `"step1_close_search"`, `"step1_growing_seeking"`,
    /// `"step1_growing_matching"`, `"step2"`, `"step3_rain"`,
    /// `"step3_anti_rain"`, `"final"`, `"step4"`, `"step5_gravity"`) minus
    /// the CLI's two PNG outputs (full-raster and hypsometric-elevation),
    /// which have no in-memory SVG equivalent, and minus the two Step 3 SVGs
    /// when Step 3 never ran (Step 2 already resolved every contour) --
    /// unlike the CLI, which still writes them empty for consistent file
    /// numbering, a caller collecting named results has no such numbering to
    /// keep consistent. Coordinates are ground meters, this crate's own
    /// native unit throughout (see `origin_ground`'s own doc comment) --
    /// converting to the caller's own units is left to the caller.
    pub debug_svgs: Vec<(String, String)>,
}

/// Runs Steps 1 through 5 against `map`, calling `progress(stage, message)`
/// once per named stage as it completes (`"extract"`, `"close_search"`,
/// `"seeking"`, `"matching"`, `"heavy_object_gravity"`, `"step2"`,
/// optionally `"step3"` (only if Step 2 left contours undefined),
/// `"step4"`, `"step5_gravity"`, `"step5_elevation"`), each call carrying a
/// short human-readable summary of what that stage found -- the same
/// information `src/bin/contours_to_raster.rs`'s own `run()` prints to
/// stdout/stderr, just handed to a callback instead. Every warning a step
/// would otherwise `eprintln!` is forwarded the same way, tagged with that
/// step's own stage name.
///
/// `debug_svg` mirrors the CLI binary's own `--create_svg`: when `true`,
/// every `--create_svg` diagnostic this pipeline can produce in memory (see
/// [`DemFromContoursResult::debug_svgs`]) is built at the same point in the
/// pipeline the CLI writes its own file, using this crate's `Step1Result`
/// state at that exact moment -- the two Growing Process SVGs
/// (`"step1_growing_seeking"`/`"step1_growing_matching"`) in particular
/// share one drawing function called twice against the same
/// progressively-mutated state, exactly as the CLI does. `false` skips all
/// of it, at no extra cost over the pre-existing behavior.
///
/// Returns an error (from Step 1's extraction, or Step 2/Step 3's own
/// gravity-conflict checks) exactly where the CLI binary would exit
/// non-zero for the same reason.
pub fn compute(
    map: &Map,
    config: &Config,
    equidistance: f64,
    debug_svg: bool,
    progress: &mut dyn FnMut(&str, &str),
) -> Result<DemFromContoursResult, String> {
    let mut debug_svgs: Vec<(String, String)> = Vec::new();
    if debug_svg {
        debug_svgs.push((
            "contours_function".to_string(),
            contours_to_raster_svg::contours_function_svg_string(config),
        ));
    }

    let mut step1 = step1_extract::extract(map, config).map_err(|e| e.message().to_string())?;
    for warning in &step1.warnings {
        progress("extract", &format!("Warning: {warning}"));
    }
    progress(
        "extract",
        &format!("extracted {} contour(s)", step1.contours.len()),
    );
    if debug_svg {
        debug_svgs.push((
            "step1".to_string(),
            contours_to_raster_svg::step1_svg_string(&step1),
        ));
    }

    if config.growing_enabled != 0.0 {
        step1_extract::run_growing_close_search(&mut step1, config);
        progress("close_search", "ran Growing Process close search");
        if debug_svg {
            debug_svgs.push((
                "step1_close_search".to_string(),
                contours_to_raster_svg::step1_close_search_svg_string(&step1),
            ));
        }

        let (growing_state, seeking_warnings) =
            step1_extract::run_growing_seeking(&mut step1, config);
        for warning in &seeking_warnings {
            progress("seeking", &format!("Warning: {warning}"));
        }
        step1.warnings.extend(seeking_warnings);
        progress("seeking", "ran Growing Process seeking phase");
        if debug_svg {
            debug_svgs.push((
                "step1_growing_seeking".to_string(),
                contours_to_raster_svg::step1_growing_svg_string(&step1, config),
            ));
        }

        let matching_warnings =
            step1_extract::run_growing_matching(&mut step1, config, growing_state);
        for warning in &matching_warnings {
            progress("matching", &format!("Warning: {warning}"));
        }
        step1.warnings.extend(matching_warnings);
        progress("matching", "ran Growing Process matching phase");
    }

    let heavy_object_warnings = step1_extract::resolve_heavy_object_gravity(&mut step1, config);
    for warning in &heavy_object_warnings {
        progress("heavy_object_gravity", &format!("Warning: {warning}"));
    }
    step1.warnings.extend(heavy_object_warnings);
    progress(
        "heavy_object_gravity",
        "resolved Heavy Object gravity readings",
    );
    // Matches the CLI's own ordering: "step1_growing_matching" is captured
    // here, after Heavy Object gravity, not right after run_growing_matching
    // above -- by now it (like the CLI's own file) shows a Heavy Object's
    // own arrow too.
    if debug_svg && config.growing_enabled != 0.0 {
        debug_svgs.push((
            "step1_growing_matching".to_string(),
            contours_to_raster_svg::step1_growing_svg_string(&step1, config),
        ));
    }

    let step2 = step2_obvious_gravity::resolve(
        &mut step1.contours,
        &step1.point_definers,
        &step1.line_definers,
        config,
    )?;
    for warning in &step2.warnings {
        progress("step2", &format!("Warning: {warning}"));
    }
    progress(
        "step2",
        &format!(
            "gravity resolved by {} slope line(s), {} closed hill(s), {} heavy-object/jump \
             vote(s)",
            step2.resolved_by_slope_line, step2.resolved_by_hill, step2.resolved_by_vote,
        ),
    );
    if debug_svg {
        debug_svgs.push((
            "step2".to_string(),
            contours_to_raster_svg::step2_svg_string(&step1),
        ));
    }

    let step3 = if step2.still_undefined.is_empty() {
        None
    } else {
        let result = step3_rain_drop::resolve(
            &mut step1.contours,
            &mut step1.raster,
            &step1.line_definers,
            config,
        );
        for warning in &result.warnings {
            progress("step3", &format!("Warning: {warning}"));
        }
        progress(
            "step3",
            &format!(
                "gravity resolved by {} rain/anti-rain drop vote(s)",
                result.resolved_by_votes
            ),
        );
        if debug_svg {
            debug_svgs.push((
                "step3_rain".to_string(),
                contours_to_raster_svg::step3_rain_svg_string(&step1, &result),
            ));
            debug_svgs.push((
                "step3_anti_rain".to_string(),
                contours_to_raster_svg::step3_anti_rain_svg_string(&step1, &result),
            ));
        }
        Some(result)
    };

    let step4 = step4_elevation::resolve(&mut step1.contours, &mut step1.raster, config);
    for warning in &step4.warnings {
        progress("step4", &format!("Warning: {warning}"));
    }
    let contour_count = step1.contours.len();
    progress(
        "step4",
        &format!(
            "elevation resolved for {}/{contour_count} contour(s) ({} dropped, still undefined)",
            step4.resolved,
            contour_count as u64 - step4.resolved,
        ),
    );

    let gravity = step5_gravity_raster::resolve(&step1.contours, &mut step1.raster, config);
    progress("step5_gravity", "resolved Step 5 gravity direction field");

    // Matches the CLI's own ordering: "final"/"step4"/"step5_gravity" are
    // all written together, right here, once gravity is fully settled.
    if debug_svg {
        debug_svgs.push((
            "final".to_string(),
            contours_to_raster_svg::final_svg_string(&step1),
        ));
        debug_svgs.push((
            "step4".to_string(),
            contours_to_raster_svg::step4_svg_string(&step1, &step4)?,
        ));
        debug_svgs.push((
            "step5_gravity".to_string(),
            contours_to_raster_svg::step5_gravity_svg_string(&step1, &gravity),
        ));
    }

    let step5 = step5_elevation_raster::resolve(&step1.contours, &step1.raster, &gravity, config);
    progress(
        "step5_elevation",
        &format!(
            "{} pixel(s) from contours/rain, {} more from gap-filling, {} still undefined",
            step5.filled_by_rain, step5.filled_by_gap_fill, step5.still_undefined,
        ),
    );

    let (width, height) = (step5.e2v.width, step5.e2v.height);
    let mut elev = Vec::with_capacity(width * height);
    for y in 0..height {
        for x in 0..width {
            elev.push(
                step5
                    .e2v
                    .get(x, y)
                    .map(|v| (v * equidistance) as f32)
                    .unwrap_or(f32::NAN),
            );
        }
    }

    Ok(DemFromContoursResult {
        elev,
        width,
        height,
        px_size_m: step1.raster.px_size,
        origin_ground: (step1.raster.origin.x, step1.raster.origin.y),
        contour_count,
        resolved_by_slope_line: step2.resolved_by_slope_line,
        resolved_by_hill: step2.resolved_by_hill,
        resolved_by_vote: step2.resolved_by_vote,
        resolved_by_rain_drop_votes: step3.as_ref().map(|s| s.resolved_by_votes).unwrap_or(0),
        step4_resolved: step4.resolved,
        filled_by_rain: step5.filled_by_rain,
        filled_by_gap_fill: step5.filled_by_gap_fill,
        still_undefined: step5.still_undefined,
        debug_svgs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xml_reader::read_xml_map;
    use std::path::Path;

    #[test]
    fn computes_a_non_trivial_elevation_grid() {
        // maps/contours_to_altitude_map_1.omap is one of this crate's own
        // purpose-built contours_to_raster test maps (see the sibling
        // contours_to_altitude_map_2..7.omap files).
        let (map, _warnings) = read_xml_map(Path::new("maps/contours_to_altitude_map_1.omap"))
            .expect("maps/contours_to_altitude_map_1.omap must be readable");
        let config = Config::default_shipped();
        let mut stages: Vec<String> = Vec::new();
        let result = compute(&map, &config, 5.0, false, &mut |stage, _message| {
            stages.push(stage.to_string());
        })
        .expect("compute must succeed on a map with contours");

        assert!(result.width > 0 && result.height > 0);
        assert_eq!(result.elev.len(), result.width * result.height);
        assert!(
            result.elev.iter().any(|v| v.is_finite()),
            "expected at least one defined elevation pixel"
        );
        assert!(result.contour_count > 0);
        assert!(stages.contains(&"extract".to_string()));
        assert!(stages.contains(&"step5_elevation".to_string()));
        assert!(
            result.debug_svgs.is_empty(),
            "debug_svg=false must build no SVGs"
        );
    }

    #[test]
    fn debug_svg_true_builds_every_expected_named_svg() {
        let (map, _warnings) = read_xml_map(Path::new("maps/contours_to_altitude_map_1.omap"))
            .expect("maps/contours_to_altitude_map_1.omap must be readable");
        let config = Config::default_shipped();
        let result = compute(&map, &config, 5.0, true, &mut |_stage, _message| {})
            .expect("compute must succeed on a map with contours");

        let names: Vec<&str> = result.debug_svgs.iter().map(|(n, _)| n.as_str()).collect();
        // "step3_rain"/"step3_anti_rain" are only present when Step 3 itself
        // ran (Step 2 left something undefined) -- not asserted here since
        // this fixture's own outcome isn't the point of this test.
        for expected in [
            "contours_function",
            "step1",
            "step1_close_search",
            "step1_growing_seeking",
            "step1_growing_matching",
            "step2",
            "final",
            "step4",
            "step5_gravity",
        ] {
            assert!(
                names.contains(&expected),
                "expected a {expected:?} debug SVG; got {names:?}"
            );
        }
        for (name, svg) in &result.debug_svgs {
            assert!(
                svg.trim_start().starts_with("<svg"),
                "{name}'s own SVG string must start with <svg, got: {}",
                &svg[..svg.len().min(80)]
            );
        }
    }
}
