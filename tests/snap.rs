//! That the table a control is placed against says what is where.

use maur_o::map::{Georeferencing, Map};
use maur_o::snap::{snap_table, Kind};
use maur_o::xml_reader::read_xml_map_str;

fn shapes() -> Map {
    read_xml_map_str(&std::fs::read_to_string("tests/data/shapes.xmap").unwrap())
        .unwrap()
        .0
}

#[test]
fn every_object_with_a_code_and_a_shape_is_in_the_table() {
    let map = shapes();
    let table = snap_table(&map);

    // The fixture's objects are all drawn with a coded symbol, except the
    // text one -- text is not a feature to snap to.
    let drawable = map
        .objects
        .iter()
        .filter(|o| o.symbol_index.is_some() && !o.coords.is_empty())
        .count();
    assert!(table.len() <= drawable);
    assert!(!table.is_empty());
    for entry in &table {
        assert!(!entry.code.is_empty());
        assert!(!entry.points.is_empty());
    }
}

#[test]
fn a_symbol_is_the_kind_of_thing_it_draws() {
    let map = shapes();
    let table = snap_table(&map);
    let kind_of = |code: &str| table.iter().find(|e| e.code == code).map(|e| e.kind);

    assert_eq!(kind_of("109"), Some(Kind::Point), "a knoll is a point");
    assert_eq!(kind_of("505"), Some(Kind::Line), "a path is a line");
    assert_eq!(kind_of("308"), Some(Kind::Area), "a marsh is an area");
    assert_eq!(kind_of("105"), None, "a text label is not a feature");
}

#[test]
fn the_box_is_the_one_the_points_fall_in() {
    for entry in snap_table(&shapes()) {
        let (mut left, mut top, mut right, mut bottom) = (
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        );
        for point in &entry.points {
            left = left.min(point.x);
            top = top.min(point.y);
            right = right.max(point.x);
            bottom = bottom.max(point.y);
        }
        assert_eq!((entry.bounds.left(), entry.bounds.top()), (left, top));
        assert_eq!(
            (entry.bounds.right(), entry.bounds.bottom()),
            (right, bottom)
        );
    }
}

#[test]
fn the_points_are_the_objects_own_coordinates() {
    let map = shapes();
    let table = snap_table(&map);
    // Curves are not flattened and control points are not dropped: an entry
    // has exactly as many points as the object it came from.
    let object_sizes: Vec<usize> = map
        .objects
        .iter()
        .filter(|o| o.symbol_index.is_some() && !o.coords.is_empty())
        .map(|o| o.coords.len())
        .collect();
    for entry in &table {
        assert!(object_sizes.contains(&entry.points.len()));
    }
}

#[test]
fn a_map_with_nothing_on_it_has_an_empty_table() {
    let map = read_xml_map_str(&std::fs::read_to_string("tests/data/empty.xmap").unwrap())
        .unwrap()
        .0;
    assert!(snap_table(&map).is_empty());
}

#[test]
fn the_scale_is_read_even_where_the_map_is_on_no_ground() {
    let map = shapes();
    let georeferencing = map.georeferencing.expect("the fixture has the element");
    assert_eq!(georeferencing.scale, 10000);
    assert_eq!(georeferencing.epsg, 0, "no projected CRS in the fixture");
    assert!(!georeferencing.grivation_specified);
    assert_eq!(georeferencing.grivation, 0.0);
}

#[test]
fn a_georeferenced_map_says_where_it_is_and_which_way_it_points() {
    let xml = r#"<?xml version="1.0"?>
<map version="9">
<georeferencing scale="15000" grivation="7.1">
  <projected_crs id="EPSG">
    <spec language="PROJ.4">+init=epsg:3006</spec>
    <parameter>3006</parameter>
    <ref_point x="322500" y="6397500"/>
  </projected_crs>
</georeferencing>
<colors count="0"/>
<symbols count="0" id="ISOM 2017-2"/>
<parts count="0"/>
</map>"#;
    let (map, _) = read_xml_map_str(xml).unwrap();
    assert_eq!(
        map.georeferencing,
        Some(Georeferencing {
            scale: 15000,
            epsg: 3006,
            ref_point_x: 322500.0,
            ref_point_y: 6397500.0,
            grivation: 7.1,
            grivation_specified: true,
        })
    );
    assert_eq!(map.symbol_set.as_deref(), Some("ISOM 2017-2"));
}

#[test]
fn an_undeclared_rotation_is_not_a_rotation_of_zero() {
    let xml = r#"<?xml version="1.0"?>
<map version="9">
<georeferencing scale="15000"/>
<colors count="0"/>
<symbols count="0" id=""/>
<parts count="0"/>
</map>"#;
    let (map, _) = read_xml_map_str(xml).unwrap();
    let georeferencing = map.georeferencing.expect("the element is there");
    assert_eq!(georeferencing.grivation, 0.0);
    assert!(
        !georeferencing.grivation_specified,
        "the file never said which way the map points"
    );
    assert_eq!(map.symbol_set, None, "an empty id names no symbol set");
}
