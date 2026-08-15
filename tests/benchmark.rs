//! End to end tests for the `benchmark` tool: an archive goes in, a run
//! folder comes out. The archives are built here rather than checked in, so
//! that a test says in one place what it puts in and what it expects back.

use std::io::Write;
use std::path::{Path, PathBuf};

use assert_cmd::Command;

fn benchmark() -> Command {
    Command::cargo_bin("benchmark").unwrap()
}

/// Writes a zip holding `maps/` and `expected/` under `root`, from
/// (name, contents) pairs.
fn archive(path: &Path, root: &str, maps: &[(&str, Vec<u8>)], expected: &[(&str, Vec<u8>)]) {
    let file = std::fs::File::create(path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default();
    for (folder, files) in [("maps", maps), ("expected", expected)] {
        for (name, contents) in files {
            zip.start_file(format!("{root}{folder}/{name}"), options).unwrap();
            zip.write_all(contents).unwrap();
        }
    }
    zip.finish().unwrap();
}

/// The single run folder inside a results folder.
fn run_folder(results: &Path) -> PathBuf {
    let mut runs: Vec<PathBuf> = std::fs::read_dir(results)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(runs.len(), 1, "expected exactly one run folder in {}", results.display());
    runs.pop().unwrap()
}

/// Renders a map the way the run will, to use its output as a reference image.
fn render(map: &str) -> Vec<u8> {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("reference.png");
    Command::cargo_bin("map_to_image")
        .unwrap()
        .arg(map)
        .arg(&out)
        .assert()
        .success();
    std::fs::read(out).unwrap()
}

fn map(name: &str) -> Vec<u8> {
    std::fs::read(name).unwrap()
}

/// A reference image which is not what the renderer produces, so that the
/// pair is guaranteed to differ.
fn wrong_image() -> Vec<u8> {
    let mut png = Vec::new();
    image::RgbImage::from_pixel(64, 64, image::Rgb([0, 128, 0]))
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
    png
}

#[test]
fn an_archive_which_follows_the_rules_is_run_as_it_is() {
    let dir = tempfile::tempdir().unwrap();
    let zip = dir.path().join("suite.zip");
    archive(
        &zip,
        "",
        &[("000__empty.xmap", map("tests/data/empty.xmap"))],
        &[("000__empty.png", render("tests/data/empty.xmap"))],
    );

    let results = dir.path().join("Results");
    benchmark()
        .arg("--results")
        .arg(&results)
        .arg(&zip)
        .assert()
        .success()
        .stdout(predicates::str::contains("names are as they should be"))
        .stdout(predicates::str::contains("1 image: 1 identical, 0 antialiasing only, 0 differing"));

    assert!(!zip.with_file_name("suite_corrected.zip").exists(), "nothing to correct");

    let run = run_folder(&results);
    assert!(run.file_name().unwrap().to_string_lossy().starts_with("suite_"));
    assert!(run.join("predictions/000__empty.png").is_file());
    // An identical pair gets no report of its own, but the folder is there.
    assert!(run.join("differences").is_dir());
    assert_eq!(std::fs::read_dir(run.join("differences")).unwrap().count(), 0);
    // Nothing was wrong with the names, so there is nothing to write about them.
    assert!(!run.join("naming.txt").exists());

    let results_txt = std::fs::read_to_string(run.join("results.txt")).unwrap();
    let row = results_txt
        .lines()
        .find(|line| line.contains("000__empty"))
        .unwrap_or_else(|| panic!("no row for the map in:\n{results_txt}"));
    // An identical pair: real, antialiasing, wrong, largest, mean, name.
    assert_eq!(
        row.split_whitespace().collect::<Vec<_>>(),
        ["0.0000%", "0.0000%", "0.0000%", "0", "n/a", "000__empty"]
    );

    // The settings are recorded whether or not anything went wrong. Matched
    // without their column padding, which is as wide as the longest name.
    let info = std::fs::read_to_string(run.join("info.txt")).unwrap();
    let setting = |name: &str, value: &str| {
        let wanted = format!("{name} {value}");
        assert!(
            info.lines().any(|line| line.split_whitespace().collect::<Vec<_>>().join(" ") == wanted),
            "no setting line {wanted:?} in:\n{info}"
        );
    };
    setting("resolution", "3 pixels per meter on the ground");
    setting("tolerance", "3");
    setting("crop size", "128 pixels");
    setting("antialiasing", "classified and set aside");
}

/// Also the archive-under-a-top-level-folder case, which is how a folder
/// zipped up by a file manager arrives.
#[test]
fn a_differing_pair_gets_the_whole_report() {
    let dir = tempfile::tempdir().unwrap();
    let zip = dir.path().join("suite.zip");
    archive(
        &zip,
        "suite/",
        &[("000__empty.xmap", map("tests/data/empty.xmap"))],
        &[("000__empty.png", wrong_image())],
    );

    let results = dir.path().join("Results");
    benchmark()
        .arg("--results")
        .arg(&results)
        .args(["--crops", "2"])
        .arg(&zip)
        .assert()
        .success()
        .stdout(predicates::str::contains("1 image: 0 identical, 0 antialiasing only, 1 differing"));

    let run = run_folder(&results);
    // The map is a white square and the reference a small green one, so every
    // measure of the row is non-zero.
    let results_txt = std::fs::read_to_string(run.join("results.txt")).unwrap();
    let row: Vec<String> = results_txt
        .lines()
        .find(|line| line.contains("000__empty"))
        .unwrap_or_else(|| panic!("no row for the map in:\n{results_txt}"))
        .split_whitespace()
        .map(str::to_string)
        .collect();
    // real, antialiasing, wrong, largest, mean, ±, sd, name.
    assert_eq!(row.len(), 8, "{row:?}");
    // The two images are different sizes, so most of it is real.
    assert!(row[0].ends_with('%') && row[0] != "0.0000%", "{row:?}");
    assert!(row[2].ends_with('%') && row[2] != "0.0000%", "{row:?}");
    assert!(row[3].parse::<i32>().unwrap() > 0, "{row:?}");
    // The mean is over the wrong pixels only, so it cannot be below the
    // tolerance the pixels had to clear to be counted.
    assert!(row[4].parse::<f64>().unwrap() > 0.0, "{row:?}");
    assert_eq!(row[5], "±", "{row:?}");
    assert_eq!(row[7], "000__empty");

    let report = run.join("differences/000__empty");
    assert!(report.join("diff.png").is_file());
    assert!(report.join("side_by_side.png").is_file());
    let crops: Vec<String> = std::fs::read_dir(&report)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
        .filter(|name| name.starts_with("crop_"))
        .collect();
    assert_eq!(crops.len(), 2, "{crops:?}");
    // The crop names carry the corner of the region in the image.
    assert!(crops.iter().all(|name| name.contains('x') && name.ends_with(".png")), "{crops:?}");
}

#[test]
fn broken_names_are_reported_and_written_out_corrected() {
    let dir = tempfile::tempdir().unwrap();
    let zip = dir.path().join("suite.zip");
    // Sorted as text this is 1, 10, 2; the ordinals have to be read as
    // numbers for the order to survive, and the archive is under a top level
    // folder, which the corrected copy has to keep.
    archive(
        &zip,
        "suite/",
        &[
            ("10_c.xmap", map("tests/data/empty.xmap")),
            ("1_a.xmap", map("tests/data/empty.xmap")),
            ("2_b map.xmap", map("tests/data/empty.xmap")),
        ],
        &[
            ("10_c.png", wrong_image()),
            ("1_a.png", wrong_image()),
            ("2_b map.png", wrong_image()),
        ],
    );

    let results = dir.path().join("Results");
    benchmark()
        .arg("--results")
        .arg(&results)
        .arg("--names-only")
        .arg(&zip)
        .assert()
        .success()
        // One line on screen, no matter how many problems there were.
        .stdout(predicates::str::contains("6 problems found and corrected"))
        .stdout(predicates::str::contains("naming.txt"));

    // The detail is all in the file, and nothing was rendered.
    let run = run_folder(&results);
    let naming = std::fs::read_to_string(run.join("naming.txt")).unwrap();
    for expected in [
        "maps/1_a.xmap -> maps/000__a.xmap",
        "maps/2_b map.xmap -> maps/001__b_map.xmap",
        "maps/10_c.xmap -> maps/002__c.xmap",
        "contains spaces",
        "leading number out of sequence",
    ] {
        assert!(naming.contains(expected), "{expected:?} missing from:\n{naming}");
    }
    assert!(!run.join("predictions").exists());
    assert!(!run.join("results.txt").exists());

    let corrected = dir.path().join("suite_corrected.zip");
    assert!(corrected.is_file());

    // The corrected archive keeps the top level folder and passes the check.
    let names: Vec<String> = {
        let mut zip = zip::ZipArchive::new(std::fs::File::open(&corrected).unwrap()).unwrap();
        (0..zip.len()).map(|i| zip.by_index(i).unwrap().name().to_string()).collect()
    };
    assert!(names.contains(&"suite/maps/000__a.xmap".to_string()), "{names:?}");
    assert!(names.contains(&"suite/expected/001__b_map.png".to_string()), "{names:?}");

    benchmark()
        .arg("--names-only")
        .arg(&corrected)
        .assert()
        .success()
        .stdout(predicates::str::contains("3 maps, names are as they should be"));
}

#[test]
fn an_archive_without_maps_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let zip = dir.path().join("empty.zip");
    archive(&zip, "", &[], &[("000__a.png", wrong_image())]);
    benchmark()
        .arg(&zip)
        .assert()
        .failure()
        .stderr(predicates::str::contains("holds no maps/*.omap"));
}
