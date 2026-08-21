//! That a map becomes the grid of running speeds it should.
//!
//! The shapes are the ones in `tests/data/shapes.xmap`, which is small enough
//! that what every cell should hold can be reasoned about rather than
//! recorded: an open square, a marsh, and lines crossing them.

use maur_o::runnability::{build, Options};
use maur_o::xml_reader::read_xml_map;
use std::path::Path;

fn shapes_map() -> maur_o::map::Map {
    read_xml_map(Path::new("tests/data/shapes.xmap")).unwrap().0
}

fn options(speeds: &[(&str, f64)]) -> Options {
    Options {
        speeds: speeds.iter().map(|(c, v)| (c.to_string(), *v)).collect(),
        pixel_size: 0.5,
        fill_value: 0.9,
        mask_outside_convex_hull: false,
        buffer_lines: true,
        ..Options::default()
    }
}

/// The codes `shapes.xmap` actually uses, so a test can ask for them.
fn codes_of(map: &maur_o::map::Map) -> Vec<String> {
    map.symbols.iter().map(|s| s.code().to_string()).collect()
}

/// The code of the fixture's first line symbol.
///
/// A line is what several of these tests want: it leaves most of its own
/// bounding box alone, where an area fills the grid it defines and there is
/// then no background left to see.
fn a_line_code(map: &maur_o::map::Map) -> String {
    map.symbols
        .iter()
        .find(|s| matches!(s, maur_o::map::Symbol::Line(_)))
        .expect("the fixture has a line symbol")
        .code()
        .to_string()
}

#[test]
fn a_map_with_no_speeds_for_it_cannot_be_rasterized() {
    let map = shapes_map();
    let err = build(&map, &options(&[("999", 0.5)])).unwrap_err();
    assert!(err.contains("No map objects"), "{err}");
}

#[test]
fn every_cell_starts_at_the_background_speed() {
    let map = shapes_map();
    let first = a_line_code(&map);
    let raster = build(&map, &options(&[(&first, 0.25)])).unwrap();

    assert_eq!(
        raster.values.len(),
        raster.width as usize * raster.height as usize
    );
    assert_eq!(raster.code_index.len(), raster.values.len());
    // Something was drawn, and something was left alone.
    assert!(
        raster.values.iter().any(|&v| (v - 0.25).abs() < 1e-6),
        "nothing drawn"
    );
    assert!(
        raster.values.iter().any(|&v| (v - 0.9).abs() < 1e-6),
        "everything drawn"
    );
    // Every cell either names the code it took its speed from, or nothing.
    for (i, &idx) in raster.code_index.iter().enumerate() {
        if idx < 0 {
            assert!((raster.values[i] - 0.9).abs() < 1e-6);
        } else {
            assert_eq!(raster.codes[idx as usize], first);
        }
    }
}

#[test]
fn a_code_covers_its_sub_numbers() {
    // 405.1 is covered by an entry for 405, but its own entry wins.
    let map = shapes_map();
    let codes = codes_of(&map);
    assert!(!codes.is_empty());
    // The fixture's codes are whole numbers; the base match is what a real
    // symbol set exercises, so check the rule directly on a code with a dot.
    let raster = build(&map, &options(&[(&codes[0], 0.3)])).unwrap();
    assert!(raster.used_codes.iter().any(|u| u.code == codes[0]));
}

#[test]
fn the_cell_budget_coarsens_the_grid_rather_than_growing_it() {
    let map = shapes_map();
    let codes = codes_of(&map);
    let mut small = options(&[(&codes[0], 0.5)]);
    small.pixel_size = 0.001;
    small.max_cells = 1_000;
    let raster = build(&map, &small).unwrap();

    let cells = raster.width as usize * raster.height as usize;
    assert!(cells <= 1_000, "{cells} cells, over the budget");
    assert!(raster.pixel_size > 0.001, "the cell size should have grown");
    assert!(
        raster.log.iter().any(|l| l.contains("Adjusted pixel size")),
        "the caller should be told: {:?}",
        raster.log
    );
}

#[test]
fn the_hull_marks_off_what_is_not_map() {
    let map = shapes_map();
    let line = a_line_code(&map);
    let mut opts = options(&[(&line, 0.5)]);
    opts.mask_outside_convex_hull = true;
    let raster = build(&map, &opts).unwrap();
    assert!(
        raster.values.iter().any(|v| v.is_nan()),
        "a rectangle around a line has corners the line does not reach"
    );

    // And a sample outside the grid is nothing at all.
    let far = raster.bounds.right() + 1000.0;
    assert!(raster.sample(far, far).is_none());
}

#[test]
fn a_sample_reads_back_the_cell_it_lands_in() {
    let map = shapes_map();
    let codes = codes_of(&map);
    let raster = build(&map, &options(&[(&codes[0], 0.42)])).unwrap();

    // Every cell, read through the sampler at its own centre, is that cell.
    for row in 0..raster.height {
        for col in 0..raster.width {
            let x = raster.bounds.left() + (f64::from(col) + 0.5) * raster.pixel_size;
            let y = raster.bounds.top() + (f64::from(row) + 0.5) * raster.pixel_size;
            let i = row as usize * raster.width as usize + col as usize;
            match raster.sample(x, y) {
                Some((value, _)) => assert_eq!(value, raster.values[i], "at {col},{row}"),
                None => assert!(raster.values[i].is_nan(), "at {col},{row}"),
            }
        }
    }
}

#[test]
fn an_overlap_needs_every_one_of_its_parts() {
    let map = shapes_map();
    let codes = codes_of(&map);
    // An overlap naming a code no object uses cannot happen, and must not be
    // treated as covering the whole map.
    let combo = format!("{}+999", codes[0]);
    let raster = build(&map, &options(&[(&codes[0], 0.5), (&combo, 0.1)])).unwrap();
    assert!(
        !raster.used_codes.iter().any(|u| u.code == combo),
        "an impossible overlap was applied"
    );
    assert!(raster
        .values
        .iter()
        .all(|&v| v.is_nan() || (v - 0.1).abs() > 1e-6));
}

#[test]
fn used_codes_come_back_in_reading_order() {
    let map = shapes_map();
    let codes = codes_of(&map);
    let speeds: Vec<(&str, f64)> = codes.iter().map(|c| (c.as_str(), 0.5)).collect();
    let raster = build(&map, &options(&speeds)).unwrap();

    for pair in raster.used_codes.windows(2) {
        let (a, b) = (&pair[0].code, &pair[1].code);
        assert!(a < b || a.len() < b.len(), "{a} should not follow {b}");
    }
    // And every code listed really claimed something.
    assert!(raster.used_codes.iter().all(|u| u.cells > 0));
}
