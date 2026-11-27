//! The one lattice every fill pattern of the tiles is laid out on, whichever map
//! an area came from.
//!
//! Mapper lays a pattern out from the paper's origin, square to the paper (or,
//! where it is rotatable, from the object's own origin and turned by it), and each
//! map has a paper of its own: two maps side by side, or an area redrawn in a piece
//! cut out of the collection, put their dots and hatchings in different places.
//! The tiles lay them all out on one paper instead, the world's: the UTM grid of
//! the zone of the point, grid north up, at the scale the symbols are drawn at,
//! with its origin at the grid's (easting 0, northing 0). A pattern's elements are
//! where Mapper would put them on that paper, so areas of one symbol continue
//! across the edges between maps, and come out the same from any map.
//!
//! No point of the paper is on every pattern's lattice at once (the least common
//! multiple of the periods is longer than the earth), and a pattern is laid out in
//! a tile from a point of the lattice near what is drawn
//! ([`crate::mot_render`]): the paper of a tile is not quite the grid's, and laid
//! out from far away the elements would drift by a fraction of their spacing.
//!
//! An area whose pattern was turned in its map (rotatable, a rotation other than
//! zero) keeps its direction on the ground: the tile holds it as a turn from grid
//! north ([`crate::mot::Pattern::rotation`]), and only its phase comes from the
//! lattice. The north lines (601.x) are the exception: they are the map's own
//! (its magnetic north, its spacing from its own origin), and keep the map's
//! origin and turn ([`keeps_its_map`]).

use crate::map::{fill_pattern_type, FillPattern, Point};
use crate::mot::{ground_angle, lat_lon};
use crate::orthophoto::web_mercator;
use crate::osm_convert::{utm, utm_inverse, utm_zone};

/// Whether the areas of the symbol `code` are laid out as their map lays them,
/// not on the lattice: the north lines, which belong to the map they were drawn
/// on.
pub fn keeps_its_map(code: &str) -> bool {
    code == "601" || code.starts_with("601.")
}

/// The direction of the UTM grid's east at the Mercator point `at` (meters, `y`
/// north), on the ground: counterclockwise from east, in radians. What a pattern
/// square to grid north is turned by in a tile.
pub fn grid_turn(at: (f64, f64)) -> f64 {
    let (lat, lon) = lat_lon(at.0, at.1);
    let zone = utm_zone(lon);
    let (e, n) = utm(zone, (lon, lat));
    let (lon1, lat1) = utm_inverse(zone, (e + 10.0, n));
    // Back through the same projection, so the two points agree to the digit.
    let (lon0, lat0) = utm_inverse(zone, (e, n));
    let a = web_mercator(lat0, lon0);
    let b = web_mercator(lat1, lon1);
    ground_angle(a, b, lat)
}

/// A turn from grid north smaller than this (radians, some 0.006°) is none: the
/// turn the grid and a paper on it differ by, worked out through two projections,
/// or a turn written to a file and read back.
pub const NO_TURN: f64 = 1e-4;

/// The side of the squares of the grid (meters) from whose corners a turned
/// pattern is laid out.
const TURNED_FROM_M: f64 = 1000.0;

/// `rotation`, a pattern's turn from grid north, as the lattice has it: in
/// `(-π, π]`, and none where it is less than [`NO_TURN`].
pub fn from_grid(rotation: f64) -> f64 {
    use std::f64::consts::{PI, TAU};
    let r = rotation - TAU * ((rotation + PI) / TAU).floor();
    let r = if r <= -PI { r + TAU } else { r };
    if r.abs() < NO_TURN {
        0.0
    } else {
        r
    }
}

/// The point of the lattice of `patterns` (paper mm at 1:`scale`), turned by
/// `rotation` from grid north, nearest the Mercator point `near`: where to lay
/// them out from, in Mercator meters. The patterns' own `rotatable` decides, as
/// in Mapper, whether `rotation` turns them.
pub fn anchor(
    patterns: &[&FillPattern],
    rotation: f64,
    scale: f64,
    near: (f64, f64),
) -> (f64, f64) {
    let (lat, lon) = lat_lon(near.0, near.1);
    let zone = utm_zone(lon);
    let at = anchor_in_grid(patterns, rotation, scale, utm(zone, (lon, lat)));
    let (lon, lat) = utm_inverse(zone, at);
    web_mercator(lat, lon)
}

/// [`anchor`] in the grid: `near` and the point of the lattice nearest it as UTM
/// `(easting, northing)`.
///
/// A pattern square to the grid is laid out from the grid's origin. One turned
/// is laid out from the corner of the grid's kilometre squares nearest `near`:
/// turned about a point thousands of kilometres away, the slightest difference
/// in the turn (a millionth of a radian between two tiles of one area) would move
/// its elements by their spacing. Where an area so turned crosses the middle
/// between two corners its pattern starts again; only areas turned are.
pub fn anchor_in_grid(
    patterns: &[&FillPattern],
    rotation: f64,
    scale: f64,
    (e, n): (f64, f64),
) -> (f64, f64) {
    let mm = 1000.0 / scale;
    let from = if rotation == 0.0 {
        Point::new(0.0, 0.0)
    } else {
        let corner = |v: f64| (v / TURNED_FROM_M).round() * TURNED_FROM_M * mm;
        Point::new(corner(e), -corner(n))
    };
    let at = nearest_origin(patterns, rotation, from, Point::new(e * mm, -n * mm));
    (at.x / mm, -at.y / mm)
}

/// The pattern origin `origin` (paper mm) of an object whose patterns are
/// `patterns`, turned by `rotation` where rotatable, moved by whole periods of
/// the patterns to as near `near` as they allow: the same lines and the same
/// elements along them, laid out from beside the object.
///
/// The lines across which the elements repeat are those of the first pattern
/// and of the first pattern at another angle (the two hatchings of an
/// out-of-bounds area); where the patterns are all on one angle, the origin
/// moves freely along their lines unless they lay elements along them.
pub fn nearest_origin(
    patterns: &[&FillPattern],
    rotation: f64,
    origin: Point,
    near: Point,
) -> Point {
    // (unit vector, period): moving the origin by a multiple of the period
    // along the vector leaves the pattern as it was. As the renderer lays the
    // patterns out: y down, the normal of lines at angle r is (sin r, cos r).
    let mut periods: Vec<(Point, f64)> = Vec::new();
    for p in patterns {
        if p.line_spacing <= 0.0 {
            continue;
        }
        let r = p.angle + if p.rotatable { rotation } else { 0.0 };
        periods.push((Point::new(r.sin(), r.cos()), p.line_spacing));
        if p.pattern_type == fill_pattern_type::POINT && p.point_distance > 0.0 {
            periods.push((Point::new(r.cos(), -r.sin()), p.point_distance));
        }
    }
    let Some(&(u1, p1)) = periods.first() else {
        return origin;
    };
    let cross = |a: Point, b: Point| a.x * b.y - a.y * b.x;
    let v = near - origin;
    let whole = |u: Point, p: f64| (u.dot(v) / p).round() * p;
    let a = whole(u1, p1);
    let shift = match periods.iter().find(|(u, _)| cross(u1, *u).abs() > 1e-6) {
        Some(&(u2, p2)) => {
            // dot(shift, u1) = a, dot(shift, u2) = b
            let b = whole(u2, p2);
            let det = cross(u1, u2);
            Point::new((a * u2.y - b * u1.y) / det, (u1.x * b - u2.x * a) / det)
        }
        // Lines only, all at one angle: free along them.
        None => u1 * a + (v - u1 * u1.dot(v)),
    };
    origin + shift
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orthophoto::web_mercator;

    fn dots(spacing: f64, distance: f64) -> FillPattern {
        FillPattern {
            pattern_type: fill_pattern_type::POINT,
            line_spacing: spacing,
            point_distance: distance,
            rotatable: true,
            ..FillPattern::default()
        }
    }

    /// Two points far apart in one zone are laid out from points of one lattice:
    /// whole periods apart in the grid, east and north.
    #[test]
    fn anchors_are_whole_periods_apart_in_the_grid() {
        let p = dots(1.5, 2.0);
        let scale = 4000.0;
        let zone = utm_zone(11.34);
        let grid = |m: (f64, f64)| {
            let (lat, lon) = lat_lon(m.0, m.1);
            utm(zone, (lon, lat))
        };
        let a = grid(anchor(&[&p], 0.0, scale, web_mercator(44.516, 11.34)));
        let b = grid(anchor(&[&p], 0.0, scale, web_mercator(44.531, 11.37)));
        // 1.5 mm and 2 mm at 1:4000: 6 m north-south, 8 m east-west.
        for (d, period) in [(a.0 - b.0, 8.0), (a.1 - b.1, 6.0)] {
            let off = (d / period - (d / period).round()).abs() * period;
            assert!(
                off < 0.001,
                "{d} m is {off} m off a whole number of {period} m"
            );
        }
        // And on the grid's own lattice, from its origin.
        assert!((a.0 / 8.0 - (a.0 / 8.0).round()).abs() * 8.0 < 0.001);
    }

    /// The grid's east turns from the Mercator east by the convergence: none on
    /// the central meridian, some 2° three degrees of longitude away at 44°N.
    #[test]
    fn the_grid_turns_by_the_convergence() {
        let middle = grid_turn(web_mercator(44.5, 9.0));
        assert!(middle.abs() < 1e-6, "{middle}");
        let east = grid_turn(web_mercator(44.5, 11.9)).to_degrees();
        // γ = Δλ sin φ, 2.9° · 0.70. East of the central meridian true north
        // leans west of grid north: grid north and east are turned clockwise.
        assert!(
            (east + 2.9 * 44.5f64.to_radians().sin()).abs() < 0.02,
            "{east}"
        );
    }
}
