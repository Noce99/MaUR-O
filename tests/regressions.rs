//! Inputs which once crashed a tool, lost part of a map, or gave a wrong
//! answer, each kept here so it stays fixed.
//!
//! Every one is a small hand-written map rather than a real file: what
//! matters is the one thing about it which went wrong, and a fixture with
//! nothing else in it says so.

use assert_cmd::Command;
use maur_o::map::{symbol_leaves, Symbol};
use maur_o::runnability::{build, Options};
use maur_o::xml_reader::read_xml_map_str;

/// A map file around `colors`, `symbols` and `objects`, at 1:10000.
fn xmap(colors: &str, symbols: &str, objects: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<map xmlns="http://openorienteering.org/apps/mapper/xml/v2" version="9">
<georeferencing scale="10000"/>
<colors count="1">{colors}</colors>
<symbols count="1">{symbols}</symbols>
<parts count="1"><part name="p"><objects count="1">{objects}</objects></part></parts>
</map>"#
    )
}

const BLACK: &str = r#"<color priority="0" name="Black" c="0" m="0" y="0" k="1" opacity="1"/>"#;

const A_LINE: &str =
    r#"<object type="1" symbol="0"><coords count="2">0 0;10000 10000;</coords></object>"#;

/// Renders `xml` with `map_to_image` and asserts that it worked.
fn renders(xml: &str) {
    let dir = tempfile::tempdir().unwrap();
    let map = dir.path().join("map.xmap");
    std::fs::write(&map, xml).unwrap();
    Command::cargo_bin("map_to_image")
        .unwrap()
        .arg(&map)
        .arg(dir.path().join("map.png"))
        .assert()
        .success();
}

/// A combined symbol listing itself as a part is a symbol made of itself.
/// Everything that looks through combinations recursed into it until the
/// stack overflowed.
#[test]
fn a_combined_symbol_which_contains_itself_draws_and_classifies() {
    let xml = xmap(
        BLACK,
        r#"<symbol type="16" id="0" code="1" name="loop"><combined_symbol parts="1"><part symbol="0"/></combined_symbol></symbol>"#,
        A_LINE,
    );
    let (map, _) = read_xml_map_str(&xml).unwrap();
    let mut leaves = Vec::new();
    symbol_leaves(&map.symbols[0], &map, &mut leaves);
    assert!(leaves.is_empty(), "a symbol made of itself draws nothing");
    renders(&xml);
}

/// The same, two symbols apart -- what an OCD area whose border is an area
/// with the first as its border turns into. One part closing the cycle is
/// cut (b's reference back to a, a being met first); everything else still
/// draws, so a keeps its line.
#[test]
fn two_combined_symbols_which_contain_each_other_draw_and_classify() {
    let xml = xmap(
        BLACK,
        r#"<symbol type="16" id="0" code="1" name="a"><combined_symbol parts="2"><part symbol="1"/><part symbol="2"/></combined_symbol></symbol>
<symbol type="16" id="1" code="2" name="b"><combined_symbol parts="1"><part symbol="0"/></combined_symbol></symbol>
<symbol type="2" id="2" code="3" name="line"><line_symbol color="0" line_width="250"/></symbol>"#,
        A_LINE,
    );
    let (map, _) = read_xml_map_str(&xml).unwrap();
    let leaves_of = |i: usize| {
        let mut leaves = Vec::new();
        symbol_leaves(&map.symbols[i], &map, &mut leaves);
        assert!(leaves.iter().all(|s| !matches!(s, Symbol::Combined(_))));
        leaves
            .iter()
            .map(|s| s.code().to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(leaves_of(0), ["3"]);
    assert!(leaves_of(1).is_empty());
    renders(&xml);
}

/// The colour table is indexed by priority, so a priority of two billion
/// meant allocating 160 GB and aborting.
#[test]
fn an_absurd_color_priority_is_a_warning_not_an_allocation() {
    let xml = xmap(
        r#"<color priority="2000000000" name="Huge" c="0" m="0" y="0" k="1" opacity="1"/>"#,
        "",
        "",
    );
    let (map, warnings) = read_xml_map_str(&xml).unwrap();
    assert_eq!(map.colors.len(), 1);
    assert_eq!(map.colors[0].name, "Huge");
    assert!(
        warnings.iter().any(|w| w.contains("2000000000")),
        "{warnings:?}"
    );
}

/// A self-closing element read as text (`<parameter/>`) made the reader take
/// the element's following siblings and its parent's end tag for its own
/// content, and the rest of the map -- colours, symbols, objects -- was lost
/// without a word.
#[test]
fn an_empty_parameter_does_not_swallow_the_rest_of_the_map() {
    let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<map xmlns="http://openorienteering.org/apps/mapper/xml/v2" version="9">
<georeferencing scale="10000"><projected_crs id="EPSG"><parameter/><ref_point x="5" y="7"/></projected_crs></georeferencing>
<colors count="1"><color priority="0" name="Black" c="0" m="0" y="0" k="1" opacity="1"/></colors>
<symbols count="1"><symbol type="2" id="0" code="1" name="line"><line_symbol color="0" line_width="250"/></symbol></symbols>
<parts count="1"><part name="p"><objects count="1"><object type="1" symbol="0"><coords count="2">0 0;10000 10000;</coords></object></objects></part></parts>
</map>"#;
    let (map, _) = read_xml_map_str(xml).unwrap();
    let georef = map.georeferencing.as_ref().unwrap();
    assert_eq!(
        (georef.epsg, georef.ref_point_x, georef.ref_point_y),
        (0, 5.0, 7.0)
    );
    assert_eq!(map.colors.len(), 1);
    assert_eq!(map.symbols.len(), 1);
    assert_eq!(map.objects.len(), 1);
}

/// The same for an empty text object's `<text/>` with a sibling after it:
/// the sibling, the object's end and the objects after it were lost.
#[test]
fn an_empty_text_does_not_swallow_the_objects_after_it() {
    let xml = xmap(
        BLACK,
        r#"<symbol type="8" id="0" code="1" name="text"><text_symbol><font family="sans-serif" size="4000"/><text color="0"/></text_symbol></symbol>"#,
        r#"<object type="4" symbol="0"><text/><coords count="1">1000 2000;</coords></object>
<object type="4" symbol="0"><coords count="1">5000 5000;</coords><text>second</text></object>"#,
    );
    let (map, _) = read_xml_map_str(&xml).unwrap();
    assert_eq!(map.objects.len(), 2);
    assert_eq!(
        map.objects[0].coords.len(),
        1,
        "the coords after <text/> were read"
    );
    assert_eq!(
        (map.objects[0].coords[0].x, map.objects[0].coords[0].y),
        (1.0, 2.0)
    );
}

/// The top-level `<georeferencing><ref_point>` is the reference point on the
/// paper, and is not the projected one inside `<projected_crs>`.
#[test]
fn the_map_reference_point_is_read_apart_from_the_projected_one() {
    let xml = r#"<?xml version="1.0"?>
<map version="9">
<georeferencing scale="15000"><ref_point x="12.5" y="-3"/>
  <projected_crs id="EPSG"><parameter>3006</parameter><ref_point x="322500" y="6397500"/></projected_crs>
</georeferencing>
<colors count="0"/><symbols count="0"/><parts count="0"/>
</map>"#;
    let (map, _) = read_xml_map_str(xml).unwrap();
    let georef = map.georeferencing.unwrap();
    assert_eq!(
        (georef.map_ref_point_x, georef.map_ref_point_y),
        (12.5, -3.0)
    );
    assert_eq!(
        (georef.ref_point_x, georef.ref_point_y),
        (322500.0, 6397500.0)
    );
    assert_eq!(georef.epsg, 3006);
}

/// JPEG has no alpha channel, and the encoder refused the RGBA it was
/// handed: `.jpg` output always failed and left an empty file.
#[test]
fn a_map_renders_to_jpeg() {
    let dir = tempfile::tempdir().unwrap();
    for name in ["map.jpg", "map.JPEG"] {
        let out = dir.path().join(name);
        Command::cargo_bin("map_to_image")
            .unwrap()
            .arg("tests/data/shapes.xmap")
            .arg(&out)
            .assert()
            .success();
        let image = image::open(&out).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(image.width() > 0 && image.height() > 0);
    }
}

/// Where lines cross, the slower one has to win the cell: a path does not
/// make a fence crossable. The sort put the fastest line last instead.
#[test]
fn a_fence_is_not_crossable_where_a_path_crosses_it() {
    let xml = xmap(
        BLACK,
        r#"<symbol type="2" id="0" code="505" name="Path"><line_symbol color="0" line_width="250"/></symbol>
<symbol type="2" id="1" code="516" name="Fence"><line_symbol color="0" line_width="250"/></symbol>"#,
        // A path along y = 5 mm and a fence along x = 5 mm, crossing at (5, 5).
        // The fence comes first in the file, the path is faster: both used
        // to hand the crossing to the path.
        r#"<object type="1" symbol="1"><coords count="2">5000 0;5000 10000;</coords></object>
<object type="1" symbol="0"><coords count="2">0 5000;10000 5000;</coords></object>"#,
    );
    let (map, _) = read_xml_map_str(&xml).unwrap();
    let raster = build(
        &map,
        &Options {
            speeds: vec![("505".to_string(), 1.0), ("516".to_string(), 0.0)],
            pixel_size: 0.5,
            mask_outside_convex_hull: false,
            ..Options::default()
        },
    )
    .unwrap();
    let (speed, code) = raster
        .sample(5.0, 5.0)
        .expect("the crossing is on the grid");
    assert_eq!((speed, code), (0.0, Some("516")));
    // Away from the crossing the path is still a path.
    let (speed, code) = raster.sample(2.0, 5.0).unwrap();
    assert_eq!((speed, code), (1.0, Some("505")));
}
