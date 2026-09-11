//! Step 1 of `Contours-to-Raster.md`: setting a contour's gravity directly
//! from the evidence Step 0 gathered, wherever that evidence alone already
//! points at an answer -- a `PointGravityDefiners` or `LineGravityDefiners`
//! reading, or (for a closed contour enclosing nothing) the assumption that
//! it is a hill.

use geo::algorithm::Contains;
use geo::Coord;

use crate::contour_geometry::{local_tangent, nearest_index};
use crate::contour_raster::ContourRaster;
use crate::gravity_model::{
    contour_polygon, encloses_another_contour, gravity_vector_for_side, lwg_gravity_side,
    set_or_check_gravity, vector_on_side, Contour, LineGravityDefiners, PointGravityDefiners,
};

/// What Step 1 resolved, and what it could not.
pub struct Step1Result {
    /// How many contours got their gravity set from a Slope Line or a Heavy
    /// Object intersection.
    pub resolved_by_points: u64,
    /// How many contours got their gravity set from a Jump.
    pub resolved_by_lines: u64,
    /// How many contours got their gravity from the closed-hill heuristic.
    pub resolved_by_hill: u64,
    /// Indices into the `Vec<Contour>` still without a defined gravity.
    pub still_undefined: Vec<usize>,
    /// Recoverable problems found along the way (currently always empty --
    /// kept for symmetry with the other steps' results, and because a
    /// future producer of definers with no derivable evidence would want
    /// somewhere to report it).
    pub warnings: Vec<String>,
}

/// Runs Step 1: applies every [`PointGravityDefiners`] reading, then every
/// [`LineGravityDefiners`] one, to the contour(s) each is evidence about
/// (`Err`ing, naming the conflict, the moment two pieces of evidence
/// disagree about the same contour, per the doc's "crash with an error
/// explaining the problem"); then assumes every closed contour enclosing
/// nothing else is a hill, with gravity pointing away from its own enclosed
/// area.
pub fn resolve(
    contours: &mut [Contour],
    point_definers: &[PointGravityDefiners],
    line_definers: &[LineGravityDefiners],
    raster: &ContourRaster,
) -> Result<Step1Result, String> {
    let mut resolved_by_points = 0u64;
    let mut resolved_by_lines = 0u64;

    for definer in point_definers {
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
                "Step 1: a Slope Line or Heavy Object reading at ({:.2}, {:.2}) conflicts with \
                 contour {}'s gravity: {e}",
                definer.x, definer.y, definer.reference_contour
            )
        })?;
        if !was_defined && contour.lwg.gravity_dx.is_some() {
            resolved_by_points += 1;
        }
    }

    for definer in line_definers {
        let Some(side) = lwg_gravity_side(&definer.lwg) else {
            continue;
        };
        for (px, py) in raster.pixels_in_polygon(&definer.poly) {
            let val = raster.get(px, py);
            if val == 0 {
                continue;
            }
            let contour_idx = (val - 1) as usize;
            let Some(contour) = contours.get_mut(contour_idx) else {
                continue;
            };
            let center = raster.pixel_center(px, py);
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
            let was_defined = contour.lwg.gravity_dx.is_some();
            set_or_check_gravity(contour, from, to, dx, dy).map_err(|e| {
                format!("Step 1: a Jump conflicts with contour {contour_idx}'s gravity: {e}")
            })?;
            if !was_defined && contour.lwg.gravity_dx.is_some() {
                resolved_by_lines += 1;
            }
        }
    }

    let resolved_by_hill = assign_hill_gravity(contours);

    let still_undefined = contours
        .iter()
        .enumerate()
        .filter(|(_, c)| c.lwg.gravity_dx.is_none())
        .map(|(i, _)| i)
        .collect();

    Ok(Step1Result {
        resolved_by_points,
        resolved_by_lines,
        resolved_by_hill,
        still_undefined,
        warnings: Vec::new(),
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
        }
    }

    fn raster_with(contours: &[Contour]) -> ContourRaster {
        let mut r = ContourRaster::new(c(-5.0, -5.0), 1.0, 40, 40);
        for (i, contour) in contours.iter().enumerate() {
            let densified = crate::contour_geometry::densify(&contour.lwg.ls, 0.5, 1.0);
            r.write_contour(i as u64, &densified).unwrap();
        }
        r
    }

    #[test]
    fn agreeing_point_definers_set_gravity_once() {
        let mut contours = vec![straight_contour()];
        let raster = raster_with(&contours);
        let definers = vec![
            PointGravityDefiners {
                x: 10.0,
                y: 5.0,
                reference_contour: 0,
                gravity_dx: Some(0.0),
                gravity_dy: Some(-1.0),
            },
            PointGravityDefiners {
                x: 15.0,
                y: 5.0,
                reference_contour: 0,
                gravity_dx: Some(0.0),
                gravity_dy: Some(-1.0),
            },
        ];
        let result = resolve(&mut contours, &definers, &[], &raster).unwrap();
        assert_eq!(result.resolved_by_points, 1);
        assert!(result.still_undefined.is_empty());
    }

    #[test]
    fn disagreeing_point_definers_error() {
        let mut contours = vec![straight_contour()];
        let raster = raster_with(&contours);
        let definers = vec![
            PointGravityDefiners {
                x: 10.0,
                y: 5.0,
                reference_contour: 0,
                gravity_dx: Some(0.0),
                gravity_dy: Some(-1.0),
            },
            PointGravityDefiners {
                x: 15.0,
                y: 5.0,
                reference_contour: 0,
                gravity_dx: Some(0.0),
                gravity_dy: Some(1.0),
            },
        ];
        assert!(resolve(&mut contours, &definers, &[], &raster).is_err());
    }

    #[test]
    fn a_line_definer_sets_gravity_on_the_contours_its_polygon_overlaps() {
        let mut contours = vec![straight_contour()];
        let raster = raster_with(&contours);
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
        let definers = vec![LineGravityDefiners { lwg, poly }];

        let result = resolve(&mut contours, &[], &definers, &raster).unwrap();
        assert_eq!(result.resolved_by_lines, 1);
        assert!(contours[0].lwg.gravity_dx.is_some());
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
        }
    }

    #[test]
    fn a_closed_contour_enclosing_nothing_resolves_by_the_hill_heuristic() {
        let mut contours = vec![contour(square_ls(0.0, 0.0, 10.0))];
        let raster = raster_with(&contours);
        let result = resolve(&mut contours, &[], &[], &raster).unwrap();
        assert_eq!(result.resolved_by_hill, 1);
        assert!(result.still_undefined.is_empty());
        assert!(contours[0].lwg.gravity_dx.is_some());
    }

    #[test]
    fn a_closed_contour_enclosing_another_is_left_for_step_2() {
        let mut contours = vec![
            contour(square_ls(0.0, 0.0, 10.0)),
            contour(square_ls(2.0, 2.0, 2.0)),
        ];
        let raster = raster_with(&contours);
        let result = resolve(&mut contours, &[], &[], &raster).unwrap();
        // Only the inner square (encloses nothing) resolves via the hill
        // heuristic; the outer one, enclosing it, is left undefined for
        // Step 2's rain-drop production.
        assert_eq!(result.resolved_by_hill, 1);
        assert_eq!(result.still_undefined, vec![0]);
        assert!(contours[1].lwg.gravity_dx.is_some());
    }
}
