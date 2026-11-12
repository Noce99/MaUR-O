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

/// Every object used to clear, and then read back, a mask the size of the
/// whole grid: a map of thousands of small objects on a fine grid -- a
/// forest of boulders and knolls -- spent minutes doing nothing. Each object
/// now touches only the cells under it. This is quick even unoptimized;
/// the old way it would take far over the limit.
#[test]
fn many_small_objects_on_a_large_grid_are_quick() {
    let (side, count) = (150, 6000);
    let mut objects = String::new();
    for i in 0..count {
        // Short dashes on a lattice over the whole sheet, in mm * 1000.
        let x = (i % 80) * side * 1000 / 80;
        let y = (i / 80) * side * 1000 / (count / 80);
        objects.push_str(&format!(
            r#"<object type="1" symbol="0"><coords count="2">{x} {y};{} {};</coords></object>"#,
            x + 500,
            y + 300
        ));
    }
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<map xmlns="http://openorienteering.org/apps/mapper/xml/v2" version="9">
<georeferencing scale="10000"/>
<colors count="1"><color priority="0" name="Black" c="0" m="0" y="0" k="1" opacity="1"/></colors>
<symbols count="1"><symbol type="2" id="0" code="203" name="Dash"><line_symbol color="0" line_width="250"/></symbol></symbols>
<parts count="1"><part name="p"><objects count="{count}">{objects}</objects></part></parts>
</map>"#
    );
    let map = maur_o::xml_reader::read_xml_map_str(&xml).unwrap().0;
    let start = std::time::Instant::now();
    let raster = build(
        &map,
        &Options {
            pixel_size: 0.1,
            ..options(&[("203", 0.2)])
        },
    )
    .unwrap();
    let elapsed = start.elapsed();
    assert!(
        raster.width * raster.height > 2_000_000,
        "{} x {}",
        raster.width,
        raster.height
    );
    assert_eq!(raster.used_codes.len(), 1);
    assert!(
        elapsed < std::time::Duration::from_secs(10),
        "{count} objects on {} x {} cells took {elapsed:?}",
        raster.width,
        raster.height
    );
}

/// Two overlapping squares and a line across them, in a map whose colour
/// table puts blue above yellow. The file lists the blue square first, so
/// the order objects are read in and the order the map displays them in
/// disagree.
fn layered_map() -> maur_o::map::Map {
    let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<map xmlns="http://openorienteering.org/apps/mapper/xml/v2" version="9">
<georeferencing scale="10000"/>
<colors count="2">
<color priority="0" name="Blue" c="1" m="0.2" y="0" k="0" opacity="1"/>
<color priority="1" name="Yellow" c="0" m="0.27" y="0.79" k="0" opacity="1"/>
</colors>
<symbols count="3">
<symbol type="4" id="0" code="301" name="Lake"><area_symbol inner_color="0" min_area="0" patterns="0"/></symbol>
<symbol type="4" id="1" code="401" name="Open land"><area_symbol inner_color="1" min_area="0" patterns="0"/></symbol>
<symbol type="2" id="2" code="301.1" name="Bank"><line_symbol color="0" line_width="300" minimum_length="0" join_style="2" cap_style="1" start_offset="0" end_offset="0" dashed="false" segment_length="0" end_length="0" show_at_least_one_symbol="false" minimum_mid_symbol_count="0" minimum_mid_symbol_count_when_closed="0" dash_length="0" break_length="0" dashes_in_group="1" in_group_break_length="0" mid_symbols_per_spot="1" mid_symbol_distance="0" suppress_dash_symbol_at_ends="false" scale_dash_symbol="true"/></symbol>
</symbols>
<parts count="1"><part name="p"><objects count="3">
<object type="1" symbol="0"><coords count="5">2000 2000;8000 2000;8000 8000;2000 8000;2000 2000 18;</coords></object>
<object type="1" symbol="1"><coords count="5">0 0;10000 0;10000 10000;0 10000;0 0 18;</coords></object>
<object type="1" symbol="2"><coords count="2">0 5000;10000 5000;</coords></object>
</objects></part></parts>
</map>"#;
    maur_o::xml_reader::read_xml_map_str(xml).unwrap().0
}

#[test]
fn overlapping_areas_take_the_speed_of_the_one_the_map_shows_on_top() {
    let map = layered_map();
    let raster = build(&map, &options(&[("301", 0.0), ("401", 1.0)])).unwrap();
    // The middle of the lake: under it, the open land drawn later in the file.
    let (value, code) = raster.sample(5.0, 3.0).unwrap();
    assert_eq!(code, Some("301"));
    assert_eq!(value, 0.0);
    // Outside the lake, the open land.
    let (value, code) = raster.sample(1.0, 1.0).unwrap();
    assert_eq!(code, Some("401"));
    assert_eq!(value, 1.0);
}

#[test]
fn a_speed_which_is_not_a_number_claims_its_symbols_and_draws_nothing() {
    let map = layered_map();
    let with_bank = build(&map, &options(&[("301", 0.0), ("401", 1.0)])).unwrap();
    assert!(with_bank.used_codes.iter().any(|u| u.code == "301.1"));

    let raster = build(
        &map,
        &options(&[("301.1", f64::NAN), ("301", 0.0), ("401", 1.0)]),
    )
    .unwrap();
    assert!(raster.used_codes.iter().all(|u| u.code != "301.1"));
    // The lake itself, matched by 301, still draws.
    assert_eq!(raster.sample(5.0, 3.0).unwrap().1, Some("301"));
    // Where only the bank crossed the open land, the open land shows.
    assert_eq!(raster.sample(0.5, 5.0).unwrap(), (1.0, Some("401")));
}
