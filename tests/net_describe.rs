//! `mauro net describe`'s command line: what it writes, when it writes
//! nothing, and the exit codes `--check` gives (which CI and the git hook
//! rely on).

use assert_cmd::Command;
use predicates::str::contains;

fn describe(folder: &std::path::Path) -> Command {
    let mut command = Command::cargo_bin("mauro").unwrap();
    command.args(["net", "describe", "--networks-dir"]);
    command.arg(folder);
    command
}

#[test]
fn the_first_run_writes_version_0_1_of_every_network_and_the_second_nothing() {
    let dir = tempfile::tempdir().unwrap();
    describe(dir.path())
        .assert()
        .success()
        .stdout(contains("unet: wrote version 0.1"))
        .stdout(contains("tree: wrote version 0.1"));
    for file in [
        "image_to_omap/unet/0.1.json",
        "image_to_omap/tree/0.1.json",
        "index.json",
    ] {
        assert!(dir.path().join(file).is_file(), "{file}");
    }

    describe(dir.path())
        .assert()
        .success()
        .stdout(contains("unet: up to date (0.1)"))
        .stdout(contains("tree: up to date (0.1)"));
    assert!(!dir.path().join("image_to_omap/unet/0.2.json").exists());
}

#[test]
fn check_is_silent_about_files_and_exits_zero_when_up_to_date() {
    let dir = tempfile::tempdir().unwrap();
    describe(dir.path()).assert().success();
    describe(dir.path())
        .arg("--check")
        .assert()
        .success()
        .stdout(contains("up to date"));
}

#[test]
fn check_fails_with_3_on_an_empty_folder_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("networks");
    describe(&folder)
        .arg("--check")
        .assert()
        .code(3)
        .stdout(contains("no version yet"))
        .stdout(contains("mauro net describe"));
    assert!(!folder.exists());
}

#[test]
fn check_fails_with_3_when_a_version_was_altered() {
    let dir = tempfile::tempdir().unwrap();
    describe(dir.path()).assert().success();
    let path = dir.path().join("image_to_omap/tree/0.1.json");
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, text.replace("\"depth\": 4", "\"depth\": 5")).unwrap();

    describe(dir.path())
        .arg("--check")
        .assert()
        .code(3)
        .stdout(contains("changed after it was registered"));
}

#[test]
fn stdout_prints_one_descriptor_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("networks");
    let out = describe(&folder)
        .args(["tree", "--stdout"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(json["name"], "tree");
    assert_eq!(json["scope"], "image_to_omap");
    assert!(json["root"]["children"].as_array().unwrap().len() >= 3);
    assert!(!folder.exists());
}

#[test]
fn stdout_without_a_network_is_a_usage_error() {
    let dir = tempfile::tempdir().unwrap();
    describe(dir.path()).arg("--stdout").assert().code(1);
}

#[test]
fn check_and_stdout_exclude_each_other() {
    let dir = tempfile::tempdir().unwrap();
    describe(dir.path())
        .args(["tree", "--check", "--stdout"])
        .assert()
        .code(1);
}

/// The folder this repository keeps is what the code builds now. This is the
/// same check CI runs, as a test, so that `cargo test` finds a forgotten
/// `mauro net describe` before a push does.
#[test]
fn the_repositorys_own_networks_folder_is_up_to_date() {
    let mut command = Command::cargo_bin("mauro").unwrap();
    command
        .args(["net", "describe", "--check", "--networks-dir"])
        .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("networks"))
        .assert()
        .success();
}
