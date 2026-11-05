//! That a course is laid out over a map the way a course setter draws one.

use maur_o::course::{layout, size, Control, Kind, Options};

fn course(points: &[(f64, f64, Kind, &str)]) -> Vec<Control> {
    points
        .iter()
        .map(|&(x, y, kind, label)| Control::new(x, y, kind, label))
        .collect()
}

#[test]
fn a_leg_stops_at_the_edge_of_the_circles_it_joins() {
    let controls = course(&[
        (0.0, 0.0, Kind::Control, "1"),
        (100.0, 0.0, Kind::Control, "2"),
    ]);
    let out = layout(&controls, &Options::default());

    assert_eq!(out.legs.len(), 1);
    let seg = out.legs[0].segments[0];
    // The line begins one circle's radius out and ends one short.
    let trim = size::CONTROL_CIRCLE_RADIUS - size::LINE_WIDTH / 2.0;
    assert!((seg.x1 - trim).abs() < 1e-9, "{}", seg.x1);
    assert!((seg.x2 - (100.0 - trim)).abs() < 1e-9, "{}", seg.x2);
    assert_eq!(seg.y1, 0.0);
}

#[test]
fn two_controls_on_top_of_each_other_have_no_line_between_them() {
    let controls = course(&[
        (0.0, 0.0, Kind::Control, "1"),
        (1.0, 0.0, Kind::Control, "2"),
    ]);
    let out = layout(&controls, &Options::default());
    assert!(
        out.legs[0].segments.is_empty(),
        "{:?}",
        out.legs[0].segments
    );
}

#[test]
fn the_start_triangle_points_at_the_first_control() {
    let controls = course(&[
        (0.0, 0.0, Kind::Start, ""),
        (100.0, 0.0, Kind::Control, "1"),
    ]);
    let out = layout(&controls, &Options::default());

    assert_eq!(out.start_triangles.len(), 1);
    let apex = out.start_triangles[0].points[0];
    // Pointing east, so the apex is east of the middle by the triangle's own
    // radius.
    assert!(
        (apex.x - size::START_TRIANGLE_RADIUS).abs() < 1e-9,
        "{apex:?}"
    );
    assert!(apex.y.abs() < 1e-9, "{apex:?}");
}

#[test]
fn with_no_order_to_follow_the_triangle_points_up_the_page() {
    let controls = course(&[
        (0.0, 0.0, Kind::Start, ""),
        (100.0, 0.0, Kind::Control, "1"),
    ]);
    let out = layout(&controls, &Options { legs: false });

    let apex = out.start_triangles[0].points[0];
    assert!(apex.x.abs() < 1e-9, "{apex:?}");
    assert!(
        (apex.y + size::START_TRIANGLE_RADIUS).abs() < 1e-9,
        "{apex:?}"
    );
    assert!(out.legs.is_empty(), "and there are no legs to draw");
}

#[test]
fn a_leg_breaks_where_it_passes_another_control() {
    // Three in a row: the leg from the first to the last runs over the middle
    // one, and has to give way to it.
    let controls = course(&[
        (0.0, 0.0, Kind::Control, "1"),
        (100.0, 0.0, Kind::Control, "2"),
        (50.0, 0.0, Kind::Control, "3"),
    ]);
    let out = layout(&controls, &Options::default());

    // Leg 2 runs from control 2 back past control... the leg that matters is
    // the one from 1 to 2, which passes 3 in the middle.
    let first = &out.legs[0];
    assert_eq!(
        first.segments.len(),
        2,
        "it should be in two pieces: {:?}",
        first.segments
    );
    let gap_start = first.segments[0].x2;
    let gap_end = first.segments[1].x1;
    assert!(
        gap_start < 50.0 && gap_end > 50.0,
        "the gap should straddle the control"
    );
    // And the gap should be wider than the circle it makes room for.
    assert!(gap_end - gap_start > 2.0 * size::CONTROL_CIRCLE_RADIUS);
}

#[test]
fn a_control_at_the_very_end_of_a_leg_does_not_break_it() {
    // The third control sits on the second, which the leg already stops at.
    let controls = course(&[
        (0.0, 0.0, Kind::Control, "1"),
        (100.0, 0.0, Kind::Control, "2"),
        (100.2, 0.0, Kind::Control, "3"),
    ]);
    let out = layout(&controls, &Options::default());
    assert_eq!(out.legs[0].segments.len(), 1, "{:?}", out.legs[0].segments);
}

#[test]
fn a_number_sits_beside_its_circle() {
    let controls = course(&[(0.0, 0.0, Kind::Control, "1")]);
    let out = layout(&controls, &Options::default());

    assert_eq!(out.numbers.len(), 1);
    let n = &out.numbers[0];
    assert_eq!(n.text, "1");
    assert_eq!(n.control_index, 0);
    // Clear of the circle, and not far from it.
    let d = n.x.hypot(n.y);
    assert!(
        d > size::CONTROL_CIRCLE_RADIUS,
        "{d} should clear the circle"
    );
    assert!(
        d < size::CONTROL_CIRCLE_RADIUS * 4.0,
        "{d} should stay near it"
    );
}

#[test]
fn a_number_moves_out_of_the_way_of_a_leg() {
    // A control mid-course, with legs running east and west from it: the
    // number cannot sit on either.
    let controls = course(&[
        (-100.0, 0.0, Kind::Control, "1"),
        (0.0, 0.0, Kind::Control, "2"),
        (100.0, 0.0, Kind::Control, "3"),
    ]);
    let out = layout(&controls, &Options::default());

    let middle = out
        .numbers
        .iter()
        .find(|n| n.control_index == 1)
        .expect("the middle control has a number");
    // The legs run along y = 0, so the number has to be off that line.
    assert!(
        middle.y.abs() > 1.0,
        "the number sits on the leg: {middle:?}"
    );
}

#[test]
fn two_numbers_do_not_land_on_each_other() {
    // Controls close enough that the obvious place for both numbers is the
    // same place.
    let controls = course(&[
        (0.0, 0.0, Kind::Control, "1"),
        (9.0, 0.0, Kind::Control, "2"),
    ]);
    let out = layout(&controls, &Options::default());
    assert_eq!(out.numbers.len(), 2);
    let (a, b) = (&out.numbers[0], &out.numbers[1]);
    let apart = (a.x - b.x).hypot(a.y - b.y);
    assert!(apart > size::DIGIT_HEIGHT, "{apart} mm apart is not enough");
}

#[test]
fn the_finish_is_two_circles_and_no_number() {
    let controls = course(&[
        (0.0, 0.0, Kind::Start, ""),
        (50.0, 0.0, Kind::Control, "1"),
        (100.0, 0.0, Kind::Finish, ""),
    ]);
    let out = layout(&controls, &Options::default());

    assert_eq!(out.finish_circles.len(), 1);
    assert_eq!(out.control_circles.len(), 1);
    assert_eq!(out.start_triangles.len(), 1);
    // Only the control gets a number: the start and finish are known by shape.
    assert_eq!(out.numbers.len(), 1);
    assert_eq!(out.numbers[0].control_index, 1);
}

#[test]
fn an_empty_course_lays_out_to_nothing() {
    let out = layout(&[], &Options::default());
    assert!(out.legs.is_empty());
    assert!(out.numbers.is_empty());
    assert!(out.control_circles.is_empty());
}

#[test]
fn a_leg_leaving_the_start_stops_at_the_triangles_edge() {
    // Straight at a corner of the triangle, and straight at the middle of a
    // side: the first is trimmed further back than the second.
    let out_at_corner = layout(
        &course(&[
            (0.0, 0.0, Kind::Start, ""),
            (100.0, 0.0, Kind::Control, "1"),
        ]),
        &Options::default(),
    );
    let corner_trim = out_at_corner.legs[0].segments[0].x1;
    assert!(
        (corner_trim - size::START_TRIANGLE_RADIUS).abs() < 1e-9,
        "pointing at a corner it is trimmed to the full radius: {corner_trim}"
    );

    // A course whose start points one way and whose leg goes another.
    let controls = course(&[
        (0.0, 0.0, Kind::Start, ""),
        (100.0, 0.0, Kind::Control, "1"),
        (0.0, 100.0, Kind::Control, "2"),
    ]);
    let out = layout(&controls, &Options::default());
    let first = out.legs[0].segments[0];
    let trim = first.x1.hypot(first.y1);
    assert!(trim <= size::START_TRIANGLE_RADIUS + 1e-9);
    assert!(trim >= size::START_TRIANGLE_RADIUS * 0.5 - 1e-9);
}

#[test]
fn the_triangles_edge_is_measured_the_same_way_from_all_three_sides() {
    use maur_o::course::start_boundary_factor;
    let third = 2.0 * std::f64::consts::PI / 3.0;

    // Straight at a corner: the whole way out.
    assert!((start_boundary_factor(0.0, 0.0) - 1.0).abs() < 1e-12);
    // Straight at the middle of the opposite side: half way.
    assert!((start_boundary_factor(std::f64::consts::PI, 0.0) - 0.5).abs() < 1e-12);
    // And a triangle looks the same from three directions.
    for angle in [0.3f64, 1.1, -0.7, 2.5] {
        let here = start_boundary_factor(angle, 0.0);
        for turn in [third, -third, 2.0 * third] {
            let there = start_boundary_factor(angle + turn, 0.0);
            assert!((here - there).abs() < 1e-12, "{here} vs {there} at {angle}");
        }
    }
}

#[test]
fn a_label_is_placed_just_touching_the_circle_it_belongs_to() {
    use maur_o::course::rectangle_center;
    let (w, h) = (10.0, 4.0);
    let r = 5.0;

    // Due east: the label's left end touches, so its middle is half a width
    // further out.
    let east = rectangle_center(0.0, 0.0, r, 0.0, w, h);
    assert!((east.x - (r + w / 2.0)).abs() < 1e-9, "{east:?}");
    assert!(east.y.abs() < 1e-9, "{east:?}");

    // Due south, in a page's coordinates: its top edge touches.
    let south = rectangle_center(0.0, 0.0, r, std::f64::consts::FRAC_PI_2, w, h);
    assert!(south.x.abs() < 1e-9, "{south:?}");
    assert!((south.y - (r + h / 2.0)).abs() < 1e-9, "{south:?}");

    // And it is placed relative to the circle it is given, not the origin.
    let moved = rectangle_center(100.0, 50.0, r, 0.0, w, h);
    assert!((moved.x - (100.0 + r + w / 2.0)).abs() < 1e-9);
    assert!((moved.y - 50.0).abs() < 1e-9);
}

#[test]
fn a_start_with_nothing_after_it_still_gets_a_triangle() {
    let controls = course(&[(0.0, 0.0, Kind::Start, "")]);
    let out = layout(&controls, &Options::default());

    assert_eq!(out.start_triangles.len(), 1);
    // With nowhere to point, it points up the page.
    let apex = out.start_triangles[0].points[0];
    assert!(apex.x.abs() < 1e-9, "{apex:?}");
    assert!(
        (apex.y + size::START_TRIANGLE_RADIUS).abs() < 1e-9,
        "{apex:?}"
    );
}
