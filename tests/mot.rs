//! `mauro tiles build` and `mauro tiles query`: that a map goes into a MOT archive and
//! comes out of a query for the place it is, and that both exit with the
//! documented code when they cannot.
//!
//! Driven through the built binaries, like `cli.rs`, because the exit codes
//! and the messages are the contract. The map is made by `mauro osm` from the
//! sample OpenStreetMap extract, drawn to ISOM, so no file of ours holds one.

use assert_cmd::Command;
use predicates::prelude::*;

mod common;
use common::{make_archive, make_map, make_map_in, HERE};

#[test]
fn a_map_goes_into_an_archive_and_a_query_finds_it() {
    let dir = tempfile::tempdir().unwrap();
    let archive = make_archive(dir.path());
    assert!(std::fs::metadata(&archive).unwrap().len() > 0);

    Command::cargo_bin("mauro")
        .unwrap()
        .args(["tiles", "query"])
        .args(["--around", HERE, "--size", "300m"])
        .arg(&archive)
        .assert()
        .success()
        .stdout(predicate::str::contains("objects in the area"))
        .stdout(predicate::str::contains("Building"))
        .stdout(predicate::str::contains("in the coverage of sample.omap"));
}

#[test]
fn a_query_far_from_the_map_finds_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let archive = make_archive(dir.path());
    Command::cargo_bin("mauro")
        .unwrap()
        .args(["tiles", "query"])
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
    Command::cargo_bin("mauro")
        .unwrap()
        .args(["tiles", "query"])
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
        let out = Command::cargo_bin("mauro")
            .unwrap()
            .args(["tiles", "query"])
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
fn a_level_too_coarse_for_any_symbol_has_only_the_coverage_and_says_whose() {
    let dir = tempfile::tempdir().unwrap();
    let archive = make_archive(dir.path());
    // At zoom 9 no symbol has appeared, but the map's area is there.
    Command::cargo_bin("mauro")
        .unwrap()
        .args(["tiles", "query"])
        .args(["--around", HERE, "--zoom", "9"])
        .arg(&archive)
        .assert()
        .success()
        .stdout(predicate::str::contains("0 objects in the area"))
        .stdout(predicate::str::contains("in the coverage of sample.omap"));
    // And where it is not, the archive says it is not mapped.
    Command::cargo_bin("mauro")
        .unwrap()
        .args(["tiles", "query"])
        .args(["--around", "10,10", "--zoom", "9"])
        .arg(&archive)
        .assert()
        .success()
        .stdout(predicate::str::contains("in no map's coverage"));
}

#[test]
fn a_zoom_the_archive_does_not_have_is_exit_1() {
    let dir = tempfile::tempdir().unwrap();
    let archive = make_archive(dir.path());
    Command::cargo_bin("mauro")
        .unwrap()
        .args(["tiles", "query"])
        .args(["--around", HERE, "--zoom", "15"])
        .arg(&archive)
        .assert()
        .code(1)
        .stderr(predicate::str::contains("zoom levels 0 to 14"));
    Command::cargo_bin("mauro")
        .unwrap()
        .args(["tiles", "build"])
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
fn maps_of_two_standards_make_two_archives_each_in_its_own_symbols() {
    let dir = tempfile::tempdir().unwrap();
    let (forest, sprint) = (
        make_map_in(dir.path(), "isom"),
        make_map_in(dir.path(), "issprom"),
    );
    let out = dir.path().join("world.pmtiles");
    Command::cargo_bin("mauro")
        .unwrap()
        .args(["tiles", "build"])
        .arg("--output")
        .arg(&out)
        .arg(&forest)
        .arg(&sprint)
        .assert()
        .success()
        .stderr(
            predicate::str::contains("ISSprOM 2019").and(predicate::str::contains("ISOM 2017-2")),
        );
    assert!(
        !out.exists(),
        "with two standards the plain name is not written"
    );
    for file in ["world.isom.pmtiles", "world.issprom.pmtiles"] {
        let archive = dir.path().join(file);
        assert!(archive.exists(), "{file}");
        // Each is read with its own symbol table.
        Command::cargo_bin("mauro")
            .unwrap()
            .args(["tiles", "query"])
            .args(["--around", HERE, "--size", "300m"])
            .arg(&archive)
            .assert()
            .success()
            .stdout(predicate::str::contains("Building"));
    }
}

#[test]
fn a_standard_can_be_asked_for() {
    let dir = tempfile::tempdir().unwrap();
    let map = make_map_in(dir.path(), "isom");
    let out = dir.path().join("sprint.pmtiles");
    Command::cargo_bin("mauro")
        .unwrap()
        .args(["tiles", "build"])
        .args(["--standard", "issprom", "--output"])
        .arg(&out)
        .arg(&map)
        .assert()
        .success()
        .stderr(predicate::str::contains("(ISSprOM 2019)"));
    assert!(out.exists());
}

#[test]
fn a_map_without_a_place_is_skipped_and_with_nothing_left_it_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let archive = dir.path().join("out.pmtiles");
    Command::cargo_bin("mauro")
        .unwrap()
        .args(["tiles", "build"])
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
    Command::cargo_bin("mauro")
        .unwrap()
        .args(["tiles", "build"])
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
    Command::cargo_bin("mauro")
        .unwrap()
        .args(["tiles", "build"])
        .arg("--output")
        .arg(dir.path().join("no_such_dir").join("out.pmtiles"))
        .arg(&map)
        .assert()
        .code(4);
}

#[test]
fn usage_errors_are_exit_1() {
    Command::cargo_bin("mauro")
        .unwrap()
        .args(["tiles", "build"])
        .assert()
        .code(1);
    Command::cargo_bin("mauro")
        .unwrap()
        .args(["tiles", "build"])
        .args(["--output", "x", "--zoom", "30", "m.omap"])
        .assert()
        .code(1);
    let dir = tempfile::tempdir().unwrap();
    let archive = make_archive(dir.path());
    Command::cargo_bin("mauro")
        .unwrap()
        .args(["tiles", "query"])
        .arg(&archive)
        .assert()
        .code(1)
        .stderr(predicate::str::contains("--around or --bbox"));
    Command::cargo_bin("mauro")
        .unwrap()
        .args(["tiles", "query"])
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
    Command::cargo_bin("mauro")
        .unwrap()
        .args(["tiles", "query"])
        .args(["--around", HERE])
        .arg(&junk)
        .assert()
        .code(2);
}
