//! The quick checks on `map_to_image`'s command line interface: that it
//! renders what it is given, and that it says something useful and exits with
//! the documented code when it cannot.
//!
//! Everything is driven through the built binary rather than through the
//! library, because the interface is the contract here — the exit codes are
//! documented and scripted against, and a change to one of them is a breaking
//! change no library-level test would notice.

use assert_cmd::Command;

fn map_to_image() -> Command {
    Command::cargo_bin("map_to_image").unwrap()
}

/// An empty map with the default options is a white square of 100 by 100 meters.
#[test]
fn empty_map() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("empty.png");
    map_to_image()
        .arg("tests/data/empty.xmap")
        .arg(&out)
        .assert()
        .success()
        .stdout(predicates::str::contains("300x300 pixels, 100x100 meters"));
}

/// The size of an empty map depends on the frame and the resolution only.
#[test]
fn empty_map_options() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("empty2.png");
    map_to_image()
        .args(["--resolution=2", "--frame=25"])
        .arg("tests/data/empty.xmap")
        .arg(&out)
        .assert()
        .success()
        .stdout(predicates::str::contains("100x100 pixels, 50x50 meters"));
}

/// A map with one object of every type, at a scale of 1:10000.
#[test]
fn shapes() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("shapes.png");
    map_to_image()
        .arg("--frame=10")
        .arg("tests/data/shapes.xmap")
        .arg(&out)
        .assert()
        .success()
        .stdout(predicates::str::contains("map scale 1:10000"));
}

#[test]
fn missing_file() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("missing.png");
    map_to_image()
        .arg("tests/data/does-not-exist.xmap")
        .arg(&out)
        .assert()
        .failure();
}

#[test]
fn invalid_resolution() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("invalid.png");
    map_to_image()
        .arg("--resolution=nonsense")
        .arg("tests/data/empty.xmap")
        .arg(&out)
        .assert()
        .failure();
}

/// A window is drawn on its own, at the map's resolution: 4 mm on the paper at
/// 1:10000 are 40 meters, i.e. 120 pixels at the default 3 pixels per meter.
#[test]
fn window() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("window.png");
    map_to_image()
        .args(["--window", "-1,-1,3,3"])
        .arg("tests/data/shapes.xmap")
        .arg(&out)
        .assert()
        .success()
        .stdout(predicates::str::contains("120x120 pixels, 40x40 meters"));
}

/// A window that is not four numbers with right > left and bottom > top is a
/// usage error.
#[test]
fn invalid_window() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("invalid.png");
    for window in [
        "1,2,3",
        "0,0,4,4,",
        "a,b,c,d",
        "4,0,0,4",
        "0,4,4,0",
        "nan,0,1,1",
    ] {
        map_to_image()
            .args(["--window", window])
            .arg("tests/data/shapes.xmap")
            .arg(&out)
            .assert()
            .code(1);
    }
}

/// The frame has no meaning for a window, so asking for both is a usage error.
#[test]
fn window_and_frame() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("invalid.png");
    map_to_image()
        .args(["--window", "0,0,4,4", "--frame=10"])
        .arg("tests/data/shapes.xmap")
        .arg(&out)
        .assert()
        .code(1);
}

/// A window too large for any image fails with the geometry exit code.
#[test]
fn huge_window() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("huge.png");
    map_to_image()
        .args(["--window", "0,0,1e9,1e9"])
        .arg("tests/data/shapes.xmap")
        .arg(&out)
        .assert()
        .code(3)
        .stderr(predicates::str::contains("too large"));
}
