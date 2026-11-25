//! `mauro terrain`'s command line: the exit codes it gives before it ever
//! reaches the network.
//!
//! Everything past that point talks to an elevation server, which a test
//! suite has no business depending on. What can be pinned offline is that a
//! map the tool cannot place, or that no service covers, or too large an
//! area, is turned away with the documented code and nothing written.

use assert_cmd::Command;

fn terrain() -> Command {
    let mut command = Command::cargo_bin("mauro").unwrap();
    command.args(["terrain"]);
    command
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
    terrain()
        .arg("--list-services")
        .assert()
        .success()
        .stdout(predicates::str::contains("it-er-lidar-2023-24"))
        .stdout(predicates::str::contains("it-tinitaly"));
}

#[test]
fn a_map_is_needed() {
    terrain().assert().code(1);
}

#[test]
fn an_unknown_service_is_a_usage_error() {
    terrain()
        .args(["--service", "atlantis-1999", "tests/data/empty.xmap"])
        .assert()
        .code(1)
        .stderr(predicates::str::contains("--list-services"));
}

#[test]
fn a_pixel_size_must_be_positive() {
    terrain()
        .args(["--pixel-size", "0", "tests/data/empty.xmap"])
        .assert()
        .code(1)
        .stderr(predicates::str::contains("--pixel-size"));
}

#[test]
fn a_margin_cannot_be_negative() {
    terrain()
        .args(["--margin=-5", "tests/data/empty.xmap"])
        .assert()
        .code(1)
        .stderr(predicates::str::contains("--margin"));
}

#[test]
fn a_missing_map_cannot_be_read() {
    terrain()
        .arg("tests/data/no_such_map.omap")
        .assert()
        .code(2)
        .stderr(predicates::str::contains("cannot read"));
}

/// No projected CRS: nowhere to download from, and nothing written.
#[test]
fn a_map_not_on_the_earth_is_turned_away() {
    let dir = tempfile::tempdir().unwrap();
    let map = dir.path().join("local.xmap");
    std::fs::copy("tests/data/shapes.xmap", &map).unwrap();
    terrain()
        .arg(&map)
        .assert()
        .code(3)
        .stderr(predicates::str::contains("not georeferenced"));
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

/// In the middle of the Atlantic, no service is worth asking: a warning,
/// exit code 5, nothing written -- and no network needed to find that out.
#[test]
fn a_map_no_service_covers_is_a_warning() {
    let dir = tempfile::tempdir().unwrap();
    let map = dir.path().join("atlantic.xmap");
    std::fs::write(&map, shapes_at(40.0, -40.0)).unwrap();
    terrain()
        .arg(&map)
        .assert()
        .code(5)
        .stderr(predicates::str::contains(
            "Warning: none of the elevation services",
        ));
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

/// Too many pixels at the service's own resolution: turned away with a
/// pixel size that fits, before any server is asked.
#[test]
fn too_large_an_area_names_a_pixel_size_that_fits() {
    let dir = tempfile::tempdir().unwrap();
    let map = dir.path().join("bologna.xmap");
    std::fs::write(&map, shapes_at(44.69, 11.29)).unwrap();
    terrain()
        .args(["--service", "it-er-lidar-2023-24", "--margin", "5000"])
        .arg(&map)
        .assert()
        .code(1)
        .stderr(predicates::str::contains("give --pixel-size"));
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}
