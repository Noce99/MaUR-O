//! The checks on `generate_maps_dataset`'s command line interface: that it
//! writes the folder of maps it was asked for, that the maps are maps, and
//! that the same options give the same dataset twice running.
//!
//! Driven through the built binary rather than through the library, like the
//! other tools' tests: the exit codes are documented and scripted against,
//! and what the tool prints about a symbol set is how a person finds out
//! which symbols they have to work with.

use std::path::Path;

use assert_cmd::Command;

/// A small map with one symbol of every kind: an opaque area, an area which
/// is only a pattern, a line, a point symbol and a text symbol.
const SYMBOL_SET: &str = "tests/data/shapes.xmap";

fn generate() -> Command {
    Command::cargo_bin("generate_maps_dataset").unwrap()
}

/// How many objects a map file holds.
fn objects(path: &Path) -> usize {
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
        .matches("<object ")
        .count()
}

#[test]
fn a_dataset_is_one_map_per_ask_and_one_object_per_cell() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("dataset");
    generate()
        .arg(SYMBOL_SET)
        .arg(&folder)
        .args(["--maps=3", "--layout-size=4", "--border-symbol=Path"])
        .assert()
        .success()
        .stdout(predicates::str::contains("3 maps of 4 by 4 cells"))
        // Four cells of the default 30 m.
        .stdout(predicates::str::contains("120 by 120 meters"));

    for name in ["map_001.omap", "map_002.omap", "map_003.omap"] {
        let map = folder.join(name);
        assert!(map.is_file(), "{} is missing", map.display());
        assert_eq!(objects(&map), 16, "{}", map.display());
    }
    assert!(!folder.join("map_004.omap").exists());
}

/// The maps are maps: they render, with the symbol set and the scale of the
/// file they were generated from.
#[test]
fn a_generated_map_renders() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("dataset");
    generate()
        .arg(SYMBOL_SET)
        .arg(&folder)
        .args(["--maps=1", "--border-symbol=Path"])
        .assert()
        .success();

    Command::cargo_bin("map_to_image")
        .unwrap()
        .arg(folder.join("map_001.omap"))
        .arg(dir.path().join("map_001.png"))
        .args(["--resolution=2", "--frame=5"])
        .assert()
        .success()
        // The 90 m square of the default layout, plus the 5 m frame on each
        // side and the width of the line the outlines are drawn with.
        .stdout(predicates::str::contains(
            "103x103 meters, map scale 1:10000",
        ));
}

#[test]
fn the_symbol_set_is_reported_by_kind() {
    let dir = tempfile::tempdir().unwrap();
    generate()
        .arg(SYMBOL_SET)
        .arg(dir.path().join("dataset"))
        .args(["--maps=1", "--border-symbol=Path"])
        .assert()
        .success()
        .stdout(predicates::str::contains("5 symbols, map scale 1:10000"))
        .stdout(predicates::str::contains("opaque area          1"))
        .stdout(predicates::str::contains("transparent area     1"))
        .stdout(predicates::str::contains("line                 1"))
        .stdout(predicates::str::contains("point                1"))
        .stdout(predicates::str::contains("text                 1"));
}

/// The whole point of seeding the generator by hand: a dataset can be
/// generated again, byte for byte.
#[test]
fn the_same_seed_gives_the_same_maps() {
    let dir = tempfile::tempdir().unwrap();
    let map = |folder: &str, seed: &str| {
        let into = dir.path().join(folder);
        generate()
            .arg(SYMBOL_SET)
            .arg(&into)
            .args(["--maps=1", "--border-symbol=Path", seed])
            .assert()
            .success();
        std::fs::read_to_string(into.join("map_001.omap")).unwrap()
    };
    assert_eq!(map("one", "--seed=8"), map("again", "--seed=8"));
    assert_ne!(map("one", "--seed=8"), map("other", "--seed=9"));
}

/// A map keeps its shape however many maps were asked for, which is what
/// seeding the n-th map with `seed + n` buys.
#[test]
fn a_map_is_the_same_map_in_a_larger_dataset() {
    let dir = tempfile::tempdir().unwrap();
    let map = |folder: &str, maps: &str| {
        let into = dir.path().join(folder);
        generate()
            .arg(SYMBOL_SET)
            .arg(&into)
            .args([maps, "--border-symbol=Path"])
            .assert()
            .success();
        std::fs::read_to_string(into.join("map_002.omap")).unwrap()
    };
    assert_eq!(map("small", "--maps=2"), map("large", "--maps=20"));
}

#[test]
fn a_symbol_the_set_does_not_have_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    generate()
        .arg(SYMBOL_SET)
        .arg(dir.path().join("dataset"))
        .args(["--maps=1", "--border-symbol=Motorway"])
        .assert()
        .code(2)
        .stderr(predicates::str::contains("no symbol named \"Motorway\""));
}

#[test]
fn a_map_which_cannot_be_read_is_reported_before_anything_is_written() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("dataset");
    generate()
        .arg("tests/data/no_such_map.omap")
        .arg(&folder)
        .assert()
        .code(2);
    assert!(!folder.exists());
}

#[test]
fn a_layout_of_no_cells_is_a_usage_error() {
    let dir = tempfile::tempdir().unwrap();
    for option in ["--layout-size=0", "--background-cell-size=0", "--maps=0"] {
        generate()
            .arg(SYMBOL_SET)
            .arg(dir.path().join("dataset"))
            .arg(option)
            .assert()
            .code(1)
            .stderr(predicates::str::contains("greater than zero"));
    }
}
