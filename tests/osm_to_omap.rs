//! `osm_to_omap`'s command line: that what it writes is a map this crate and
//! Mapper read back as one, georeferenced and clipped, drawn to the standard
//! asked for, and that it exits with the documented code when it cannot.
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

/// The command, drawing like openstreetmap.org: what most tests here are
/// about.
fn osm_to_omap() -> Command {
    let mut command = orienteering();
    command.args(["--standard", "osm"]);
    command
}

/// The command as it is, which draws to ISSprOM.
fn orienteering() -> Command {
    Command::cargo_bin("osm_to_omap").unwrap()
}

/// The symbol code of each object of a map, in order.
fn object_codes(map: &maur_o::map::Map) -> Vec<String> {
    map.objects
        .iter()
        .map(|o| match &map.symbols[o.symbol_index.unwrap()] {
            Symbol::Point(s) => s.code.clone(),
            Symbol::Line(s) => s.code.clone(),
            Symbol::Area(s) => s.code.clone(),
            Symbol::Text(s) => s.code.clone(),
            Symbol::Combined(s) => s.code.clone(),
        })
        .collect()
}

#[test]
fn the_default_is_a_sprint_map_with_the_whole_issprom_symbol_set() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("sprint.omap");
    orienteering()
        .arg(BBOX)
        .arg(SAMPLE)
        .arg(&out)
        .assert()
        .success()
        .stdout(predicates::str::contains("ISSprOM 2019, map scale 1:4000"))
        .stdout(predicates::str::contains("EPSG:32632"));

    let (map, _warnings) = read_xml_map(&out).unwrap();
    assert_eq!(map.scale_denominator, 4000);
    // The building, the street as a paved area, the footway; the tree is no
    // landmark.
    let mut codes = object_codes(&map);
    codes.sort();
    assert_eq!(codes, ["501.5", "501.6", "521"]);

    // Every colour and symbol as the standard has them.
    let (golden, _) = read_xml_map(std::path::Path::new("maps/ISSprOM_4k.omap")).unwrap();
    assert_eq!(map.symbols.len(), golden.symbols.len());
    let none = Default::default();
    let reference = maur_o::validate::Reference {
        golden: &golden,
        base_scale: 4000.0,
        code_descriptions: &Default::default(),
        impassable_codes: &none,
        no_cross_codes: &none,
        forbidden_area_overlaps: &[],
        gap_exempt_pairs: &[],
        min_gap_mm: 0.0,
        min_gap_impassable_mm: 0.0,
    };
    let issues = maur_o::validate::validate_colors(&map, &reference);
    assert!(issues.is_empty(), "{issues:?}");
    let issues = maur_o::validate::validate_symbols(&map, &reference);
    assert!(issues.is_empty(), "{issues:?}");
}

#[test]
fn isom_draws_a_forest_map_at_1_10000() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("forest.omap");
    orienteering()
        .args([BBOX, "--standard", "isom", SAMPLE])
        .arg(&out)
        .assert()
        .success()
        .stdout(predicates::str::contains("ISOM 2017-2, map scale 1:10000"));
    let (map, _warnings) = read_xml_map(&out).unwrap();
    assert_eq!(map.scale_denominator, 10_000);
    let mut codes = object_codes(&map);
    codes.sort();
    assert_eq!(codes, ["503", "505", "521"]);
}

#[test]
fn an_option_of_the_osm_look_is_a_usage_error_with_a_standard() {
    for option in [&["--legend"][..], &["--details", "2"], &["--bus", "13"]] {
        orienteering()
            .args(option)
            .arg(SAMPLE)
            .assert()
            .code(1)
            .stderr(predicates::str::contains("needs --standard osm"));
    }
}

#[test]
fn a_park_in_a_meadow_is_cut_out_of_it_and_its_lawn_out_of_the_park() {
    let dir = tempfile::tempdir().unwrap();
    let osm = dir.path().join("park.osm");
    // A meadow 0.004° across, a park in its middle and a lawn in the park,
    // each a closed way.
    let square = |id: i64, first: i64, (lon0, lat0, lon1, lat1): (f64, f64, f64, f64)| {
        let corners = [(lon0, lat0), (lon1, lat0), (lon1, lat1), (lon0, lat1)];
        let nodes: String = corners
            .iter()
            .enumerate()
            .map(|(i, (lon, lat))| {
                format!(
                    "<node id=\"{}\" lat=\"{lat}\" lon=\"{lon}\"/>\n",
                    first + i as i64
                )
            })
            .collect();
        let refs: String = (0..5)
            .map(|i| format!("<nd ref=\"{}\"/>", first + i % 4))
            .collect();
        (nodes, format!("<way id=\"{id}\">{refs}"))
    };
    let (park_nodes, park) = square(1, 1, (11.340, 44.490, 11.344, 44.494));
    let (wood_nodes, wood) = square(2, 11, (11.341, 44.491, 11.343, 44.493));
    let (lawn_nodes, lawn) = square(3, 21, (11.3415, 44.4915, 11.3425, 44.4925));
    std::fs::write(
        &osm,
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<osm version="0.6">
  <bounds minlat="44.489" minlon="11.339" maxlat="44.495" maxlon="11.345"/>
{park_nodes}{wood_nodes}{lawn_nodes}{park}<tag k="landuse" v="meadow"/></way>
{wood}<tag k="leisure" v="park"/></way>
{lawn}<tag k="landuse" v="grass"/></way>
</osm>
"#
        ),
    )
    .unwrap();
    let out = dir.path().join("park.omap");
    orienteering().arg(&osm).arg(&out).assert().success();
    let (map, _warnings) = read_xml_map(&out).unwrap();
    let codes = object_codes(&map);
    let rings = |o: &maur_o::map::Object| o.coords.iter().filter(|c| c.is_close_point()).count();
    let open: Vec<usize> = codes
        .iter()
        .enumerate()
        .filter(|(_, c)| *c == "401")
        .map(|(i, _)| rings(&map.objects[i]))
        .collect();
    let park = &map.objects[codes.iter().position(|c| c == "402").unwrap()];
    // The meadow keeps a hole where the park is, the park one where the
    // lawn is, the lawn is whole.
    assert_eq!(open.len(), 2);
    assert!(open.contains(&2) && open.contains(&1), "{open:?}");
    assert_eq!(rings(park), 2);
}

#[test]
fn a_portico_is_cut_out_of_the_building_it_runs_under() {
    let dir = tempfile::tempdir().unwrap();
    let osm = dir.path().join("portico.osm");
    std::fs::write(
        &osm,
        r#"<?xml version="1.0" encoding="UTF-8"?>
<osm version="0.6">
  <bounds minlat="44.4920" minlon="11.3400" maxlat="44.4950" maxlon="11.3450"/>
  <node id="1" lat="44.4930" lon="11.3410"/>
  <node id="2" lat="44.4930" lon="11.3440"/>
  <node id="3" lat="44.4940" lon="11.3440"/>
  <node id="4" lat="44.4940" lon="11.3410"/>
  <node id="5" lat="44.4930" lon="11.3405"/>
  <node id="6" lat="44.4930" lon="11.3445"/>
  <way id="1"><nd ref="1"/><nd ref="2"/><nd ref="3"/><nd ref="4"/><nd ref="1"/>
    <tag k="building" v="yes"/></way>
  <way id="2"><nd ref="5"/><nd ref="6"/>
    <tag k="highway" v="footway"/><tag k="covered" v="arcade"/></way>
</osm>
"#,
    )
    .unwrap();
    let out = dir.path().join("portico.omap");
    orienteering().arg(&osm).arg(&out).assert().success();
    let (map, _warnings) = read_xml_map(&out).unwrap();
    let codes = object_codes(&map);
    let canopy = &map.objects[codes.iter().position(|c| c == "522").unwrap()];
    let building = &map.objects[codes.iter().position(|c| c == "521").unwrap()];
    // The walkway runs along the building's south side: the 4 m canopy
    // takes 2 m off the building, which is 0.001° (111.2 m) deep. On the
    // paper, at 1:4000: 27.30 mm of building, 1 mm of canopy.
    let span = |o: &maur_o::map::Object| {
        let ys: Vec<f64> = o.coords.iter().map(|c| c.y).collect();
        ys.iter().cloned().fold(f64::MIN, f64::max) - ys.iter().cloned().fold(f64::MAX, f64::min)
    };
    assert!((span(building) - 27.30).abs() < 0.05, "{}", span(building));
    assert!((span(canopy) - 1.0).abs() < 0.05, "{}", span(canopy));
    // No footway line is drawn under it.
    assert!(!codes.iter().any(|c| c.starts_with("501.6")));
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
fn a_line_not_running_that_date_is_left_out() {
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
            "Left out 1 lines not running on 20260110: 13",
        ))
        .stdout(predicates::str::contains("bus 13 (").not());
}

#[test]
fn a_day_and_a_date_do_not_go_together() {
    osm_to_omap()
        .args([
            BBOX,
            "--bus",
            "13",
            "--gtfs",
            "tests/data/gtfs_sample",
            "--day",
            "sunday",
            "--date",
            "20260110",
            SAMPLE,
        ])
        .assert()
        .code(1);
}

#[test]
fn a_day_the_feed_has_no_service_on_is_reported() {
    osm_to_omap()
        .args([
            BBOX,
            "--bus",
            "13",
            "--gtfs",
            "tests/data/gtfs_sample",
            "--day",
            "sunday",
            SAMPLE,
        ])
        .arg(tempfile::tempdir().unwrap().path().join("sunday.omap"))
        .assert()
        .success()
        .stderr(predicates::str::contains(
            "no sunday with service in the GTFS feed",
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
    // Near each edge, inside, where it is 5 mm in along the line — but
    // off the line itself.
    let mut xs: Vec<f64> = labels.iter().map(|&(x, _)| x).collect();
    xs.sort_by(f64::total_cmp);
    assert!(
        xs[0] > west && xs[0] - west < 5.0 + 12.0,
        "{xs:?}, west {west}"
    );
    assert!(
        east > xs[1] && east - xs[1] < 5.0 + 12.0,
        "{xs:?}, east {east}"
    );
    let line = names.iter().position(|n| n == "train MEX").unwrap();
    for &(x, y) in &labels {
        for c in &map.objects[line].coords {
            let off = ((c.x - x).abs() - 2.4).max((c.y - y).abs() - 2.2);
            assert!(off > 0.0, "the number at {x},{y} covers the line");
        }
    }
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

#[test]
fn maps_draws_each_maps_area_and_name() {
    let dir = tempfile::tempdir().unwrap();
    let list = dir.path().join("aree.csv");
    std::fs::write(
        &list,
        "map_number,map_name,latitude,longitude\n\
         1,Dentro,44.4930,11.3420\n\
         1,Dentro,44.4940,11.3420\n\
         1,Dentro,44.4940,11.3440\n\
         1,Dentro,44.4930,11.3440\n\
         2,Lontano,45.0,12.0\n\
         2,Lontano,45.1,12.0\n\
         2,Lontano,45.1,12.1\n",
    )
    .unwrap();
    let out = dir.path().join("maps.omap");
    osm_to_omap()
        .args([BBOX, "--maps"])
        .arg(&list)
        .args(["--legend", SAMPLE])
        .arg(&out)
        .assert()
        .success()
        .stderr(predicates::str::contains(
            "map 2 - Lontano has no area on the map",
        ));

    let (map, _) = read_xml_map(&out).unwrap();
    let symbol_name = |o: &maur_o::map::Object| match &map.symbols[o.symbol_index.unwrap()] {
        Symbol::Combined(s) => s.name.clone(),
        Symbol::Text(s) => s.name.clone(),
        _ => String::new(),
    };
    let areas = map
        .objects
        .iter()
        .filter(|o| symbol_name(o) == "1 - Dentro")
        .count();
    // The area on the map, and its sample in the legend.
    assert_eq!(areas, 2);
    let name = map
        .objects
        .iter()
        .find(|o| symbol_name(o) == "1 - Dentro name")
        .unwrap();
    assert!(matches!(&name.kind, ObjectKind::Text(t) if t.text == "1 - Dentro"));
    let texts: Vec<String> = map
        .objects
        .iter()
        .filter_map(|o| match &o.kind {
            ObjectKind::Text(text) => Some(text.text.clone()),
            _ => None,
        })
        .collect();
    assert!(texts.iter().any(|t| t == "Area 1 - Dentro"), "{texts:?}");
    assert!(!texts.iter().any(|t| t.contains("Lontano")));
}

#[test]
fn a_maps_list_that_cannot_be_read_exits_with_2() {
    osm_to_omap()
        .args(["--maps", "no/such/list.csv", SAMPLE])
        .assert()
        .code(2);
}

#[test]
fn maps_takes_the_colours_of_a_csv_list() {
    let dir = tempfile::tempdir().unwrap();
    let list = dir.path().join("aree.csv");
    std::fs::write(
        &list,
        "map_id,map_number,map_name,area,vertex,latitude,longitude,\
         stroke_color,stroke_alpha,fill_color,fill_alpha\n\
         7,1,Dentro,1,1,44.4930,11.3420,#0d6efd,1,#dc3545,0.25\n\
         7,1,Dentro,1,2,44.4940,11.3420,#0d6efd,1,#dc3545,0.25\n\
         7,1,Dentro,1,3,44.4940,11.3440,#0d6efd,1,#dc3545,0.25\n\
         7,1,Dentro,1,4,44.4930,11.3440,#0d6efd,1,#dc3545,0.25\n",
    )
    .unwrap();
    let out = dir.path().join("maps.omap");
    osm_to_omap()
        .args([BBOX, "--maps"])
        .arg(&list)
        .arg(SAMPLE)
        .arg(&out)
        .assert()
        .success();

    let (map, _) = read_xml_map(&out).unwrap();
    let color = |name: &str| {
        map.colors
            .iter()
            .find(|c| c.name == name)
            .unwrap_or_else(|| panic!("no colour {name}"))
            .clone()
    };
    let rgb = |c: &maur_o::map::Color| {
        (
            (c.rgb.0 * 255.0).round() as u8,
            (c.rgb.1 * 255.0).round() as u8,
            (c.rgb.2 * 255.0).round() as u8,
        )
    };
    assert_eq!(rgb(&color("map 0")), (0x0d, 0x6e, 0xfd));
    assert_eq!(rgb(&color("map 0 name")), (0x0d, 0x6e, 0xfd));
    let fill = color("map 0 fill");
    assert_eq!(rgb(&fill), (0xdc, 0x35, 0x45));
    assert!((fill.opacity - 0.25).abs() < 1e-6);
    assert!(map
        .objects
        .iter()
        .any(|o| matches!(&o.kind, ObjectKind::Text(t) if t.text == "1 - Dentro")));
}

#[test]
fn maps_draws_the_forbidden_zones_the_start_and_the_finish() {
    let dir = tempfile::tempdir().unwrap();
    let list = dir.path().join("aree.csv");
    std::fs::write(
        &list,
        "map_id,map_number,map_name,area,vertex,latitude,longitude,\
         stroke_color,stroke_alpha,fill_color,fill_alpha,kind\n\
         7,1,Dentro,1,1,44.4930,11.3420,#0d6efd,1,#0d6efd,0.15,area\n\
         7,1,Dentro,1,2,44.4940,11.3420,#0d6efd,1,#0d6efd,0.15,area\n\
         7,1,Dentro,1,3,44.4940,11.3430,#0d6efd,1,#0d6efd,0.15,area\n\
         ,,Zone vietate,1,1,44.4930,11.3435,#a626ff,1,#a626ff,1,forbidden\n\
         ,,Zone vietate,1,2,44.4940,11.3435,#a626ff,1,#a626ff,1,forbidden\n\
         ,,Zone vietate,1,3,44.4940,11.3440,#a626ff,1,#a626ff,1,forbidden\n\
         ,,Partenza,,,44.4932,11.3425,#212529,1,#212529,1,start\n\
         ,,Arrivo,,,44.4933,11.3426,#212529,1,#212529,1,finish\n",
    )
    .unwrap();
    let out = dir.path().join("maps.omap");
    osm_to_omap()
        .args([BBOX, "--maps"])
        .arg(&list)
        .args(["--legend", SAMPLE])
        .arg(&out)
        .assert()
        .success();

    let (map, _) = read_xml_map(&out).unwrap();
    let symbol_of = |o: &maur_o::map::Object| &map.symbols[o.symbol_index.unwrap()];
    let zones: Vec<_> = map
        .objects
        .iter()
        .filter(|o| matches!(symbol_of(o), Symbol::Combined(s) if s.name == "Zone vietate"))
        .collect();
    // On the map, and in the legend.
    assert_eq!(zones.len(), 2);
    let Symbol::Combined(zone) = symbol_of(zones[0]) else {
        unreachable!()
    };
    assert!(zone
        .owned_parts
        .iter()
        .any(|p| matches!(p, Symbol::Area(a) if !a.patterns.is_empty())));
    for name in ["Partenza", "Arrivo"] {
        assert!(map
            .objects
            .iter()
            .any(|o| matches!(o.kind, ObjectKind::Point)
                && matches!(symbol_of(o), Symbol::Point(p) if p.name == name)));
    }
    let texts: Vec<String> = map
        .objects
        .iter()
        .filter_map(|o| match &o.kind {
            ObjectKind::Text(text) => Some(text.text.clone()),
            _ => None,
        })
        .collect();
    // Only the map's name written on it; the others in the legend.
    assert_eq!(texts.iter().filter(|t| *t == "1 - Dentro").count(), 1);
    for caption in ["Zone vietate", "Partenza", "Arrivo"] {
        assert_eq!(
            texts.iter().filter(|t| *t == caption).count(),
            1,
            "{texts:?}"
        );
    }
}

#[test]
fn areas_single_color_draws_every_area_in_black() {
    let dir = tempfile::tempdir().unwrap();
    let list = dir.path().join("aree.csv");
    std::fs::write(
        &list,
        "map_id,map_number,map_name,area,vertex,latitude,longitude,\
         stroke_color,stroke_alpha,fill_color,fill_alpha\n\
         7,1,Dentro,1,1,44.4930,11.3420,#0d6efd,1,#dc3545,0.25\n\
         7,1,Dentro,1,2,44.4940,11.3420,#0d6efd,1,#dc3545,0.25\n\
         7,1,Dentro,1,3,44.4940,11.3440,#0d6efd,1,#dc3545,0.25\n",
    )
    .unwrap();
    let out = dir.path().join("maps.omap");
    osm_to_omap()
        .args([BBOX, "--areas"])
        .arg(&list)
        .args(["--areas-single-color", SAMPLE])
        .arg(&out)
        .assert()
        .success();

    let (map, _) = read_xml_map(&out).unwrap();
    let color = |name: &str| map.colors.iter().find(|c| c.name == name).unwrap().clone();
    for name in ["map 0", "map 0 name", "map 0 fill"] {
        assert_eq!(color(name).rgb, (0.0, 0.0, 0.0), "{name}");
    }
    assert_eq!(color("map 0").opacity, 1.0);
    assert!((color("map 0 fill").opacity - 0.15).abs() < 1e-6);
}

#[test]
fn areas_single_color_needs_a_list_of_maps() {
    osm_to_omap()
        .args(["--areas-single-color", SAMPLE])
        .assert()
        .code(1);
}

#[test]
fn drinking_water_marks_the_fountains_with_a_glass() {
    let dir = tempfile::tempdir().unwrap();
    let osm = dir.path().join("water.osm");
    std::fs::write(
        &osm,
        r#"<?xml version="1.0" encoding="UTF-8"?>
<osm version="0.6">
  <bounds minlat="44.4925" minlon="11.3415" maxlat="44.4945" maxlon="11.3445"/>
  <node id="1" lat="44.4930" lon="11.3420"><tag k="amenity" v="drinking_water"/></node>
  <node id="2" lat="44.4935" lon="11.3425">
    <tag k="man_made" v="water_tap"/><tag k="drinking_water" v="yes"/>
  </node>
  <node id="3" lat="44.4940" lon="11.3430"><tag k="amenity" v="fountain"/></node>
</osm>
"#,
    )
    .unwrap();
    let out = dir.path().join("water.omap");
    let glasses = |water: bool| {
        let mut command = osm_to_omap();
        if water {
            command.arg("--drinking-water");
        }
        command.arg(&osm).arg(&out).assert().success();
        let (map, _) = read_xml_map(&out).unwrap();
        map.objects
            .iter()
            .filter(|o| {
                matches!(&map.symbols[o.symbol_index.unwrap()],
                    Symbol::Point(p) if p.name == "drinking water")
            })
            .count()
    };
    assert_eq!(glasses(false), 0);
    // The fountain and the tap, not the ornamental fountain.
    assert_eq!(glasses(true), 2);
}

#[test]
fn houses_wall_to_wall_are_one_building() {
    let dir = tempfile::tempdir().unwrap();
    let osm = dir.path().join("houses.osm");
    // Four 10 m wide houses in a row, 0.0001° of latitude (11 m) deep. The
    // first two share a wall (nodes 2-3), the third is drawn 0.2 m off the
    // second, the fourth 3 m beyond the third, across an alley.
    // A degree of longitude is 79 430 m here (on the UTM grid, near
    // enough for these sizes).
    let lon = |m: f64| 11.34 + m / 79_430.0;
    let mut nodes = String::new();
    let mut ways = String::new();
    let mut add = |id: i64, x0: f64, x1: f64, shared: Option<(i64, i64)>| {
        let (a, b) = shared.unwrap_or((id * 10 + 1, id * 10 + 2));
        let (c, d) = (id * 10 + 3, id * 10 + 4);
        if shared.is_none() {
            nodes += &format!("<node id=\"{a}\" lat=\"44.4900\" lon=\"{}\"/>\n", lon(x0));
            nodes += &format!("<node id=\"{b}\" lat=\"44.4901\" lon=\"{}\"/>\n", lon(x0));
        }
        nodes += &format!("<node id=\"{c}\" lat=\"44.4901\" lon=\"{}\"/>\n", lon(x1));
        nodes += &format!("<node id=\"{d}\" lat=\"44.4900\" lon=\"{}\"/>\n", lon(x1));
        ways += &format!(
            "<way id=\"{id}\"><nd ref=\"{a}\"/><nd ref=\"{b}\"/><nd ref=\"{c}\"/><nd ref=\"{d}\"/><nd ref=\"{a}\"/><tag k=\"building\" v=\"yes\"/></way>\n"
        );
        (d, c)
    };
    let shared = add(1, 0.0, 10.0, None);
    add(2, 10.0, 20.0, Some(shared));
    add(3, 20.2, 30.0, None);
    add(4, 33.0, 43.0, None);
    std::fs::write(
        &osm,
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<osm version=\"0.6\">\n\
             <bounds minlat=\"44.4895\" minlon=\"11.3395\" maxlat=\"44.4906\" maxlon=\"11.3410\"/>\n\
             {nodes}{ways}</osm>\n"
        ),
    )
    .unwrap();
    let out = dir.path().join("houses.omap");
    orienteering().arg(&osm).arg(&out).assert().success();
    let (map, _warnings) = read_xml_map(&out).unwrap();
    let codes = object_codes(&map);
    let buildings: Vec<&maur_o::map::Object> = codes
        .iter()
        .zip(&map.objects)
        .filter(|(c, _)| *c == "521")
        .map(|(_, o)| o)
        .collect();
    // The first three are one block, 30 m (7.5 mm) long; the fourth stays apart.
    assert_eq!(buildings.len(), 2);
    let width = |o: &maur_o::map::Object| {
        let xs: Vec<f64> = o.coords.iter().map(|c| c.x).collect();
        xs.iter().cloned().fold(f64::MIN, f64::max) - xs.iter().cloned().fold(f64::MAX, f64::min)
    };
    let mut widths: Vec<f64> = buildings.iter().map(|o| width(o)).collect();
    widths.sort_by(f64::total_cmp);
    assert!((widths[0] - 2.5).abs() < 0.05, "{widths:?}");
    assert!((widths[1] - 7.5).abs() < 0.05, "{widths:?}");
}

#[test]
fn around_converts_a_square_centred_on_a_point() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("square.omap");
    orienteering()
        .args(["--around", "44.4935,11.3430", "--size", "200m", SAMPLE])
        .arg(&out)
        .assert()
        .success();
    let (map, _warnings) = read_xml_map(&out).unwrap();
    // The building, the street, the footway, as with the box around them.
    assert_eq!(map.objects.len(), 3);
    // Every map credits OpenStreetMap's contributors, as the ODbL asks.
    let text = std::fs::read_to_string(&out).unwrap();
    assert!(text.contains("<notes>Map data © OpenStreetMap contributors, ODbL"));
}

#[test]
fn download_options_are_checked_before_anything_is_asked() {
    let cases: [(&[&str], &str); 6] = [
        (
            &["--download", "out.omap"],
            "--download needs --bbox or --around",
        ),
        (
            &["--download", "--around", "44.49,11.34"],
            "--download needs the map to write",
        ),
        (
            &["--download", "--around", "44.49,11.34", "a.osm", "b.omap"],
            "name only the map to write",
        ),
        (
            &[
                "--download",
                "--around",
                "44.49,11.34",
                "--size",
                "6km",
                "out.omap",
            ],
            "over --max-area 25",
        ),
        (&["--size", "2km", SAMPLE], "--size needs --around"),
        (&["--save-osm", "a.osm", SAMPLE], "--download"),
    ];
    for (args, message) in cases {
        orienteering()
            .args(args)
            .assert()
            .code(1)
            .stderr(predicates::str::contains(message));
    }
    orienteering()
        .args([
            "--bbox=11.34,44.49,11.35,44.50",
            "--around",
            "44.49,11.34",
            SAMPLE,
        ])
        .assert()
        .code(1);
}

#[test]
fn a_server_that_cannot_be_reached_exits_with_3() {
    let dir = tempfile::tempdir().unwrap();
    // Nothing listens on the discard port: the connection is refused at once.
    orienteering()
        .args([
            "--download",
            "--around",
            "44.49,11.34",
            "--size",
            "100m",
            "--overpass-url",
            "http://127.0.0.1:9/api/interpreter",
        ])
        .arg(dir.path().join("out.omap"))
        .assert()
        .code(3)
        .stderr(predicates::str::contains(
            "download from http://127.0.0.1:9",
        ));
    assert!(!dir.path().join("out.omap").exists());
}
