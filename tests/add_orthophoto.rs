//! `add_orthophoto`'s command line: the exit codes it gives before it ever
//! reaches the network.
//!
//! Everything past that point talks to a mapping agency's WMS, which a test
//! suite has no business depending on. What can be pinned offline is that a
//! map the tool cannot place, or that no service covers, is turned away with
//! the documented code and that the map is never touched when it is.

use assert_cmd::Command;

fn add_orthophoto() -> Command {
    Command::cargo_bin("add_orthophoto").unwrap()
}

/// `shapes.xmap`, its objects intact, placed on the earth at `lat`, `lon`
/// (in UTM zone 32, whatever the place: only the degrees pick a service).
fn shapes_at(lat: f64, lon: f64) -> String {
    let shapes = std::fs::read_to_string("tests/data/shapes.xmap").unwrap();
    let start = shapes.find("<georeferencing").unwrap();
    let end = shapes.find("</georeferencing>").unwrap() + "</georeferencing>".len();
    let georeferencing = format!(
        r#"<georeferencing scale="10000"><projected_crs id="UTM"><spec language="PROJ.4">+proj=utm +datum=WGS84 +zone=32</spec><parameter>32</parameter><ref_point x="682364" y="4952128"/></projected_crs><geographic_crs id="Geographic coordinates"><spec language="PROJ.4">+proj=latlong +datum=WGS84</spec><ref_point_deg lat="{lat}" lon="{lon}"/></geographic_crs></georeferencing>"#
    );
    format!("{}{georeferencing}{}", &shapes[..start], &shapes[end..])
}

#[test]
fn lists_the_services() {
    add_orthophoto()
        .arg("--list-services")
        .assert()
        .success()
        .stdout(predicates::str::contains("it-emilia-romagna-2023"));
}

#[test]
fn an_unknown_service_is_a_usage_error() {
    add_orthophoto()
        .args(["--service", "atlantis-1999", "tests/data/empty.xmap"])
        .assert()
        .code(1)
        .stderr(predicates::str::contains("--list-services"));
}

#[test]
fn a_pixel_size_must_be_positive() {
    add_orthophoto()
        .args(["--pixel-size", "0", "tests/data/empty.xmap"])
        .assert()
        .code(1)
        .stderr(predicates::str::contains("--pixel-size"));
}

#[test]
fn a_missing_map_cannot_be_read() {
    add_orthophoto()
        .arg("tests/data/no_such_map.omap")
        .assert()
        .code(2)
        .stderr(predicates::str::contains("cannot read"));
}

#[test]
fn only_mapper_files_are_edited() {
    let dir = tempfile::tempdir().unwrap();
    let ocd = dir.path().join("map.ocd");
    std::fs::write(&ocd, b"not really OCAD").unwrap();
    add_orthophoto()
        .arg(&ocd)
        .assert()
        .code(2)
        .stderr(predicates::str::contains("not a Mapper .omap/.xmap file"));
}

/// No place on the earth: nowhere to download from, and the map is left as
/// it was.
#[test]
fn a_map_not_on_the_earth_is_turned_away_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let map = dir.path().join("local.xmap");
    std::fs::copy("tests/data/shapes.xmap", &map).unwrap();
    let before = std::fs::read(&map).unwrap();
    add_orthophoto()
        .arg(&map)
        .assert()
        .code(3)
        .stderr(predicates::str::contains("not georeferenced"));
    assert_eq!(std::fs::read(&map).unwrap(), before);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

/// Georeferenced, but with nothing on it to download a photo of.
#[test]
fn an_empty_map_has_nothing_to_download() {
    let dir = tempfile::tempdir().unwrap();
    let map = dir.path().join("empty.xmap");
    std::fs::write(
        &map,
        r#"<?xml version="1.0" encoding="UTF-8"?>
<map xmlns="http://openorienteering.org/apps/mapper/xml/v2" version="9">
<georeferencing scale="4000"><projected_crs id="UTM"><spec language="PROJ.4">+proj=utm +datum=WGS84 +zone=32</spec><parameter>32</parameter><ref_point x="682364" y="4952128"/></projected_crs><geographic_crs id="Geographic coordinates"><spec language="PROJ.4">+proj=latlong +datum=WGS84</spec><ref_point_deg lat="44.69" lon="11.29"/></geographic_crs></georeferencing>
<colors count="0"></colors>
<parts count="1" current="0"><part name="default"><objects count="0"></objects></part></parts>
</map>"#,
    )
    .unwrap();
    add_orthophoto()
        .arg(&map)
        .assert()
        .code(3)
        .stderr(predicates::str::contains("no objects"));
}

/// In the middle of the Atlantic, no service is worth asking: a warning,
/// exit code 5, nothing written -- and no network needed to find that out.
#[test]
fn a_map_no_service_covers_is_a_warning() {
    let dir = tempfile::tempdir().unwrap();
    let map = dir.path().join("atlantic.xmap");
    std::fs::write(&map, shapes_at(40.0, -40.0)).unwrap();
    let before = std::fs::read(&map).unwrap();
    add_orthophoto()
        .arg(&map)
        .assert()
        .code(5)
        .stderr(predicates::str::contains(
            "Warning: none of the orthophoto services",
        ));
    assert_eq!(std::fs::read(&map).unwrap(), before);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}
