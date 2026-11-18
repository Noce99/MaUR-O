//! A run of `mauro net train` for each model, start to finish, and the run
//! read back.
//!
//! Small enough to take seconds, and there for what a unit test cannot reach:
//! the learner burn drives, with the real optimizer, scheduler, checkpointer
//! and metrics. Whether the model's parameters track gradients, whether the
//! second step can still reach what the first one updated, and whether a
//! checkpoint is written and read back are properties of that whole, and each
//! of them has broken a run which every unit test passed.

use std::path::{Path, PathBuf};

use assert_cmd::Command;

/// Three opaque areas, two of which fill with a pattern which turns.
const SYMBOL_SET: &str = "tests/data/turning_patterns.xmap";

fn mauro() -> Command {
    Command::cargo_bin("mauro").unwrap()
}

/// A dataset of four small maps, with their labels.
fn dataset(into: &Path) -> PathBuf {
    let folder = into.join("dataset");
    mauro()
        .args(["net", "dataset", SYMBOL_SET])
        .arg(&folder)
        .args(["-n", "4", "-l", "2", "-c", "20", "-j", "-r", "2", "-f", "5"])
        .assert()
        .success();
    folder
}

/// The one run folder under `trainings`, whose name starts with `prefix`.
fn run_of(trainings: &Path, prefix: &str) -> PathBuf {
    std::fs::read_dir(trainings)
        .expect("the trainings folder")
        .map(|entry| entry.expect("an entry").path())
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(prefix))
        })
        .unwrap_or_else(|| panic!("no run starting with {prefix} in {}", trainings.display()))
}

#[test]
fn a_unet_trains_checkpoints_and_reads_a_picture_back() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let folder = dataset(dir.path());
    let trainings = dir.path().join("trainings");

    mauro()
        .args(["net", "train"])
        .arg(&folder)
        .arg(&trainings)
        .args(["-e", "2", "-b", "2", "-c", "16", "--crops-per-map", "2"])
        .args(["--base-channels", "2", "-w", "1", "--image-valid", "1"])
        .assert()
        .success();

    let run = run_of(&trainings, "UNet_");
    assert!(run.join("best.bpk").is_file(), "no best.bpk in {run:?}");
    // Both epochs checkpointed: the weights, the optimizer and the schedule.
    for epoch in ["1", "2"] {
        for file in ["model.bpk", "optim.bpk", "scheduler.bpk"] {
            let path = run.join("checkpoint").join(epoch).join(file);
            assert!(path.is_file(), "missing {path:?}");
        }
    }
    assert!(run.join("train").join("2").is_dir());
    assert!(run.join("valid").join("2").is_dir());
    assert!(run.join("image_valid").join("2").is_dir());

    // The weights come back: a picture is read into a map with them.
    let picture = std::fs::read_dir(folder.join("images"))
        .expect("the images")
        .next()
        .expect("an image")
        .expect("an entry")
        .path();
    let map = dir.path().join("read.omap");
    mauro()
        .args(["net", "image-to-map"])
        .arg(&run)
        .arg(&picture)
        .arg(&map)
        .assert()
        .success();
    assert!(map.is_file());
}

#[test]
fn a_tree_trains_and_reads_a_picture_back() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let folder = dataset(dir.path());
    let trainings = dir.path().join("trainings");

    // Three steps an epoch: what failed was the second step of a run.
    mauro()
        .args(["net", "train"])
        .arg(&folder)
        .arg(&trainings)
        .args(["--model", "tree", "-e", "2", "-b", "1"])
        .args(["--size", "16", "--depth", "1", "--base-channels", "2"])
        .assert()
        .success();

    let run = run_of(&trainings, "TreeNet_");
    assert!(run.join("best.bpk").is_file(), "no best.bpk in {run:?}");

    let picture = std::fs::read_dir(folder.join("images"))
        .expect("the images")
        .next()
        .expect("an image")
        .expect("an entry")
        .path();
    let map = dir.path().join("read.omap");
    mauro()
        .args(["net", "image-to-map"])
        .arg(&run)
        .arg(&picture)
        .arg(&map)
        .assert()
        .success();
    assert!(map.is_file());
}

/// A dataset drawn over a range of densities gives the U-Net a scale head:
/// the run says so in its configuration, reports the Scale error, and
/// `image-to-map` asks it how dense a picture is when it is not told — and
/// does not when it is.
#[test]
fn a_unet_on_a_range_of_densities_learns_to_say_how_dense_a_picture_is() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let folder = dir.path().join("dataset");
    mauro()
        .args(["net", "dataset", SYMBOL_SET])
        .arg(&folder)
        .args(["-n", "4", "-l", "2", "-c", "20", "-j", "-r", "2", "-f", "5"])
        .args(["--min-resolution", "1", "--max-resolution", "4"])
        .assert()
        .success();
    let trainings = dir.path().join("trainings");

    mauro()
        .args(["net", "train"])
        .arg(&folder)
        .arg(&trainings)
        .args(["-e", "1", "-b", "2", "-c", "16", "--crops-per-map", "2"])
        .args(["--base-channels", "2", "-w", "1", "--image-valid", "1"])
        .assert()
        .success()
        .stdout(predicates::str::contains("scale head"));

    let run = run_of(&trainings, "UNet_");
    let config = std::fs::read_to_string(run.join("training.json")).expect("the configuration");
    assert!(config.contains("\"scale_head\": true"), "{config}");
    for split in ["train", "valid"] {
        let log = run.join(split).join("1").join("Scale_error.log");
        assert!(log.is_file(), "no scale error in {log:?}");
    }

    let picture = folder.join("images").join("map_001.png");
    let map = dir.path().join("read.omap");
    mauro()
        .args(["net", "image-to-map"])
        .arg(&run)
        .arg(&picture)
        .arg(&map)
        .assert()
        .success()
        .stdout(predicates::str::contains("scale head"));
    assert!(map.is_file());

    // Told a density it was trained over, it reads the picture as it is.
    mauro()
        .args(["net", "image-to-map"])
        .arg(&run)
        .arg(&picture)
        .arg(&map)
        .args(["--resolution", "4"])
        .assert()
        .success()
        .stdout(predicates::str::contains("from --resolution"))
        .stdout(predicates::str::contains("read as it is"));

    // Told one past it, it resamples to the nearer end and no further: the
    // 100 pixels at 8 px/m are 50 at the 4 it was trained up to.
    mauro()
        .args(["net", "image-to-map"])
        .arg(&run)
        .arg(&picture)
        .arg(&map)
        .args(["--resolution", "8"])
        .assert()
        .success()
        .stdout(predicates::str::contains("resampled to 50x50"));
}
