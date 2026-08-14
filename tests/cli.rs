//! Ports the quick CLI tests from `tests/CMakeLists.txt`: the tool is
//! tested through its command line interface, keeping the tests free of
//! any test framework dependency beyond `assert_cmd`.

use assert_cmd::Command;

fn map_to_image() -> Command {
    Command::cargo_bin("map_to_image").unwrap()
}

/// An empty map with the default options is a white square of 100 by 100 meters.
#[test]
fn empty_map() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("empty.png");
    map_to_image()
        .arg("tests/data/empty.xmap")
        .arg(&out)
        .assert()
        .success()
        .stdout(predicates::str::contains("300x300 pixels, 100x100 meters"));
}

/// The size of an empty map depends on the frame and the resolution only.
#[test]
fn empty_map_options() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("empty2.png");
    map_to_image()
        .args(["--resolution=2", "--frame=25"])
        .arg("tests/data/empty.xmap")
        .arg(&out)
        .assert()
        .success()
        .stdout(predicates::str::contains("100x100 pixels, 50x50 meters"));
}

/// A map with one object of every type, at a scale of 1:10000.
#[test]
fn shapes() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("shapes.png");
    map_to_image()
        .arg("--frame=10")
        .arg("tests/data/shapes.xmap")
        .arg(&out)
        .assert()
        .success()
        .stdout(predicates::str::contains("map scale 1:10000"));
}

#[test]
fn missing_file() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("missing.png");
    map_to_image()
        .arg("tests/data/does-not-exist.xmap")
        .arg(&out)
        .assert()
        .failure();
}

#[test]
fn invalid_resolution() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("invalid.png");
    map_to_image()
        .arg("--resolution=nonsense")
        .arg("tests/data/empty.xmap")
        .arg(&out)
        .assert()
        .failure();
}
