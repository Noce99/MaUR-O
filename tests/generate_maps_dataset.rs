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

/// Two grounds and two overlays, arranged so that the overlays show up on
/// "Open land" (id 0) and are covered up by "Bare rock" (id 1). The marsh is
/// id 2, the boulder id 3 and the path id 4.
const OVER_AND_UNDER: &str = "tests/data/over_and_under.xmap";

/// Nothing but the ground: no lines along the sides, nothing over the cells.
const ONLY_THE_GROUND: [&str; 3] = [
    "--empty-sides=1",
    "--transparent-areas=0",
    "--point-symbols=0",
];

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

/// Everything drawn on the map, as the `<object ...>` tag of each and the
/// text up to the next one.
///
/// Only what is drawn: the symbol set the file carries holds objects of its
/// own — the elements a point symbol is built out of — and those are in the
/// symbol table rather than on the map.
fn drawn(path: &Path) -> Vec<String> {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    let parts = text.find("<parts").expect("a map has parts");
    text[parts..]
        .split("<object ")
        .skip(1)
        .map(|chunk| chunk.to_string())
        .collect()
}

/// The drawn objects of the given type: 0 a point, 1 a path.
fn drawn_of_type(objects: &[String], kind: &str) -> Vec<String> {
    objects
        .iter()
        .filter(|object| attribute(object, "type") == kind)
        .cloned()
        .collect()
}

#[test]
fn a_dataset_is_one_map_per_ask_and_one_fill_per_cell() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("dataset");
    generate()
        .arg(SYMBOL_SET)
        .arg(&folder)
        .args(["--maps=3", "--layout-size=4"])
        .args(ONLY_THE_GROUND)
        .assert()
        .success()
        .stdout(predicates::str::contains("3 maps of 4 by 4 cells"))
        // Four cells of the default 150 m.
        .stdout(predicates::str::contains("600 by 600 meters"))
        .stdout(predicates::str::contains("filled from 1 opaque area"))
        .stdout(predicates::str::contains(
            "48 fills, 0 lines, 0 transparent areas, 0 point symbols drawn",
        ));

    for name in ["map_001.omap", "map_002.omap", "map_003.omap"] {
        let map = folder.join(name);
        assert!(map.is_file(), "{} is missing", map.display());
        assert_eq!(drawn(&map).len(), 16, "{}", map.display());
    }
    assert!(!folder.join("map_004.omap").exists());
}

/// What the three steps after the ground put on a map: a line along the
/// sides which were not left empty, a see-through area over the cells which
/// drew one, and the point symbols scattered into them.
#[test]
fn every_step_after_the_ground_draws_what_it_was_asked_for() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("dataset");
    generate()
        .arg(SYMBOL_SET)
        .arg(&folder)
        .args(["--maps=1", "--layout-size=3"])
        // A line on every side, a transparent area over every cell, and a
        // point symbol in every cell.
        .args([
            "--empty-sides=0",
            "--transparent-areas=1",
            "--point-symbols=1",
        ])
        .assert()
        .success();

    let objects = drawn(&folder.join("map_001.omap"));
    let paths = drawn_of_type(&objects, "1");
    let points = drawn_of_type(&objects, "0");
    // Nine fills, nine areas over them, and a line along each of the two by
    // three by four sides of a three by three layout.
    assert_eq!(paths.len(), 9 + 9 + 24);
    let with_symbol = |id: &str| {
        paths
            .iter()
            .filter(|object| attribute(object, "symbol") == id)
            .count()
    };
    assert_eq!(with_symbol("0"), 9, "the opaque areas");
    assert_eq!(with_symbol("1"), 9, "the transparent areas");
    assert_eq!(with_symbol("2"), 24, "the lines");
    // At least one point symbol per cell, and every one of them the only
    // point symbol the set holds.
    assert!(points.len() >= 9, "{} point symbols", points.len());
    assert!(points
        .iter()
        .all(|object| attribute(object, "symbol") == "3"));
}

/// The share of sides left empty is what it says it is.
#[test]
fn the_sides_left_empty_are_left_empty() {
    let dir = tempfile::tempdir().unwrap();
    for (empty, lines) in [("0", 24), ("1", 0)] {
        let folder = dir.path().join(format!("dataset{empty}"));
        generate()
            .arg(SYMBOL_SET)
            .arg(&folder)
            .args(["--maps=1", "--layout-size=3", "--transparent-areas=0"])
            .args([format!("--empty-sides={empty}"), "--point-symbols=0".into()])
            .assert()
            .success()
            .stdout(predicates::str::contains(format!("9 fills, {lines} lines")));
    }
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
        .args(ONLY_THE_GROUND)
        .assert()
        .success();

    let objects = drawn(&folder.join("map_001.omap"));
    assert_eq!(objects.len(), 9);
    assert!(objects
        .iter()
        .all(|object| attribute(object, "symbol") == "0"));
}

/// Asking for the ground alone leaves the ground alone: the three steps over
/// it are skipped however much of them was asked for.
#[test]
fn just_the_opaque_areas_skips_everything_drawn_over_them() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("dataset");
    generate()
        .arg(SYMBOL_SET)
        .arg(&folder)
        .args(["--maps=1", "--layout-size=3", "--just-opaque-areas"])
        // A line on every side, an area over every cell and a point symbol in
        // every cell — all of which the flag overrides.
        .args([
            "--empty-sides=0",
            "--transparent-areas=1",
            "--point-symbols=1",
        ])
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "9 fills, 0 lines, 0 transparent areas, 0 point symbols drawn \
             (just the opaque areas: nothing was drawn over the ground)",
        ));

    let objects = drawn(&folder.join("map_001.omap"));
    assert_eq!(objects.len(), 9);
    assert!(objects
        .iter()
        .all(|object| attribute(object, "symbol") == "0"));
}

/// Nothing is drawn where it would not be seen. The overlays of this set
/// draw in a colour above one ground and below the other, so a cell filled
/// with the second is left bare however much is asked for.
#[test]
fn an_overlay_is_drawn_only_where_it_shows_up() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("dataset");
    generate()
        .arg(OVER_AND_UNDER)
        .arg(&folder)
        // One cell per map, so the ground of a map is the fill of its only
        // path object, and everything else on it was drawn over that.
        .args(["--maps=12", "--layout-size=1", "--empty-sides=1"])
        .args(["--transparent-areas=1", "--point-symbols=1"])
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "1 of 2 transparent areas over a fill show up",
        ))
        .stdout(predicates::str::contains(
            "1 of 2 point symbols over a fill show up",
        ));

    let (mut open, mut rock) = (0, 0);
    for name in 1..=12 {
        let objects = drawn(&folder.join(format!("map_{name:03}.omap")));
        let ground = attribute(&objects[0], "symbol");
        let overlays: Vec<String> = objects[1..]
            .iter()
            .map(|object| attribute(object, "symbol"))
            .collect();
        if ground == "0" {
            // Open land: the marsh over it, and a boulder or two on it.
            open += 1;
            assert_eq!(overlays[0], "2", "map_{name:03}.omap");
            assert!(overlays[1..].iter().all(|symbol| symbol == "3"));
        } else {
            // Bare rock covers both of them, so neither was drawn.
            rock += 1;
            assert!(overlays.is_empty(), "map_{name:03}.omap: {overlays:?}");
        }
    }
    // Both grounds came up, so both halves of that were really tried.
    assert!(open > 0 && rock > 0, "{open} open land, {rock} bare rock");
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
        let cells = drawn(&folder.join(format!("map_00{name}.omap")));
        assert_eq!(cells.len(), 25, "map_00{name}.omap");
        for cell in cells {
            let head = &cell[..cell.find('>').unwrap()];
            // A rotation is carried twice, as Mapper carries it: once as the
            // object's own attribute and once on its <pattern>.
            let rotation = attribute(head, "rotation");
            let pattern_rotation = attribute(&cell, "<pattern rotation");
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
    let render = |name: &str| {
        Command::cargo_bin("map_to_image")
            .unwrap()
            .arg(folder.join(name))
            .arg(dir.path().join(format!("{name}.png")))
            .args(["--resolution=2", "--frame=5"])
            .assert()
            .success()
    };

    generate()
        .arg(SYMBOL_SET)
        .arg(&folder)
        .args(["--maps=2"])
        .args(ONLY_THE_GROUND)
        .assert()
        .success();
    // The 450 m square of the default layout, plus the 5 m frame on each
    // side. An area fill ends where its outline is, so with nothing but the
    // ground on it the extent is the square itself.
    render("map_001.omap").stdout(predicates::str::contains(
        "460x460 meters, map scale 1:10000",
    ));

    // And with everything else on it, which reaches past the square: a line
    // along the edge is half its width outside it, and so is a point symbol
    // dropped next to it.
    generate()
        .arg(SYMBOL_SET)
        .arg(&folder)
        .args(["--maps=2", "--empty-sides=0", "--point-symbols=1"])
        .assert()
        .success();
    render("map_002.omap").stdout(predicates::str::contains("map scale 1:10000"));
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

/// A share is a share: what is asked for outside `[0, 1]` is a mistake, not
/// a dataset with a strange number of lines on it.
#[test]
fn a_share_outside_zero_to_one_is_a_usage_error() {
    let dir = tempfile::tempdir().unwrap();
    for option in [
        "--empty-sides=1.5",
        "--empty-sides=-0.1",
        "--transparent-areas=2",
        "--point-symbols=-1",
    ] {
        generate()
            .arg(SYMBOL_SET)
            .arg(dir.path().join("dataset"))
            .arg(option)
            .assert()
            .code(1)
            .stderr(predicates::str::contains("between zero and one"));
    }
}
