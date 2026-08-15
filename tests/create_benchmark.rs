//! End to end tests for the `create_benchmark` tool: maps go in, an archive
//! `benchmark` can run comes out.
//!
//! The ground truth renderer is a stub script rather than the C++ tool the
//! real thing is pointed at: what is being tested here is what
//! `create_benchmark` does around a renderer — which maps it finds, what it
//! calls them, and what it leaves out — and a stub can be told to fail on
//! cue, which the real renderer cannot.

#![cfg(unix)]

use std::path::{Path, PathBuf};

use assert_cmd::Command;

fn create_benchmark() -> Command {
    Command::cargo_bin("create_benchmark").unwrap()
}

/// Writes an executable script and returns its path.
fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// A stand-in for the C++ `map_to_image`: it writes a file where the image
/// goes, and fails on any map whose name says it should.
fn stub_renderer(dir: &Path) -> PathBuf {
    script(
        dir,
        "stub_renderer",
        r#"#!/bin/sh
if [ "$1" = "--version" ]; then echo "stub_renderer 1.2.3"; exit 0; fi
case "$1" in *unrenderable*) echo "Error: Failed to load $1" >&2; exit 2;; esac
printf 'image of %s' "$1" > "$2"
"#,
    )
}

/// Writes a map file, making the folders above it as needed.
fn map(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

/// The names of everything in an archive, files and folders alike.
fn entries(archive: &Path) -> Vec<String> {
    let file = std::fs::File::open(archive).unwrap();
    let mut zip = zip::ZipArchive::new(file).unwrap();
    (0..zip.len()).map(|i| zip.by_index(i).unwrap().name().to_string()).collect()
}

/// What one file of an archive holds.
fn contents(archive: &Path, name: &str) -> String {
    use std::io::Read;
    let file = std::fs::File::open(archive).unwrap();
    let mut zip = zip::ZipArchive::new(file).unwrap();
    let mut text = String::new();
    zip.by_name(name).unwrap().read_to_string(&mut text).unwrap();
    text
}

#[test]
fn a_folder_becomes_an_archive_which_follows_the_naming_rules() {
    let dir = tempfile::tempdir().unwrap();
    let renderer = stub_renderer(dir.path());
    let source = dir.path().join("suite");
    map(&source, "02 second map.omap", "second");
    map(&source, "01 first map.omap", "first");
    map(&source, "deeper/10 third map.xmap", "third");
    map(&source, "deeper/notes.txt", "not a map");
    let archive = dir.path().join("suite.zip");

    create_benchmark().arg(&renderer).arg(&source).arg("-o").arg(&archive).assert().success();

    let names = entries(&archive);
    // The ordinals the maps came with order them, and are then handed out
    // afresh, padded and separated the way the rules ask for.
    for wanted in [
        "suite/maps/000__first_map.omap",
        "suite/maps/001__second_map.omap",
        "suite/maps/002__third_map.xmap",
        "suite/expected/000__first_map.png",
        "suite/expected/001__second_map.png",
        "suite/expected/002__third_map.png",
        "suite/info.txt",
    ] {
        assert!(names.contains(&wanted.to_string()), "{wanted} is not in {names:?}");
    }
    // A map is copied as it is, and its image is what the renderer wrote.
    assert_eq!(contents(&archive, "suite/maps/000__first_map.omap"), "first");
    assert!(contents(&archive, "suite/expected/002__third_map.png").ends_with("10 third map.xmap"));
    // Nothing which is not a map comes along.
    assert!(!names.iter().any(|name| name.ends_with("notes.txt")), "{names:?}");
}

#[test]
fn an_archive_nobody_named_lands_in_a_folder_of_its_own() {
    let dir = tempfile::tempdir().unwrap();
    let renderer = stub_renderer(dir.path());
    let source = dir.path().join("a suite");
    map(&source, "a.omap", "a");
    let here = dir.path().join("somewhere else");
    std::fs::create_dir(&here).unwrap();

    // Named after the source, in benchmarks/, which is made on the way.
    create_benchmark().current_dir(&here).arg(&renderer).arg(&source).assert().success();

    let archive = here.join("benchmarks").join("benchmark_a_suite.zip");
    assert!(archive.is_file(), "{} was not written", archive.display());
    assert!(entries(&archive).contains(&"benchmark_a_suite/maps/000__a.omap".to_string()));
}

#[test]
fn the_archive_it_writes_needs_no_correcting() {
    let dir = tempfile::tempdir().unwrap();
    let renderer = stub_renderer(dir.path());
    let source = dir.path().join("suite");
    for name in ["b map.omap", "a map.omap", "sub/c map.omap"] {
        map(&source, name, "map");
    }
    let archive = dir.path().join("suite.zip");
    create_benchmark().arg(&renderer).arg(&source).arg("-o").arg(&archive).assert().success();

    Command::cargo_bin("benchmark")
        .unwrap()
        .arg("--names-only")
        .arg(&archive)
        .assert()
        .success()
        .stdout(predicates::str::contains("names are as they should be"));
}

#[test]
fn a_map_which_cannot_be_rendered_leaves_no_hole_in_the_ordinals() {
    let dir = tempfile::tempdir().unwrap();
    let renderer = stub_renderer(dir.path());
    let source = dir.path().join("suite");
    map(&source, "a.omap", "a");
    map(&source, "unrenderable.omap", "b");
    map(&source, "z.omap", "c");
    let archive = dir.path().join("suite.zip");

    // Exit code 2: the archive was written, but not every map is in it.
    create_benchmark()
        .arg(&renderer)
        .arg(&source)
        .arg("-o")
        .arg(&archive)
        .assert()
        .code(2)
        .stdout(predicates::str::contains("FAILED to render unrenderable"));

    let names = entries(&archive);
    assert!(names.contains(&"suite/maps/000__a.omap".to_string()), "{names:?}");
    assert!(names.contains(&"suite/maps/001__z.omap".to_string()), "{names:?}");
    assert!(!names.iter().any(|name| name.contains("unrenderable")), "{names:?}");
    // info.txt is where a reader finds out what became of the third map.
    assert!(contents(&archive, "suite/info.txt").contains("unrenderable: Error: Failed to load"));
}

#[test]
fn two_maps_of_the_same_name_are_told_apart_by_their_folder() {
    let dir = tempfile::tempdir().unwrap();
    let renderer = stub_renderer(dir.path());
    let source = dir.path().join("suite");
    map(&source, "north/Contour.omap", "north");
    map(&source, "south/Contour.omap", "south");
    let archive = dir.path().join("suite.zip");

    create_benchmark().arg(&renderer).arg(&source).arg("-o").arg(&archive).assert().success();

    let names = entries(&archive);
    assert!(names.contains(&"suite/maps/000__Contour.omap".to_string()), "{names:?}");
    assert!(names.contains(&"suite/maps/001__south_Contour.omap".to_string()), "{names:?}");
    assert_eq!(contents(&archive, "suite/maps/001__south_Contour.omap"), "south");
}

#[test]
fn a_map_file_becomes_one_map_per_symbol_with_its_description() {
    let dir = tempfile::tempdir().unwrap();
    let renderer = stub_renderer(dir.path());
    let archive = dir.path().join("symbols.zip");

    create_benchmark()
        .arg(&renderer)
        .arg("tests/data/shapes.xmap")
        .arg("-o")
        .arg(&archive)
        .assert()
        .success();

    // The ordinal the generator gave a map is replaced by the archive's own,
    // which is the same order: the naming rules read the one it came with.
    let names = entries(&archive);
    for wanted in [
        "symbols/maps/000__area_401_Open_land.omap",
        "symbols/maps/002__line_505_Path.omap",
        "symbols/expected/002__line_505_Path.png",
        "symbols/index/002__line_505_Path.txt",
    ] {
        assert!(names.contains(&wanted.to_string()), "{wanted} is not in {names:?}");
    }
    assert!(contents(&archive, "symbols/info.txt").contains("one map per symbol"));
}

#[test]
fn the_settings_the_images_were_drawn_at_are_written_down() {
    let dir = tempfile::tempdir().unwrap();
    let renderer = stub_renderer(dir.path());
    let source = dir.path().join("suite");
    map(&source, "a.omap", "a");
    let archive = dir.path().join("suite.zip");

    create_benchmark()
        .arg(&renderer)
        .arg(&source)
        .arg("-o")
        .arg(&archive)
        .args(["-r", "5", "-f", "10"])
        .assert()
        .success();

    let info = contents(&archive, "suite/info.txt");
    assert!(info.contains("5 pixels per meter"), "{info}");
    assert!(info.contains("10 meters on the ground"), "{info}");
    assert!(info.contains("stub_renderer 1.2.3"), "{info}");

    // benchmark reads these two back out of the header, so the line has to
    // start with the key and a bare number, whatever it says after that.
    let starts_with = |key: &str, value: &str| {
        info.lines().any(|line| line.split_whitespace().take(2).collect::<Vec<_>>() == [key, value])
    };
    assert!(starts_with("resolution", "5"), "{info}");
    assert!(starts_with("frame", "10"), "{info}");
}

#[test]
fn an_archive_which_is_already_there_is_kept_unless_it_is_to_be_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let renderer = stub_renderer(dir.path());
    let source = dir.path().join("suite");
    map(&source, "a.omap", "a");
    let archive = dir.path().join("suite.zip");
    std::fs::write(&archive, "not an archive").unwrap();

    create_benchmark()
        .arg(&renderer)
        .arg(&source)
        .arg("-o")
        .arg(&archive)
        .assert()
        .code(1)
        .stderr(predicates::str::contains("--force replaces it"));
    assert_eq!(std::fs::read_to_string(&archive).unwrap(), "not an archive");

    create_benchmark().arg(&renderer).arg(&source).arg("-o").arg(&archive).arg("--force").assert().success();
    assert!(entries(&archive).contains(&"suite/maps/000__a.omap".to_string()));
}

#[test]
fn a_renderer_which_cannot_be_run_is_reported_before_anything_is_made() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("suite");
    map(&source, "a.omap", "a");
    let archive = dir.path().join("suite.zip");

    create_benchmark()
        .arg(dir.path().join("no_such_renderer"))
        .arg(&source)
        .arg("-o")
        .arg(&archive)
        .assert()
        .code(1)
        .stderr(predicates::str::contains("cannot run"));
    assert!(!archive.exists());
}
