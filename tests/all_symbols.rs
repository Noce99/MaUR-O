//! End to end tests for `maur_o::all_symbols::create_maps`: a symbol set goes
//! in, one map per symbol comes out.
//!
//! The maps are checked by reading them back with this project's own reader
//! and rendering them, which is the property that matters — a generated map
//! is only worth generating if a renderer can draw the symbol it was made
//! for from it. `create_benchmark`, the only thing that calls this function,
//! has its own tests for turning the result into an archive; these are the
//! ones for what ends up in each generated map.

use std::path::Path;

/// The symbol set the tests generate from: five symbols, one of each type
/// except combined.
const SOURCE: &str = "tests/data/shapes.xmap";

fn generate(into: &Path) -> maur_o::all_symbols::Summary {
    maur_o::all_symbols::create_maps(Path::new(SOURCE), into, |_, _| {}).unwrap()
}

fn names(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(folder)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn every_symbol_gets_a_map_and_a_description() {
    let dir = tempfile::tempdir().unwrap();
    let into = dir.path().join("symbols");
    let summary = generate(&into);

    // Numbered by the symbol's place in the source, then its type, its number
    // and its name, with everything a file name cannot hold replaced.
    assert_eq!(
        names(&into),
        [
            "001_area_401_Open_land.omap",
            "001_area_401_Open_land.txt",
            "002_area_308_Marsh.omap",
            "002_area_308_Marsh.txt",
            "003_line_505_Path.omap",
            "003_line_505_Path.txt",
            "004_point_109_Small_knoll.omap",
            "004_point_109_Small_knoll.txt",
            "005_text_105_Contour_value.omap",
            "005_text_105_Contour_value.txt",
        ]
    );
    assert_eq!(summary.written, 5);
    assert_eq!(summary.scale_denominator, 10000);
}

#[test]
fn a_generated_map_holds_the_symbol_it_was_made_for_and_its_objects() {
    let dir = tempfile::tempdir().unwrap();
    let into = dir.path().join("symbols");
    generate(&into);

    let (mut map, warnings) = maur_o::xml_reader::read_xml_map(&into.join("003_line_505_Path.omap")).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    map.resolve_references();

    assert_eq!(map.scale_denominator, 10000);
    assert_eq!(map.symbols.len(), 1);
    // Five shapes at three sizes, and this symbol has no minimum length.
    assert_eq!(map.objects.len(), 15);
    for object in &map.objects {
        assert_eq!(object.symbol_index, Some(0), "an object is drawn with no symbol");
    }
    // The first row is the straight line, at 5, 50 and 100 m on the ground;
    // at 1:10000 a meter is a tenth of a millimeter of paper.
    let width = |object: &maur_o::map::Object| object.coords[1].x - object.coords[0].x;
    assert!((width(&map.objects[0]) - 0.5).abs() < 1e-9, "{}", width(&map.objects[0]));
    assert!((width(&map.objects[1]) - 5.0).abs() < 1e-9, "{}", width(&map.objects[1]));
    assert!((width(&map.objects[2]) - 10.0).abs() < 1e-9, "{}", width(&map.objects[2]));
}

#[test]
fn a_generated_map_renders() {
    let dir = tempfile::tempdir().unwrap();
    let into = dir.path().join("symbols");
    generate(&into);

    for name in ["001_area_401_Open_land", "004_point_109_Small_knoll", "005_text_105_Contour_value"] {
        let map = into.join(format!("{name}.omap"));
        let rendering = maur_o::render::render_map(&map, 3.0, 50.0).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(rendering.pixmap.width() > 100, "{name} came out {} wide", rendering.pixmap.width());
        // Something was drawn: the frame alone would leave it all white.
        let white = rendering.pixmap.data().chunks(4).filter(|p| p[0..3] == [255, 255, 255]).count();
        assert!(white < rendering.pixmap.data().len() / 4, "{name} is blank");
    }
}

#[test]
fn the_description_says_what_is_in_each_cell_of_the_grid() {
    let dir = tempfile::tempdir().unwrap();
    let into = dir.path().join("symbols");
    generate(&into);

    let text = std::fs::read_to_string(into.join("004_point_109_Small_knoll.txt")).unwrap();
    assert!(text.contains("Symbol:      109 Small knoll"), "{text}");
    assert!(text.contains("Type:        point"), "{text}");
    assert!(text.contains("Map scale:   1:10000"), "{text}");
    // A point symbol has one column and a row per rotation, or a single row
    // where it cannot be rotated.
    assert!(text.contains("c1: nominal size"), "{text}");
    assert!(text.contains("r1: "), "{text}");
    assert!(text.contains("Cells:"), "{text}");
}

#[test]
fn a_map_which_is_not_one_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("not a map.omap");
    std::fs::write(&source, "this is not XML").unwrap();

    let result = maur_o::all_symbols::create_maps(&source, &dir.path().join("out"), |_, _| {});
    assert!(result.is_err());
}
