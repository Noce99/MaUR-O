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

/// A map with no symbols at all, so nothing a cell could be filled with.
const NO_SYMBOLS: &str = "tests/data/empty.xmap";

/// Three opaque areas, two of which fill with a pattern that turns with the
/// object it is drawn on: "Open land" (id 0) has nothing to turn, while
/// "Rough open land with scattered trees" (1) and "with undergrowth" (2) do.
const TURNING_PATTERNS: &str = "tests/data/turning_patterns.xmap";

fn generate() -> Command {
    Command::cargo_bin("generate_maps_dataset").unwrap()
}

/// The value `name` is given in `text`, or "" where it is not given at all.
/// The name carries no quote of its own, so it matches an attribute and an
/// element alike: `symbol`, or `<pattern rotation`.
fn attribute(text: &str, name: &str) -> String {
    let key = format!("{name}=\"");
    match text.find(&key) {
        Some(at) => {
            let rest = &text[at + key.len()..];
            rest[..rest.find('"').expect("an attribute is closed")].to_string()
        }
        None => String::new(),
    }
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
        .args(["--maps=3", "--layout-size=4"])
        .assert()
        .success()
        .stdout(predicates::str::contains("3 maps of 4 by 4 cells"))
        // Four cells of the default 30 m.
        .stdout(predicates::str::contains("120 by 120 meters"))
        .stdout(predicates::str::contains("filled from 1 opaque area"));

    for name in ["map_001.omap", "map_002.omap", "map_003.omap"] {
        let map = folder.join(name);
        assert!(map.is_file(), "{} is missing", map.display());
        assert_eq!(objects(&map), 16, "{}", map.display());
    }
    assert!(!folder.join("map_004.omap").exists());
}

/// Every cell is filled with an opaque area symbol, and with nothing else:
/// the one opaque area of this set is "Open land", id 0, and the line, point
/// and text symbols are not what a piece of ground is made of.
#[test]
fn every_cell_is_filled_with_an_opaque_area() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("dataset");
    generate()
        .arg(SYMBOL_SET)
        .arg(&folder)
        .args(["--maps=1"])
        .assert()
        .success();

    let map = std::fs::read_to_string(folder.join("map_001.omap")).unwrap();
    assert_eq!(map.matches("<object ").count(), 9);
    assert_eq!(map.matches("symbol=\"0\"").count(), 9);
}

/// A fill whose pattern turns is given an angle of its own, and one whose
/// pattern is fixed is left alone: a rotation on an object which cannot use
/// one would be a number Mapper writes nowhere.
#[test]
fn a_fill_is_turned_only_where_its_pattern_turns() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("dataset");
    generate()
        .arg(TURNING_PATTERNS)
        .arg(&folder)
        .args(["--maps=8", "--layout-size=5"])
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "2 of them fill with a pattern which turns",
        ));

    let mut turned = 0;
    for name in 1..=8 {
        let map = std::fs::read_to_string(folder.join(format!("map_00{name}.omap"))).unwrap();
        let cells: Vec<&str> = map.split("<object ").skip(1).collect();
        assert_eq!(cells.len(), 25, "map_00{name}.omap");
        for cell in cells {
            let head = &cell[..cell.find('>').unwrap()];
            // A rotation is carried twice, as Mapper carries it: once as the
            // object's own attribute and once on its <pattern>.
            let rotation = attribute(head, "rotation");
            let pattern_rotation = attribute(cell, "<pattern rotation");
            let turns = attribute(head, "symbol") != "0";
            if turns {
                // Every angle of a whole turn is allowed; what would mean
                // the draw never happened is no angle at all.
                let angle: f64 = rotation.parse().unwrap_or(0.0);
                assert!((0.0..std::f64::consts::TAU).contains(&angle), "{angle}");
                assert_eq!(rotation, pattern_rotation, "the two rotations disagree");
                turned += 1;
            } else {
                assert_eq!(rotation, "", "a fixed pattern was given a rotation");
                assert_eq!(pattern_rotation, "0", "a fixed pattern was turned");
            }
        }
    }
    // Two of the three fills turn, so across 200 cells a good many did.
    assert!(turned > 50, "{turned} cells turned");
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
        .args(["--maps=1"])
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
        // side. An area fill ends where its outline is, so the extent is the
        // square itself.
        .stdout(predicates::str::contains(
            "100x100 meters, map scale 1:10000",
        ));
}

#[test]
fn the_symbol_set_is_reported_by_kind() {
    let dir = tempfile::tempdir().unwrap();
    generate()
        .arg(SYMBOL_SET)
        .arg(dir.path().join("dataset"))
        .args(["--maps=1"])
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
            .args(["--maps=1", seed])
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
            .arg(maps)
            .assert()
            .success();
        std::fs::read_to_string(into.join("map_002.omap")).unwrap()
    };
    assert_eq!(map("small", "--maps=2"), map("large", "--maps=20"));
}

/// A set with nothing to cover the ground with cannot be generated from, and
/// says so rather than writing a folder of empty maps.
#[test]
fn a_set_with_no_opaque_area_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("dataset");
    generate()
        .arg(NO_SYMBOLS)
        .arg(&folder)
        .args(["--maps=1"])
        .assert()
        .code(2)
        .stderr(predicates::str::contains("no opaque area symbol"));
    assert!(!folder.exists());
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
