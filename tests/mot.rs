//! `omap_to_tiles` and `tiles_query`: that a map goes into a MOT archive and
//! comes out of a query for the place it is, and that both exit with the
//! documented code when they cannot.
//!
//! Driven through the built binaries, like `cli.rs`, because the exit codes
//! and the messages are the contract. The map is made by `osm_to_omap` from the
//! sample OpenStreetMap extract, drawn to ISOM, so no file of ours holds one.

use assert_cmd::Command;
use predicates::prelude::*;
use std::path::Path;

const SAMPLE: &str = "tests/data/bologna_sample.osm";
const BBOX: &str = "--bbox=11.3415,44.4925,11.3445,44.4945";
/// A point inside that box.
const HERE: &str = "44.4935,11.3430";

fn make_map(dir: &Path) -> std::path::PathBuf {
    let map = dir.join("sample.omap");
    Command::cargo_bin("osm_to_omap")
        .unwrap()
        .args(["--standard", "isom", BBOX, SAMPLE])
        .arg(&map)
        .assert()
        .success();
    map
}

fn make_archive(dir: &Path) -> std::path::PathBuf {
    let map = make_map(dir);
    let archive = dir.join("sample.pmtiles");
    Command::cargo_bin("omap_to_tiles")
        .unwrap()
        .arg("--output")
        .arg(&archive)
        .arg(&map)
        .assert()
        .success();
    archive
}

#[test]
fn a_map_goes_into_an_archive_and_a_query_finds_it() {
    let dir = tempfile::tempdir().unwrap();
    let archive = make_archive(dir.path());
    assert!(std::fs::metadata(&archive).unwrap().len() > 0);

    Command::cargo_bin("tiles_query")
        .unwrap()
        .args(["--around", HERE, "--size", "300m"])
        .arg(&archive)
        .assert()
        .success()
        .stdout(predicate::str::contains("objects in the area"))
        .stdout(predicate::str::contains("Building"));
}

#[test]
fn a_query_far_from_the_map_finds_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let archive = make_archive(dir.path());
    Command::cargo_bin("tiles_query")
        .unwrap()
        .args(["--around", "10,10"])
        .arg(&archive)
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "0 tiles read at zoom 14, 0 objects",
        ));
}

#[test]
fn objects_can_be_listed() {
    let dir = tempfile::tempdir().unwrap();
    let archive = make_archive(dir.path());
    Command::cargo_bin("tiles_query")
        .unwrap()
        .args(["--around", HERE, "--size", "300m", "--objects"])
        .arg(&archive)
        .assert()
        .success()
        .stdout(predicate::str::contains("object 0:").or(predicate::str::contains("object 1:")));
}

#[test]
fn a_coarser_level_keeps_less_than_the_finest() {
    let dir = tempfile::tempdir().unwrap();
    let archive = make_archive(dir.path());
    let query = |zoom: &str| {
        let out = Command::cargo_bin("tiles_query")
            .unwrap()
            .args(["--around", HERE, "--size", "300m", "--zoom", zoom])
            .arg(&archive)
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        String::from_utf8(out).unwrap()
    };
    // A building is drawn from zoom 13 on, and at 14 everything is as drawn;
    // at 12 it is not in the archive's tiles at all.
    assert!(query("14").contains("Building"));
    assert!(!query("12").contains("Building"));
    assert!(query("11").contains("zoom 11"));
}

#[test]
fn a_zoom_the_archive_does_not_have_is_exit_1() {
    let dir = tempfile::tempdir().unwrap();
    let archive = make_archive(dir.path());
    Command::cargo_bin("tiles_query")
        .unwrap()
        .args(["--around", HERE, "--zoom", "9"])
        .arg(&archive)
        .assert()
        .code(1)
        .stderr(predicate::str::contains("zoom levels 11 to 14"));
    Command::cargo_bin("omap_to_tiles")
        .unwrap()
        .args([
            "--output",
            "x",
            "--zoom",
            "12",
            "--min-zoom",
            "13",
            "m.omap",
        ])
        .assert()
        .code(1);
}

#[test]
fn a_map_without_a_place_is_skipped_and_with_nothing_left_it_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let archive = dir.path().join("out.pmtiles");
    Command::cargo_bin("omap_to_tiles")
        .unwrap()
        .arg("--output")
        .arg(&archive)
        .arg("tests/data/shapes.xmap")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("no geographic reference point"));
    assert!(!archive.exists());
}

#[test]
fn an_unreadable_map_is_exit_2() {
    let dir = tempfile::tempdir().unwrap();
    Command::cargo_bin("omap_to_tiles")
        .unwrap()
        .arg("--output")
        .arg(dir.path().join("out.pmtiles"))
        .arg(dir.path().join("missing.omap"))
        .assert()
        .code(2)
        .stderr(predicate::str::contains("cannot read"));
}

#[test]
fn an_archive_that_cannot_be_written_is_exit_4() {
    let dir = tempfile::tempdir().unwrap();
    let map = make_map(dir.path());
    Command::cargo_bin("omap_to_tiles")
        .unwrap()
        .arg("--output")
        .arg(dir.path().join("no_such_dir").join("out.pmtiles"))
        .arg(&map)
        .assert()
        .code(4);
}

#[test]
fn usage_errors_are_exit_1() {
    Command::cargo_bin("omap_to_tiles")
        .unwrap()
        .assert()
        .code(1);
    Command::cargo_bin("omap_to_tiles")
        .unwrap()
        .args(["--output", "x", "--zoom", "30", "m.omap"])
        .assert()
        .code(1);
    let dir = tempfile::tempdir().unwrap();
    let archive = make_archive(dir.path());
    Command::cargo_bin("tiles_query")
        .unwrap()
        .arg(&archive)
        .assert()
        .code(1)
        .stderr(predicate::str::contains("--around or --bbox"));
    Command::cargo_bin("tiles_query")
        .unwrap()
        .args(["--around", "not a point"])
        .arg(&archive)
        .assert()
        .code(1);
}

#[test]
fn something_that_is_not_an_archive_is_exit_2() {
    let dir = tempfile::tempdir().unwrap();
    let junk = dir.path().join("junk.pmtiles");
    std::fs::write(&junk, b"not an archive").unwrap();
    Command::cargo_bin("tiles_query")
        .unwrap()
        .args(["--around", HERE])
        .arg(&junk)
        .assert()
        .code(2);
}
