//! That a map counted up says what is on it.

use maur_o::stats::{coordinate_bounds, stats, symbol_usage};
use maur_o::xml_reader::read_xml_map_str;

fn shapes() -> maur_o::map::Map {
    read_xml_map_str(&std::fs::read_to_string("tests/data/shapes.xmap").unwrap())
        .unwrap()
        .0
}

#[test]
fn the_box_is_the_one_the_coordinates_fall_in() {
    let map = shapes();
    let bounds = coordinate_bounds(&map).expect("the fixture has objects");

    // Every coordinate is inside it, and it is no bigger than it needs to be.
    let (mut left, mut top, mut right, mut bottom) = (
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    );
    for object in &map.objects {
        for c in &object.coords {
            left = left.min(c.x);
            top = top.min(c.y);
            right = right.max(c.x);
            bottom = bottom.max(c.y);
        }
    }
    assert_eq!((bounds.left(), bounds.top()), (left, top));
    assert_eq!((bounds.right(), bounds.bottom()), (right, bottom));

    // The drawn extent is bigger, because a line has width.
    let drawn = maur_o::renderer::Renderer::new(&map).extent();
    assert!(drawn.left() <= bounds.left() && drawn.right() >= bounds.right());
}

#[test]
fn a_map_with_nothing_on_it_has_no_box() {
    let map = read_xml_map_str(&std::fs::read_to_string("tests/data/empty.xmap").unwrap())
        .unwrap()
        .0;
    assert!(coordinate_bounds(&map).is_none());
    let counted = stats(&map);
    assert_eq!(counted.object_count, 0);
    assert_eq!(counted.symbol_count, 0);
}

#[test]
fn only_the_symbols_actually_drawn_with_are_listed() {
    let map = shapes();
    let used = symbol_usage(&map);

    assert!(!used.is_empty());
    assert!(used.len() <= map.symbols.len());
    for entry in &used {
        assert!(entry.count > 0, "{} is listed but never used", entry.code);
        let drawn = map
            .objects
            .iter()
            .filter(|o| o.symbol_index == Some(entry.index))
            .count();
        assert_eq!(entry.count, drawn, "{} is counted wrong", entry.code);
    }
}

#[test]
fn lines_are_measured_and_areas_are_covered() {
    let map = shapes();
    let used = symbol_usage(&map);

    let lines: Vec<_> = used.iter().filter(|s| s.kind == "line").collect();
    let areas: Vec<_> = used.iter().filter(|s| s.kind == "area").collect();
    assert!(
        !lines.is_empty() && !areas.is_empty(),
        "the fixture has both"
    );

    for line in &lines {
        assert!(line.length_m > 0.0, "{} has no length", line.code);
        assert_eq!(line.area_m2, 0.0, "a line covers no ground");
    }
    for area in &areas {
        assert!(area.area_m2 > 0.0, "{} covers nothing", area.code);
        assert_eq!(area.length_m, 0.0, "an area has no length");
    }
}

#[test]
fn the_totals_are_the_sum_of_the_parts() {
    let map = shapes();
    let used = symbol_usage(&map);
    let counted = stats(&map);

    assert_eq!(counted.object_count, map.objects.len());
    assert_eq!(counted.symbol_count, used.len());
    let length: f64 = used.iter().map(|s| s.length_m).sum();
    let area: f64 = used.iter().map(|s| s.area_m2).sum();
    assert!((counted.total_line_length_m - length).abs() < 1e-9);
    assert!((counted.total_area_m2 - area).abs() < 1e-9);
}

#[test]
fn ground_measurements_follow_the_scale() {
    // The same shapes at 1:10000 and at 1:15000 cover different ground.
    let text = std::fs::read_to_string("tests/data/shapes.xmap").unwrap();
    let at_10k = read_xml_map_str(&text).unwrap().0;
    let mut at_15k = read_xml_map_str(&text).unwrap().0;
    assert_eq!(at_10k.scale_denominator, 10000);
    at_15k.scale_denominator = 15000;

    let a = stats(&at_10k);
    let b = stats(&at_15k);
    assert!((b.total_line_length_m / a.total_line_length_m - 1.5).abs() < 1e-9);
    // Area goes with the square of it.
    assert!((b.total_area_m2 / a.total_area_m2 - 2.25).abs() < 1e-6);
}

#[test]
fn a_symbol_carries_whatever_picture_the_file_has_of_it() {
    // shapes.xmap has no <icon> elements; a map from OCAD does.
    let map = shapes();
    for entry in symbol_usage(&map) {
        assert_eq!(entry.icon_src, "");
    }

    let ocd = std::path::Path::new("maps/SampleMap.ocd");
    if !ocd.exists() {
        return;
    }
    let imported = maur_o::ocd::ocd_to_omap_xml(&std::fs::read(ocd).unwrap()).unwrap();
    let (from_ocd, _) = read_xml_map_str(&imported.xml).unwrap();
    // The importer writes no icons either, so this is the shape of the field
    // rather than its contents.
    assert!(symbol_usage(&from_ocd)
        .iter()
        .all(|s| s.icon_src.is_empty()));
}

#[test]
fn symbols_come_back_in_the_order_a_symbol_set_has_them() {
    let map = shapes();
    let used = symbol_usage(&map);
    for pair in used.windows(2) {
        let a: f64 = pair[0].code.parse().unwrap_or(0.0);
        let b: f64 = pair[1].code.parse().unwrap_or(0.0);
        assert!(a <= b, "{} came before {}", pair[0].code, pair[1].code);
    }
}
