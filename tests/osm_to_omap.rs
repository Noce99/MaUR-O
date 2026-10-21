//! `osm_to_omap`'s command line: that what it writes is a map this crate and
//! Mapper read back as one, georeferenced and clipped, and that it exits with
//! the documented code when it cannot.
//!
//! Driven through the built binary, like `cli.rs`, because the interface —
//! the exit codes and the resolution hint it prints — is the contract.

use assert_cmd::Command;

use maur_o::map::{ObjectKind, Symbol};
use maur_o::xml_reader::read_xml_map;

const SAMPLE: &str = "tests/data/bologna_sample.osm";
/// Everything in the sample but the far-away node, with the footway's two
/// ends cut off.
const BBOX: &str = "--bbox=11.3415,44.4925,11.3445,44.4945";

fn osm_to_omap() -> Command {
    Command::cargo_bin("osm_to_omap").unwrap()
}

#[test]
fn converts_and_reads_back_as_a_georeferenced_map() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("sample.omap");
    osm_to_omap()
        .arg(BBOX)
        .arg(SAMPLE)
        .arg(&out)
        .assert()
        .success()
        .stdout(predicates::str::contains("map scale 1:6085"))
        .stdout(predicates::str::contains("EPSG:32632"))
        .stdout(predicates::str::contains("map_to_image -r 0.5869"));

    let (map, _warnings) = read_xml_map(&out).unwrap();
    assert_eq!(map.scale_denominator, 6085);
    let text = std::fs::read_to_string(&out).unwrap();
    assert!(text.contains("<parameter>32632</parameter>"));
    // Turned to true north: about 1.6° east of the zone's central meridian.
    assert!(text.contains("grivation=\"-1.6"), "{}", &text[..600]);

    // The land, the building, the street, the footway, the tree.
    assert_eq!(map.objects.len(), 5);
    let name = |o: &maur_o::map::Object| match &map.symbols[o.symbol_index.unwrap()] {
        Symbol::Point(s) => s.name.clone(),
        Symbol::Line(s) => s.name.clone(),
        Symbol::Area(s) => s.name.clone(),
        Symbol::Text(s) => s.name.clone(),
        Symbol::Combined(s) => s.name.clone(),
    };
    let building = map.objects.iter().find(|o| name(o) == "building").unwrap();
    // Two rings: the courtyard is a hole.
    let ends = building
        .coords
        .iter()
        .filter(|c| c.is_close_point())
        .count();
    assert_eq!(ends, 2);
    assert!(building.coords.iter().any(|c| c.is_hole_point()));

    let tree = map.objects.iter().find(|o| name(o) == "tree").unwrap();
    assert!(matches!(tree.kind, ObjectKind::Point));
    assert!(matches!(
        map.symbols[tree.symbol_index.unwrap()],
        Symbol::Point(_)
    ));

    // The footway runs from 11.3410 to 11.3450 and is cut at the box's
    // edges, 0.0030° of longitude apart: about 238 m, 39 mm at 1:6085.
    let footway = map
        .objects
        .iter()
        .find(|o| name(o).starts_with("footway"))
        .unwrap();
    let (first, last) = (footway.coords[0], *footway.coords.last().unwrap());
    let length = (last.x - first.x).hypot(last.y - first.y);
    assert!((length - 39.1).abs() < 0.5, "{length} mm");
}

#[test]
fn the_written_map_renders() {
    let dir = tempfile::tempdir().unwrap();
    let omap = dir.path().join("sample.omap");
    let png = dir.path().join("sample.png");
    osm_to_omap()
        .arg(BBOX)
        .arg(SAMPLE)
        .arg(&omap)
        .assert()
        .success();
    Command::cargo_bin("map_to_image")
        .unwrap()
        .args(["-r", "0.5869", "-f", "0"])
        .arg(&omap)
        .arg(&png)
        .assert()
        .success();
    assert!(png.exists());
}

#[test]
fn a_missing_file_exits_with_2() {
    osm_to_omap()
        .arg("tests/data/does_not_exist.osm")
        .assert()
        .code(2)
        .stderr(predicates::str::contains("cannot open"));
}

#[test]
fn a_malformed_bounding_box_is_a_usage_error() {
    osm_to_omap()
        .args(["--bbox=11.34,44.49", SAMPLE])
        .assert()
        .code(1);
}

/// The symbol name of each object of a map, in order.
fn symbol_names(map: &maur_o::map::Map) -> Vec<String> {
    map.objects
        .iter()
        .map(|o| match &map.symbols[o.symbol_index.unwrap()] {
            Symbol::Point(s) => s.name.clone(),
            Symbol::Line(s) => s.name.clone(),
            Symbol::Area(s) => s.name.clone(),
            Symbol::Text(s) => s.name.clone(),
            Symbol::Combined(s) => s.name.clone(),
        })
        .collect()
}

#[test]
fn a_bus_line_from_the_route_relations_gets_its_route_stops_and_numbers() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("bus.omap");
    osm_to_omap()
        .args([BBOX, "--bus", "13,99", SAMPLE])
        .arg(&out)
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "bus 13 (#e6194b): 1 stops, 13 : West -> East",
        ))
        .stderr(predicates::str::contains("no bus line 99 found"));

    let (map, _) = read_xml_map(&out).unwrap();
    let names = symbol_names(&map);
    let count = |name: &str| names.iter().filter(|n| *n == name).count();
    assert_eq!(count("bus 13"), 1);
    assert_eq!(count("bus 13 stop"), 1);
    assert_eq!(count("bus 13 label"), 2, "one number at each end");
    let label = map
        .objects
        .iter()
        .find_map(|o| match &o.kind {
            ObjectKind::Text(text) => Some(text.text.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(label, "13");
}

#[test]
fn a_bus_line_from_a_gtfs_feed() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("gtfs.omap");
    osm_to_omap()
        .args([
            BBOX,
            "--bus",
            "13",
            "--gtfs",
            "tests/data/gtfs_sample",
            SAMPLE,
        ])
        .arg(&out)
        .assert()
        .success()
        // Its own two stops and the one of the way back.
        .stdout(predicates::str::contains(
            "bus 13 (#e6194b): 3 stops, West, East",
        ));

    let (map, _) = read_xml_map(&out).unwrap();
    let names = symbol_names(&map);
    assert_eq!(names.iter().filter(|n| *n == "bus 13 stop").count(), 3);
    assert_eq!(names.iter().filter(|n| *n == "bus 13 label").count(), 2);
}

#[test]
fn a_missing_gtfs_feed_exits_with_2() {
    osm_to_omap()
        .args([
            BBOX,
            "--bus",
            "13",
            "--gtfs",
            "tests/data/no_such_feed.zip",
            SAMPLE,
        ])
        .assert()
        .code(2)
        .stderr(predicates::str::contains("cannot open"));
}
