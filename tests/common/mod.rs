//! What the MOT tests share: a small map, made by `mauro osm` from the sample
//! OpenStreetMap extract so that no file of ours holds one, and the archives made
//! from it.
//!
//! Each test file uses some of it, so what it leaves out is not dead.
#![allow(dead_code)]

use assert_cmd::Command;
use std::path::{Path, PathBuf};

/// The OpenStreetMap extract the maps are made from.
pub const SAMPLE: &str = "tests/data/bologna_sample.osm";
/// The part of it to convert: everything but the far-away node.
pub const BBOX: &str = "--bbox=11.3415,44.4925,11.3445,44.4945";
/// A point inside it, as `lat,lon`.
pub const HERE: &str = "44.4935,11.3430";
/// The same point as latitude and longitude.
pub const HERE_LAT_LON: (f64, f64) = (44.4935, 11.3430);

/// The sample map drawn to an `mauro osm` standard (`isom`, `issprom`), in `dir`.
pub fn make_map_in(dir: &Path, standard: &str) -> PathBuf {
    let map = dir.join(format!("{standard}.omap"));
    Command::cargo_bin("mauro")
        .unwrap()
        .arg("osm")
        .args(["--standard", standard, BBOX, SAMPLE])
        .arg(&map)
        .assert()
        .success();
    map
}

/// The sample map drawn to ISOM, as `sample.omap`.
pub fn make_map(dir: &Path) -> PathBuf {
    let map = dir.join("sample.omap");
    Command::cargo_bin("mauro")
        .unwrap()
        .arg("osm")
        .args(["--standard", "isom", BBOX, SAMPLE])
        .arg(&map)
        .assert()
        .success();
    map
}

/// An archive of the ISOM sample map, `sample.pmtiles`.
pub fn make_archive(dir: &Path) -> PathBuf {
    let map = make_map(dir);
    let archive = dir.join("sample.pmtiles");
    Command::cargo_bin("mauro")
        .unwrap()
        .args(["tiles", "build"])
        .arg("--output")
        .arg(&archive)
        .arg(&map)
        .assert()
        .success();
    archive
}

/// The archives of the sample drawn to ISSprOM and to ISOM, in that order.
pub fn make_two_archives(dir: &Path) -> (PathBuf, PathBuf) {
    let maps = [make_map_in(dir, "issprom"), make_map_in(dir, "isom")];
    Command::cargo_bin("mauro")
        .unwrap()
        .args(["tiles", "build"])
        .arg("--output")
        .arg(dir.join("w.pmtiles"))
        .args(&maps)
        .assert()
        .success();
    (dir.join("w.issprom.pmtiles"), dir.join("w.isom.pmtiles"))
}
