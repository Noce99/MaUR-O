//! The checks on `generate_maps_dataset`'s command line interface: that it
//! writes the folder of maps it was asked for, that the maps are maps, that
//! the images and the labels beside them describe those maps, and that the
//! same options give the same dataset twice running.
//!
//! Driven through the built binary rather than through the library, like the
//! other tools' tests: the exit codes are documented and scripted against,
//! and what the tool prints about a symbol set is how a person finds out
//! which symbols they have to work with.

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use maur_o::dataset::{GROUND_TRUTH_FOLDER, IMAGES_FOLDER, MAPS_FOLDER};
use maur_o::ground_truth::{GroundTruth, BACKGROUND, NO_ROTATION};

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

/// Drawing every map is most of the work, and most of these tests are about
/// what a map holds rather than what it looks like. The ones which are about
/// the images say so by leaving this off.
const NO_IMAGES: &str = "--no-images";

/// Turns the labels off without turning the images off with them.
const NO_GT: &str = "--no-gt";

fn generate() -> Command {
    Command::cargo_bin("generate_maps_dataset").unwrap()
}

/// The n-th map of a dataset, where the tool puts it.
fn map_at(folder: &Path, n: usize) -> PathBuf {
    folder.join(MAPS_FOLDER).join(format!("map_{n:03}.omap"))
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
        .arg(NO_IMAGES)
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
        let map = folder.join(MAPS_FOLDER).join(name);
        assert!(map.is_file(), "{} is missing", map.display());
        assert_eq!(drawn(&map).len(), 16, "{}", map.display());
    }
    assert!(!map_at(&folder, 4).exists());
}

/// What the three steps after the ground put on a map: a line along the
/// sides which were not left empty, a see-through area over the cells which
/// drew one, and the point symbols scattered into them.
#[test]
fn every_step_after_the_ground_draws_what_it_was_asked_for() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("dataset");
    generate()
        .arg(NO_IMAGES)
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

    let objects = drawn(&map_at(&folder, 1));
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
            .arg(NO_IMAGES)
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
        .arg(NO_IMAGES)
        .arg(SYMBOL_SET)
        .arg(&folder)
        .args(["--maps=1"])
        .args(ONLY_THE_GROUND)
        .assert()
        .success();

    let objects = drawn(&map_at(&folder, 1));
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
        .arg(NO_IMAGES)
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

    let objects = drawn(&map_at(&folder, 1));
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
        .arg(NO_IMAGES)
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
        let objects = drawn(&map_at(&folder, name));
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
        .arg(NO_IMAGES)
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
        let cells = drawn(&map_at(&folder, name));
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
            .arg(folder.join(MAPS_FOLDER).join(name))
            .arg(dir.path().join(format!("{name}.png")))
            .args(["--resolution=2", "--frame=5"])
            .assert()
            .success()
    };

    generate()
        .arg(NO_IMAGES)
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
        .arg(NO_IMAGES)
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
        .arg(NO_IMAGES)
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

/// A dataset is the three folders and the file naming what the labels mean,
/// and the three files of one map share a name.
#[test]
fn a_dataset_is_maps_images_and_labels_under_one_name() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("dataset");
    generate()
        .arg(SYMBOL_SET)
        .arg(&folder)
        // Small enough to draw quickly: a 40 m square of ground with 5 m of
        // frame, one pixel to the meter.
        .args(["--maps=2", "--layout-size=2", "--background-cell-size=20"])
        .args(["--resolution=1", "--frame=5", "--just-opaque-areas"])
        .assert()
        .success()
        .stdout(predicates::str::contains("maps/ 2 maps written"))
        .stdout(predicates::str::contains(
            "images/ 2 images of 50 by 50 pixels, at 1 px/m with a 5 m frame",
        ))
        .stdout(predicates::str::contains(
            "gt/ 2 labels of 50 by 50 by 3 (1 classes, then the sine and the cosine of the \
             pattern angle)",
        ));

    for n in 1..=2 {
        for (sub, suffix) in [
            (MAPS_FOLDER, "omap"),
            (IMAGES_FOLDER, "png"),
            (GROUND_TRUTH_FOLDER, "bin"),
        ] {
            let file = folder.join(sub).join(format!("map_{n:03}.{suffix}"));
            assert!(file.is_file(), "{} is missing", file.display());
        }
    }
    let classes = std::fs::read_to_string(folder.join("classes.json")).expect("classes.json");
    // The one opaque area of this set, and the sine and the cosine after it.
    assert!(classes.contains(r#""channels": 3"#), "{classes}");
    assert!(classes.contains(r#""sin_channel": 1"#), "{classes}");
    assert!(classes.contains(r#""cos_channel": 2"#), "{classes}");
    assert!(classes.contains(r#""name": "Open land""#), "{classes}");
}

/// A label is a label of the image beside it: the same pixels, the class of
/// the ground under each one, and the background where the frame is.
#[test]
fn a_label_describes_the_image_beside_it() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("dataset");
    generate()
        .arg(TURNING_PATTERNS)
        .arg(&folder)
        .args(["--maps=3", "--layout-size=2", "--background-cell-size=20"])
        .args(["--resolution=2", "--frame=5", "--just-opaque-areas"])
        .assert()
        .success();

    for n in 1..=3 {
        let truth = GroundTruth::read(
            &folder
                .join(GROUND_TRUTH_FOLDER)
                .join(format!("map_{n:03}.bin")),
        )
        .unwrap_or_else(|e| panic!("map_{n:03}.bin: {e}"));
        // A 40 m square with 5 m on each side, two pixels to the meter.
        assert_eq!((truth.height, truth.width), (100, 100));
        // Three opaque areas in this set, and the sine and the cosine after
        // them.
        assert_eq!(truth.classes, 3);
        assert_eq!(truth.channels(), 5);
        assert_eq!(truth.class_of.len(), 100 * 100);

        // The corner is frame, and the middle of the image is not.
        assert_eq!(truth.class_of[0], BACKGROUND, "map_{n:03}: the corner");
        let centre = truth.class_of[50 * 100 + 50];
        assert!(centre < 3, "map_{n:03}: the middle is class {centre}");

        // Every class is one this set holds, and every angle is an angle or
        // no angle at all.
        assert!(truth
            .class_of
            .iter()
            .all(|&class| class == BACKGROUND || class < 3));
        assert!(truth
            .rotation
            .iter()
            .all(|&turn| turn == NO_ROTATION || (0.0..1.0).contains(&turn)));

        // "Open land" is class 0 and has no pattern to turn, so wherever it
        // is the ground there is no angle — and the frame has none either.
        // Both come out of the tensor as the zero vector rather than as a
        // point on the circle, which is what tells them from a real angle.
        for (at, &class) in truth.class_of.iter().enumerate() {
            if class == 0 || class == BACKGROUND {
                assert_eq!(
                    truth.rotation[at], NO_ROTATION,
                    "map_{n:03}: class {class} was turned",
                );
                assert_eq!(truth.sin_cos(at), (0.0, 0.0));
            } else {
                // A pattern which turns is a point on the unit circle.
                let (sin, cos) = truth.sin_cos(at);
                assert!(
                    (sin * sin + cos * cos - 1.0).abs() < 1e-6,
                    "map_{n:03}: ({sin}, {cos}) is off the circle",
                );
            }
        }
    }
}

/// The angle a label carries is the angle the map was drawn at, and the
/// sine and the cosine it goes into the tensor as are that same angle.
#[test]
fn a_label_carries_the_angle_its_ground_was_turned_to() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("dataset");
    generate()
        .arg(TURNING_PATTERNS)
        .arg(&folder)
        // One cell per map, so a map has one ground and one angle.
        .args(["--maps=10", "--layout-size=1", "--background-cell-size=40"])
        .args(["--resolution=2", "--frame=5", "--just-opaque-areas"])
        .assert()
        .success();

    let mut turned = 0;
    for n in 1..=10 {
        let cell = &drawn(&map_at(&folder, n))[0];
        let head = &cell[..cell.find('>').unwrap()];
        // "Open land" is the one fill of this set with no pattern to turn,
        // and it is the one the file gives no rotation at all.
        let written = attribute(head, "rotation");

        let truth = GroundTruth::read(
            &folder
                .join(GROUND_TRUTH_FOLDER)
                .join(format!("map_{n:03}.bin")),
        )
        .unwrap_or_else(|e| panic!("map_{n:03}.bin: {e}"));
        // The middle of the image is the middle of the map's only cell.
        let at = 50 * 100 + 50;
        assert_eq!(
            truth.class_of[at].to_string(),
            attribute(head, "symbol"),
            "map_{n:03}: the label disagrees with the map about the ground",
        );

        let (sin, cos) = truth.sin_cos(at);
        if written.is_empty() {
            // Nothing to turn: no angle, which is the zero vector.
            assert_eq!(truth.rotation[at], NO_ROTATION, "map_{n:03}");
            assert_eq!((sin, cos), (0.0, 0.0), "map_{n:03}");
            continue;
        }

        let angle: f64 = written.parse().expect("a rotation is a number");
        let share = angle / std::f64::consts::TAU;
        assert!(
            (truth.rotation[at] as f64 - share).abs() < 1e-6,
            "map_{n:03}: {} labelled for an angle of {angle}",
            truth.rotation[at],
        );
        // And the pair the tensor carries is that same angle, back out of
        // the circle it was put on.
        let back = (sin as f64)
            .atan2(cos as f64)
            .rem_euclid(std::f64::consts::TAU);
        assert!(
            (back - angle.rem_euclid(std::f64::consts::TAU)).abs() < 1e-5,
            "map_{n:03}: ({sin}, {cos}) is {back}, not {angle}",
        );
        turned += 1;
    }
    // Two of the three fills turn, so most of ten maps were turned.
    assert!(turned > 2, "{turned} of ten maps turned");
}

/// A map with something drawn over the ground has no answer to write down,
/// and says so rather than writing a label which is wrong about the pixels a
/// line covered.
#[test]
fn a_map_which_is_not_just_ground_is_not_labelled() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("dataset");
    generate()
        .arg(SYMBOL_SET)
        .arg(&folder)
        .args(["--maps=1", "--layout-size=2", "--background-cell-size=20"])
        .args(["--resolution=1", "--frame=5"])
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "gt/ nothing: a pixel's label is the one piece of ground cover under it",
        ));

    assert!(folder.join(IMAGES_FOLDER).join("map_001.png").is_file());
    assert!(!folder.join(GROUND_TRUTH_FOLDER).exists());
    assert!(!folder.join("classes.json").exists());
}

/// Asking for no images leaves the maps alone, and takes the labels with it:
/// there is nothing left for them to be labels of.
#[test]
fn no_images_writes_the_maps_and_nothing_else() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("dataset");
    generate()
        .arg(NO_IMAGES)
        .arg(SYMBOL_SET)
        .arg(&folder)
        .args(["--maps=1", "--just-opaque-areas"])
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "images/ nothing: --no-images was asked for",
        ));

    assert!(map_at(&folder, 1).is_file());
    assert!(!folder.join(IMAGES_FOLDER).exists());
    assert!(!folder.join(GROUND_TRUTH_FOLDER).exists());
}

/// Asking for no gt leaves the maps and their images alone, and drops only
/// the labels: somebody after the images with no use for the answers on
/// disk. classes.json still goes out, since training reads it to compute a
/// label from a map in maps/ instead of from gt/.
#[test]
fn no_gt_writes_the_maps_and_images_but_not_the_labels() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("dataset");
    generate()
        .arg(NO_GT)
        .arg(SYMBOL_SET)
        .arg(&folder)
        .args(["--maps=1", "--just-opaque-areas"])
        .assert()
        .success()
        .stdout(predicates::str::contains("gt/ nothing: --no-gt was asked for"));

    assert!(map_at(&folder, 1).is_file());
    assert!(folder.join(IMAGES_FOLDER).join("map_001.png").is_file());
    assert!(!folder.join(GROUND_TRUTH_FOLDER).exists());
    assert!(folder.join("classes.json").is_file());
}

/// An image and its labels are the same size whatever landed on the map, so
/// a folder of them stacks into a batch as it is.
#[test]
fn every_image_of_a_dataset_is_the_same_size() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("dataset");
    generate()
        .arg(SYMBOL_SET)
        .arg(&folder)
        .args(["--maps=4", "--layout-size=2", "--background-cell-size=20"])
        .args(["--resolution=2", "--frame=5", "--just-opaque-areas"])
        .assert()
        .success();

    for n in 1..=4 {
        let png = std::fs::read(folder.join(IMAGES_FOLDER).join(format!("map_{n:03}.png")))
            .expect("an image");
        // A PNG's IHDR is the first chunk, its width and height big-endian.
        let size = |at: usize| u32::from_be_bytes(png[at..at + 4].try_into().unwrap());
        // 40 m of layout and 5 on each side, two pixels to the meter.
        assert_eq!((size(16), size(20)), (100, 100), "map_{n:03}.png");
    }
}

/// The whole point of seeding the generator by hand: a dataset can be
/// generated again, byte for byte.
#[test]
fn the_same_seed_gives_the_same_maps() {
    let dir = tempfile::tempdir().unwrap();
    let map = |folder: &str, seed: &str| {
        let into = dir.path().join(folder);
        generate()
            .arg(NO_IMAGES)
            .arg(SYMBOL_SET)
            .arg(&into)
            .args(["--maps=1", seed])
            .assert()
            .success();
        std::fs::read_to_string(map_at(&into, 1)).unwrap()
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
            .arg(NO_IMAGES)
            .arg(SYMBOL_SET)
            .arg(&into)
            .arg(maps)
            .assert()
            .success();
        std::fs::read_to_string(map_at(&into, 2)).unwrap()
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
        .arg(NO_IMAGES)
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
        .arg(NO_IMAGES)
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
            .arg(NO_IMAGES)
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
            .arg(NO_IMAGES)
            .arg(SYMBOL_SET)
            .arg(dir.path().join("dataset"))
            .arg(option)
            .assert()
            .code(1)
            .stderr(predicates::str::contains("between zero and one"));
    }
}
