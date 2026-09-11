//! Step 0's data model (`Contours-to-Raster.md`): what a contour knows about
//! its own downhill direction, and the two kinds of evidence ("definers")
//! that pin it down. [`side_of_tangent`] and [`encloses_another_contour`]
//! (Appendix 4) are ported verbatim from the doc; everything else here is the
//! glue the doc leaves implicit -- turning a "side" into a stored vector, and
//! comparing or accumulating votes from a reading taken anywhere along a
//! contour without ever comparing raw `(dx, dy)` components across two
//! different points (which, per the doc's own commentary on
//! `side_of_tangent`, are only valid relative to the tangent they were
//! measured against).

use geo::algorithm::Contains;
use geo::{Coord, LineString, Polygon};

/// How many rain-drop (or anti-rain-drop) votes a contour has received for
/// each side, while its gravity is still undefined.
#[derive(Clone, Copy, Debug, Default)]
pub struct GravityVotes {
    /// Votes for the side [`side_of_tangent`] calls left: standing on the
    /// first point of the LineString and looking toward the second point of
    /// it.
    pub left: u64,
    /// Votes for the opposite (right) side.
    pub right: u64,
}

/// A line that is always perpendicular to gravity (a contour), or always
/// parallel to it (a Jump's buffered polygon): the `LineString` defines its
/// nodes, and its gravity direction -- one of the two possible ones -- is
/// defined by `(ls[0].x + gravity_dx, ls[0].y + gravity_dy)`, a vector that
/// must be perpendicular to the segment `ls[0] -> ls[1]` and of unit length.
#[derive(Clone)]
pub struct LineWithGravity {
    /// The line's own nodes.
    pub ls: LineString<f64>,
    /// Gravity's x component, `None` while undefined.
    pub gravity_dx: Option<f64>,
    /// Gravity's y component, `None` while undefined.
    pub gravity_dy: Option<f64>,
    /// Rain-drop votes accumulated while gravity is still undefined.
    pub gravity_votes: GravityVotes,
}

impl LineWithGravity {
    /// A `LineWithGravity` with no gravity yet and no votes.
    pub fn new(ls: LineString<f64>) -> LineWithGravity {
        LineWithGravity {
            ls,
            gravity_dx: None,
            gravity_dy: None,
            gravity_votes: GravityVotes::default(),
        }
    }
}

/// One contour line, and the elevation Step 3 will eventually assign it.
pub struct Contour {
    /// The contour's own line and gravity.
    pub lwg: LineWithGravity,
    /// Step 3's output; always `None` until Step 3 exists.
    pub elevation_height: Option<f64>,
}

/// A single directional reading taken at a known position on a known
/// contour: a Slope Line, or a contour/Heavy-Object intersection with a
/// circle-fitted gravity direction.
pub struct PointGravityDefiners {
    /// Ground-meter x position of the reading.
    pub x: f64,
    /// Ground-meter y position of the reading.
    pub y: f64,
    /// Index into the `Vec<Contour>` this reading is about.
    pub reference_contour: u64,
    /// Gravity's x component, `None` if it could not be derived.
    pub gravity_dx: Option<f64>,
    /// Gravity's y component, `None` if it could not be derived.
    pub gravity_dy: Option<f64>,
}

/// A Jump, whose gravity direction Mapper's own symbol definition gives
/// directly (see [`crate::contour_symbols::jump_gravity_side`]), together
/// with the polygon Step 1 rasterizes to find which contours it touches.
pub struct LineGravityDefiners {
    /// The Jump's own line and (always defined) gravity.
    pub lwg: LineWithGravity,
    /// The buffered polygon (Appendix 2) used to find intersected contours.
    pub poly: Polygon<f64>,
}

/// Which side of the tangent `tangent_from -> tangent_to` the vector
/// `(dx, dy)` points to: a positive result means left, a negative result
/// means right. Comparing the sign computed here at `ls[0] -> ls[1]` against
/// the sign computed at any other point's local tangent is how "in
/// accordance" is decided everywhere in Step 1 and Step 2 -- never a direct
/// comparison of raw `(dx, dy)` components, which are only valid relative to
/// the tangent they were computed against.
pub fn side_of_tangent(tangent_from: Coord<f64>, tangent_to: Coord<f64>, dx: f64, dy: f64) -> f64 {
    let (tx, ty) = (tangent_to.x - tangent_from.x, tangent_to.y - tangent_from.y);
    tx * dy - ty * dx
}

/// The unit vector perpendicular to the tangent `tangent_from -> tangent_to`,
/// on the side [`side_of_tangent`] would call left (`side > 0.0`) or right
/// (`side <= 0.0`). This is how Step 2 recovers the actual gravity vector
/// *at* any point along a contour from the contour's own stored side (see
/// [`contour_gravity_side`]): the doc's own commentary on `side_of_tangent`
/// is that a "side" -- unlike a raw `(dx, dy)` -- means the same thing at
/// every point of a well-behaved (non-self-crossing) contour.
pub fn vector_on_side(tangent_from: Coord<f64>, tangent_to: Coord<f64>, side: f64) -> (f64, f64) {
    let (tx, ty) = (tangent_to.x - tangent_from.x, tangent_to.y - tangent_from.y);
    // (-ty, tx) is `side_of_tangent`'s left (tx*tx + ty*ty > 0 always);
    // (ty, -tx) is its right, by the same substitution.
    let (dx, dy) = if side > 0.0 { (-ty, tx) } else { (ty, -tx) };
    let len = dx.hypot(dy);
    (dx / len, dy / len)
}

/// The unit vector perpendicular to `ls[0] -> ls[1]`, on the given side. See
/// [`vector_on_side`].
pub fn gravity_vector_for_side(ls: &LineString<f64>, side: f64) -> (f64, f64) {
    vector_on_side(ls.0[0], ls.0[1], side)
}

/// The gravity direction *at* node `i` of `ls`, on `side`: the mean of the
/// perpendicular of the segment before it and the one after, since a node
/// sits between the two rather than belonging to either segment alone. A
/// node with only one neighboring segment (an open contour's first or last
/// node) takes that segment's own perpendicular alone. A closed contour
/// (`ls`'s first point repeated as its last) wraps node 0's "previous"
/// segment around to the contour's own last one. `None` only for a
/// degenerate `ls` with fewer than two points.
///
/// Used both to draw the `--create_svg` gravity arrows and (interpolated
/// between a segment's two endpoint nodes) to give Rain Drop Production's
/// own sources a direction that varies smoothly along a segment instead of
/// jumping at each one.
pub fn node_direction(ls: &LineString<f64>, i: usize, side: f64) -> Option<(f64, f64)> {
    let n = ls.0.len();
    if n < 2 {
        return None;
    }
    let next = (i + 1 < n).then(|| vector_on_side(ls.0[i], ls.0[i + 1], side));
    let prev = if i > 0 {
        Some(vector_on_side(ls.0[i - 1], ls.0[i], side))
    } else if ls.is_closed() && n > 2 {
        // Node 0's "previous" segment wraps to the contour's own last
        // (real) one: ls[n-2] -> ls[n-1], where ls[n-1] is the same point
        // as ls[0].
        Some(vector_on_side(ls.0[n - 2], ls.0[n - 1], side))
    } else {
        None
    };
    match (prev, next) {
        (Some((px, py)), Some((nx, ny))) => {
            let (sx, sy) = (px + nx, py + ny);
            let len = sx.hypot(sy);
            Some(if len > 1e-9 {
                (sx / len, sy / len)
            } else {
                // The two perpendiculars cancel out exactly (a near
                // U-turn): fall back to the outgoing segment alone.
                (nx, ny)
            })
        }
        (Some(v), None) | (None, Some(v)) => Some(v),
        (None, None) => None,
    }
}

/// Which side of its own `ls[0] -> ls[1]` a [`LineWithGravity`]'s already-set
/// gravity is on (`1.0` left, `-1.0` right, per [`side_of_tangent`]'s
/// convention), or `None` while its gravity is still undefined. The inverse
/// of [`gravity_vector_for_side`]: recovers the "side" a stored absolute
/// `(gravity_dx, gravity_dy)` was built from, so it can be re-expressed at a
/// different point's local tangent via [`vector_on_side`]. Used for both a
/// [`Contour`]'s own gravity ([`contour_gravity_side`]) and a Jump's
/// (`LineGravityDefiners::lwg`) -- like a contour, a Jump's gravity is only
/// ever perpendicular to its line *at the point read*, and is stored
/// relative to `ls[0] -> ls[1]` purely as a persistent "side" recovered here,
/// never as one fixed vector valid along its whole (possibly curved) length.
pub fn lwg_gravity_side(lwg: &LineWithGravity) -> Option<f64> {
    let (gx, gy) = (lwg.gravity_dx?, lwg.gravity_dy?);
    let (a, b) = (lwg.ls.0[0], lwg.ls.0[1]);
    Some(side_of_tangent(a, b, gx, gy).signum())
}

/// [`lwg_gravity_side`] for a [`Contour`]'s own `lwg`.
pub fn contour_gravity_side(contour: &Contour) -> Option<f64> {
    lwg_gravity_side(&contour.lwg)
}

/// Sets `contour`'s gravity from a directional reading `(dx, dy)` taken at
/// the local tangent `tangent_from -> tangent_to`, if it has none yet; or, if
/// it already has one, checks that this reading agrees with it (same side of
/// the contour, via `side_of_tangent`'s sign at each reading's own local
/// tangent -- never a raw `(dx, dy)` comparison). `Err` on a genuine
/// conflict, naming the disagreement, per the doc's "crash with an error
/// explaining the problem."
pub fn set_or_check_gravity(
    contour: &mut Contour,
    tangent_from: Coord<f64>,
    tangent_to: Coord<f64>,
    dx: f64,
    dy: f64,
) -> Result<(), String> {
    let side = side_of_tangent(tangent_from, tangent_to, dx, dy);
    if side == 0.0 {
        return Err(
            "a gravity reading was exactly parallel to its own local tangent, so it does not \
             pick out either side of the contour"
                .to_string(),
        );
    }
    match (contour.lwg.gravity_dx, contour.lwg.gravity_dy) {
        (Some(gx), Some(gy)) => {
            let (a, b) = (contour.lwg.ls.0[0], contour.lwg.ls.0[1]);
            let already_set_side = side_of_tangent(a, b, gx, gy);
            if side.signum() != already_set_side.signum() {
                return Err(
                    "conflicting gravity readings for the same contour: one reading puts \
                     downhill on the opposite side from an earlier one"
                        .to_string(),
                );
            }
            Ok(())
        }
        _ => {
            let (gx, gy) = gravity_vector_for_side(&contour.lwg.ls, side);
            contour.lwg.gravity_dx = Some(gx);
            contour.lwg.gravity_dy = Some(gy);
            Ok(())
        }
    }
}

/// Which vote bucket a directional reading `(dx, dy)`, taken at the local
/// tangent `tangent_from -> tangent_to`, falls into: `1.0` for left, `-1.0`
/// for right (via [`side_of_tangent`]'s sign at that same local tangent).
pub fn vote_side(tangent_from: Coord<f64>, tangent_to: Coord<f64>, dx: f64, dy: f64) -> f64 {
    if side_of_tangent(tangent_from, tangent_to, dx, dy) >= 0.0 {
        1.0
    } else {
        -1.0
    }
}

/// Builds a Polygon from a closed contour's LineString (no holes). Appendix 4.
pub fn contour_polygon(c: &Contour) -> Polygon<f64> {
    Polygon::new(c.lwg.ls.clone(), vec![])
}

/// Returns true if the closed contour at `candidate_idx` encloses at least
/// one other closed contour. Appendix 4.
pub fn encloses_another_contour(candidate_idx: usize, contours: &[Contour]) -> bool {
    debug_assert!(contours[candidate_idx].lwg.ls.is_closed());

    let candidate_poly = contour_polygon(&contours[candidate_idx]);

    contours.iter().enumerate().any(|(i, c)| {
        i != candidate_idx && c.lwg.ls.is_closed() && candidate_poly.contains(&c.lwg.ls.0[0])
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn coord(x: f64, y: f64) -> Coord<f64> {
        Coord { x, y }
    }

    fn square_ls(x0: f64, y0: f64, side: f64) -> LineString<f64> {
        LineString::new(vec![
            coord(x0, y0),
            coord(x0 + side, y0),
            coord(x0 + side, y0 + side),
            coord(x0, y0 + side),
            coord(x0, y0),
        ])
    }

    fn contour(ls: LineString<f64>) -> Contour {
        Contour {
            lwg: LineWithGravity::new(ls),
            elevation_height: None,
        }
    }

    #[test]
    fn side_of_tangent_sign_matches_left_right() {
        let from = coord(0.0, 0.0);
        let to = coord(1.0, 0.0);
        // Standing at (0,0) looking toward (1,0): "up" (0,-1) is one side...
        let up = side_of_tangent(from, to, 0.0, -1.0);
        let down = side_of_tangent(from, to, 0.0, 1.0);
        assert!(
            up * down < 0.0,
            "opposite directions must land on opposite sides"
        );
    }

    #[test]
    fn gravity_vector_for_side_round_trips_through_side_of_tangent() {
        let ls = LineString::new(vec![coord(0.0, 0.0), coord(1.0, 0.0), coord(1.0, 1.0)]);
        let (dx, dy) = gravity_vector_for_side(&ls, 1.0);
        let side = side_of_tangent(ls.0[0], ls.0[1], dx, dy);
        assert!(side > 0.0);
        let (dx, dy) = gravity_vector_for_side(&ls, -1.0);
        let side = side_of_tangent(ls.0[0], ls.0[1], dx, dy);
        assert!(side < 0.0);
    }

    #[test]
    fn set_or_check_gravity_consistent_reading_on_a_bend_does_not_conflict() {
        // An "L" shaped contour: the same physical side, read at two very
        // different local tangents, must not be reported as a conflict.
        let ls = LineString::new(vec![coord(0.0, 0.0), coord(10.0, 0.0), coord(10.0, 10.0)]);
        let mut c = contour(ls);
        // Both readings point away from the square this L is two edges of:
        // (0,-1) away from the bottom edge, (1,0) away from the right edge.
        // Different raw vectors, but the same physical (exterior) side.
        set_or_check_gravity(&mut c, coord(0.0, 0.0), coord(10.0, 0.0), 0.0, -1.0).unwrap();
        set_or_check_gravity(&mut c, coord(10.0, 0.0), coord(10.0, 10.0), 1.0, 0.0).unwrap();
        assert!(c.lwg.gravity_dx.is_some());
    }

    #[test]
    fn set_or_check_gravity_flipped_reading_conflicts() {
        let ls = LineString::new(vec![coord(0.0, 0.0), coord(10.0, 0.0)]);
        let mut c = contour(ls);
        set_or_check_gravity(&mut c, coord(0.0, 0.0), coord(10.0, 0.0), 0.0, -1.0).unwrap();
        let err = set_or_check_gravity(&mut c, coord(0.0, 0.0), coord(10.0, 0.0), 0.0, 1.0);
        assert!(err.is_err());
    }

    #[test]
    fn encloses_another_contour_nested_vs_side_by_side() {
        let outer = contour(square_ls(0.0, 0.0, 10.0));
        let inner = contour(square_ls(2.0, 2.0, 2.0));
        let beside = contour(square_ls(20.0, 20.0, 2.0));

        let nested = vec![outer, inner];
        assert!(encloses_another_contour(0, &nested));
        assert!(!encloses_another_contour(1, &nested));

        let side_by_side = vec![contour(square_ls(0.0, 0.0, 10.0)), beside];
        assert!(!encloses_another_contour(0, &side_by_side));
    }

    #[test]
    fn node_direction_open_contour_uses_the_lone_neighbor_at_the_ends_and_averages_in_the_middle() {
        // An open "L": (0,0) -> (10,0) -> (10,10).
        let ls = LineString::new(vec![coord(0.0, 0.0), coord(10.0, 0.0), coord(10.0, 10.0)]);

        // Node 0 has no preceding segment: the following one's own
        // perpendicular, (0,1), alone.
        assert_eq!(node_direction(&ls, 0, 1.0), Some((0.0, 1.0)));
        // Node 2 has no following segment: the preceding one's own
        // perpendicular, (-1,0), alone.
        assert_eq!(node_direction(&ls, 2, 1.0), Some((-1.0, 0.0)));

        // Node 1 (the bend) sits between both: the mean of (0,1) and
        // (-1,0), normalized -- diagonal, not just one segment's own.
        let (dx, dy) = node_direction(&ls, 1, 1.0).unwrap();
        let expected = (-1.0 / 2.0f64.sqrt(), 1.0 / 2.0f64.sqrt());
        assert!((dx - expected.0).abs() < 1e-9);
        assert!((dy - expected.1).abs() < 1e-9);
    }

    #[test]
    fn node_direction_closed_contour_wraps_node_zero_to_its_own_last_segment() {
        // A closed square; the last point repeats the first, as every
        // closed contour in this crate's own data does.
        let ls = LineString::new(vec![
            coord(0.0, 0.0),
            coord(10.0, 0.0),
            coord(10.0, 10.0),
            coord(0.0, 10.0),
            coord(0.0, 0.0),
        ]);

        // Node 0's "previous" segment wraps to (0,10)->(0,0) (perpendicular
        // (1,0)), averaged with its next segment (0,0)->(10,0)
        // (perpendicular (0,1)) -- diagonal, not simply (0,1) alone.
        let (dx, dy) = node_direction(&ls, 0, 1.0).unwrap();
        let expected = (1.0 / 2.0f64.sqrt(), 1.0 / 2.0f64.sqrt());
        assert!((dx - expected.0).abs() < 1e-9);
        assert!((dy - expected.1).abs() < 1e-9);
    }
}
