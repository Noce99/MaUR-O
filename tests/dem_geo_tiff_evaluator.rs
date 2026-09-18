//! CLI-level checks for `dem_geo_tiff_evaluator`: it scores a predicted DEM
//! against a ground truth into its own timestamped `Results` folder, and the
//! documented exit codes fire on a missing file or an uncomparable pair.

use std::path::Path;

use assert_cmd::Command;
use tiff::encoder::colortype::Gray32Float;
use tiff::encoder::TiffEncoder;
use tiff::tags::Tag;

const MODEL_PIXEL_SCALE_TAG: u16 = 33550;
const MODEL_TIEPOINT_TAG: u16 = 33922;
const GEO_KEY_DIRECTORY_TAG: u16 = 34735;

fn dem_geo_tiff_evaluator() -> Command {
    Command::cargo_bin("dem_geo_tiff_evaluator").unwrap()
}

/// A minimal single-band, tiepoint-georeferenced DEM GeoTIFF -- the
/// real-world (SRTM/LiDAR-style) shape, rather than this crate's own
/// `ModelTransformationTag` writer.
fn write_dem(
    path: &Path,
    width: u32,
    height: u32,
    values: &[f32],
    origin: (f64, f64),
    px_size: f64,
    epsg: u16,
) {
    let file = std::fs::File::create(path).unwrap();
    let mut encoder = TiffEncoder::new(file).unwrap();
    let mut image = encoder.new_image::<Gray32Float>(width, height).unwrap();
    image
        .encoder()
        .write_tag(Tag::Unknown(MODEL_PIXEL_SCALE_TAG), &[px_size, px_size, 0.0][..])
        .unwrap();
    image
        .encoder()
        .write_tag(
            Tag::Unknown(MODEL_TIEPOINT_TAG),
            &[0.0, 0.0, 0.0, origin.0, origin.1, 0.0][..],
        )
        .unwrap();
    image
        .encoder()
        .write_tag(
            Tag::Unknown(GEO_KEY_DIRECTORY_TAG),
            &maur_o::geotiff::geo_key_directory(epsg)[..],
        )
        .unwrap();
    image.write_data(values).unwrap();
}

#[test]
fn scores_two_identical_resolution_dems_into_a_timestamped_run_folder() {
    let dir = tempfile::tempdir().unwrap();
    let gt = dir.path().join("gt.tif");
    let pred = dir.path().join("pred.tif");
    write_dem(&gt, 4, 4, &[10.0; 16], (0.0, 0.0), 1.0, 3006);
    write_dem(&pred, 4, 4, &[7.0; 16], (0.0, 0.0), 1.0, 3006);

    let results = dir.path().join("Results");
    dem_geo_tiff_evaluator()
        .arg(&gt)
        .arg(&pred)
        .arg("--results")
        .arg(&results)
        .assert()
        .success()
        .stdout(predicates::str::contains("16 valid pixel(s)"))
        .stdout(predicates::str::contains("best constant 3.0000"))
        .stdout(predicates::str::contains("MSE 0.0000"));

    let entries: Vec<_> = std::fs::read_dir(&results)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(entries.len(), 1, "expected exactly one run folder");
    let run_dir = &entries[0];
    assert!(run_dir
        .file_name()
        .unwrap()
        .to_string_lossy()
        .starts_with("dem_evaluation_"));

    for name in ["metrics.txt", "error_abs.png", "error_signed.png", "error_histogram.png"] {
        let f = run_dir.join(name);
        assert!(f.exists(), "expected {} to exist", f.display());
    }
    let report = std::fs::read_to_string(run_dir.join("metrics.txt")).unwrap();
    assert!(report.contains("best additive constant: 3.000000"));
}

#[test]
fn resamples_a_coarser_prediction_onto_the_ground_truths_own_grid() {
    let dir = tempfile::tempdir().unwrap();
    let gt = dir.path().join("gt.tif");
    let pred = dir.path().join("pred.tif");
    // A constant field survives resampling exactly, whatever resolution the
    // prediction comes in at.
    write_dem(&gt, 6, 6, &[50.0; 36], (0.0, 0.0), 1.0, 3006);
    write_dem(&pred, 6, 6, &[45.0; 36], (-2.0, 2.0), 2.0, 3006);

    dem_geo_tiff_evaluator()
        .arg(&gt)
        .arg(&pred)
        .arg("--results")
        .arg(dir.path().join("Results"))
        .assert()
        .success()
        .stdout(predicates::str::contains("36 valid pixel(s)"))
        .stdout(predicates::str::contains("best constant 5.0000"));
}

#[test]
fn mismatched_crs_exits_4() {
    let dir = tempfile::tempdir().unwrap();
    let gt = dir.path().join("gt.tif");
    let pred = dir.path().join("pred.tif");
    write_dem(&gt, 2, 2, &[1.0; 4], (0.0, 0.0), 1.0, 3006);
    write_dem(&pred, 2, 2, &[1.0; 4], (0.0, 0.0), 1.0, 4326);

    dem_geo_tiff_evaluator()
        .arg(&gt)
        .arg(&pred)
        .arg("--results")
        .arg(dir.path().join("Results"))
        .assert()
        .failure()
        .code(4)
        .stderr(predicates::str::contains("CRS"));
}

#[test]
fn missing_ground_truth_exits_2() {
    let dir = tempfile::tempdir().unwrap();
    let pred = dir.path().join("pred.tif");
    write_dem(&pred, 2, 2, &[1.0; 4], (0.0, 0.0), 1.0, 3006);

    dem_geo_tiff_evaluator()
        .arg(dir.path().join("does-not-exist.tif"))
        .arg(&pred)
        .arg("--results")
        .arg(dir.path().join("Results"))
        .assert()
        .failure()
        .code(2);
}

#[test]
fn missing_prediction_exits_3() {
    let dir = tempfile::tempdir().unwrap();
    let gt = dir.path().join("gt.tif");
    write_dem(&gt, 2, 2, &[1.0; 4], (0.0, 0.0), 1.0, 3006);

    dem_geo_tiff_evaluator()
        .arg(&gt)
        .arg(dir.path().join("does-not-exist.tif"))
        .arg("--results")
        .arg(dir.path().join("Results"))
        .assert()
        .failure()
        .code(3);
}
