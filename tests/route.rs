//! That the fastest route is the one a runner would actually take.
//!
//! The grids here are built by hand, small enough that the right answer is
//! obvious by looking: a wall has to be gone round, a track has to be
//! preferred to the rough ground beside it, a hill has to cost something.

use maur_o::geometry::Rect;
use maur_o::route::{
    estimate_straight_time_s, solve_leg, solve_leg_via, Algorithm, Elevation, Grid, Options, Point,
};

const CELL: f64 = 10.0;
/// One unit of the grid is one metre on the ground, to keep the arithmetic
/// in these tests readable.
const METERS_PER_UNIT: f64 = 1.0;

struct Fixture {
    values: Vec<f32>,
    width: usize,
    height: usize,
    codes: Vec<String>,
    code_index: Vec<i32>,
}

impl Fixture {
    /// A grid of one speed throughout.
    fn uniform(width: usize, height: usize, speed: f32) -> Fixture {
        Fixture {
            values: vec![speed; width * height],
            width,
            height,
            codes: vec!["403".to_string(), "410".to_string()],
            code_index: vec![0; width * height],
        }
    }

    fn set(&mut self, r: usize, c: usize, speed: f32, code: i32) {
        self.values[r * self.width + c] = speed;
        self.code_index[r * self.width + c] = code;
    }

    fn grid(&self) -> Grid<'_> {
        Grid {
            values: &self.values,
            width: self.width,
            height: self.height,
            pixel_size: CELL,
            bounds: Rect::from_ltrb(
                0.0,
                0.0,
                self.width as f64 * CELL,
                self.height as f64 * CELL,
            ),
            meters_per_unit: METERS_PER_UNIT,
            code_index: Some(&self.code_index),
            codes: &self.codes,
        }
    }
}

/// The centre of a cell, which is where a route's points land.
fn at(r: usize, c: usize) -> Point {
    Point {
        x: (c as f64 + 0.5) * CELL,
        y: (r as f64 + 0.5) * CELL,
    }
}

#[test]
fn on_even_ground_the_route_is_the_straight_line() {
    let fixture = Fixture::uniform(20, 5, 1.0);
    let leg = solve_leg(
        &fixture.grid(),
        None,
        at(2, 1),
        at(2, 18),
        &Options::default(),
    )
    .unwrap();

    // Straightened, a run along one row is its two ends and nothing between.
    assert_eq!(leg.path.len(), 2);
    assert_eq!(leg.path[0], at(2, 1));
    assert_eq!(leg.path[1], at(2, 18));
    assert!((leg.distance_m - 170.0).abs() < 1e-9, "{}", leg.distance_m);
    assert_eq!(leg.climb_m, 0.0);

    // 170 m at 3 min/km on ground of speed 1, with Tobler's flat-ground
    // factor: 170 * 3 * 0.06 * e^0.175 seconds.
    let expected = 170.0 * 3.0 * 0.06 * (3.5f64 * 0.05).exp();
    assert!(
        (leg.time_s - expected).abs() < 1e-6,
        "{} vs {expected}",
        leg.time_s
    );
}

#[test]
fn a_wall_is_gone_round() {
    let mut fixture = Fixture::uniform(11, 11, 1.0);
    // A wall across the middle, with a gap at the top.
    for r in 2..11 {
        fixture.set(r, 5, 0.0, 1);
    }
    let leg = solve_leg(
        &fixture.grid(),
        None,
        at(6, 1),
        at(6, 9),
        &Options::default(),
    )
    .unwrap();

    assert!(
        leg.distance_m > 80.0,
        "a way round is longer than the way through"
    );
    // And it really goes round the top rather than through the wall.
    let crossed_wall = leg
        .path
        .iter()
        .any(|p| (p.x - 5.5 * CELL).abs() < 1e-9 && p.y > 1.5 * CELL);
    assert!(
        !crossed_wall,
        "the route went through the wall: {:?}",
        leg.path
    );
}

#[test]
fn a_track_is_worth_a_detour() {
    // Slow ground everywhere, with one fast row along the top.
    let mut fixture = Fixture::uniform(20, 9, 0.2);
    for c in 0..20 {
        fixture.set(0, c, 1.0, 1);
    }
    let leg = solve_leg(
        &fixture.grid(),
        None,
        at(2, 1),
        at(2, 18),
        &Options::default(),
    )
    .unwrap();

    let used_track = leg.path.iter().any(|p| p.y < CELL);
    assert!(used_track, "the fast row was ignored: {:?}", leg.path);

    // And going straight through the slow ground would have been slower.
    let straight = 170.0 * (3.0 / 0.2) * 0.06 * (3.5f64 * 0.05).exp();
    assert!(
        leg.time_s < straight,
        "{} should beat {straight}",
        leg.time_s
    );
}

#[test]
fn a_hill_costs_something() {
    let fixture = Fixture::uniform(20, 5, 1.0);
    let flat = solve_leg(
        &fixture.grid(),
        None,
        at(2, 1),
        at(2, 18),
        &Options::default(),
    )
    .unwrap();

    // The same ground, tilted: 40 m of climb over the leg.
    let mut elev = vec![0f32; 20 * 5];
    for r in 0..5 {
        for c in 0..20 {
            elev[r * 20 + c] = c as f32 * 2.0;
        }
    }
    let elevation = Elevation {
        values: &elev,
        width: 20,
        height: 5,
        bounds: Rect::from_ltrb(0.0, 0.0, 20.0 * CELL, 5.0 * CELL),
    };
    let uphill = solve_leg(
        &fixture.grid(),
        Some(&elevation),
        at(2, 1),
        at(2, 18),
        &Options::default(),
    )
    .unwrap();

    assert!(
        uphill.time_s > flat.time_s,
        "{} should beat {}",
        flat.time_s,
        uphill.time_s
    );
    assert!(uphill.climb_m > 30.0, "climb was {}", uphill.climb_m);
    assert_eq!(flat.climb_m, 0.0);
}

#[test]
fn a_control_on_impassable_ground_steps_aside() {
    let mut fixture = Fixture::uniform(11, 11, 1.0);
    fixture.set(5, 5, 0.0, 1);
    // The start is in the middle of the one cell nothing crosses.
    let leg = solve_leg(
        &fixture.grid(),
        None,
        at(5, 5),
        at(5, 9),
        &Options::default(),
    )
    .unwrap();
    assert!(leg.distance_m > 0.0);
    assert_ne!(
        leg.path[0],
        at(5, 5),
        "the start should have moved off the cell"
    );
}

#[test]
fn ground_nothing_crosses_is_reported_rather_than_guessed_at() {
    let mut fixture = Fixture::uniform(11, 11, 1.0);
    // A wall from edge to edge: the two halves are not connected at all.
    for r in 0..11 {
        fixture.set(r, 5, 0.0, 1);
    }
    let err = solve_leg(
        &fixture.grid(),
        None,
        at(5, 1),
        at(5, 9),
        &Options::default(),
    )
    .unwrap_err();
    assert!(err.0.contains("not connected"), "{}", err.0);

    // And a map nothing can be crossed on at all.
    let boxed = Fixture::uniform(11, 11, 0.0);
    let err = solve_leg(&boxed.grid(), None, at(5, 5), at(5, 9), &Options::default()).unwrap_err();
    assert!(err.0.contains("impassable"), "{}", err.0);
}

#[test]
fn the_window_search_finds_what_the_whole_map_search_does() {
    let mut fixture = Fixture::uniform(30, 30, 1.0);
    for r in 5..28 {
        fixture.set(r, 15, 0.05, 1);
    }
    let grid = fixture.grid();
    let a_star = solve_leg(&grid, None, at(20, 2), at(20, 27), &Options::default()).unwrap();
    let dijkstra = solve_leg(
        &grid,
        None,
        at(20, 2),
        at(20, 27),
        &Options {
            algorithm: Algorithm::Dijkstra,
            ..Options::default()
        },
    )
    .unwrap();

    // The window may be smaller than the map, but not so small that it
    // changes the answer.
    assert!(
        (a_star.time_s - dijkstra.time_s).abs() / dijkstra.time_s < 0.01,
        "{} vs {}",
        a_star.time_s,
        dijkstra.time_s
    );
}

#[test]
fn a_route_is_broken_into_the_ground_it_crosses() {
    // Half fast, half slow, split down the middle.
    let mut fixture = Fixture::uniform(20, 5, 1.0);
    for r in 0..5 {
        for c in 10..20 {
            fixture.set(r, c, 0.2, 1);
        }
    }
    let leg = solve_leg(
        &fixture.grid(),
        None,
        at(2, 1),
        at(2, 18),
        &Options::default(),
    )
    .unwrap();

    assert!(leg.segments.len() >= 2, "expected both kinds of ground");
    assert_eq!(leg.segments.first().unwrap().code.as_deref(), Some("403"));
    assert_eq!(leg.segments.last().unwrap().code.as_deref(), Some("410"));
    // The stretches add up to the leg.
    let total: f64 = leg.segments.iter().map(|s| s.time_s).sum();
    assert!(
        (total - leg.time_s).abs() / leg.time_s < 0.02,
        "{total} vs {}",
        leg.time_s
    );
    let length: f64 = leg.segments.iter().map(|s| s.length_m).sum();
    assert!((length - leg.distance_m).abs() / leg.distance_m < 0.02);
}

#[test]
fn a_waypoint_is_gone_through() {
    let fixture = Fixture::uniform(20, 20, 1.0);
    let grid = fixture.grid();
    let direct = solve_leg(&grid, None, at(10, 1), at(10, 18), &Options::default()).unwrap();
    let via = solve_leg_via(
        &grid,
        None,
        at(10, 1),
        &[at(1, 10)],
        at(10, 18),
        &Options::default(),
    )
    .unwrap();

    assert!(via.distance_m > direct.distance_m, "a detour is longer");
    assert_eq!(
        via.straight_m, direct.straight_m,
        "the crow flies the same way"
    );
    let passed_through = via.path.iter().any(|p| (p.y - at(1, 10).y).abs() < CELL);
    assert!(passed_through, "the waypoint was skipped");
}

#[test]
fn a_straight_line_can_be_timed_without_a_grid() {
    let flat = estimate_straight_time_s(1000.0, 3.0, 0.9);
    let expected = 1000.0 * (3.0 / 0.9) * (3.5f64 * 0.05).exp() * 0.06;
    assert!((flat - expected).abs() < 1e-9);
}
