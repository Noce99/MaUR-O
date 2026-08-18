//! The checks on `image_to_map`'s command line interface: that a run folder
//! which cannot say what its answers mean is refused, and refused with the
//! name of the file which is missing.
//!
//! What the tool does when the folder *is* complete is not tested here.
//! Weights only exist once something has trained, and training is minutes on
//! the pure Rust backend where a test suite has milliseconds; the geometry
//! behind the tool is tested in `maur_o::vectorize`, and the tiling and the
//! reloading in `maur_o::net::predict`, both without a trained network.

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use maur_o::dataset::CLASSES_FILE;

fn image_to_map() -> Command {
    Command::cargo_bin("image_to_map").unwrap()
}

/// A one-pixel PNG in `run`, which is a picture as far as the argument goes:
/// nothing in these tests gets as far as reading it.
fn picture(run: &Path) -> PathBuf {
    let at = run.join("map.png");
    image::RgbImage::new(1, 1).save(&at).expect("a picture");
    at
}

/// A `classes.json` as `generate_maps_dataset` writes one, naming `set` as
/// the symbol set beside it.
fn notes(run: &Path, set: Option<&str>) {
    let mut json = String::from("{\n  \"format\": \"MAUROGT2\",\n");
    if let Some(set) = set {
        json.push_str(&format!("  \"symbol_set\": \"{set}\",\n"));
    }
    json.push_str(
        "  \"classes\": 3,\n  \"image_size\": 100,\n  \"resolution\": 3,\n  \"frame\": 50,\n  \
         \"cell_size\": 150,\n  \"layout_size\": 3\n}\n",
    );
    std::fs::write(run.join(CLASSES_FILE), json).expect("the notes");
}

#[test]
fn a_run_folder_without_the_dataset_notes_is_refused() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let image = picture(dir.path());

    image_to_map()
        .args([dir.path(), image.as_path()])
        .assert()
        .code(2)
        .stderr(predicates::str::contains(CLASSES_FILE));
}

#[test]
fn a_run_folder_whose_notes_name_no_symbol_set_is_refused() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let image = picture(dir.path());
    notes(dir.path(), None);

    image_to_map()
        .args([dir.path(), image.as_path()])
        .assert()
        .code(2)
        .stderr(predicates::str::contains("names no symbol set"));
}

/// The notes may name a set which is not there, which is a folder somebody
/// moved half of.
#[test]
fn a_symbol_set_which_is_not_there_is_refused() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let image = picture(dir.path());
    notes(dir.path(), Some("ISOM_10k.omap"));

    image_to_map()
        .args([dir.path(), image.as_path()])
        .assert()
        .code(2)
        .stderr(predicates::str::contains("ISOM_10k.omap"));
}

#[test]
fn a_negative_tolerance_is_a_usage_error() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let image = picture(dir.path());

    image_to_map()
        .args([dir.path(), image.as_path()])
        .arg("--tolerance=-1")
        .assert()
        .code(1)
        .stderr(predicates::str::contains("--tolerance"));
}
