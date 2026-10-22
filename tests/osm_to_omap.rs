//! `osm_to_omap`'s command line: that what it writes is a map this crate and
//! Mapper read back as one, georeferenced and clipped, and that it exits with
//! the documented code when it cannot.
//!
//! Driven through the built binary, like `cli.rs`, because the interface —
//! the exit codes and the resolution hint it prints — is the contract.

use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;

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
        .stdout(predicates::str::contains("map scale 1:5000"))
        .stdout(predicates::str::contains("EPSG:32632"))
        .stdout(predicates::str::contains("map_to_image -r 0.7143"));

    let (map, _warnings) = read_xml_map(&out).unwrap();
    assert_eq!(map.scale_denominator, 5000);
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
    // edges, 0.0030° of longitude apart: about 238 m, 47.6 mm at 1:5000.
    let footway = map
        .objects
        .iter()
        .find(|o| name(o).starts_with("footway"))
        .unwrap();
    let (first, last) = (footway.coords[0], *footway.coords.last().unwrap());
    let length = (last.x - first.x).hypot(last.y - first.y);
    assert!((length - 47.6).abs() < 0.5, "{length} mm");
}

#[test]
fn fewer_details_leave_out_what_carto_leaves_out_at_that_zoom() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("sample.omap");
    osm_to_omap()
        .arg(BBOX)
        .args(["--details", "0"])
        .arg(SAMPLE)
        .arg(&out)
        .assert()
        .success()
        .stdout(predicates::str::contains("map scale 1:24000"));
    let (map, _warnings) = read_xml_map(&out).unwrap();
    assert_eq!(map.scale_denominator, 24_000);
    // Zoom 13 has no building, footway or tree: the land and the street.
    assert_eq!(symbol_names(&map), ["land", "highway residential"]);

    osm_to_omap()
        .arg(BBOX)
        .args(["--details", "5"])
        .arg(SAMPLE)
        .arg(&out)
        .assert()
        .code(1);
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
        .args(["-r", "0.7143", "-f", "0"])
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
    // Its stop is on the street it runs along: a bar across it, not a
    // circle.
    assert_eq!(count("transit stop bar"), 1);
    assert_eq!(count("bus 13 stop"), 0);
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
        // The Wednesday of the feed's weekday service with the most of it.
        .stdout(predicates::str::contains(
            "trips leaving between 10:00 and 16:00 on 20260107",
        ))
        // Its own two stops and the one of the way back; two trips of the
        // direction drawn leave between 10 and 16 (t2 runs the other way).
        .stdout(predicates::str::contains(
            "bus 13 (#e6194b): 3 stops, every 180 min, West, East",
        ));

    let (map, _) = read_xml_map(&out).unwrap();
    let names = symbol_names(&map);
    // Its own two stops are bars across the street; the one of the way
    // back stands a few metres from the second, and is the same bar.
    assert_eq!(names.iter().filter(|n| *n == "transit stop bar").count(), 2);
    assert_eq!(names.iter().filter(|n| *n == "bus 13 stop").count(), 0);
    assert_eq!(names.iter().filter(|n| *n == "bus 13 label").count(), 2);
    // How often it runs, written once along the short stretch of it the
    // map shows.
    let headways: Vec<&str> = map
        .objects
        .iter()
        .filter_map(|o| match &o.kind {
            ObjectKind::Text(text) if text.text.starts_with("ogni") => Some(text.text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(headways, vec!["ogni 180 min"]);
}

#[test]
fn a_date_with_no_service_gives_no_headway() {
    osm_to_omap()
        .args([
            BBOX,
            "--bus",
            "13",
            "--gtfs",
            "tests/data/gtfs_sample",
            "--date",
            "20260110",
            SAMPLE,
        ])
        .arg(tempfile::tempdir().unwrap().path().join("saturday.omap"))
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "bus 13 (#e6194b): 3 stops, West, East",
        ));
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

#[test]
fn a_train_line_keeps_its_own_colour_and_is_not_a_bus() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("train.omap");
    osm_to_omap()
        .args([BBOX, "--train", "MEX", "--bus", "MEX", SAMPLE])
        .arg(&out)
        .assert()
        .success()
        // The colour is the route master's.
        .stdout(predicates::str::contains(
            "train MEX (#000000): 1 stops, Marconi Express",
        ))
        .stderr(predicates::str::contains("no bus line MEX found"));

    let (map, _) = read_xml_map(&out).unwrap();
    let names = symbol_names(&map);
    assert_eq!(names.iter().filter(|n| *n == "train MEX").count(), 1);
    assert_eq!(names.iter().filter(|n| *n == "train MEX stop").count(), 1);
    // The footway it runs along leaves the box at both ends: its termini
    // are off the map, so its number goes where the edge cuts it, inside.
    let labels: Vec<(f64, f64)> = map
        .objects
        .iter()
        .filter(|o| matches!(&o.kind, ObjectKind::Text(t) if t.text == "MEX"))
        .map(|o| (o.coords[0].x, o.coords[0].y))
        .collect();
    assert_eq!(labels.len(), 2);
    let land = &map.objects[0].coords;
    let (west, east) = (
        land.iter().map(|c| c.x).fold(f64::INFINITY, f64::min),
        land.iter().map(|c| c.x).fold(f64::NEG_INFINITY, f64::max),
    );
    // About 5 mm in from each edge, along the line.
    let mut xs: Vec<f64> = labels.iter().map(|&(x, _)| x).collect();
    xs.sort_by(f64::total_cmp);
    assert!((xs[0] - west - 5.0).abs() < 0.5, "{xs:?}, west edge {west}");
    assert!((east - xs[1] - 5.0).abs() < 0.5, "{xs:?}, east edge {east}");
}

#[test]
fn a_legend_explains_the_symbols_the_map_uses_below_it() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("legend.omap");
    osm_to_omap()
        .args([BBOX, "--bus", "13", "--legend", SAMPLE])
        .arg(&out)
        .assert()
        .success();

    let (map, _) = read_xml_map(&out).unwrap();
    let texts: Vec<(String, f64)> = map
        .objects
        .iter()
        .filter_map(|o| match &o.kind {
            ObjectKind::Text(text) => Some((text.text.clone(), o.coords[0].y)),
            _ => None,
        })
        .collect();
    let has = |caption: &str| texts.iter().any(|(t, _)| t == caption);
    assert!(has("Legenda"));
    assert!(has("Bus 13: West -> East"), "{texts:?}");
    assert!(has("Fermata, stazione"));
    assert!(has("Edificio"));
    assert!(has("Strada residenziale"));
    assert!(has("Albero"));
    // Nothing the map does not use: there is no water in the sample.
    assert!(!has("Acqua"));

    // Below the map: every caption lower on the paper than any of the land.
    let land_bottom = map.objects[0]
        .coords
        .iter()
        .map(|c| c.y)
        .fold(f64::NEG_INFINITY, f64::max);
    let legend_top = texts
        .iter()
        .filter(|(t, _)| t == "Legenda")
        .map(|(_, y)| *y)
        .next()
        .unwrap();
    assert!(legend_top > land_bottom);
}

#[test]
fn all_takes_every_line_of_the_feed_or_else_of_the_extract() {
    let dir = tempfile::tempdir().unwrap();
    // From the GTFS feed: its one route.
    osm_to_omap()
        .args([
            BBOX,
            "--bus",
            "all",
            "--gtfs",
            "tests/data/gtfs_sample",
            SAMPLE,
        ])
        .arg(dir.path().join("gtfs.omap"))
        .assert()
        .success()
        .stdout(predicates::str::contains("bus 13 (#e6194b)"))
        .stderr(predicates::str::contains("Warning").not());
    // With no feed, from the route relations: the bus, and the monorail as
    // the one train.
    osm_to_omap()
        .args([BBOX, "--bus", "all", "--train", "all", SAMPLE])
        .arg(dir.path().join("osm.omap"))
        .assert()
        .success()
        .stdout(predicates::str::contains("bus 13 (#e6194b)"))
        .stdout(predicates::str::contains("train MEX (#000000)"));
}

#[test]
fn a_cap_on_the_headway_leaves_out_the_buses_running_less_often() {
    let dir = tempfile::tempdir().unwrap();
    let gtfs = |cap: &str, name: &str| {
        osm_to_omap()
            .args([
                BBOX,
                "--bus",
                "13",
                "--gtfs",
                "tests/data/gtfs_sample",
                "--max-headway",
                cap,
                SAMPLE,
            ])
            .arg(dir.path().join(name))
            .assert()
            .success()
    };
    // Line 13 runs every 180 minutes in the sample feed.
    gtfs("60", "capped.omap")
        .stdout(predicates::str::contains(
            "Left out 1 bus lines running less often than every 60 min: 13",
        ))
        .stdout(predicates::str::contains("bus 13 (").not());
    gtfs("180", "kept.omap").stdout(predicates::str::contains("bus 13 ("));

    // From the route relations, with no interval tag: nothing says how
    // often it runs, so it is kept, and said so.
    osm_to_omap()
        .args([BBOX, "--bus", "13", "--max-headway", "10", SAMPLE])
        .arg(dir.path().join("unknown.omap"))
        .assert()
        .success()
        .stdout(predicates::str::contains("bus 13 ("))
        .stderr(predicates::str::contains(
            "bus 13 has no known frequency, kept",
        ));
}

#[test]
fn two_bus_lines_on_one_street_are_drawn_side_by_side_with_one_bar() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("bundle.omap");
    osm_to_omap()
        .args([BBOX, "--bus", "14,13", SAMPLE])
        .arg(&out)
        .assert()
        .success();

    let (map, _) = read_xml_map(&out).unwrap();
    let names = symbol_names(&map);
    let route = |name: &str| -> Vec<(f64, f64)> {
        let i = names.iter().position(|n| n == name).unwrap();
        map.objects[i].coords.iter().map(|c| (c.x, c.y)).collect()
    };
    let (thirteen, fourteen) = (route("bus 13"), route("bus 14"));
    // A bus line is 3 carto pixels, 0.84 mm, wide: the two run that far
    // apart, either side of the street, in the order of their numbers
    // whichever order they were asked for in.
    let middle = |r: &[(f64, f64)]| r[r.len() / 2];
    let (a, b) = (middle(&thirteen), middle(&fourteen));
    let apart = (a.0 - b.0).hypot(a.1 - b.1);
    assert!((apart - 0.84).abs() < 0.05, "{apart} mm apart");
    // The street runs east: 13, ranked first, on its left, the north, the
    // paper's y pointing down.
    assert!(a.1 < b.1, "13 at {a:?}, 14 at {b:?}");
    // Their one stop is one bar across both, with one arrow beside it
    // pointing east, the way both run.
    assert_eq!(names.iter().filter(|n| *n == "transit stop bar").count(), 1);
    let arrows: Vec<&maur_o::map::Object> = map
        .objects
        .iter()
        .zip(&names)
        .filter(|(_, n)| *n == "transit stop arrow")
        .map(|(o, _)| o)
        .collect();
    assert_eq!(arrows.len(), 1);
    assert!(arrows[0].rotation.abs() < 0.1, "{}", arrows[0].rotation);
}

#[test]
fn excluded_buses_are_left_out_of_those_selected() {
    let dir = tempfile::tempdir().unwrap();
    // From all the extract's buses, 13 and 14, leave out 13 — under either
    // spelling of the option.
    for option in ["--bus-exclude", "--bus_exclude"] {
        osm_to_omap()
            .args([BBOX, "--bus", "all", option, "13", SAMPLE])
            .arg(dir.path().join("excluded.omap"))
            .assert()
            .success()
            .stdout(predicates::str::contains("bus 14 ("))
            .stdout(predicates::str::contains("bus 13 (").not());
    }
    // Leaving out a line not selected says so.
    osm_to_omap()
        .args([BBOX, "--bus", "14", "--bus-exclude", "99", SAMPLE])
        .arg(dir.path().join("unknown.omap"))
        .assert()
        .success()
        .stderr(predicates::str::contains(
            "bus line 99 to leave out is not among those selected",
        ));
}
