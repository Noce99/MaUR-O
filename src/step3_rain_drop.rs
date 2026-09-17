//! The Rain Drop Production Definition (`Contours-to-Raster.md`) shared
//! engine: source placement, stepping, and the Cold/Hot/Reverse Cold
//! evaporation rules. Step 3 (this file's original purpose) uses the Cold
//! variants to resolve every contour Step 2 left without a gravity
//! direction, then the Reverse Cold variants (see [`simulate_reverse_pass`])
//! as a fallback for whatever the Cold passes still leave undefined; Step 1's
//! own flood-fill sub-step uses the Hot variants (see
//! [`flood_fill_from_contour`]) to mark reachable "no contour, in bound"
//! pixels before any contour has a real gravity direction at all.

use std::collections::HashMap;

use geo::algorithm::Contains;
use geo::{Coord, Euclidean, Length, LineString};

use crate::contour_geometry::{local_tangent, nearest_index};
use crate::contour_raster::{ContourRaster, StepHit};
use crate::contours_to_raster_config::Config;
use crate::gravity_model::{
    contour_gravity_side, gravity_vector_for_side, lwg_gravity_side, node_direction,
    vector_on_side, vote_side, Contour, LineGravityDefiners, LineWithGravity,
};

/// A generous cap on one rain drop's simulated steps, purely as a safety
/// valve against an unbounded loop if some future change breaks
/// out-of-bound detection -- the doc's own algorithm always terminates well
/// before this on any sanely-sized map.
const MAX_DROP_STEPS: u64 = 1_000_000;

/// Cold evaporates on an already-defined contour, high density, or out of
/// bound, and votes on an undefined contour; Hot evaporates on any contour,
/// high density, or out of bound, and never votes. Fill evaporates on
/// exactly what Hot does -- any contour (including, past
/// `rain_drop_starting_voting_hysteresis` steps, its own starting one, the
/// same exemption window Hot's own starting contour gets), high density, or
/// out of bound -- but never votes either; what a high-density hit *means*
/// to its own caller differs from every other variant's, though (Step 5's
/// own Elevation Fill Rain Drop Production supposes a fallback elevation for
/// it, one step below its own source, rather than treating it as a dead
/// end). Reverse Cold evaporates on exactly what Cold does, but the other
/// way round: a
/// still-undefined contour is a silent pass (never a vote), while an
/// already-defined contour or a Jump's own high-density area
/// (`line_definers`) casts one vote onto the drop's own *source* instead of
/// the hit -- see [`simulate_reverse_pass`]/[`simulate_one_drop`]'s own
/// handling. See the Rain Drop Production Definition.
#[derive(Clone, Copy)]
enum Temperature<'a> {
    Cold,
    Hot,
    Fill,
    ReverseCold {
        line_definers: &'a [LineGravityDefiners],
    },
}

/// One rain (or anti rain) drop's simulated path, plus the `--create_svg`
/// diagnostics recorded along it: which points were still inside a
/// hysteresis window, and the step (previous position to current position)
/// during which it cast each vote. Always empty for a Hot drop, which never
/// votes.
#[derive(Default)]
struct DropTrace {
    path: Vec<Coord<f64>>,
    hysteresis_points: Vec<Coord<f64>>,
    vote_segments: Vec<(Coord<f64>, Coord<f64>)>,
    /// What the drop actually evaporated on, if it evaporated at all before
    /// `MAX_DROP_STEPS` -- `None` for a drop that ran out its own step
    /// budget without hitting anything. Step 4's own Hot Rain/Anti Rain Drop
    /// Production ([`hot_drop_evaporation_contour`]) is the only caller that
    /// reads this; every other caller already knows what it needs from
    /// `path`/`vote_segments` alone.
    final_hit: Option<StepHit>,
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

/// What Step 3 resolved, and the paths it simulated (for the `--create_svg`
/// visualization).
pub struct Step3Result {
    /// How many contours got their gravity from the combined Cold Rain +
    /// Cold Anti Rain vote tally, plus however many the Reverse Cold
    /// fallback pass went on to resolve afterward. Gravity is no longer
    /// assigned right after any single pass alone -- see [`resolve`] -- so
    /// there is only one combined count.
    pub resolved_by_votes: u64,
    /// One message per contour whose left/right vote counts were close
    /// enough to flag as ambiguous (`undefined_gravity_vote_threshold`), plus
    /// one more per contour still without a gravity direction once every
    /// pass has finished -- naming that contour's own length in meters,
    /// since it is dropped rather than resolved (see [`resolve`]).
    pub warnings: Vec<String>,
    /// Every rain drop's path, source to evaporation.
    pub rain_paths: Vec<Vec<Coord<f64>>>,
    /// Every anti rain drop's path.
    pub anti_rain_paths: Vec<Vec<Coord<f64>>>,
    /// Which contours already had gravity defined right after Cold Rain
    /// Drop Production finished, before Cold Anti Rain Drop Production
    /// started -- `contours` itself is mutated in place by both passes in
    /// turn, so by the time any `--create_svg` file is written it always
    /// holds the final, post-both-passes state; this snapshot is what lets
    /// the rain SVG draw an arrow only for a contour actually resolved by
    /// that point, not one only the final vote tally goes on to add
    /// afterward. Since votes are no longer turned into gravity until both
    /// passes have finished (see [`resolve`]), this is now always identical
    /// to the state before Step 3 even started -- the rain pass itself never
    /// changes it -- but it is kept as its own snapshot rather than folded
    /// away, so the rain SVG's filtering logic doesn't have to know that.
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

/// Runs Step 3: a Cold Rain Drop Production, then a Cold Anti Rain Drop
/// Production, resolving every contour Step 2 left without a gravity
/// direction by simulation. Neither pass assigns gravity from its own votes
/// alone: both run to completion first (the anti rain pass sourced only from
/// contours that already had gravity *before* the rain pass, deliberately
/// not the possibly-larger set the rain pass's own votes could go on to
/// resolve -- see [`simulate_pass`]), so a contour crossed by both ends up
/// judged on their combined tally rather than whichever pass reached it
/// first. If anything is still undefined after that, a Reverse Cold Rain and
/// Reverse Cold Anti Rain Drop Production pass (see
/// [`simulate_reverse_pass`]) runs from every such contour as a fallback,
/// using `line_definers` (Jumps) as an extra source of votes alongside
/// whatever contours have gravity by that point. [`finalize_votes`] turns
/// accumulated votes into gravity once after the Cold passes and again after
/// the Reverse Cold ones, so a contour already resolved by the first call is
/// simply skipped by the second.
///
/// Any contour still without a gravity direction after all of the above is
/// dropped rather than treated as a failure: unlike the doc's earlier
/// assumption that this "cannot happen," a sparse enough map (too few
/// sources, or a contour genuinely isolated from everything else by
/// out-of-bound/high-density) can leave a handful of contours simply
/// unreachable, and there is no reason the whole run should abort over a
/// contour or two the source data never gave enough evidence for. Each one
/// gets a warning naming its own length in meters instead, so whoever
/// reviews the run's own warnings can judge for themselves whether it was
/// worth digitizing at all. Always returns the full [`Step3Result`] --
/// including every simulated path and warning, for `--create_svg` to draw
/// and the caller to print regardless.
pub fn resolve(
    contours: &mut [Contour],
    raster: &mut ContourRaster,
    line_definers: &[LineGravityDefiners],
    config: &Config,
) -> Step3Result {
    let mut warnings = Vec::new();

    let mut rain_trace = PassTrace::default();
    simulate_pass(
        contours,
        raster,
        config,
        Temperature::Cold,
        1.0,
        &mut rain_trace,
    );

    let defined_after_rain: Vec<bool> = contours
        .iter()
        .map(|c| c.lwg.gravity_dx.is_some())
        .collect();

    let mut anti_rain_trace = PassTrace::default();
    simulate_pass(
        contours,
        raster,
        config,
        Temperature::Cold,
        -1.0,
        &mut anti_rain_trace,
    );

    let mut resolved_by_votes = finalize_votes(contours, config, &mut warnings);

    if contours.iter().any(|c| c.lwg.gravity_dx.is_none()) {
        let mut reverse_trace = PassTrace::default();
        simulate_reverse_pass(contours, raster, line_definers, config, &mut reverse_trace);
        resolved_by_votes += finalize_votes(contours, config, &mut warnings);
    }

    for (idx, c) in contours.iter().enumerate() {
        if c.lwg.gravity_dx.is_none() {
            let length_m = Euclidean.length(&c.lwg.ls);
            warnings.push(format!(
                "contour {idx} ({length_m:.1}m long) still has no gravity direction after \
                 every Step 3 pass; dropped"
            ));
        }
    }

    Step3Result {
        resolved_by_votes,
        warnings,
        defined_after_rain,
        rain_paths: rain_trace.paths,
        anti_rain_paths: anti_rain_trace.paths,
        rain_hysteresis_points: rain_trace.hysteresis_points,
        anti_rain_hysteresis_points: anti_rain_trace.hysteresis_points,
        rain_vote_segments: rain_trace.vote_segments,
        anti_rain_vote_segments: anti_rain_trace.vote_segments,
    }
}

/// Step 1's flood-fill sub-step: runs a Hot Rain Drop Production and a Hot
/// Anti Rain Drop Production from `contour_idx`'s own `ls`, marking every
/// `UNDEFINED` pixel each drop's steps touch as `NO_CONTOUR_IN_BOUND`. No
/// contour has a real gravity direction yet at this point in Step 1, so
/// `placeholder_side` (`1.0` or `-1.0`, arbitrary) stands in for it -- a Hot
/// Rain Drop Production and its Hot Anti Rain counterpart together cover
/// both perpendicular sides of the contour regardless of which one is
/// picked, so the choice can't affect the result (see the Rain Drop
/// Production Definition).
pub fn flood_fill_from_contour(
    raster: &mut ContourRaster,
    ls: &LineString<f64>,
    contour_idx: u64,
    placeholder_side: f64,
    config: &Config,
) {
    for (source, (dx, dy)) in
        placed_sources(ls, placeholder_side, config.sources_per_contour_segment)
    {
        for &direction_sign in &[1.0, -1.0] {
            let dir = (dx * direction_sign, dy * direction_sign);
            simulate_one_drop(
                &mut [],
                raster,
                config,
                Temperature::Hot,
                contour_idx,
                source,
                dir,
                direction_sign,
            );
        }
    }
}

/// Every source [`Coord`] Source Placement puts along `ls`'s segments
/// (`sources_per_contour_segment` per segment), paired with its own blended
/// perpendicular unit direction on `side` (Source Direction) -- the two
/// pieces of the Rain Drop Production Definition every one of its variants
/// shares, whether `side` is a contour's own real gravity or, for a variant
/// whose source has none yet, an arbitrary placeholder.
///
/// A segment's sources do not all leave in the same, flat direction: the
/// source at its start node leaves along [`node_direction`] there, the one
/// at its end node leaves along [`node_direction`] there, and one in between
/// blends the two by the same fraction used to place it along the segment --
/// so the direction turns smoothly along the contour instead of jumping at
/// each node.
pub(crate) fn placed_sources(
    ls: &LineString<f64>,
    side: f64,
    sources_per_contour_segment: usize,
) -> Vec<(Coord<f64>, (f64, f64))> {
    let mut sources = Vec::new();
    if ls.0.len() < 2 {
        return sources;
    }
    let source_count = sources_per_contour_segment.max(1);
    for w in 0..ls.0.len() - 1 {
        let (a, b) = (ls.0[w], ls.0[w + 1]);
        let (Some(dir_a), Some(dir_b)) =
            (node_direction(ls, w, side), node_direction(ls, w + 1, side))
        else {
            continue;
        };
        for k in 0..source_count {
            let t = k as f64 / source_count as f64;
            let source = Coord {
                x: a.x + t * (b.x - a.x),
                y: a.y + t * (b.y - a.y),
            };
            // A linear blend of two unit vectors is not itself unit length in
            // general, and this direction's own length is exactly how far
            // one rain_drop_step actually moves the drop -- so this
            // renormalizes rather than using the blend directly.
            let (bx, by) = (
                (1.0 - t) * dir_a.0 + t * dir_b.0,
                (1.0 - t) * dir_a.1 + t * dir_b.1,
            );
            let blend_len = bx.hypot(by);
            let dir = if blend_len > 1e-9 {
                (bx / blend_len, by / blend_len)
            } else {
                // dir_a and dir_b cancel out exactly (a near U-turn): fall
                // back to the start node's own direction.
                dir_a
            };
            sources.push((source, dir));
        }
    }
    sources
}

/// One Rain Drop Production (`direction_sign = 1.0`) or Anti Rain Drop
/// Production (`direction_sign = -1.0`) Cold pass: sources are placed along
/// every contour that already has gravity *at the moment this call starts*,
/// and every drop is simulated to evaporation, accumulating votes on
/// whatever undefined contours it crosses. It does not turn those votes into
/// gravity itself -- that only happens once, in [`finalize_votes`], after
/// both the rain and anti rain passes have finished (see [`resolve`]) -- so
/// a contour this call votes for stays undefined for the rest of this call,
/// and (crucially, for the anti rain call) is not yet a source a later call
/// within the same [`resolve`] can place drops from.
///
/// A segment's `sources_per_contour_segment` sources do not all leave in the
/// same, flat direction: the source at `A` (the segment's own start node)
/// leaves along [`node_direction`] at `A`, the one at `B` along
/// [`node_direction`] at `B`, and one in between blends the two by the same
/// fraction `t` used to place it along the segment -- so the direction
/// field turns smoothly along the contour instead of jumping at each node.
fn simulate_pass(
    contours: &mut [Contour],
    raster: &mut ContourRaster,
    config: &Config,
    temperature: Temperature<'_>,
    direction_sign: f64,
    trace: &mut PassTrace,
) {
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
        for (source, (dx, dy)) in placed_sources(&ls, side, config.sources_per_contour_segment) {
            let dir = (dx * direction_sign, dy * direction_sign);
            let drop = simulate_one_drop(
                contours,
                raster,
                config,
                temperature,
                src_idx as u64,
                source,
                dir,
                direction_sign,
            );
            trace.record(drop);
        }
    }
}

/// Reverse Cold Rain + Reverse Cold Anti Rain Drop Production (Step 3's
/// fallback substep): sourced from every contour still without gravity once
/// the Cold rain/anti rain votes have been tallied (see [`resolve`]). Since
/// the source has no gravity of its own, source placement uses an arbitrary
/// placeholder side, the same trick Step 1's Hot flood-fill already uses --
/// running both perpendicular directions from every source point stands in
/// for the Rain/Anti Rain distinction here, regardless of which one got the
/// placeholder label.
///
/// Unlike Cold, a Reverse Cold drop never votes on what it merely crosses --
/// it passes silently through any still-undefined contour (see
/// `simulate_one_drop`'s own `Temperature::ReverseCold` handling), so it
/// never needs its own source's gravity to judge a side the way Cold does;
/// there is none yet. It only ever casts one vote, for its own source, at
/// the first already-defined contour or Jump-caused high-density pixel it
/// reaches (see `accordance_downhill`/`jump_reverse_vote_direction`).
fn simulate_reverse_pass(
    contours: &mut [Contour],
    raster: &mut ContourRaster,
    line_definers: &[LineGravityDefiners],
    config: &Config,
    trace: &mut PassTrace,
) {
    const PLACEHOLDER_SIDE: f64 = 1.0;

    let undefined_contours: Vec<usize> = contours
        .iter()
        .enumerate()
        .filter(|(_, c)| c.lwg.gravity_dx.is_none())
        .map(|(i, _)| i)
        .collect();

    for src_idx in undefined_contours {
        let ls = contours[src_idx].lwg.ls.clone();
        for (source, (dx, dy)) in
            placed_sources(&ls, PLACEHOLDER_SIDE, config.sources_per_contour_segment)
        {
            for &direction_sign in &[1.0, -1.0] {
                let dir = (dx * direction_sign, dy * direction_sign);
                let drop = simulate_one_drop(
                    contours,
                    raster,
                    config,
                    Temperature::ReverseCold { line_definers },
                    src_idx as u64,
                    source,
                    dir,
                    direction_sign,
                );
                trace.record(drop);
            }
        }
    }
}

/// Steps one rain drop from `source` in the fixed direction `dir` until it
/// evaporates. A Cold drop evaporates on an out-of-bound pixel, a
/// high-density pixel, or an already-defined contour (subject to its own
/// `rain_drop_starting_voting_hysteresis` exemptions below), and
/// votes-and-continues on an undefined contour. A Hot drop evaporates on an
/// out-of-bound pixel, a high-density pixel, or any contour at all, never
/// votes, and is exempt from evaporating on its own starting contour only
/// within `rain_drop_starting_voting_hysteresis` steps of its own creation
/// (the same window Cold's own starting-contour exemption uses, just without
/// Cold's separate per-vote one, which a Hot drop has no use for since it
/// never votes) -- long enough to clear a concave source's own immediate
/// self-overlap right at its own origin, but not forever: a source contour
/// re-crossed *later*, well clear of its own origin, is a real crossing this
/// drop should evaporate on like any other, exactly as a source concave
/// enough to loop back across itself demands (see `Contours-to-Raster.md`'s
/// Step 4 for where this was actually discovered). A Reverse Cold drop
/// evaporates on exactly what Cold does, but
/// with each outcome flipped: crossing a still-undefined contour is a silent
/// pass (never a vote -- there is no gravity at its own source yet to judge a
/// side from), while evaporating on an already-defined contour, or a
/// high-density pixel inside one of `line_definers`' own Jump polygons,
/// casts one vote onto the drop's own *source* instead of the hit (see
/// [`accordance_downhill`]/[`jump_reverse_vote_direction`]); a high-density
/// pixel outside every Jump polygon is a genuine conflict and evaporates the
/// drop with no vote, same as for the other two temperatures. Since it never
/// votes on what it merely crosses, a Reverse Cold drop needs no hysteresis
/// window at all: it casts at most one vote in its whole lifetime, right
/// before evaporating, so there is never a re-crossing to protect. `contours`
/// is only read/written for Cold's and Reverse Cold's own voting -- a Hot
/// drop (used by Step 1's flood-fill, before any contour has votes to cast)
/// is simulated with an empty slice.
///
/// `direction_sign` is `1.0` for a Rain drop and `-1.0` for an Anti Rain one
/// (see [`simulate_pass`]); it is irrelevant to a Hot or Reverse Cold drop,
/// neither of which vote on what they themselves were launched from. A Cold
/// vote must always be judged against the *source's own* downhill direction,
/// not the drop's own literal direction of travel -- nearby contours share
/// the same local downhill sense (Assumption 1), so an Anti Rain drop, which
/// travels uphill (opposite its source's downhill direction), must vote as
/// if travelling the other way. Since `dir` already has `direction_sign`
/// folded in, multiplying it back in undoes exactly that flip
/// (`direction_sign` squares to `1.0`), recovering the source's own downhill
/// direction for the vote regardless of which way the drop itself moved to
/// get there. A Reverse Cold vote has no such source gravity to recover in
/// the first place -- its whole point is to infer one -- so it instead
/// compares `dir` directly against the *hit's* own gravity direction (the
/// doc's accordance/discordance check, reused from Step 4).
///
/// Returns its path, together with every point along that path reached while
/// some hysteresis window -- the drop's own creation, or a vote it made --
/// was still open, and the (previous position, current position) step during
/// which it actually cast each vote (all three always empty for a Hot or
/// Reverse Cold drop, though a Reverse Cold drop's own final position is
/// still whatever hit it voted from).
fn simulate_one_drop(
    contours: &mut [Contour],
    raster: &mut ContourRaster,
    config: &Config,
    temperature: Temperature<'_>,
    source_contour_idx: u64,
    source: Coord<f64>,
    dir: (f64, f64),
    direction_sign: f64,
) -> DropTrace {
    let hysteresis = config.rain_drop_starting_voting_hysteresis;
    let mut pos = source;
    let mut path = vec![pos];
    // Each voted contour's own step, so a *later* re-crossing of it is
    // judged against *its own* window, not the drop's creation -- otherwise
    // a vote cast long after the drop started would never be able to
    // protect a re-crossing at all. Unused (stays empty) for a Hot or
    // Reverse Cold drop.
    let mut voted: HashMap<u64, u64> = HashMap::new();
    let mut steps = 0u64;

    // The step (exclusive) up to which the union of every window opened so
    // far -- the drop's own creation, and each vote -- is still open. Purely
    // a visualization aid: it never gates evaporation, only which points get
    // marked below.
    let mut covered_until = hysteresis;
    let mut hysteresis_points = Vec::new();
    let mut vote_segments = Vec::new();
    if matches!(temperature, Temperature::Cold) && steps < covered_until {
        hysteresis_points.push(pos);
    }

    // Every temperature relies on its own time-limited (or, for Reverse
    // Cold, permanent-by-construction) exemption below to survive crossing
    // its own source contour, so nothing is excluded at the raster level
    // itself: a concave source contour can legitimately send a drop back
    // across its *own* boundary later in its life (not just at step 0,
    // right at its own origin), and that later crossing must still be able
    // to evaporate the drop like any other -- a permanent raster-level
    // exclusion would instead let the drop sail straight through every
    // later self-crossing too, right past a contour that is very much still
    // there. See the Cold/Hot hysteresis handling and Reverse Cold's own
    // "still undefined" handling below.
    let exclude = u64::MAX;

    // A Hot drop's own touched-but-still-undefined pixels, accumulated
    // across its whole life and only committed (via
    // `raster.commit_flood_pixels`) once it evaporates by hitting a contour
    // or high density -- never when it instead leaves the map, since that
    // would plant a firebreak blocking the later out-of-bound computation's
    // own flood-fill from ever correctly reaching those same pixels (see
    // `ContourRaster::commit_flood_pixels`). Unused for a Cold or Reverse
    // Cold drop.
    let mut flood_candidates: Vec<(i64, i64)> = Vec::new();
    let mut final_hit: Option<StepHit> = None;

    loop {
        let next = Coord {
            x: pos.x + dir.0 * config.rain_drop_step,
            y: pos.y + dir.1 * config.rain_drop_step,
        };

        let hit = match temperature {
            Temperature::Cold | Temperature::ReverseCold { .. } | Temperature::Fill => {
                raster.first_hit_along_step(pos, next, exclude)
            }
            Temperature::Hot => {
                let (hit, candidates) = raster.step_flood_candidates(pos, next, exclude);
                flood_candidates.extend(candidates);
                hit
            }
        };

        let evaporate = match (temperature, hit) {
            (_, None) => false,
            (_, Some(StepHit::OutOfBound)) => true,
            (Temperature::ReverseCold { line_definers }, Some(StepHit::HighDensity)) => {
                if let Some(inferred_downhill) =
                    jump_reverse_vote_direction(line_definers, next, dir)
                {
                    vote_segments.push((pos, next));
                    cast_reverse_vote(
                        &mut contours[source_contour_idx as usize],
                        source,
                        inferred_downhill,
                    );
                }
                true // always evaporates on high density, whether or not it voted
            }
            (_, Some(StepHit::HighDensity)) => true,
            (Temperature::Hot | Temperature::Fill, Some(StepHit::Contour(hit_idx))) => {
                // Exempt only within `hysteresis` steps of the drop's own
                // creation, and only for its own starting contour -- long
                // enough to clear a concave source's own immediate
                // self-overlap right at its own origin, but any other
                // contour (or this same one again, once outside the window)
                // evaporates it immediately, exactly as the Rain Drop
                // Production Definition says a Hot (or Fill) drop should.
                // Unlike Cold, there is no separate per-vote window to
                // track, since neither ever votes.
                !(hit_idx == source_contour_idx && steps < hysteresis)
            }
            (Temperature::ReverseCold { .. }, Some(StepHit::Contour(hit_idx))) => {
                match accordance_downhill(&contours[hit_idx as usize].lwg, next, dir) {
                    Some(inferred_downhill) => {
                        vote_segments.push((pos, next));
                        cast_reverse_vote(
                            &mut contours[source_contour_idx as usize],
                            source,
                            inferred_downhill,
                        );
                        true // evaporates: found a defined neighbor, done
                    }
                    None => false, // still undefined: pass straight through, no vote
                }
            }
            (Temperature::Cold, Some(StepHit::Contour(hit_idx))) => {
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
                if starting_exempt || voted_exempt {
                    false
                } else if contours[hit_idx as usize].lwg.gravity_dx.is_some() {
                    true // evaporates: already defined
                } else if let std::collections::hash_map::Entry::Vacant(e) = voted.entry(hit_idx) {
                    e.insert(steps);
                    covered_until = covered_until.max(steps + hysteresis + 1);
                    vote_segments.push((pos, next));
                    let c = &mut contours[hit_idx as usize];
                    let idx = nearest_index(&c.lwg.ls, next);
                    let (tf, tt) = local_tangent(&c.lwg.ls, idx);
                    if vote_side(tf, tt, dir.0 * direction_sign, dir.1 * direction_sign) > 0.0 {
                        c.lwg.gravity_votes.left += 1;
                    } else {
                        c.lwg.gravity_votes.right += 1;
                    }
                    false
                } else {
                    true // evaporates: already voted for once, outside hysteresis
                }
            }
        };

        if evaporate {
            path.push(next);
            final_hit = hit;
            if matches!(temperature, Temperature::Hot) && !matches!(hit, Some(StepHit::OutOfBound))
            {
                raster.commit_flood_pixels(&flood_candidates);
            }
            break;
        }

        pos = next;
        path.push(pos);
        steps += 1;
        if matches!(temperature, Temperature::Cold) && steps < covered_until {
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
        final_hit,
    }
}

/// Step 4's own Hot Rain/Anti Rain Drop Production ([`crate::step4_elevation`]):
/// simulates one Hot drop from `source` in direction `dir`, excluding
/// `source_contour_idx` (its own source contour) from counting as a hit, and
/// returns which contour it evaporated on, where, and its own whole path
/// (source to evaporation) -- kept, unlike every other caller of
/// [`simulate_one_drop`], so a near-tied vote or a voided cycle-forming
/// eviction can name a real, representative landing point in its own
/// warning, and there is no other way to recover a specific drop's path
/// after the fact. `None` for a drop that instead evaporates on a high
/// density or out-of-bound pixel -- a non-operation, per the doc (Step 4).
/// Reuses [`simulate_one_drop`]'s own physics and discards everything else
/// about its trace; `contours` is never touched (a Hot drop never votes), so
/// an empty slice is passed.
pub(crate) fn hot_drop_evaporation_contour(
    raster: &mut ContourRaster,
    source_contour_idx: u64,
    source: Coord<f64>,
    dir: (f64, f64),
    config: &Config,
) -> Option<(u64, Coord<f64>, Vec<Coord<f64>>)> {
    let drop = simulate_one_drop(
        &mut [],
        raster,
        config,
        Temperature::Hot,
        source_contour_idx,
        source,
        dir,
        1.0,
    );
    match drop.final_hit {
        Some(StepHit::Contour(hit_idx)) => {
            let at = *drop.path.last()?;
            Some((hit_idx, at, drop.path))
        }
        _ => None,
    }
}

/// Step 5's own Elevation Fill Rain Drop Production
/// ([`crate::step5_elevation_raster`]): simulates one Fill drop from
/// `source` in direction `dir`, returning what it evaporated on -- a
/// [`StepHit::Contour`] or a [`StepHit::HighDensity`], the caller's own
/// fallback-elevation case (Step 5) -- together with the drop's own straight
/// track (`source`, then the point it evaporated at). `None` for a drop that
/// instead leaves the map ([`StepHit::OutOfBound`], a no-op per the doc:
/// nothing it touched gets written). Unlike [`hot_drop_evaporation_contour`],
/// the caller has no use for every intermediate step position: since a
/// drop's own direction never changes once it starts (Rain Drop Production
/// Definition), its whole track is a single straight segment, and the caller
/// re-walks that segment itself (`ContourRaster::pixels_along_segment`) to
/// find every pixel needing a value, so only the two endpoints are returned.
pub(crate) fn elevation_fill_drop_track(
    raster: &mut ContourRaster,
    source_contour_idx: u64,
    source: Coord<f64>,
    dir: (f64, f64),
    config: &Config,
) -> Option<(StepHit, Coord<f64>, Coord<f64>)> {
    let drop = simulate_one_drop(
        &mut [],
        raster,
        config,
        Temperature::Fill,
        source_contour_idx,
        source,
        dir,
        1.0,
    );
    match drop.final_hit {
        Some(hit @ (StepHit::Contour(_) | StepHit::HighDensity)) => {
            let end = *drop.path.last()?;
            Some((hit, source, end))
        }
        _ => None,
    }
}

/// The downhill direction a Reverse Cold drop infers for its own *source*
/// contour, having just reached `hit_lwg` (a contour or a Jump) at the point
/// `at`, after travelling in direction `dir` (its own literal direction of
/// travel -- unlike a Cold vote, there is no `direction_sign` to undo here,
/// since there is no source gravity yet to recover). `None` if `hit_lwg` has
/// no gravity yet, meaning there is nothing to infer from.
///
/// This is the doc's own Step 4 accordance/discordance check (there used to
/// decide an elevation, here to decide a side): [`vector_on_side`] recovers
/// `hit_lwg`'s actual gravity vector at `at`'s own local tangent (never a
/// raw comparison of `(gravity_dx, gravity_dy)`, which is only meaningful
/// relative to the tangent it was stored against -- see
/// [`crate::gravity_model::side_of_tangent`]). If `dir` agrees with that
/// vector (their dot product is non-negative -- accordance), the drop was
/// travelling generally downhill when it arrived, so its own launch
/// direction `dir` *is* the source's real downhill side too, by the same
/// regional downhill-sense reasoning the doc already relies on for an
/// ordinary Anti Rain vote. In discordance, the source's real downhill is
/// the opposite of `dir` instead.
fn accordance_downhill(
    hit_lwg: &LineWithGravity,
    at: Coord<f64>,
    dir: (f64, f64),
) -> Option<(f64, f64)> {
    let side = lwg_gravity_side(hit_lwg)?;
    let idx = nearest_index(&hit_lwg.ls, at);
    let (tf, tt) = local_tangent(&hit_lwg.ls, idx);
    let (hit_gx, hit_gy) = vector_on_side(tf, tt, side);
    let accordance = dir.0 * hit_gx + dir.1 * hit_gy >= 0.0;
    Some(if accordance { dir } else { (-dir.0, -dir.1) })
}

/// [`accordance_downhill`] for a high-density hit at `at`, if `at` falls
/// inside one of `line_definers`' own buffered polygons -- a Jump's own
/// gravity (unlike a genuine two-contour rasterization conflict, which has
/// none) is real and reliable, read directly from the .omap tick (Step 1).
/// `None` if `at` isn't inside any Jump's polygon, meaning this high-density
/// pixel really is just an unresolvable conflict.
fn jump_reverse_vote_direction(
    line_definers: &[LineGravityDefiners],
    at: Coord<f64>,
    dir: (f64, f64),
) -> Option<(f64, f64)> {
    let jump = line_definers.iter().find(|j| j.poly.contains(&at))?;
    accordance_downhill(&jump.lwg, at, dir)
}

/// Casts one Reverse Cold vote onto `source`, at the point `source_point` on
/// its own `ls` the drop was launched from, for the side `inferred_downhill`
/// (see [`accordance_downhill`]) points to.
fn cast_reverse_vote(
    source: &mut Contour,
    source_point: Coord<f64>,
    inferred_downhill: (f64, f64),
) {
    let idx = nearest_index(&source.lwg.ls, source_point);
    let (tf, tt) = local_tangent(&source.lwg.ls, idx);
    if vote_side(tf, tt, inferred_downhill.0, inferred_downhill.1) > 0.0 {
        source.lwg.gravity_votes.left += 1;
    } else {
        source.lwg.gravity_votes.right += 1;
    }
}

/// Turns accumulated votes into gravity for every contour that received any
/// and is still undefined, flagging a near tie per
/// `undefined_gravity_vote_threshold`. Called once, after both the rain and
/// anti rain passes have finished, so a contour crossed by drops from both
/// is judged on their combined left/right tally rather than whichever pass
/// reached it first. Returns how many contours resolved.
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
    use geo::{LineString, Polygon};

    fn c(x: f64, y: f64) -> Coord<f64> {
        Coord { x, y }
    }

    fn default_config() -> Config {
        Config {
            bezier_linearization_step: 0.1,
            contours_step: 1.0,
            rasterization_px_size: 0.5,
            heavy_object_width: 1.0,
            heavy_object_growing: 0.2,
            circumference_fitting_points_number: 4,
            slope_lines_contours_search_radius: 3.0,
            step2_vote_min_total_weight: 0.3,
            step2_vote_min_margin: 0.15,
            rain_drop_step: 0.25,
            sources_per_contour_segment: 3,
            rain_drop_starting_voting_hysteresis: 3,
            undefined_gravity_vote_threshold: 0.8,
            elevation_vote_min_total_weight: 0.3,
            elevation_vote_min_margin: 0.15,
            growing_enabled: 1.0,
            obvious_to_close_contour_distance: 0.0,
            searching_fov: 0.0,
            searching_distance: 0.0,
            growing_oob_seeking_max_steps: 0,
            contour_force_window: 4.0,
            attraction_force_window: 4.0,
            contour_force_max_repulsion: 2.0,
            contour_force_equilibrium: 1.0,
            contour_force_max_attraction: -0.5,
            contour_force_second_equilibrium: 3.0,
            out_of_bound_force: 0.5,
            density_region_force: 1.0,
            flying_end_force: 1.0,
            flying_end_merge_distance: 0.5,
            matching_min_force: 0.0,
            grow_time_step: 1.0,
            growing_visualization_push_pull_vectors_scale: 1.0,
            gravity_gaussian_kernel_size: 5,
            elevation_gaussian_kernel_size: 5,
        }
    }

    fn straight_ls(y: f64) -> LineString<f64> {
        LineString::new(vec![c(0.0, y), c(10.0, y), c(20.0, y)])
    }

    fn raster_for(contours: &[Contour]) -> ContourRaster {
        let mut r = ContourRaster::new(c(-5.0, -5.0), 0.5, 60, 60);
        for (i, contour) in contours.iter().enumerate() {
            r.write_contour(i as u64, &contour.lwg.ls);
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
            empty_progeny: false,
        };
        source.lwg.gravity_dx = Some(gx);
        source.lwg.gravity_dy = Some(gy);
        let mut contours = vec![source];
        let mut raster = raster_for(&contours);

        let mut config = default_config();
        config.sources_per_contour_segment = 3;

        let mut trace = PassTrace::default();
        simulate_pass(
            &mut contours,
            &mut raster,
            &config,
            Temperature::Cold,
            1.0,
            &mut trace,
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
            empty_progeny: false,
        };
        defined.lwg.gravity_dx = Some(0.0);
        defined.lwg.gravity_dy = Some(1.0); // downhill = +y, toward the second contour
        let undefined = Contour {
            lwg: LineWithGravity::new(straight_ls(5.0)),
            elevation_height: None,
            empty_progeny: false,
        };
        let mut contours = vec![defined, undefined];
        let mut raster = raster_for(&contours);
        let config = default_config();

        let result = resolve(&mut contours, &mut raster, &[], &config);
        assert!(result.resolved_by_votes >= 1);
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
            empty_progeny: false,
        };
        top.lwg.gravity_dx = Some(0.0);
        top.lwg.gravity_dy = Some(1.0);
        let middle = Contour {
            lwg: LineWithGravity::new(straight_ls(5.0)),
            elevation_height: None,
            empty_progeny: false,
        };
        let mut bottom = Contour {
            lwg: LineWithGravity::new(straight_ls(10.0)),
            elevation_height: None,
            empty_progeny: false,
        };
        bottom.lwg.gravity_dx = Some(0.0);
        bottom.lwg.gravity_dy = Some(-1.0);
        let mut contours = vec![top, middle, bottom];
        let mut raster = raster_for(&contours);
        let config = default_config();

        let result = resolve(&mut contours, &mut raster, &[], &config);
        assert!(contours[1].lwg.gravity_dx.is_some());
        assert!(
            !result.warnings.is_empty(),
            "expected a near-tied vote warning"
        );
    }

    #[test]
    fn anti_rain_drop_production_gives_the_hit_contour_the_same_downhill_sense_as_its_source() {
        // The source's downhill is +y; its undefined neighbor sits at
        // y = -3, reachable only by travelling *against* gravity (Anti
        // Rain), never by a Rain drop (which only ever travels toward +y).
        // Since nearby contours share the same local downhill sense
        // (Assumption 1), the neighbor -- lying on the uphill side of the
        // source -- should end up with downhill pointing back toward the
        // source, i.e. still +y, not the opposite (-y).
        let mut defined = Contour {
            lwg: LineWithGravity::new(straight_ls(0.0)),
            elevation_height: None,
            empty_progeny: false,
        };
        defined.lwg.gravity_dx = Some(0.0);
        defined.lwg.gravity_dy = Some(1.0);
        let undefined = Contour {
            lwg: LineWithGravity::new(straight_ls(-3.0)),
            elevation_height: None,
            empty_progeny: false,
        };
        let mut contours = vec![defined, undefined];
        let mut raster = raster_for(&contours);
        let config = default_config();

        let result = resolve(&mut contours, &mut raster, &[], &config);
        assert_eq!(
            result.resolved_by_votes, 1,
            "expected the neighbor to be resolved, by votes only Cold Anti Rain Drop \
             Production could have cast"
        );
        assert_eq!(
            contours[1].lwg.gravity_dy,
            Some(1.0),
            "the uphill neighbor's downhill direction should point back toward its source \
             (same sense as the source's own downhill), not away from it"
        );
    }

    #[test]
    fn reverse_cold_recovers_a_contour_anti_rain_could_never_have_sourced_from() {
        // A (defined, downhill +y) spans x in [0, 30]; its rain-drop sources
        // fall at x = 0, 5, 10, 15, 20, 25 (6 evenly spaced sources on its
        // one segment), two of which (20 and 25) fall inside B's own x span
        // and so vote it toward gravity_dy = 1.0, same sense as A.
        //
        // B (undefined) spans x in [20, 50] at y = 5 -- reachable from A's
        // rain drops, but never from A's own anti rain drops, which travel
        // straight down (-y) from x in [0, 25] and so never cross B at all.
        //
        // C (undefined) spans x in [40, 60] at y = -5 -- overlapping B's own
        // span, but not A's ([0, 30]). Under the Cold passes alone, the only
        // drop that could ever reach it is an anti rain drop sourced from B
        // itself -- and B is never promoted to a source mid-pass (its votes
        // are only tallied once, after Anti Rain Drop Production has already
        // run), so the Cold passes alone leave C stranded. This is exactly
        // the gap the Reverse Cold fallback closes: sourced from C itself
        // (not from B), it searches straight through B's own still-undefined
        // status at the time the Cold passes ran, past it, and now finds B
        // already resolved by the time Reverse Cold gets to run, letting C
        // resolve too.
        let mut a = Contour {
            lwg: LineWithGravity::new(LineString::new(vec![c(0.0, 0.0), c(30.0, 0.0)])),
            elevation_height: None,
            empty_progeny: false,
        };
        a.lwg.gravity_dx = Some(0.0);
        a.lwg.gravity_dy = Some(1.0);
        let b = Contour {
            lwg: LineWithGravity::new(LineString::new(vec![c(20.0, 5.0), c(50.0, 5.0)])),
            elevation_height: None,
            empty_progeny: false,
        };
        let c_contour = Contour {
            lwg: LineWithGravity::new(LineString::new(vec![c(40.0, -5.0), c(60.0, -5.0)])),
            elevation_height: None,
            empty_progeny: false,
        };
        let mut contours = vec![a, b, c_contour];
        let mut raster = ContourRaster::new(c(-5.0, -10.0), 0.5, 140, 40);
        for (i, contour) in contours.iter().enumerate() {
            raster.write_contour(i as u64, &contour.lwg.ls);
        }
        let mut config = default_config();
        config.sources_per_contour_segment = 6;

        let result = resolve(&mut contours, &mut raster, &[], &config);
        assert_eq!(
            contours[1].lwg.gravity_dy,
            Some(1.0),
            "B should resolve from its rain-pass votes, same downhill sense as A"
        );
        assert_eq!(
            contours[2].lwg.gravity_dy,
            Some(1.0),
            "C should be recovered by the Reverse Cold fallback, searching from C itself past \
             B (undefined during the Cold passes) to B's own now-resolved gravity -- same \
             downhill sense as A and B, by the same regional-consistency reasoning as everywhere \
             else in Step 3"
        );
        assert_eq!(result.resolved_by_votes, 2);
    }

    #[test]
    fn reverse_cold_votes_from_a_jump_high_density_area_but_not_from_a_genuine_conflict() {
        // C (undefined) spans x in [0, 10] at y = 0, with no other contour
        // anywhere -- the only thing it could ever learn from is a Jump.
        let c_contour = Contour {
            lwg: LineWithGravity::new(LineString::new(vec![c(0.0, 0.0), c(10.0, 0.0)])),
            elevation_height: None,
            empty_progeny: false,
        };
        let mut contours = vec![c_contour];
        let mut raster = ContourRaster::new(c(-10.0, -10.0), 0.5, 60, 40);
        raster.write_contour(0, &contours[0].lwg.ls);

        // A Jump's own buffered polygon, straddling C's whole x span,
        // reachable by a Reverse Cold drop heading north (+y) from any
        // source on C. Its gravity (read from the .omap tick in the real
        // pipeline) points the same way, +y. Its own lower edge (2.9) is
        // deliberately off the `rain_drop_step` (0.25) grid every source
        // steps on from y = 0, so a drop's own landing point falls cleanly
        // inside the polygon rather than exactly on its boundary, where
        // `geo`'s own point-in-polygon containment is false.
        let jump_poly = Polygon::new(
            LineString::new(vec![
                c(-2.0, 2.9),
                c(12.0, 2.9),
                c(12.0, 6.0),
                c(-2.0, 6.0),
                c(-2.0, 2.9),
            ]),
            vec![],
        );
        raster.mark_high_density_polygon(&jump_poly);
        let mut jump_lwg = LineWithGravity::new(LineString::new(vec![c(0.0, 4.5), c(10.0, 4.5)]));
        jump_lwg.gravity_dx = Some(0.0);
        jump_lwg.gravity_dy = Some(1.0);
        let line_definers = vec![LineGravityDefiners {
            lwg: jump_lwg,
            poly: jump_poly,
            touched_contours: Vec::new(),
        }];

        let config = default_config();
        let result = resolve(&mut contours, &mut raster, &line_definers, &config);
        assert_eq!(
            contours[0].lwg.gravity_dy,
            Some(1.0),
            "C should be recovered from the Jump's own gravity, read from the high-density \
             pixel a Reverse Cold drop hit inside the Jump's own polygon"
        );
        assert_eq!(result.resolved_by_votes, 1);
    }

    #[test]
    fn reverse_cold_does_not_vote_from_a_high_density_pixel_outside_every_jump_polygon() {
        // Same lone-contour setup, but the high-density pixels it will hit
        // come from a genuine two-contour rasterization conflict (Step 1),
        // not any Jump -- `line_definers` is empty, so there is nothing to
        // read a vote from, and C must stay undefined.
        let c_contour = Contour {
            lwg: LineWithGravity::new(LineString::new(vec![c(0.0, 0.0), c(10.0, 0.0)])),
            elevation_height: None,
            empty_progeny: false,
        };
        let mut contours = vec![c_contour];
        let mut raster = ContourRaster::new(c(-10.0, -10.0), 0.5, 60, 40);
        raster.write_contour(0, &contours[0].lwg.ls);
        // Two overlapping contour writes at the same spot conflict into a
        // high-density pixel, the same trick `cold_drop_evaporates_on_high_density`
        // uses.
        raster.write_contour(1, &LineString::new(vec![c(4.9, 5.0), c(5.1, 5.0)]));
        raster.write_contour(2, &LineString::new(vec![c(4.9, 5.0), c(5.1, 5.0)]));

        let config = default_config();
        let result = resolve(&mut contours, &mut raster, &[], &config);

        assert!(
            contours[0].lwg.gravity_dx.is_none(),
            "a genuine conflict has no gravity to offer, so C must stay undefined"
        );
        assert_eq!(result.resolved_by_votes, 0);
        assert!(
            result
                .warnings
                .iter()
                .any(|w| w.contains("contour 0") && w.contains("dropped")),
            "expected a dropped-contour warning naming contour 0: {:?}",
            result.warnings
        );
    }

    #[test]
    fn a_contour_no_drop_ever_reaches_is_dropped_with_a_length_warning() {
        // A lone undefined contour with no other contour ever giving it
        // gravity: no drop from it (it has none, being undefined) and no
        // drop from elsewhere reaches it, so it can never be resolved --
        // it should be dropped, with a warning naming its own length,
        // rather than failing the whole run.
        let mut contours = vec![Contour {
            lwg: LineWithGravity::new(straight_ls(0.0)),
            elevation_height: None,
            empty_progeny: false,
        }];
        let mut raster = raster_for(&contours);
        let config = default_config();

        let result = resolve(&mut contours, &mut raster, &[], &config);
        assert!(
            contours[0].lwg.gravity_dx.is_none(),
            "nothing could ever have resolved this lone contour"
        );
        // straight_ls(0.0) runs from x=0 to x=20: a 20m long contour.
        assert!(
            result
                .warnings
                .iter()
                .any(|w| w.contains("contour 0") && w.contains("20.0m") && w.contains("dropped")),
            "expected a dropped-contour warning naming contour 0's own 20m length: {:?}",
            result.warnings
        );
        // The partial result -- here, simply no rain drops at all, since
        // there was no defined contour to send any from -- is still handed
        // back rather than discarded, so a caller can still inspect it.
        assert_eq!(result.resolved_by_votes, 0);
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
            empty_progeny: false,
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
            empty_progeny: false,
        };
        vec![source, bracket]
    }

    #[test]
    fn re_crossing_a_voted_undefined_contour_within_hysteresis_does_not_evaporate() {
        let mut contours = source_and_bracket_contours();
        let mut raster = ContourRaster::new(c(0.0, 0.0), 0.5, 40, 40);
        raster.write_contour(1, &contours[1].lwg.ls);

        let mut config = default_config();
        config.rain_drop_step = 0.25;
        // Both crossings (around step 4 and step 8) fall well inside this.
        config.rain_drop_starting_voting_hysteresis = 10;

        let drop = simulate_one_drop(
            &mut contours,
            &mut raster,
            &config,
            Temperature::Cold,
            0,
            c(5.0, 0.0),
            (0.0, 1.0),
            1.0,
        );
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
        raster.write_contour(1, &contours[1].lwg.ls);

        let mut config = default_config();
        config.rain_drop_step = 0.25;
        config.rain_drop_starting_voting_hysteresis = 0;

        let drop = simulate_one_drop(
            &mut contours,
            &mut raster,
            &config,
            Temperature::Cold,
            0,
            c(5.0, 0.0),
            (0.0, 1.0),
            1.0,
        );
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
                    empty_progeny: false,
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
                empty_progeny: false,
            },
        ];
        let mut raster = ContourRaster::new(c(0.0, 0.0), 0.5, 20, 220);
        raster.write_contour(1, &contours[1].lwg.ls);

        let mut config = default_config();
        // Matches the raster's own pixel size, so each step touches exactly
        // one pixel row -- a finer step would let a single geometric
        // crossing register as two separate hits (one per step still inside
        // the same pixel), which is a rasterization artifact unrelated to
        // what this test means to exercise.
        config.rain_drop_step = 0.5;
        config.rain_drop_starting_voting_hysteresis = 5;

        let drop = simulate_one_drop(
            &mut contours,
            &mut raster,
            &config,
            Temperature::Cold,
            0,
            c(5.0, 0.0),
            (0.0, 1.0),
            1.0,
        );
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
                    empty_progeny: false,
                };
                source.lwg.gravity_dx = Some(0.0);
                source.lwg.gravity_dy = Some(1.0);
                source
            },
            Contour {
                lwg: LineWithGravity::new(LineString::new(vec![c(0.0, 20.0), c(10.0, 20.0)])),
                elevation_height: None,
                empty_progeny: false,
            },
        ];
        let mut raster = ContourRaster::new(c(0.0, 0.0), 1.0, 20, 40);
        raster.write_contour(1, &contours[1].lwg.ls);

        let mut config = default_config();
        config.rain_drop_step = 1.0;
        config.rain_drop_starting_voting_hysteresis = 3;

        let drop = simulate_one_drop(
            &mut contours,
            &mut raster,
            &config,
            Temperature::Cold,
            0,
            c(5.0, 0.0),
            (0.0, 1.0),
            1.0,
        );
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

    #[test]
    fn hot_drop_evaporates_on_any_contour_regardless_of_gravity() {
        let mut raster = ContourRaster::new(c(0.0, 0.0), 0.5, 40, 40);
        let target_ls = LineString::new(vec![c(0.0, 10.0), c(20.0, 10.0)]);
        raster.write_contour(1, &target_ls); // no gravity ever set on contour 1
        let drop = simulate_one_drop(
            &mut [],
            &mut raster,
            &default_config(),
            Temperature::Hot,
            0,
            c(5.0, 0.0),
            (0.0, 1.0),
            1.0,
        );
        // Evaporated right at the target contour, well short of the far border.
        assert!(drop.path.last().unwrap().y < 12.0);
    }

    #[test]
    fn hot_drop_evaporates_on_high_density() {
        let mut raster = ContourRaster::new(c(0.0, 0.0), 0.5, 40, 40);
        raster.write_contour(1, &LineString::new(vec![c(4.9, 5.0), c(5.1, 5.0)]));
        raster.write_contour(2, &LineString::new(vec![c(4.9, 5.0), c(5.1, 5.0)])); // conflict -> high density
        let drop = simulate_one_drop(
            &mut [],
            &mut raster,
            &default_config(),
            Temperature::Hot,
            0,
            c(5.0, 0.0),
            (0.0, 1.0),
            1.0,
        );
        assert!(drop.path.last().unwrap().y < 7.0);
    }

    #[test]
    fn hot_drop_clears_its_own_starting_contour_without_evaporating_on_it() {
        let mut raster = ContourRaster::new(c(0.0, 0.0), 0.5, 40, 40);
        let own_ls = LineString::new(vec![c(0.0, 5.0), c(20.0, 5.0)]);
        raster.write_contour(0, &own_ls);
        let drop = simulate_one_drop(
            &mut [],
            &mut raster,
            &default_config(),
            Temperature::Hot,
            0,
            c(5.0, 5.0),
            (0.0, 1.0),
            1.0,
        );
        // Never hits its own contour again on the way out; only the map
        // border (an out-of-array pixel) stops it. A straight source never
        // re-crosses itself, so this alone can't tell a time-limited
        // exemption apart from a permanent one -- see the test below for
        // that.
        assert!(drop.path.last().unwrap().y > 15.0);
    }

    #[test]
    fn hot_drop_evaporates_on_its_own_source_contour_once_outside_the_hysteresis_window() {
        // A concave source contour that curves back across its own path:
        // the drop, launched perpendicular to the near arm, re-crosses the
        // far arm of the *same* contour well outside its own starting
        // window. It must evaporate there instead of sailing straight
        // through, or a concave hill contour in Step 4 could send a Hot
        // drop back onto its own boundary and never stop -- see
        // Contours-to-Raster.md's Step 4 for where this was discovered.
        let mut raster = ContourRaster::new(c(-20.0, -5.0), 0.5, 80, 80);
        // An upside-down "U" (no bottom edge): a horizontal ray at y = 5
        // crosses the near arm (x = 0) right at the drop's own origin, and
        // the far arm (x = -15) again much later -- a real self-crossing of
        // the *same* contour, not a different one.
        let concave_ls = LineString::new(vec![
            c(0.0, 0.0),
            c(0.0, 20.0),
            c(-15.0, 20.0),
            c(-15.0, 0.0),
        ]);
        raster.write_contour(0, &concave_ls);
        let mut config = default_config();
        config.rain_drop_starting_voting_hysteresis = 3;

        let drop = simulate_one_drop(
            &mut [],
            &mut raster,
            &config,
            Temperature::Hot,
            0,
            c(0.0, 5.0),
            (-1.0, 0.0),
            1.0,
        );
        // Evaporates around x = -15 (the far arm), nowhere near the map's
        // own border at x = -20.
        assert!(
            drop.path.last().unwrap().x > -17.0,
            "expected the drop to evaporate on its own contour's far arm, got {:?}",
            drop.path.last()
        );
    }

    #[test]
    fn fill_drop_evaporates_on_high_density() {
        let mut raster = ContourRaster::new(c(0.0, 0.0), 0.5, 40, 40);
        raster.write_contour(1, &LineString::new(vec![c(4.9, 5.0), c(5.1, 5.0)]));
        raster.write_contour(2, &LineString::new(vec![c(4.9, 5.0), c(5.1, 5.0)])); // conflict -> high density
        raster.write_contour(3, &LineString::new(vec![c(0.0, 10.0), c(20.0, 10.0)]));
        let drop = simulate_one_drop(
            &mut [],
            &mut raster,
            &default_config(),
            Temperature::Fill,
            0,
            c(5.0, 0.0),
            (0.0, 1.0),
            1.0,
        );
        // Stops at the high-density pixel around y = 5, exactly like a Hot
        // drop does (`hot_drop_evaporates_on_high_density` above), never
        // reaching the real contour further along at y = 10 -- unlike a
        // high-density conflict really does hide a whole cluster of real,
        // distinct contours the drop can no longer just leapfrog past.
        assert_eq!(drop.final_hit, Some(StepHit::HighDensity));
        assert!(drop.path.last().unwrap().y < 7.0);
    }

    #[test]
    fn fill_drop_evaporates_on_any_other_contour_regardless_of_gravity() {
        let mut raster = ContourRaster::new(c(0.0, 0.0), 0.5, 40, 40);
        let target_ls = LineString::new(vec![c(0.0, 10.0), c(20.0, 10.0)]);
        raster.write_contour(1, &target_ls); // no gravity ever set on contour 1
        let drop = simulate_one_drop(
            &mut [],
            &mut raster,
            &default_config(),
            Temperature::Fill,
            0,
            c(5.0, 0.0),
            (0.0, 1.0),
            1.0,
        );
        assert_eq!(drop.final_hit, Some(StepHit::Contour(1)));
        assert!(drop.path.last().unwrap().y < 12.0);
    }

    #[test]
    fn fill_drop_evaporates_on_its_own_starting_contour_once_outside_the_hysteresis_window() {
        // Same concave "U" shape `hot_drop_evaporates_on_its_own_source_contour_once_outside_the_hysteresis_window`
        // uses: a Fill drop must get exactly the same starting-contour
        // exemption window Hot gets, past which even its own source
        // contour evaporates it (the doc's own "also the contours that has
        // generate them").
        let mut raster = ContourRaster::new(c(-20.0, -5.0), 0.5, 80, 80);
        let concave_ls = LineString::new(vec![
            c(0.0, 0.0),
            c(0.0, 20.0),
            c(-15.0, 20.0),
            c(-15.0, 0.0),
        ]);
        raster.write_contour(0, &concave_ls);
        let mut config = default_config();
        config.rain_drop_starting_voting_hysteresis = 3;

        let drop = simulate_one_drop(
            &mut [],
            &mut raster,
            &config,
            Temperature::Fill,
            0,
            c(0.0, 5.0),
            (-1.0, 0.0),
            1.0,
        );
        assert_eq!(drop.final_hit, Some(StepHit::Contour(0)));
        assert!(
            drop.path.last().unwrap().x > -17.0,
            "expected the drop to evaporate on its own contour's far arm, got {:?}",
            drop.path.last()
        );
    }

    #[test]
    fn fill_drop_clears_its_own_starting_contour_within_the_hysteresis_window() {
        let mut raster = ContourRaster::new(c(0.0, 0.0), 0.5, 40, 40);
        let own_ls = LineString::new(vec![c(0.0, 5.0), c(20.0, 5.0)]);
        raster.write_contour(0, &own_ls);
        let drop = simulate_one_drop(
            &mut [],
            &mut raster,
            &default_config(),
            Temperature::Fill,
            0,
            c(5.0, 5.0),
            (0.0, 1.0),
            1.0,
        );
        // Never hits its own contour again on the way out; only the map
        // border (out of bound) stops it.
        assert_eq!(drop.final_hit, Some(StepHit::OutOfBound));
        assert!(drop.path.last().unwrap().y > 15.0);
    }

    #[test]
    fn elevation_fill_drop_track_is_none_when_the_drop_leaves_the_map() {
        let mut raster = ContourRaster::new(c(0.0, 0.0), 0.5, 20, 20);
        // Nothing else on the raster: straight out of bound.
        let result =
            elevation_fill_drop_track(&mut raster, 0, c(5.0, 5.0), (0.0, 1.0), &default_config());
        assert!(result.is_none());
    }

    #[test]
    fn elevation_fill_drop_track_reports_the_hit_contour_and_the_straight_track_endpoints() {
        let mut raster = ContourRaster::new(c(0.0, 0.0), 0.5, 40, 40);
        raster.write_contour(1, &LineString::new(vec![c(0.0, 10.0), c(20.0, 10.0)]));
        let source = c(5.0, 0.0);
        let (hit, start, end) =
            elevation_fill_drop_track(&mut raster, 0, source, (0.0, 1.0), &default_config())
                .unwrap();
        assert_eq!(hit, StepHit::Contour(1));
        assert_eq!(start, source);
        assert!(end.y > 9.5 && end.y < 12.0);
    }

    #[test]
    fn elevation_fill_drop_track_reports_high_density_as_a_hit_too() {
        let mut raster = ContourRaster::new(c(0.0, 0.0), 0.5, 40, 40);
        raster.write_contour(1, &LineString::new(vec![c(4.9, 5.0), c(5.1, 5.0)]));
        raster.write_contour(2, &LineString::new(vec![c(4.9, 5.0), c(5.1, 5.0)])); // conflict -> high density
        let source = c(5.0, 0.0);
        let (hit, start, end) =
            elevation_fill_drop_track(&mut raster, 0, source, (0.0, 1.0), &default_config())
                .unwrap();
        assert_eq!(hit, StepHit::HighDensity);
        assert_eq!(start, source);
        assert!(end.y < 7.0);
    }

    #[test]
    fn cold_drop_evaporates_on_high_density() {
        let mut raster = ContourRaster::new(c(0.0, 0.0), 0.5, 40, 40);
        raster.write_contour(1, &LineString::new(vec![c(4.9, 5.0), c(5.1, 5.0)]));
        raster.write_contour(2, &LineString::new(vec![c(4.9, 5.0), c(5.1, 5.0)])); // conflict -> high density
        let mut contours: Vec<Contour> = Vec::new();
        let drop = simulate_one_drop(
            &mut contours,
            &mut raster,
            &default_config(),
            Temperature::Cold,
            u64::MAX,
            c(5.0, 0.0),
            (0.0, 1.0),
            1.0,
        );
        // Evaporated right at the high-density pixel, well short of the far border.
        assert!(drop.path.last().unwrap().y < 7.0);
    }

    #[test]
    fn flood_fill_marks_pixels_between_two_contours_but_not_pixels_that_leave_the_map() {
        let mut raster = ContourRaster::new(c(-5.0, -5.0), 0.5, 60, 60);
        let ls = LineString::new(vec![c(0.0, 0.0), c(10.0, 0.0), c(20.0, 0.0)]);
        raster.write_contour(0, &ls);
        // A "ceiling" contour a few meters above: every upward Hot drop
        // evaporates by hitting it (not by leaving the map), so its path
        // gets committed. Nothing else exists below contour 0, so every
        // downward drop leaves the map instead and must commit nothing.
        raster.write_contour(1, &LineString::new(vec![c(-10.0, 3.0), c(30.0, 3.0)]));
        let config = default_config();
        flood_fill_from_contour(&mut raster, &ls, 0, 1.0, &config);

        let has_no_contour_in_bound = |y_lo: f64, y_hi: f64| {
            (0..raster.width as i64).any(|x| {
                ((y_lo / 0.5) as i64..(y_hi / 0.5) as i64).any(|py_off| {
                    let (_, py) = raster.to_px(c(0.0, y_lo));
                    raster.get(x, py + py_off) == crate::contour_raster::NO_CONTOUR_IN_BOUND
                })
            })
        };
        assert!(
            has_no_contour_in_bound(1.0, 2.5),
            "expected a marked pixel between the two contours, where the upward \
             drop evaporates by hitting one rather than leaving the map"
        );
        assert!(
            !has_no_contour_in_bound(-4.5, -0.5),
            "a downward drop leaves the map with nothing else to hit; its path \
             must not have been committed, or the later out-of-bound computation \
             could never flood-fill through it"
        );
    }
}
