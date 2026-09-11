//! CLI-level checks for `contours_to_raster`: it runs Steps 0-2 end to end
//! on a small fixture, `--create_svg` writes the five validation files, and
//! the documented exit codes fire on a missing map and on a genuine
//! Contour-Raster pixel conflict.

use assert_cmd::Command;

fn contours_to_raster() -> Command {
    Command::cargo_bin("contours_to_raster").unwrap()
}

/// Two nested closed contours, one resolved by a Slope Line and the other by
/// the closed-hill heuristic -- both within Step 1, so this fixture never
/// reaches Step 2. See the fixture's own comment in
/// `tests/data/contours.xmap`.
#[test]
fn contours() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("contours.tif");
    contours_to_raster()
        .arg("tests/data/contours.xmap")
        .arg(&out)
        .assert()
        .success()
        .stdout(predicates::str::contains("2 contours"))
        .stdout(predicates::str::contains(
            "1 slope line/heavy-object reading(s)",
        ))
        .stdout(predicates::str::contains("1 closed hill(s)"));
}

/// `--create_svg` writes into this run's own `contours_to_raster_<timestamp>`
/// folder under `--results`, not next to the map or the output file.
#[test]
fn create_svg_writes_five_non_empty_files_in_a_timestamped_run_folder() {
    let dir = tempfile::tempdir().unwrap();
    let results = dir.path().join("Results");
    let out = dir.path().join("contours.tif");
    contours_to_raster()
        .arg("tests/data/contours.xmap")
        .arg(&out)
        .arg("--results")
        .arg(&results)
        .arg("--create_svg")
        .assert()
        .success();

    let entries: Vec<_> = std::fs::read_dir(&results)
        .unwrap_or_else(|e| panic!("{}: {e}", results.display()))
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(entries.len(), 1, "expected exactly one run folder");
    let run_dir = &entries[0];
    assert!(
        run_dir
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("contours_to_raster_"),
        "unexpected run folder name: {}",
        run_dir.display()
    );

    for step in ["step0", "step1", "step2_rain", "step2_anti_rain", "final"] {
        let svg = run_dir.join(format!("contours_{step}.svg"));
        let contents =
            std::fs::read_to_string(&svg).unwrap_or_else(|e| panic!("{}: {e}", svg.display()));
        assert!(contents.starts_with("<svg"));
        assert!(contents.trim_end().ends_with("</svg>"));
    }
}

#[test]
fn missing_map_file_exits_2() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("missing.tif");
    contours_to_raster()
        .arg("tests/data/does-not-exist.xmap")
        .arg(&out)
        .assert()
        .failure()
        .code(2);
}

#[test]
fn missing_config_file_exits_3() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("contours.tif");
    contours_to_raster()
        .arg("tests/data/contours.xmap")
        .arg(&out)
        .arg("--config")
        .arg("tests/data/does-not-exist.conf")
        .assert()
        .failure()
        .code(3);
}

/// Two contours 0.5m apart, with `rasterization_px_size` deliberately set to
/// 2m (far coarser than that gap): the doc's own crash-on-conflict path,
/// exercised end to end rather than just at the unit level.
#[test]
fn conflicting_pixel_exits_4() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("coarse.conf");
    std::fs::write(
        &config_path,
        "bezier_linearization_step = 0.5\n\
         contours_step = 5.0\n\
         rasterization_px_size = 2.0\n\
         rasterization_step_factor = 0.5\n\
         heavy_object_width = 3.0\n\
         heavy_object_growing = 0.5\n\
         circumference_fitting_points_number = 4\n\
         slope_lines_contours_search_radius = 3.0\n\
         rain_drop_step = 1.0\n\
         sources_per_contour_segment = 3\n\
         rain_drop_starting_voting_hysteresis = 5\n\
         undefined_gravity_vote_threshold = 0.8\n\
         contour_gap_merge_radius = 0.1\n",
    )
    .unwrap();
    let out = dir.path().join("conflict.tif");
    contours_to_raster()
        .arg("tests/data/contours_conflict.xmap")
        .arg(&out)
        .arg("--config")
        .arg(&config_path)
        .assert()
        .failure()
        .code(4)
        .stderr(predicates::str::contains("rasterization_px_size"));
}

/// The same unrecoverable conflict as `conflicting_pixel_exits_4`, but with
/// `--create_svg`: the run must still fail with the same exit code, but a
/// `<stem>_conflict.svg` diagnosing exactly what collided is written into
/// the run folder anyway, so the two too-close contours can be inspected
/// without decreasing rasterization_px_size and re-running first.
#[test]
fn conflicting_pixel_still_writes_a_diagnostic_svg_under_create_svg() {
    let dir = tempfile::tempdir().unwrap();
    let results = dir.path().join("Results");
    let config_path = dir.path().join("coarse.conf");
    std::fs::write(
        &config_path,
        "bezier_linearization_step = 0.5\n\
         contours_step = 5.0\n\
         rasterization_px_size = 2.0\n\
         rasterization_step_factor = 0.5\n\
         heavy_object_width = 3.0\n\
         heavy_object_growing = 0.5\n\
         circumference_fitting_points_number = 4\n\
         slope_lines_contours_search_radius = 3.0\n\
         rain_drop_step = 1.0\n\
         sources_per_contour_segment = 3\n\
         rain_drop_starting_voting_hysteresis = 5\n\
         undefined_gravity_vote_threshold = 0.8\n\
         contour_gap_merge_radius = 0.1\n",
    )
    .unwrap();
    let out = dir.path().join("conflict.tif");
    contours_to_raster()
        .arg("tests/data/contours_conflict.xmap")
        .arg(&out)
        .arg("--results")
        .arg(&results)
        .arg("--config")
        .arg(&config_path)
        .arg("--create_svg")
        .assert()
        .failure()
        .code(4)
        .stderr(predicates::str::contains("rasterization_px_size"))
        .stderr(predicates::str::contains("conflict.svg"));

    let run_dir = std::fs::read_dir(&results)
        .unwrap()
        .next()
        .expect("a run folder should have been created")
        .unwrap()
        .path();
    let svg_path = run_dir.join("conflict_conflict.svg");
    let text = std::fs::read_to_string(&svg_path).unwrap();
    assert!(!text.is_empty());
    // The highlight ring around the conflicting cluster is drawn in red,
    // with no fill (an unfilled stroke), same convention as every other
    // ring this crate's SVGs draw.
    assert!(text.contains(r#"stroke="rgb(220,20,60)""#));
    // Only Step 0 ever ran before this failure -- no other step's own SVG
    // should exist alongside the diagnostic one.
    assert!(!run_dir.join("conflict_step0.svg").exists());
}
