//! Step 2 of `Contours-to-Raster.md`: Rain Drop Production and Anti Rain
//! Drop Production, for every contour Step 1 left without a gravity
//! direction (its point/line evidence and closed-hill heuristic).

use std::collections::HashMap;

use geo::Coord;

use crate::contour_geometry::{local_tangent, nearest_index};
use crate::contour_raster::ContourRaster;
use crate::contours_to_raster_config::Config;
use crate::gravity_model::{
    contour_gravity_side, gravity_vector_for_side, node_direction, vote_side, Contour,
};

/// A generous cap on one rain drop's simulated steps, purely as a safety
/// valve against an unbounded loop if some future change breaks the
/// leaves-the-map check -- the doc's own algorithm always terminates well
/// before this on any sanely-sized map.
const MAX_DROP_STEPS: u64 = 1_000_000;

/// One rain (or anti rain) drop's simulated path, plus the `--create_svg`
/// diagnostics recorded along it: which points were still inside a
/// hysteresis window, and the step (previous position to current position)
/// during which it cast each vote.
#[derive(Default)]
struct DropTrace {
    path: Vec<Coord<f64>>,
    hysteresis_points: Vec<Coord<f64>>,
    vote_segments: Vec<(Coord<f64>, Coord<f64>)>,
}

/// One Rain (or Anti Rain) Drop Production pass's accumulated paths and
/// `--create_svg` diagnostics, across every drop it simulated.
#[derive(Default)]
struct PassTrace {
    paths: Vec<Vec<Coord<f64>>>,
    hysteresis_points: Vec<Coord<f64>>,
    vote_segments: Vec<(Coord<f64>, Coord<f64>)>,
}

impl PassTrace {
    fn record(&mut self, drop: DropTrace) {
        self.hysteresis_points.extend(drop.hysteresis_points);
        self.vote_segments.extend(drop.vote_segments);
        self.paths.push(drop.path);
    }
}

/// What Step 2 resolved, and the paths it simulated (for the `--create_svg`
/// visualization).
pub struct Step2Result {
    /// How many contours got their gravity from Rain Drop Production.
    pub resolved_by_rain: u64,
    /// How many contours got their gravity from Anti Rain Drop Production.
    pub resolved_by_anti_rain: u64,
    /// One message per contour whose left/right vote counts were close
    /// enough to flag as ambiguous (`undefined_gravity_vote_threshold`).
    pub ambiguous_warnings: Vec<String>,
    /// Every rain drop's path, source to evaporation.
    pub rain_paths: Vec<Vec<Coord<f64>>>,
    /// Every anti rain drop's path.
    pub anti_rain_paths: Vec<Vec<Coord<f64>>>,
    /// Which contours already had gravity defined right after Rain Drop
    /// Production finished, before Anti Rain Drop Production started --
    /// `contours` itself is mutated in place by both passes in turn, so by
    /// the time any `--create_svg` file is written it always holds the
    /// final, post-both-passes state; this snapshot is what lets the rain
    /// SVG draw an arrow only for a contour Rain Drop Production (or an
    /// earlier step) actually resolved, not one Anti Rain Drop Production
    /// goes on to add afterward.
    pub defined_after_rain: Vec<bool>,
    /// Every point, across every rain drop's path, reached while some
    /// `rain_drop_starting_voting_hysteresis` window -- the drop's own
    /// creation, or a vote it made -- was still open.
    pub rain_hysteresis_points: Vec<Coord<f64>>,
    /// The same, for anti rain drops.
    pub anti_rain_hysteresis_points: Vec<Coord<f64>>,
    /// Every rain drop step (previous position, then current position)
    /// during which it cast a vote for an undefined contour -- a vote is a
    /// property of the step it happened on, not of either endpoint alone.
    pub rain_vote_segments: Vec<(Coord<f64>, Coord<f64>)>,
    /// The same, for anti rain drops.
    pub anti_rain_vote_segments: Vec<(Coord<f64>, Coord<f64>)>,
}

/// Runs Step 2: Rain Drop Production, then (if needed) Anti Rain Drop
/// Production, resolve every contour Step 1 left without a gravity
/// direction by simulation. Always returns the full [`Step2Result`] --
/// including every simulated path, for `--create_svg` to draw even on
/// failure -- alongside an `Err` naming any contour still undefined after
/// both passes. The doc calls that "impossible," but doesn't say what to do
/// if it happens, so this reports it as a hard failure rather than silently
/// leaving a gap, without discarding the state a caller needs to see why.
pub fn resolve(
    contours: &mut [Contour],
    raster: &ContourRaster,
    config: &Config,
) -> (Step2Result, Result<(), String>) {
    let mut ambiguous_warnings = Vec::new();

    let mut rain_trace = PassTrace::default();
    let resolved_by_rain = simulate_pass(
        contours,
        raster,
        config,
        1.0,
        &mut rain_trace,
        &mut ambiguous_warnings,
    );

    let defined_after_rain: Vec<bool> = contours
        .iter()
        .map(|c| c.lwg.gravity_dx.is_some())
        .collect();

    let mut anti_rain_trace = PassTrace::default();
    let resolved_by_anti_rain = if contours.iter().all(|c| c.lwg.gravity_dx.is_some()) {
        0
    } else {
        simulate_pass(
            contours,
            raster,
            config,
            -1.0,
            &mut anti_rain_trace,
            &mut ambiguous_warnings,
        )
    };

    let still_undefined: Vec<usize> = contours
        .iter()
        .enumerate()
        .filter(|(_, c)| c.lwg.gravity_dx.is_none())
        .map(|(i, _)| i)
        .collect();
    let outcome = if still_undefined.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} contour(s) still have no defined gravity after both rain-drop passes (indices \
             {still_undefined:?}); the algorithm assumes this cannot happen",
            still_undefined.len()
        ))
    };

    (
        Step2Result {
            resolved_by_rain,
            resolved_by_anti_rain,
            ambiguous_warnings,
            defined_after_rain,
            rain_paths: rain_trace.paths,
            anti_rain_paths: anti_rain_trace.paths,
            rain_hysteresis_points: rain_trace.hysteresis_points,
            anti_rain_hysteresis_points: anti_rain_trace.hysteresis_points,
            rain_vote_segments: rain_trace.vote_segments,
            anti_rain_vote_segments: anti_rain_trace.vote_segments,
        },
        outcome,
    )
}

/// One Rain Drop Production (`direction_sign = 1.0`) or Anti Rain Drop
/// Production (`direction_sign = -1.0`) pass: sources are placed along every
/// contour that already has gravity, every drop is simulated to
/// evaporation, and the accumulated votes are turned into gravity for every
/// contour that received any. Returns how many contours that resolved.
///
/// A segment's `sources_per_contour_segment` sources do not all leave in the
/// same, flat direction: the source at `A` (the segment's own start node)
/// leaves along [`node_direction`] at `A`, the one at `B` along
/// [`node_direction`] at `B`, and one in between blends the two by the same
/// fraction `t` used to place it along the segment -- so the direction
/// field turns smoothly along the contour instead of jumping at each node.
fn simulate_pass(
    contours: &mut [Contour],
    raster: &ContourRaster,
    config: &Config,
    direction_sign: f64,
    trace: &mut PassTrace,
    ambiguous_warnings: &mut Vec<String>,
) -> u64 {
    let source_contours: Vec<usize> = contours
        .iter()
        .enumerate()
        .filter(|(_, c)| c.lwg.gravity_dx.is_some())
        .map(|(i, _)| i)
        .collect();

    for src_idx in source_contours {
        let side =
            contour_gravity_side(&contours[src_idx]).expect("a source contour always has gravity");
        let ls = contours[src_idx].lwg.ls.clone();
        if ls.0.len() < 2 {
            continue;
        }
        let source_count = config.sources_per_contour_segment.max(1);
        for w in 0..ls.0.len() - 1 {
            let (a, b) = (ls.0[w], ls.0[w + 1]);
            let dir_a =
                node_direction(&ls, w, side).expect("a segment's own start node has a direction");
            let dir_b =
                node_direction(&ls, w + 1, side).expect("a segment's own end node has a direction");
            for k in 0..source_count {
                let t = k as f64 / source_count as f64;
                let source = Coord {
                    x: a.x + t * (b.x - a.x),
                    y: a.y + t * (b.y - a.y),
                };
                // A linear blend of two unit vectors is not itself unit
                // length in general, and `dir`'s length is exactly how far
                // one rain_drop_step actually moves the drop -- so this
                // renormalizes rather than using the blend directly.
                let (bx, by) = (
                    (1.0 - t) * dir_a.0 + t * dir_b.0,
                    (1.0 - t) * dir_a.1 + t * dir_b.1,
                );
                let blend_len = bx.hypot(by);
                let (dx, dy) = if blend_len > 1e-9 {
                    (bx / blend_len, by / blend_len)
                } else {
                    // dir_a and dir_b cancel out exactly (a near U-turn):
                    // fall back to the start node's own direction.
                    dir_a
                };
                let dir = (dx * direction_sign, dy * direction_sign);
                let drop = simulate_one_drop(contours, raster, config, src_idx as u64, source, dir);
                trace.record(drop);
            }
        }
    }

    finalize_votes(contours, config, ambiguous_warnings)
}

/// Steps one rain drop from `source` in the fixed direction `dir` until it
/// evaporates (leaves the map, hits an already-defined contour, or re-hits a
/// contour it already voted for -- both subject to their own
/// `rain_drop_starting_voting_hysteresis` window below), voting for every
/// not-yet-defined contour it crosses along the way. Returns its path,
/// together with every point along that path reached while some hysteresis
/// window -- the drop's own creation, or a vote it made -- was still open,
/// and the (previous position, current position) step during which it
/// actually cast each vote (both for the `--create_svg` visualization;
/// neither is used to decide anything here).
fn simulate_one_drop(
    contours: &mut [Contour],
    raster: &ContourRaster,
    config: &Config,
    source_contour_idx: u64,
    source: Coord<f64>,
    dir: (f64, f64),
) -> DropTrace {
    let hysteresis = config.rain_drop_starting_voting_hysteresis;
    let mut pos = source;
    let mut path = vec![pos];
    // Each voted contour's own step, so a *later* re-crossing of it is
    // judged against *its own* window, not the drop's creation -- otherwise
    // a vote cast long after the drop started would never be able to
    // protect a re-crossing at all.
    let mut voted: HashMap<u64, u64> = HashMap::new();
    let mut steps = 0u64;

    // The step (exclusive) up to which the union of every window opened so
    // far -- the drop's own creation, and each vote -- is still open. Purely
    // a visualization aid: it never gates evaporation, only which points get
    // marked below.
    let mut covered_until = hysteresis;
    let mut hysteresis_points = Vec::new();
    let mut vote_segments = Vec::new();
    if steps < covered_until {
        hysteresis_points.push(pos);
    }

    loop {
        let next = Coord {
            x: pos.x + dir.0 * config.rain_drop_step,
            y: pos.y + dir.1 * config.rain_drop_step,
        };
        let (px, py) = raster.to_px(next);
        if px < 0 || py < 0 || px as usize >= raster.width || py as usize >= raster.height {
            path.push(next);
            break; // leaves the map
        }

        // Never excluded at the raster level (u64::MAX can't match a real
        // contour index): the exemptions below are Step 2's own,
        // time-limited rule, not Appendix 5's permanent one.
        if let Some(hit_idx) = raster.first_hit_along_step(pos, next, u64::MAX) {
            // Re-crossing the drop's own starting contour is exempt within
            // `hysteresis` steps of the drop's own creation (steps 0 ..
            // hysteresis-1: `hysteresis` untouched chances, since step 0 is
            // the source itself, not the result of a check). Re-crossing a
            // contour it already voted for is exempt within `hysteresis`
            // steps of *that vote* -- but the step the vote was cast on is
            // itself spent on the vote, not a free re-crossing chance, so
            // the window is shifted one step later (`voted_at + 1 ..
            // voted_at + hysteresis`) to give it the same `hysteresis`
            // usable chances afterward, not `hysteresis - 1`.
            let starting_exempt = hit_idx == source_contour_idx && steps < hysteresis;
            let voted_exempt = voted
                .get(&hit_idx)
                .is_some_and(|&voted_at| steps <= voted_at + hysteresis);
            if !(starting_exempt || voted_exempt) {
                if contours[hit_idx as usize].lwg.gravity_dx.is_some() {
                    path.push(next);
                    break;
                }
                if voted.contains_key(&hit_idx) {
                    path.push(next);
                    break;
                }
                voted.insert(hit_idx, steps);
                covered_until = covered_until.max(steps + hysteresis + 1);
                vote_segments.push((pos, next));
                let c = &mut contours[hit_idx as usize];
                let idx = nearest_index(&c.lwg.ls, next);
                let (tf, tt) = local_tangent(&c.lwg.ls, idx);
                if vote_side(tf, tt, dir.0, dir.1) > 0.0 {
                    c.lwg.gravity_votes.left += 1;
                } else {
                    c.lwg.gravity_votes.right += 1;
                }
            }
        }

        pos = next;
        path.push(pos);
        steps += 1;
        if steps < covered_until {
            hysteresis_points.push(pos);
        }
        if steps >= MAX_DROP_STEPS {
            break;
        }
    }

    DropTrace {
        path,
        hysteresis_points,
        vote_segments,
    }
}

/// Turns accumulated votes into gravity for every contour that received any
/// and is still undefined, flagging a near tie per
/// `undefined_gravity_vote_threshold`. Returns how many contours resolved.
fn finalize_votes(contours: &mut [Contour], config: &Config, warnings: &mut Vec<String>) -> u64 {
    let mut resolved = 0u64;
    for (idx, c) in contours.iter_mut().enumerate() {
        if c.lwg.gravity_dx.is_some() {
            continue;
        }
        let votes = c.lwg.gravity_votes;
        if votes.left == 0 && votes.right == 0 {
            continue;
        }
        let side = if votes.left >= votes.right { 1.0 } else { -1.0 };
        let (gx, gy) = gravity_vector_for_side(&c.lwg.ls, side);
        c.lwg.gravity_dx = Some(gx);
        c.lwg.gravity_dy = Some(gy);
        resolved += 1;

        let (lo, hi) = if votes.left < votes.right {
            (votes.left, votes.right)
        } else {
            (votes.right, votes.left)
        };
        if hi > 0 && lo as f64 > config.undefined_gravity_vote_threshold * hi as f64 {
            warnings.push(format!(
                "contour {idx} has near-tied gravity votes ({} left / {} right); flagged as ambiguous",
                votes.left, votes.right
            ));
        }
    }
    resolved
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gravity_model::LineWithGravity;
    use geo::LineString;

    fn c(x: f64, y: f64) -> Coord<f64> {
        Coord { x, y }
    }

    fn default_config() -> Config {
        Config {
            bezier_linearization_step: 0.1,
            contours_step: 1.0,
            rasterization_px_size: 0.5,
            rasterization_step_factor: 0.5,
            heavy_object_width: 1.0,
            heavy_object_growing: 0.2,
            circumference_fitting_points_number: 4,
            slope_lines_contours_search_radius: 3.0,
            rain_drop_step: 0.25,
            sources_per_contour_segment: 3,
            rain_drop_starting_voting_hysteresis: 3,
            undefined_gravity_vote_threshold: 0.8,
            contour_gap_merge_radius: 2.0,
        }
    }

    fn straight_ls(y: f64) -> LineString<f64> {
        LineString::new(vec![c(0.0, y), c(10.0, y), c(20.0, y)])
    }

    fn raster_for(contours: &[Contour]) -> ContourRaster {
        let mut r = ContourRaster::new(c(-5.0, -5.0), 0.5, 60, 60);
        for (i, contour) in contours.iter().enumerate() {
            let densified = crate::contour_geometry::densify(&contour.lwg.ls, 0.5, 0.5);
            r.write_contour(i as u64, &densified).unwrap();
        }
        r
    }

    #[test]
    fn segment_sources_blend_direction_between_the_segments_two_endpoint_nodes() {
        // An "L" bend, so the segment's start and end nodes have visibly
        // different directions (node_direction is a mean at the bend, a
        // lone perpendicular at the ends) -- exercising the case a flat,
        // one-direction-per-segment reading would get wrong.
        let ls = LineString::new(vec![c(0.0, 0.0), c(10.0, 0.0), c(10.0, 10.0)]);
        let side = 1.0;
        let (gx, gy) = gravity_vector_for_side(&ls, side);
        let mut source = Contour {
            lwg: LineWithGravity::new(ls.clone()),
            elevation_height: None,
        };
        source.lwg.gravity_dx = Some(gx);
        source.lwg.gravity_dy = Some(gy);
        let mut contours = vec![source];
        let raster = raster_for(&contours);

        let mut config = default_config();
        config.sources_per_contour_segment = 3;

        let mut trace = PassTrace::default();
        let mut warnings = Vec::new();
        simulate_pass(
            &mut contours,
            &raster,
            &config,
            1.0,
            &mut trace,
            &mut warnings,
        );

        // The first segment (node 0 -> node 1) produces the first 3 paths,
        // for k = 0, 1, 2.
        assert!(trace.paths.len() >= 3);
        let dir_a = node_direction(&ls, 0, side).unwrap();
        let dir_b = node_direction(&ls, 1, side).unwrap();

        let unit_step = |path: &[Coord<f64>]| {
            let (sx, sy) = (path[1].x - path[0].x, path[1].y - path[0].y);
            let len = sx.hypot(sy);
            (sx / len, sy / len)
        };

        for k in 0..3usize {
            let actual = unit_step(&trace.paths[k]);
            let t = k as f64 / 3.0;
            let (bx, by) = (
                (1.0 - t) * dir_a.0 + t * dir_b.0,
                (1.0 - t) * dir_a.1 + t * dir_b.1,
            );
            let blend_len = bx.hypot(by);
            let expected = (bx / blend_len, by / blend_len);
            assert!(
                (actual.0 - expected.0).abs() < 1e-9 && (actual.1 - expected.1).abs() < 1e-9,
                "source {k}: expected direction {expected:?}, got {actual:?}"
            );
        }

        // The three directions must actually differ -- otherwise sources
        // along one segment would still all leave in the same, flat
        // direction the way they used to.
        let first = unit_step(&trace.paths[0]);
        let last = unit_step(&trace.paths[2]);
        assert!(
            (first.0 - last.0).abs() > 1e-6 || (first.1 - last.1).abs() > 1e-6,
            "expected sources along the same segment to have different directions"
        );
    }

    #[test]
    fn rain_drop_production_resolves_a_parallel_undefined_contour() {
        let mut defined = Contour {
            lwg: LineWithGravity::new(straight_ls(0.0)),
            elevation_height: None,
        };
        defined.lwg.gravity_dx = Some(0.0);
        defined.lwg.gravity_dy = Some(1.0); // downhill = +y, toward the second contour
        let undefined = Contour {
            lwg: LineWithGravity::new(straight_ls(5.0)),
            elevation_height: None,
        };
        let mut contours = vec![defined, undefined];
        let raster = raster_for(&contours);
        let config = default_config();

        let (result, outcome) = resolve(&mut contours, &raster, &config);
        outcome.unwrap();
        assert!(result.resolved_by_rain >= 1 || result.resolved_by_anti_rain >= 1);
        assert!(contours[1].lwg.gravity_dx.is_some());
    }

    #[test]
    fn ambiguous_votes_are_flagged_at_threshold() {
        // Two defined contours facing each other, each sending drops that
        // vote oppositely on a contour exactly between them: a near-even
        // split.
        let mut top = Contour {
            lwg: LineWithGravity::new(straight_ls(0.0)),
            elevation_height: None,
        };
        top.lwg.gravity_dx = Some(0.0);
        top.lwg.gravity_dy = Some(1.0);
        let middle = Contour {
            lwg: LineWithGravity::new(straight_ls(5.0)),
            elevation_height: None,
        };
        let mut bottom = Contour {
            lwg: LineWithGravity::new(straight_ls(10.0)),
            elevation_height: None,
        };
        bottom.lwg.gravity_dx = Some(0.0);
        bottom.lwg.gravity_dy = Some(-1.0);
        let mut contours = vec![top, middle, bottom];
        let raster = raster_for(&contours);
        let config = default_config();

        let (result, outcome) = resolve(&mut contours, &raster, &config);
        outcome.unwrap();
        assert!(contours[1].lwg.gravity_dx.is_some());
        assert!(
            !result.ambiguous_warnings.is_empty(),
            "expected a near-tied vote warning"
        );
    }

    #[test]
    fn a_contour_no_drop_ever_reaches_is_reported_but_its_partial_result_is_still_returned() {
        // A lone undefined contour with no other contour ever giving it
        // gravity: no drop from it (it has none, being undefined) and no
        // drop from elsewhere reaches it, so it can never be resolved --
        // exercising the "should be impossible" failure path itself.
        let mut contours = vec![Contour {
            lwg: LineWithGravity::new(straight_ls(0.0)),
            elevation_height: None,
        }];
        let raster = raster_for(&contours);
        let config = default_config();

        let (result, outcome) = resolve(&mut contours, &raster, &config);
        let err = outcome.unwrap_err();
        assert!(err.contains("indices [0]"), "unexpected message: {err}");
        // The partial result -- here, simply no rain drops at all, since
        // there was no defined contour to send any from -- is still handed
        // back rather than discarded, so a caller can still inspect it.
        assert_eq!(result.resolved_by_rain, 0);
        assert!(result.rain_paths.is_empty());
    }

    /// A source contour and an undefined "bracket" contour whose two
    /// horizontal arms -- (3,1)-(7,1) and (7,2)-(3,2) -- both cross a drop
    /// travelling straight up from (5, 0), close enough together to land
    /// inside a short `rain_drop_starting_voting_hysteresis` window.
    fn source_and_bracket_contours() -> Vec<Contour> {
        let mut source = Contour {
            lwg: LineWithGravity::new(straight_ls(-100.0)),
            elevation_height: None,
        };
        source.lwg.gravity_dx = Some(0.0);
        source.lwg.gravity_dy = Some(1.0);
        let bracket = Contour {
            lwg: LineWithGravity::new(LineString::new(vec![
                c(3.0, 1.0),
                c(7.0, 1.0),
                c(7.0, 2.0),
                c(3.0, 2.0),
            ])),
            elevation_height: None,
        };
        vec![source, bracket]
    }

    #[test]
    fn re_crossing_a_voted_undefined_contour_within_hysteresis_does_not_evaporate() {
        let mut contours = source_and_bracket_contours();
        let mut raster = ContourRaster::new(c(0.0, 0.0), 0.5, 40, 40);
        let densified = crate::contour_geometry::densify(&contours[1].lwg.ls, 0.5, 0.5);
        raster.write_contour(1, &densified).unwrap();

        let mut config = default_config();
        config.rain_drop_step = 0.25;
        // Both crossings (around step 4 and step 8) fall well inside this.
        config.rain_drop_starting_voting_hysteresis = 10;

        let drop = simulate_one_drop(&mut contours, &raster, &config, 0, c(5.0, 0.0), (0.0, 1.0));
        let path = drop.path;

        // A drop that evaporated on the second crossing would stop right
        // there, around y=2; surviving both, it keeps going until it leaves
        // the map instead.
        assert!(
            path.last().unwrap().y > 15.0,
            "expected the drop to survive both crossings and leave the map, stopped at {:?}",
            path.last()
        );
        // Still voted only once: the second crossing must not double-vote.
        assert_eq!(
            contours[1].lwg.gravity_votes.left + contours[1].lwg.gravity_votes.right,
            1
        );
    }

    #[test]
    fn re_crossing_a_voted_undefined_contour_outside_hysteresis_evaporates() {
        let mut contours = source_and_bracket_contours();
        let mut raster = ContourRaster::new(c(0.0, 0.0), 0.5, 40, 40);
        let densified = crate::contour_geometry::densify(&contours[1].lwg.ls, 0.5, 0.5);
        raster.write_contour(1, &densified).unwrap();

        let mut config = default_config();
        config.rain_drop_step = 0.25;
        config.rain_drop_starting_voting_hysteresis = 0;

        let drop = simulate_one_drop(&mut contours, &raster, &config, 0, c(5.0, 0.0), (0.0, 1.0));
        let path = drop.path;

        assert!(
            path.last().unwrap().y < 3.0,
            "expected the drop to evaporate right after the second crossing, went to {:?}",
            path.last()
        );
    }

    #[test]
    fn a_vote_gets_its_own_hysteresis_window_even_late_in_the_drops_life() {
        // The bracket sits far from the source, so both crossings happen
        // long after the drop's own starting-contour window (a handful of
        // steps) has expired; only a hysteresis measured from the *vote
        // itself* -- not from the drop's creation -- can still exempt the
        // second crossing here.
        let mut contours = vec![
            {
                let mut source = Contour {
                    lwg: LineWithGravity::new(straight_ls(-100.0)),
                    elevation_height: None,
                };
                source.lwg.gravity_dx = Some(0.0);
                source.lwg.gravity_dy = Some(1.0);
                source
            },
            Contour {
                lwg: LineWithGravity::new(LineString::new(vec![
                    c(3.0, 50.0),
                    c(7.0, 50.0),
                    c(7.0, 51.0),
                    c(3.0, 51.0),
                ])),
                elevation_height: None,
            },
        ];
        let mut raster = ContourRaster::new(c(0.0, 0.0), 0.5, 20, 220);
        let densified = crate::contour_geometry::densify(&contours[1].lwg.ls, 0.5, 0.5);
        raster.write_contour(1, &densified).unwrap();

        let mut config = default_config();
        // Matches the raster's own pixel size, so each step touches exactly
        // one pixel row -- a finer step would let a single geometric
        // crossing register as two separate hits (one per step still inside
        // the same pixel), which is a rasterization artifact unrelated to
        // what this test means to exercise.
        config.rain_drop_step = 0.5;
        config.rain_drop_starting_voting_hysteresis = 5;

        let drop = simulate_one_drop(&mut contours, &raster, &config, 0, c(5.0, 0.0), (0.0, 1.0));
        let (path, hysteresis_points) = (drop.path, drop.hysteresis_points);

        assert!(
            path.last().unwrap().y > 90.0,
            "expected the drop to survive both crossings (protected by the vote's own \
             hysteresis, not the drop's creation) and leave the map, stopped at {:?}",
            path.last()
        );
        // The points right after the vote (around y=50) should be marked,
        // even though they are nowhere near the drop's own start.
        assert!(
            hysteresis_points.iter().any(|p| p.y > 49.0 && p.y < 51.0),
            "expected a hysteresis marker near the vote itself, got: {hysteresis_points:?}"
        );
    }

    #[test]
    fn a_votes_hysteresis_window_marks_exactly_as_many_points_as_the_starting_window() {
        // A single straight crossing far from the source: one vote, no
        // re-crossing, so every hysteresis point comes from either the
        // drop's own creation window (near y=0) or the vote's window (near
        // y=20) -- the two should mark the same number of points.
        let mut contours = vec![
            {
                let mut source = Contour {
                    lwg: LineWithGravity::new(straight_ls(-100.0)),
                    elevation_height: None,
                };
                source.lwg.gravity_dx = Some(0.0);
                source.lwg.gravity_dy = Some(1.0);
                source
            },
            Contour {
                lwg: LineWithGravity::new(LineString::new(vec![c(0.0, 20.0), c(10.0, 20.0)])),
                elevation_height: None,
            },
        ];
        let mut raster = ContourRaster::new(c(0.0, 0.0), 1.0, 20, 40);
        let densified = crate::contour_geometry::densify(&contours[1].lwg.ls, 0.5, 1.0);
        raster.write_contour(1, &densified).unwrap();

        let mut config = default_config();
        config.rain_drop_step = 1.0;
        config.rain_drop_starting_voting_hysteresis = 3;

        let drop = simulate_one_drop(&mut contours, &raster, &config, 0, c(5.0, 0.0), (0.0, 1.0));
        let hysteresis_points = drop.hysteresis_points;

        let at_start = hysteresis_points.iter().filter(|p| p.y < 10.0).count();
        let after_vote = hysteresis_points.iter().filter(|p| p.y > 15.0).count();
        assert_eq!(
            at_start, 3,
            "expected 3 points marked at the drop's own start"
        );
        assert_eq!(
            after_vote, at_start,
            "a vote should get exactly as many usable hysteresis points afterward as the \
             drop's own creation window does, not one fewer"
        );
    }
}
