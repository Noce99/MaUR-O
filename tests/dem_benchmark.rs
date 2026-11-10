//! `create_dem_benchmark` and `dem_benchmark`'s command lines.
//!
//! `create_dem_benchmark` downloads from Lantmäteriet and runs
//! Karttapullautin, neither of which a test suite has any business depending
//! on: what is pinned here is every exit it takes before reaching them.
//! `dem_benchmark` is wholly offline, so it is run end to end on a tiny
//! archive built here: one round hill, its contours imported the way
//! Karttapullautin's would be, and the cone they were drawn from as the
//! ground truth.

use std::f64::consts::PI;
use std::path::Path;

use assert_cmd::Command;
use predicates::str::contains;

use maur_o::dem_archive::{gaussian_smooth, water_mask, FORMAT};
use maur_o::dem_geo_tiff_evaluator::{write, AffineTransform, CrsKey, GeoRaster};
use maur_o::dxf::{Entity, EntityKind};
use maur_o::pullauta_import::{import, GroundBox, ImportOptions};

fn create() -> Command {
    let mut cmd = Command::cargo_bin("create_dem_benchmark").unwrap();
    cmd.env_remove("LANTMATERIET_USERNAME")
        .env_remove("LANTMATERIET_PASSWORD");
    cmd
}

fn bench() -> Command {
    Command::cargo_bin("dem_benchmark").unwrap()
}

#[test]
fn creating_without_credentials_is_exit_2() {
    let dir = tempfile::tempdir().unwrap();
    create()
        .args(["--pullauta", "/no/such/pullauta"])
        .arg(dir.path().join("out"))
        .assert()
        .code(2)
        .stderr(contains("LANTMATERIET_USERNAME"));
}

#[test]
fn credentials_come_from_an_env_file_too() {
    let dir = tempfile::tempdir().unwrap();
    let env = dir.path().join(".env");
    std::fs::write(
        &env,
        "# GeoTorget\nLANTMATERIET_USERNAME=someone\nexport LANTMATERIET_PASSWORD=\"secret\"\n",
    )
    .unwrap();
    // Past the credentials, the missing Karttapullautin is what stops it.
    create()
        .args(["--pullauta", "/no/such/pullauta", "--env-file"])
        .arg(&env)
        .arg(dir.path().join("out"))
        .assert()
        .code(3)
        .stderr(contains("Karttapullautin not found"));
}

#[test]
fn a_non_empty_output_folder_is_exit_4() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out");
    std::fs::create_dir(&out).unwrap();
    std::fs::write(out.join("something"), "").unwrap();
    // Any existing file stands in for Karttapullautin: it is never run.
    let pullauta = dir.path().join("pullauta");
    std::fs::write(&pullauta, "").unwrap();
    create()
        .env("LANTMATERIET_USERNAME", "someone")
        .env("LANTMATERIET_PASSWORD", "secret")
        .arg("--pullauta")
        .arg(&pullauta)
        .arg(&out)
        .assert()
        .code(4)
        .stderr(contains("not empty"));
}

#[test]
fn a_garbled_place_is_a_usage_error() {
    create()
        .args(["--pullauta", "x", "--at", "57.7;11.9", "out"])
        .assert()
        .code(1)
        .stderr(contains("LAT,LON"));
}

/// The hill: `HEIGHT - SLOPE * r` meters at `r` meters from its center.
const HEIGHT: f64 = 60.0;
const SLOPE: f64 = 0.1;
const CENTER: (f64, f64) = (500_500.0, 6_500_500.0);

/// A one-sample archive of the hill at `root`.
fn hill_archive(root: &Path) {
    let ring = |r: f64| -> Vec<(f64, f64)> {
        (0..72)
            .map(|k| {
                let a = 2.0 * PI * k as f64 / 72.0;
                (CENTER.0 + r * a.cos(), CENTER.1 + r * a.sin())
            })
            .collect()
    };
    let mut out2 = Vec::new();
    for step in 1..=9 {
        let r = 50.0 * step as f64;
        let level = HEIGHT - SLOPE * r;
        out2.push(Entity {
            layer: if level % 25.0 == 0.0 {
                "contour_index"
            } else {
                "contour"
            }
            .to_string(),
            kind: EntityKind::Polyline,
            elevation: Some(level),
            points: ring(r),
            closed: true,
        });
    }
    let bounds = GroundBox {
        min_x: CENTER.0 - 500.0,
        min_y: CENTER.1 - 500.0,
        max_x: CENTER.0 + 500.0,
        max_y: CENTER.1 + 500.0,
    };
    let options = |slope_lines| ImportOptions {
        bounds,
        scale: 10_000,
        epsg: 3006,
        center_lat_lon: (58.6, 15.0),
        slope_lines,
    };
    let sample = root.join("sample_0001");
    std::fs::create_dir_all(&sample).unwrap();
    let with = import(&out2, &[], &options(true));
    with.write(&sample.join("map_depressions.omap")).unwrap();
    import(&out2, &[], &options(false))
        .write(&sample.join("map_none.omap"))
        .unwrap();
    std::fs::write(sample.join("levels.txt"), with.levels_text()).unwrap();

    let (w, h) = (500, 500);
    let px = 2.0;
    let truth = GeoRaster {
        width: w,
        height: h,
        values: (0..w * h)
            .map(|i| {
                let x = bounds.min_x + ((i % w) as f64 + 0.5) * px;
                let y = bounds.max_y - ((i / w) as f64 + 0.5) * px;
                HEIGHT - SLOPE * (x - CENTER.0).hypot(y - CENTER.1)
            })
            .collect(),
        transform: AffineTransform {
            origin_x: bounds.min_x,
            origin_y: bounds.max_y,
            col_x: px,
            row_x: 0.0,
            col_y: 0.0,
            row_y: -px,
        },
        crs: CrsKey::Projected(3006),
    };
    write(&truth, 3006, &sample.join("ground_truth.tif")).unwrap();
    write(
        &gaussian_smooth(&truth, 2.0),
        3006,
        &sample.join("ground_truth_smoothed.tif"),
    )
    .unwrap();
    let water = GeoRaster {
        values: water_mask(&truth, 0.01, 500)
            .iter()
            .map(|&w| if w { 1.0 } else { 0.0 })
            .collect(),
        ..truth.clone()
    };
    write(&water, 3006, &sample.join("water_mask.tif")).unwrap();
    std::fs::write(sample.join("sample.txt"), "relief_m = 40\n").unwrap();
    std::fs::write(
        root.join("info.txt"),
        format!("format = {FORMAT}\nequidistance_m = 5\nsamples = 1\n"),
    )
    .unwrap();
}

#[test]
fn a_hill_is_scored_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let archive = dir.path().join("archive");
    hill_archive(&archive);
    let results = dir.path().join("results");
    bench()
        .arg(&archive)
        .arg("--results")
        .arg(&results)
        .assert()
        .success()
        .stdout(contains("sample_0001  depressions"))
        .stdout(contains("Means per variant"));

    let run = std::fs::read_dir(&results)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert!(run.join("summary.txt").is_file());
    let out = run.join("sample_0001").join("depressions");
    for file in [
        "prediction.tif",
        "metrics.txt",
        "signed_error.png",
        "abs_error.png",
        "error_histogram.png",
    ] {
        assert!(out.join(file).is_file(), "{file}");
    }
    let metrics = maur_o::dem_archive::read_key_values(&out.join("metrics.txt")).unwrap();
    let number = |key: &str| metrics[key].parse::<f64>().unwrap();
    // Concentric rings with no slope lines are a hill: every level is found.
    assert_eq!(number("lines_scored"), 9.0);
    assert_eq!(number("levels_exact"), 1.0);
    assert_eq!(number("inverted"), 0.0);
    assert!(number("rmse_m") < 2.0, "{}", number("rmse_m"));
    assert!(number("baseline_rmse_m") < 1.0);
}

#[test]
fn a_level_accuracy_below_the_threshold_is_exit_5() {
    let dir = tempfile::tempdir().unwrap();
    let archive = dir.path().join("archive");
    hill_archive(&archive);
    bench()
        .arg(&archive)
        .args([
            "--variant",
            "none",
            "--min-level-accuracy",
            "101",
            "--results",
        ])
        .arg(dir.path().join("results"))
        .assert()
        .code(5)
        .stderr(contains("variant none"));
}

#[test]
fn a_folder_that_is_not_an_archive_is_exit_2() {
    let dir = tempfile::tempdir().unwrap();
    bench()
        .arg(dir.path())
        .assert()
        .code(2)
        .stderr(contains("info.txt"));
}

#[test]
fn an_unknown_variant_is_a_usage_error() {
    let dir = tempfile::tempdir().unwrap();
    bench()
        .arg(dir.path())
        .args(["--variant", "all"])
        .assert()
        .code(1)
        .stderr(contains("unknown variant"));
}

#[test]
fn a_missing_config_is_exit_3() {
    let dir = tempfile::tempdir().unwrap();
    let archive = dir.path().join("archive");
    hill_archive(&archive);
    bench()
        .arg(&archive)
        .args(["--config", "no/such.conf"])
        .assert()
        .code(3);
}
