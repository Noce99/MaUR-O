//! Step 2 of `Contours-to-Raster.md`: setting a contour's gravity from the
//! evidence Step 1 gathered, through a priority chain instead of treating
//! every reading as equally authoritative:
//!
//! 1. A Slope Line is explicit, mapper-placed evidence -- if a contour has
//!    one, it wins outright over any Heavy Object or Jump reading on the
//!    same contour. Two *disagreeing* Slope Lines on one contour still
//!    `Err`s: that's a real mapping mistake, not sensor noise.
//! 2. A closed contour enclosing nothing else is assumed to be a hill,
//!    gravity pointing away from its own enclosed area -- checked next, for
//!    any contour a Slope Line didn't already resolve.
//! 3. Everything still undefined is resolved by a confidence-weighted vote
//!    among its Heavy Object and Jump readings, each weighted by how
//!    perpendicular it is to the contour at its own contact point (see
//!    [`crate::gravity_model::tangent_alignment_confidence`]). A contour
//!    only resolves this way if the evidence clears both
//!    `step2_vote_min_total_weight` and `step2_vote_min_margin`; otherwise
//!    it's left undefined for Step 3's rain-drop production to resolve
//!    instead.
//! 4. Whenever a contour is locked by (1) or (2) but its Heavy Object/Jump
//!    evidence would have voted the other way (clearing the same two
//!    thresholds), a warning is recorded instead of silently discarding it.

use geo::algorithm::Contains;
use geo::Coord;

use crate::contour_geometry::{local_tangent, nearest_index};
use crate::contours_to_raster_config::Config;
use crate::gravity_model::{
    contour_gravity_side, contour_polygon, encloses_another_contour, gravity_vector_for_side,
    lwg_gravity_side, set_or_check_gravity, side_of_tangent, tangent_alignment_confidence,
    vector_on_side, Contour, GravityReadingSource, LineGravityDefiners, PointGravityDefiners,
    WeightedGravityVote,
};

/// What Step 2 resolved, and what it could not.
pub struct Step2Result {
    /// How many contours got their gravity set from a Slope Line.
    pub resolved_by_slope_line: u64,
    /// How many contours got their gravity from the closed-hill heuristic.
    pub resolved_by_hill: u64,
    /// How many contours got their gravity from the Heavy Object/Jump
    /// confidence-weighted vote.
    pub resolved_by_vote: u64,
    /// Indices into the `Vec<Contour>` still without a defined gravity.
    pub still_undefined: Vec<usize>,
    /// A contour whose gravity was locked by a Slope Line or the hill
    /// heuristic, but whose Heavy Object/Jump evidence would confidently
    /// have voted the other way.
    pub warnings: Vec<String>,
}

/// Runs Step 2 (see the module doc for the full priority chain): Slope Line
/// readings first (`Err`ing, naming the conflict, only if two disagree on
/// the same contour); then the closed-hill heuristic; then a
/// confidence-weighted vote among Heavy Object and Jump readings for
/// whatever is still undefined, using `config.step2_vote_min_total_weight`/
/// `config.step2_vote_min_margin` to decide whether that evidence is trusted
/// or left for Step 3. Takes no `ContourRaster` -- unlike Step 1's own use of
/// one to find this evidence in the first place, applying it here only ever
/// needs each definer's own already-recorded position and index, not a fresh
/// raster scan (a `LineGravityDefiners`' own `touched_contours` is captured
/// before its polygon's area is stamped high density, precisely so Step 2
/// never needs to read the raster again for it).
pub fn resolve(
    contours: &mut [Contour],
    point_definers: &[PointGravityDefiners],
    line_definers: &[LineGravityDefiners],
    config: &Config,
) -> Result<Step2Result, String> {
    let mut resolved_by_slope_line = 0u64;

    // 1. Slope Line readings: the only readings still allowed to `Err` on a
    // mutual conflict, since two disagreeing Slope Lines on one contour is a
    // real mapping mistake rather than a noisy inference.
    for definer in point_definers {
        if definer.source != GravityReadingSource::SlopeLine {
            continue;
        }
        let (Some(dx), Some(dy)) = (definer.gravity_dx, definer.gravity_dy) else {
            continue;
        };
        let Some(contour) = contours.get_mut(definer.reference_contour as usize) else {
            continue;
        };
        let idx = nearest_index(
            &contour.lwg.ls,
            Coord {
                x: definer.x,
                y: definer.y,
            },
        );
        let (from, to) = local_tangent(&contour.lwg.ls, idx);
        let was_defined = contour.lwg.gravity_dx.is_some();
        set_or_check_gravity(contour, from, to, dx, dy).map_err(|e| {
            format!(
                "Step 2: a Slope Line reading at ({:.2}, {:.2}) conflicts with contour {}'s \
                 gravity: {e}",
                definer.x, definer.y, definer.reference_contour
            )
        })?;
        if !was_defined && contour.lwg.gravity_dx.is_some() {
            resolved_by_slope_line += 1;
        }
    }

    // 2. Closed-hill heuristic, before Heavy Object/Jump evidence is
    // consulted at all.
    let resolved_by_hill = assign_hill_gravity(contours);

    // 3. Accumulate confidence-weighted Heavy Object + Jump evidence per
    // contour, for every contour -- including ones already locked by (1) or
    // (2), which still need it for (4)'s disagreement check.
    let mut votes = vec![WeightedGravityVote::default(); contours.len()];

    for definer in point_definers {
        if definer.source != GravityReadingSource::HeavyObject {
            continue;
        }
        let (Some(dx), Some(dy)) = (definer.gravity_dx, definer.gravity_dy) else {
            continue;
        };
        let Some(contour) = contours.get(definer.reference_contour as usize) else {
            continue;
        };
        let idx = nearest_index(
            &contour.lwg.ls,
            Coord {
                x: definer.x,
                y: definer.y,
            },
        );
        let (from, to) = local_tangent(&contour.lwg.ls, idx);
        let side = side_of_tangent(from, to, dx, dy);
        let weight = tangent_alignment_confidence(from, to, dx, dy);
        votes[definer.reference_contour as usize].add(side, weight);
    }

    for definer in line_definers {
        let Some(side) = lwg_gravity_side(&definer.lwg) else {
            continue;
        };
        // Read from `touched_contours`, captured back in Step 1 before this
        // Jump's own polygon was stamped high density -- re-scanning
        // `definer.poly` against the raster here, the way this used to
        // work, would find nothing: every one of those pixels shows high
        // density now, not the contour that (still) actually runs under it.
        for &(contour_idx, center) in &definer.touched_contours {
            let Some(contour) = contours.get(contour_idx as usize) else {
                continue;
            };
            // The gravity reading itself must be perpendicular to the Jump's
            // own line *at this point*, not the fixed vector its lwg stores
            // relative to its own ls[0] -> ls[1] -- a curved Jump's true
            // downhill direction varies along its length exactly like a
            // contour's does (see `lwg_gravity_side`).
            let jump_idx = nearest_index(&definer.lwg.ls, center);
            let (jump_from, jump_to) = local_tangent(&definer.lwg.ls, jump_idx);
            let (dx, dy) = vector_on_side(jump_from, jump_to, side);
            let idx = nearest_index(&contour.lwg.ls, center);
            let (from, to) = local_tangent(&contour.lwg.ls, idx);
            let contour_side = side_of_tangent(from, to, dx, dy);
            let weight = tangent_alignment_confidence(from, to, dx, dy);
            votes[contour_idx as usize].add(contour_side, weight);
        }
    }

    // 4. Resolve whatever the vote can, and flag a confident disagreement on
    // whatever (1)/(2) already locked.
    let mut resolved_by_vote = 0u64;
    let mut warnings = Vec::new();
    for (i, vote) in votes.iter().enumerate() {
        let Some(side) = vote.resolve(config.step2_vote_min_total_weight, config.step2_vote_min_margin)
        else {
            continue;
        };
        match contour_gravity_side(&contours[i]) {
            Some(existing_side) => {
                if existing_side != side {
                    warnings.push(format!(
                        "Step 2: contour {i}'s Heavy Object/Jump evidence confidently favors the \
                         opposite side from the gravity a Slope Line or the closed-hill \
                         heuristic already set for it"
                    ));
                }
            }
            None => {
                let ls = contours[i].lwg.ls.clone();
                let (gx, gy) = gravity_vector_for_side(&ls, side);
                contours[i].lwg.gravity_dx = Some(gx);
                contours[i].lwg.gravity_dy = Some(gy);
                resolved_by_vote += 1;
            }
        }
    }

    let still_undefined = contours
        .iter()
        .enumerate()
        .filter(|(_, c)| c.lwg.gravity_dx.is_none())
        .map(|(i, _)| i)
        .collect();

    Ok(Step2Result {
        resolved_by_slope_line,
        resolved_by_hill,
        resolved_by_vote,
        still_undefined,
        warnings,
    })
}

/// A closed contour that encloses no other closed contour is assumed to be a
/// hill: gravity points away from its own enclosed area. Returns how many
/// contours resolved this way.
fn assign_hill_gravity(contours: &mut [Contour]) -> u64 {
    let mut resolved = 0u64;
    for i in 0..contours.len() {
        if contours[i].lwg.gravity_dx.is_some() || !contours[i].lwg.ls.is_closed() {
            continue;
        }
        if encloses_another_contour(i, contours) {
            continue;
        }
        let ls = contours[i].lwg.ls.clone();
        if ls.0.len() < 2 {
            continue;
        }
        let (a, b) = (ls.0[0], ls.0[1]);
        let mid = Coord {
            x: (a.x + b.x) / 2.0,
            y: (a.y + b.y) / 2.0,
        };
        // A short probe, relative to the contour's own node spacing, is
        // enough to land outside a convex-ish hill boundary without
        // overshooting into a different, nearby contour's interior.
        let probe_len = (a.x - b.x).hypot(a.y - b.y).max(1e-6) * 0.1;
        let poly = contour_polygon(&contours[i]);
        for side in [1.0, -1.0] {
            let (dx, dy) = vector_on_side(a, b, side);
            let probe = Coord {
                x: mid.x + dx * probe_len,
                y: mid.y + dy * probe_len,
            };
            if !poly.contains(&probe) {
                let (gx, gy) = gravity_vector_for_side(&ls, side);
                contours[i].lwg.gravity_dx = Some(gx);
                contours[i].lwg.gravity_dy = Some(gy);
                resolved += 1;
                break;
            }
        }
    }
    resolved
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gravity_model::{
        gravity_vector_for_side, GravityVotes, LineGravityDefiners, LineWithGravity,
    };
    use geo::{LineString, Polygon};

    fn c(x: f64, y: f64) -> Coord<f64> {
        Coord { x, y }
    }

    fn straight_contour() -> Contour {
        Contour {
            lwg: LineWithGravity::new(LineString::new(vec![
                c(0.0, 5.0),
                c(10.0, 5.0),
                c(20.0, 5.0),
            ])),
            elevation_height: None,
            empty_progeny: false,
        }
    }

    /// A `Config` with every field filled (it's exhaustive, no `Default`
    /// impl); only `step2_vote_min_total_weight`/`step2_vote_min_margin` are
    /// meaningful to these tests, so callers override them via
    /// `Config { step2_vote_min_total_weight: .., ..test_config() }`.
    fn test_config() -> Config {
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

    #[test]
    fn agreeing_slope_lines_set_gravity_once() {
        let mut contours = vec![straight_contour()];
        let definers = vec![
            PointGravityDefiners {
                x: 10.0,
                y: 5.0,
                reference_contour: 0,
                gravity_dx: Some(0.0),
                gravity_dy: Some(-1.0),
                source: GravityReadingSource::SlopeLine,
            },
            PointGravityDefiners {
                x: 15.0,
                y: 5.0,
                reference_contour: 0,
                gravity_dx: Some(0.0),
                gravity_dy: Some(-1.0),
                source: GravityReadingSource::SlopeLine,
            },
        ];
        let result = resolve(&mut contours, &definers, &[], &test_config()).unwrap();
        assert_eq!(result.resolved_by_slope_line, 1);
        assert!(result.still_undefined.is_empty());
    }

    #[test]
    fn disagreeing_slope_lines_still_error() {
        // Two disagreeing Slope Lines on the same contour is a real mapping
        // mistake, not sensor noise -- this must keep hard-erroring even
        // though Heavy Object/Jump conflicts no longer do.
        let mut contours = vec![straight_contour()];
        let definers = vec![
            PointGravityDefiners {
                x: 10.0,
                y: 5.0,
                reference_contour: 0,
                gravity_dx: Some(0.0),
                gravity_dy: Some(-1.0),
                source: GravityReadingSource::SlopeLine,
            },
            PointGravityDefiners {
                x: 15.0,
                y: 5.0,
                reference_contour: 0,
                gravity_dx: Some(0.0),
                gravity_dy: Some(1.0),
                source: GravityReadingSource::SlopeLine,
            },
        ];
        assert!(resolve(&mut contours, &definers, &[], &test_config()).is_err());
    }

    #[test]
    fn a_line_definer_sets_gravity_on_the_contours_its_polygon_overlaps() {
        let mut contours = vec![straight_contour()];
        // A diagonal Jump crossing the horizontal contour near x=10 -- not
        // exactly perpendicular to it (unlike a purely vertical Jump would
        // be), so its own perpendicular gravity reading is a genuine,
        // non-degenerate side hint for the contour: a Jump's gravity is only
        // ever perpendicular to *its own* line, which here is not the same
        // direction as the contour's tangent.
        let jump_ls = LineString::new(vec![c(8.0, 0.0), c(12.0, 10.0)]);
        let mut lwg = LineWithGravity::new(jump_ls.clone());
        let (gx, gy) = gravity_vector_for_side(&jump_ls, 1.0);
        lwg.gravity_dx = Some(gx);
        lwg.gravity_dy = Some(gy);
        lwg.gravity_votes = GravityVotes::default();
        let poly = Polygon::new(
            LineString::new(vec![
                c(7.0, 0.0),
                c(13.0, 0.0),
                c(13.0, 10.0),
                c(7.0, 10.0),
                c(7.0, 0.0),
            ]),
            vec![],
        );
        // Where the Jump actually crosses the contour (x=8+ (12-8)*5/10=10,
        // at the contour's own y=5) -- what Step 1 would have captured into
        // `touched_contours` before stamping this polygon's area high
        // density.
        let definers = vec![LineGravityDefiners {
            lwg,
            poly,
            touched_contours: vec![(0, c(10.0, 5.0))],
        }];

        let result = resolve(&mut contours, &[], &definers, &test_config()).unwrap();
        assert_eq!(result.resolved_by_vote, 1);
        assert!(contours[0].lwg.gravity_dx.is_some());
    }

    /// A Jump crossing `straight_contour()` near `x`, running close to
    /// *parallel* to the contour: since a Jump's usable gravity reading is
    /// perpendicular to its own tangent (see [`resolve`]'s Jump loop), a
    /// jump nearly parallel to the contour it crosses yields a reading
    /// nearly perpendicular to the contour -- high confidence. (A jump
    /// crossing the contour at a right angle would instead yield a reading
    /// exactly parallel to the contour: the degenerate, zero-confidence
    /// case `set_or_check_gravity` rejects outright.)
    fn confident_jump_touching(x: f64, side: f64) -> LineGravityDefiners {
        let jump_ls = LineString::new(vec![c(x - 1.0, 4.9), c(x + 1.0, 5.1)]);
        let (gx, gy) = gravity_vector_for_side(&jump_ls, side);
        let mut lwg = LineWithGravity::new(jump_ls.clone());
        lwg.gravity_dx = Some(gx);
        lwg.gravity_dy = Some(gy);
        let poly = Polygon::new(
            LineString::new(vec![
                c(x - 2.0, 3.0),
                c(x + 2.0, 3.0),
                c(x + 2.0, 7.0),
                c(x - 2.0, 7.0),
                c(x - 2.0, 3.0),
            ]),
            vec![],
        );
        LineGravityDefiners {
            lwg,
            poly,
            touched_contours: vec![(0, c(x, 5.0))],
        }
    }

    /// A Heavy Object reading directly at `(x, 5.0)` on `straight_contour()`,
    /// pointing straight down/up (perpendicular -- maximum confidence).
    fn confident_heavy_object_at(x: f64, side: f64) -> PointGravityDefiners {
        PointGravityDefiners {
            x,
            y: 5.0,
            reference_contour: 0,
            gravity_dx: Some(0.0),
            gravity_dy: Some(if side > 0.0 { -1.0 } else { 1.0 }),
            source: GravityReadingSource::HeavyObject,
        }
    }

    #[test]
    fn confident_heavy_object_and_jump_disagreement_resolves_by_vote_instead_of_erroring() {
        // The original crash this priority/vote scheme replaces: a Heavy
        // Object and a Jump reading disagreeing about the same contour, with
        // no Slope Line in sight. It must resolve (picking the
        // higher-weighted side), not `Err`.
        let mut contours = vec![straight_contour()];
        let point_definers = vec![confident_heavy_object_at(10.0, 1.0)];
        let line_definers = vec![confident_jump_touching(12.0, -1.0)];
        let result = resolve(
            &mut contours,
            &point_definers,
            &line_definers,
            &test_config(),
        )
        .unwrap();
        // The perpendicular Heavy Object reading outweighs the (still fairly
        // steep, but not perfectly perpendicular) Jump reading.
        assert_eq!(result.resolved_by_vote, 1);
        assert!(contours[0].lwg.gravity_dx.is_some());
        assert!(result.warnings.is_empty());
    }

    #[test]
    fn weak_evidence_is_left_undefined_for_step_3() {
        let mut contours = vec![straight_contour()];
        // A Jump running almost perpendicular to the contour: its usable
        // reading (perpendicular to the Jump's own tangent, per the Jump
        // loop above) is then only a hair off from being exactly parallel
        // to the contour -- nearly the degenerate, zero-confidence case
        // `set_or_check_gravity` rejects outright -- so its confidence
        // weight is tiny and can't clear step2_vote_min_total_weight.
        let jump_ls = LineString::new(vec![c(9.99, 0.0), c(10.01, 10.0)]);
        let (gx, gy) = gravity_vector_for_side(&jump_ls, 1.0);
        let mut lwg = LineWithGravity::new(jump_ls);
        lwg.gravity_dx = Some(gx);
        lwg.gravity_dy = Some(gy);
        let poly = Polygon::new(
            LineString::new(vec![
                c(9.0, 4.0),
                c(11.0, 4.0),
                c(11.0, 6.0),
                c(9.0, 6.0),
                c(9.0, 4.0),
            ]),
            vec![],
        );
        let definers = vec![LineGravityDefiners {
            lwg,
            poly,
            touched_contours: vec![(0, c(10.0, 5.0))],
        }];
        let result = resolve(&mut contours, &[], &definers, &test_config()).unwrap();
        assert_eq!(result.resolved_by_vote, 0);
        assert_eq!(result.still_undefined, vec![0]);
        assert!(contours[0].lwg.gravity_dx.is_none());
    }

    #[test]
    fn slope_line_wins_over_a_confident_disagreeing_jump_and_warns() {
        let mut contours = vec![straight_contour()];
        let point_definers = vec![PointGravityDefiners {
            x: 10.0,
            y: 5.0,
            reference_contour: 0,
            gravity_dx: Some(0.0),
            gravity_dy: Some(-1.0),
            source: GravityReadingSource::SlopeLine,
        }];
        // A steep, high-confidence Jump reading favoring the opposite side.
        let line_definers = vec![confident_jump_touching(12.0, 1.0)];
        let result = resolve(
            &mut contours,
            &point_definers,
            &line_definers,
            &test_config(),
        )
        .unwrap();
        assert_eq!(result.resolved_by_slope_line, 1);
        assert_eq!(result.resolved_by_vote, 0);
        // The Slope Line's own reading wins -- gravity still points the way
        // it said, not the way the disagreeing Jump would have.
        let side = side_of_tangent(c(0.0, 5.0), c(10.0, 5.0), 0.0, -1.0);
        assert_eq!(contour_gravity_side(&contours[0]), Some(side.signum()));
        assert_eq!(result.warnings.len(), 1, "{:?}", result.warnings);
    }

    fn square_ls(x0: f64, y0: f64, side: f64) -> LineString<f64> {
        LineString::new(vec![
            c(x0, y0),
            c(x0 + side, y0),
            c(x0 + side, y0 + side),
            c(x0, y0 + side),
            c(x0, y0),
        ])
    }

    fn contour(ls: LineString<f64>) -> Contour {
        Contour {
            lwg: LineWithGravity::new(ls),
            elevation_height: None,
            empty_progeny: false,
        }
    }

    #[test]
    fn a_closed_contour_enclosing_nothing_resolves_by_the_hill_heuristic() {
        let mut contours = vec![contour(square_ls(0.0, 0.0, 10.0))];
        let result = resolve(&mut contours, &[], &[], &test_config()).unwrap();
        assert_eq!(result.resolved_by_hill, 1);
        assert!(result.still_undefined.is_empty());
        assert!(contours[0].lwg.gravity_dx.is_some());
    }

    #[test]
    fn a_closed_contour_enclosing_another_is_left_for_step_3() {
        let mut contours = vec![
            contour(square_ls(0.0, 0.0, 10.0)),
            contour(square_ls(2.0, 2.0, 2.0)),
        ];
        let result = resolve(&mut contours, &[], &[], &test_config()).unwrap();
        // Only the inner square (encloses nothing) resolves via the hill
        // heuristic; the outer one, enclosing it, is left undefined for
        // Step 3's rain-drop production.
        assert_eq!(result.resolved_by_hill, 1);
        assert_eq!(result.still_undefined, vec![0]);
        assert!(contours[1].lwg.gravity_dx.is_some());
    }

    #[test]
    fn hill_heuristic_beats_a_low_confidence_disagreeing_heavy_object_without_warning() {
        let mut contours = vec![contour(square_ls(0.0, 0.0, 10.0))];
        // A Heavy Object reading nearly parallel to the bottom edge
        // (0,0)->(10,0): tiny confidence weight, shouldn't even clear
        // step2_vote_min_total_weight, so no warning either.
        let point_definers = vec![PointGravityDefiners {
            x: 5.0,
            y: 0.0,
            reference_contour: 0,
            gravity_dx: Some(1.0),
            gravity_dy: Some(0.01),
            source: GravityReadingSource::HeavyObject,
        }];
        let result = resolve(&mut contours, &point_definers, &[], &test_config()).unwrap();
        assert_eq!(result.resolved_by_hill, 1);
        assert!(result.warnings.is_empty());
    }
}
