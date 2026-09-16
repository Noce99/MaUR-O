//! CLI-level checks for `contours_to_raster`: it runs Steps 1-3 end to end
//! on a small fixture, `--create_svg` writes the nine validation files (seven
//! numbered, plus the unnumbered "final" and "contours_function" ones), and
//! the documented exit codes fire on a missing map or config file.

use assert_cmd::Command;

fn contours_to_raster() -> Command {
    Command::cargo_bin("contours_to_raster").unwrap()
}

/// Two nested closed contours, one resolved by a Slope Line and the other by
/// the closed-hill heuristic -- both within Step 2, so this fixture never
/// reaches Step 3. See the fixture's own comment in
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
        .stdout(predicates::str::contains("1 slope line(s)"))
        .stdout(predicates::str::contains("1 closed hill(s)"));
}

/// `--create_svg` writes into this run's own `contours_to_raster_<timestamp>`
/// folder under `--results`, not next to the map or the output file.
#[test]
fn create_svg_writes_nine_non_empty_files_in_a_timestamped_run_folder() {
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

    for name in [
        "contours_contours_function.svg",
        "00_contours_step1.svg",
        "01_contours_step1_close_search.svg",
        "02_contours_step1_growing_seeking.svg",
        "03_contours_step1_growing_matching.svg",
        "04_contours_step2.svg",
        "05_contours_step3_rain.svg",
        "06_contours_step3_anti_rain.svg",
        "contours_final.svg",
    ] {
        let svg = run_dir.join(name);
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
