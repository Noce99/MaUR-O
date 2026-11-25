//! `mauro contours`' command line: a terrain model drawn into a map as
//! contours, and the exit codes it turns a map or a model away with.

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use maur_o::orthophoto::Grid;
use maur_o::terrain::ElevationGrid;

fn contours() -> Command {
    let mut command = Command::cargo_bin("mauro").unwrap();
    command.arg("contours");
    command
}

/// `contours.xmap` (its symbols: a contour, 101.0, and a slope line),
/// placed in UTM zone 32 at 1:10000 with its origin at 682364, 4952128.
fn placed_map(dir: &Path) -> PathBuf {
    let map = std::fs::read_to_string("tests/data/contours.xmap").unwrap();
    let start = map.find("<georeferencing").unwrap();
    let end = map.find("</georeferencing>").unwrap() + "</georeferencing>".len();
    let georeferencing = r#"<georeferencing scale="10000"><projected_crs id="UTM"><spec language="PROJ.4">+proj=utm +datum=WGS84 +zone=32</spec><parameter>32</parameter><ref_point x="682364" y="4952128"/></projected_crs></georeferencing>"#;
    let path = dir.join("hill.xmap");
    std::fs::write(
        &path,
        format!("{}{georeferencing}{}", &map[..start], &map[end..]),
    )
    .unwrap();
    path
}

/// A cone 100 m high, falling 0.5 m per meter, centred on the map's
/// origin: 400 × 400 m at 5 m a pixel, in EPSG `epsg`.
fn cone(dir: &Path, epsg: u16) -> PathBuf {
    let grid = Grid {
        west: 682164.0,
        north: 4952328.0,
        pixel_width: 5.0,
        pixel_height: 5.0,
        width: 80,
        height: 80,
    };
    let mut dtm = ElevationGrid::new(grid);
    for row in 0..80 {
        for col in 0..80 {
            let (x, y) = (f64::from(col) * 5.0 + 2.5, f64::from(row) * 5.0 + 2.5);
            let distance = (x - 200.0).hypot(y - 200.0);
            dtm.values[(row * 80 + col) as usize] = (100.0 - distance * 0.5) as f32;
        }
    }
    let path = dir.join(format!("cone_{epsg}.tif"));
    dtm.write_geotiff(std::fs::File::create(&path).unwrap(), epsg)
        .unwrap();
    path
}

#[test]
fn a_cone_is_drawn_as_rings_in_a_part_of_their_own() {
    let dir = tempfile::tempdir().unwrap();
    let (map, dtm) = (placed_map(dir.path()), cone(dir.path(), 32632));
    let out = dir.path().join("hill_out.omap");
    contours()
        .args([&map, &dtm])
        .args(["--index", "0", "--smooth", "0", "-o"])
        .arg(&out)
        .assert()
        .success()
        .stdout(predicates::str::contains("DTM contours, 5 m"));
    let written = std::fs::read_to_string(&out).unwrap();
    assert!(written.contains("<parts count=\"2\""));
    assert!(written.contains("<part name=\"DTM contours, 5 m\">"));
    // The blue goes above the map's brown, which moves down one.
    assert!(written.contains("<colors count=\"2\""));
    assert!(written.contains("<color priority=\"0\" name=\"generated_contours\""));
    assert!(written.contains("<color priority=\"1\" name=\"Brown\""));
    assert!(
        written.contains("<symbol type=\"2\" id=\"2\" code=\"101.0\" name=\"generated_contours\">")
    );
    // The cone is 0 m high at its edges' middles (200 m from the top), so
    // every level from 5 to 95 m is one closed ring; those below are arcs
    // across the raster's corners.
    let (map, _) = maur_o::xml_reader::read_xml_map(&out).unwrap();
    // Drawn with the blue copy of the map's contour symbol (id 0), the
    // map's next id; the map's own contours keep theirs.
    let rings: Vec<_> = map
        .objects
        .iter()
        .filter(|o| o.symbol_id == 2 && o.coords.last().unwrap().is_close_point())
        .collect();
    assert_eq!(rings.len(), 19);
    // The innermost ring (95 m) is 10 m from the top: 1 mm at 1:10000.
    let innermost = rings
        .iter()
        .min_by(|a, b| {
            a.coords[0]
                .pos()
                .length()
                .total_cmp(&b.coords[0].pos().length())
        })
        .unwrap();
    for coord in &innermost.coords {
        assert!((coord.pos().length() - 1.0).abs() < 0.05, "{coord:?}");
    }

    // Again, at another equidistance: a part of its own; at the same one,
    // the same part, replaced.
    for (equidistance, parts) in [("10", 3), ("10", 3)] {
        contours()
            .args([&out, &dtm])
            .args(["--index", "0", "--equidistance", equidistance, "-o"])
            .arg(&out)
            .assert()
            .success();
        let written = std::fs::read_to_string(&out).unwrap();
        assert!(written.contains(&format!("<parts count=\"{parts}\"")));
        // The colour and the symbol are the first run's.
        assert!(written.contains("<colors count=\"2\""));
        assert_eq!(written.matches("name=\"generated_contours\"").count(), 2);
    }
}

#[test]
fn a_map_without_index_contours_needs_index_0() {
    let dir = tempfile::tempdir().unwrap();
    let (map, dtm) = (placed_map(dir.path()), cone(dir.path(), 32632));
    contours()
        .args([&map, &dtm])
        .assert()
        .code(3)
        .stderr(predicates::str::contains("--index 0"));
    assert!(!dir.path().join("hill_contours.omap").exists());
}

#[test]
fn a_model_in_another_crs_is_turned_away() {
    let dir = tempfile::tempdir().unwrap();
    let (map, dtm) = (placed_map(dir.path()), cone(dir.path(), 32633));
    contours()
        .args([&map, &dtm])
        .args(["--index", "0"])
        .assert()
        .code(4)
        .stderr(predicates::str::contains("EPSG:32633"));
}

#[test]
fn a_map_not_on_the_earth_is_turned_away() {
    let dir = tempfile::tempdir().unwrap();
    let dtm = cone(dir.path(), 32632);
    contours()
        .args([Path::new("tests/data/contours.xmap"), &dtm])
        .arg("-o")
        .arg(dir.path().join("out.omap"))
        .assert()
        .code(3);
}

#[test]
fn the_output_must_be_a_map_file() {
    contours()
        .args(["tests/data/contours.xmap", "dtm.tif", "-o", "out.tif"])
        .assert()
        .code(1)
        .stderr(predicates::str::contains(".omap or .xmap"));
}

#[test]
fn a_missing_map_or_model_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    contours()
        .args(["no_such_map.omap", "dtm.tif"])
        .arg("-o")
        .arg(dir.path().join("out.omap"))
        .assert()
        .code(2);
    let map = placed_map(dir.path());
    contours()
        .arg(&map)
        .arg(dir.path().join("no_such_dtm.tif"))
        .assert()
        .code(4);
}

#[test]
fn the_equidistance_must_be_positive() {
    contours()
        .args(["map.omap", "dtm.tif", "--equidistance", "0"])
        .assert()
        .code(1)
        .stderr(predicates::str::contains("--equidistance"));
}
