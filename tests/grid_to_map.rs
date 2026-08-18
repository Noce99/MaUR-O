//! The checks on `grid_to_map`'s command line interface: that a dataset's
//! labels come back out of it as a map, that the map is drawn with the
//! symbols those labels name, and that it says so when it is handed a grid
//! which was not written for the ground it was told about.
//!
//! Driven through the built binary rather than through the library, like the
//! other tools' tests: the exit codes are documented and scripted against.
//! The geometry the tool is built on is tested where it lives, in
//! `maur_o::vectorize`.

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use maur_o::dataset::GROUND_TRUTH_FOLDER;
use maur_o::xml_reader::read_xml_map;

/// Three opaque areas, two of which fill with a pattern that turns with the
/// object it is drawn on.
const SYMBOL_SET: &str = "tests/data/turning_patterns.xmap";

/// A layout small enough that generating and drawing one is quick, and the
/// options `grid_to_map` has to be told about it.
const SMALL: [&str; 4] = [
    "--layout-size=2",
    "--background-cell-size=40",
    "--frame=10",
    "--resolution=1",
];

fn generate() -> Command {
    Command::cargo_bin("generate_maps_dataset").unwrap()
}

fn grid_to_map() -> Command {
    Command::cargo_bin("grid_to_map").unwrap()
}

/// A dataset of one labelled map in a temporary folder, and the labels of it.
fn labelled(folder: &Path) -> PathBuf {
    generate()
        .args([SYMBOL_SET, folder.to_str().expect("a utf-8 path")])
        .args(SMALL)
        .args(["-n", "1", "--just-opaque-areas"])
        .assert()
        .success();
    folder.join(GROUND_TRUTH_FOLDER).join("map_001.bin")
}

#[test]
fn labels_come_back_out_as_a_map_of_the_symbols_they_name() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let labels = labelled(dir.path());
    let map_path = dir.path().join("back.omap");

    grid_to_map()
        .args([
            SYMBOL_SET,
            labels.to_str().expect("a utf-8 path"),
            map_path.to_str().expect("a utf-8 path"),
        ])
        .args(SMALL)
        .assert()
        .success()
        .stdout(predicates::str::contains("objects"));

    let (mut map, _) = read_xml_map(&map_path).expect("the map it wrote is a map");
    map.resolve_references();
    assert!(
        !map.objects.is_empty(),
        "a grid of ground cover is some objects"
    );
    // Every object is an area drawn with a symbol of the set it was given.
    for object in &map.objects {
        assert!(
            object.symbol_index.is_some(),
            "an object is drawn with symbol {}, which is in no symbol table",
            object.symbol_id
        );
        assert!(object.coords.len() >= 4, "an area is more than a point");
    }
}

/// Without a map file to write to, the labels' own name with a map's suffix.
#[test]
fn the_map_goes_beside_the_labels_by_default() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let labels = labelled(dir.path());

    grid_to_map()
        .args([SYMBOL_SET, labels.to_str().expect("a utf-8 path")])
        .args(SMALL)
        .assert()
        .success();
    assert!(labels.with_extension("omap").is_file());
}

/// A tolerance takes nodes off the staircase a boundary is, and the tool says
/// how many coordinates it came to either way.
#[test]
fn a_tolerance_leaves_fewer_coordinates() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let labels = labelled(dir.path());

    let coordinates = |tolerance: &str| {
        let out = grid_to_map()
            .args([
                SYMBOL_SET,
                labels.to_str().expect("a utf-8 path"),
                dir.path()
                    .join(format!("t{tolerance}.omap"))
                    .to_str()
                    .expect("a utf-8 path"),
            ])
            .args(SMALL)
            .arg(format!("--tolerance={tolerance}"))
            .assert()
            .success();
        let printed = String::from_utf8(out.get_output().stdout.clone()).expect("utf-8");
        let (before, _) = printed
            .split_once(" coordinates")
            .expect("the tool says how many coordinates");
        before
            .rsplit(", ")
            .next()
            .expect("a number before the comma")
            .parse::<usize>()
            .expect("a number")
    };
    assert!(coordinates("3") < coordinates("0"));
}

/// The ground a cell covers is not in a labels file, so being told the wrong
/// ground is a usage error rather than a map nobody asked for.
#[test]
fn labels_of_another_size_are_refused() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let labels = labelled(dir.path());

    grid_to_map()
        .args([
            SYMBOL_SET,
            labels.to_str().expect("a utf-8 path"),
            dir.path().join("back.omap").to_str().expect("a utf-8 path"),
        ])
        // The dataset was two cells of forty meters; this is three.
        .args(["--layout-size=3", "--background-cell-size=40"])
        .args(["--frame=10", "--resolution=1"])
        .assert()
        .code(1)
        .stderr(predicates::str::contains("cells"));
}

#[test]
fn a_labels_file_which_is_not_one_is_refused() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let labels = dir.path().join("not_labels.bin");
    std::fs::write(&labels, b"not a tensor").expect("a file to hand it");

    grid_to_map()
        .args([SYMBOL_SET, labels.to_str().expect("a utf-8 path")])
        .args(SMALL)
        .assert()
        .code(2);
}
