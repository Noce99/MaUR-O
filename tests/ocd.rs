//! That an OCAD file comes through the importer as the map it holds.
//!
//! The importer's output is XML, so every one of these reads that XML back
//! with the crate's own parser and asks the resulting map what it contains.
//! That is the pair the app depends on, and a change to either which the
//! other does not expect shows up here.
//!
//! Three files, one from each layout family the format has had: version 6 and
//! version 8 share the oldest, version 9 the next. They are in `maps/`, which
//! is left out of the published package.

use std::path::{Path, PathBuf};

use maur_o::map::Map;
use maur_o::ocd::{is_ocd_file, ocd_to_omap_xml};
use maur_o::renderer::Renderer;
use maur_o::xml_reader::read_xml_map_str;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("maps")
        .join(name)
}

/// Imports a file and parses what came out, as the app does.
fn import(name: &str) -> (Map, String, u16, Vec<String>) {
    let bytes = std::fs::read(fixture(name)).unwrap_or_else(|e| panic!("cannot read {name}: {e}"));
    let imported = ocd_to_omap_xml(&bytes).unwrap_or_else(|e| panic!("cannot import {name}: {e}"));
    let (map, warnings) = read_xml_map_str(&imported.xml)
        .unwrap_or_else(|e| panic!("the XML written for {name} does not parse: {e}"));
    assert!(
        warnings.is_empty(),
        "{name} parsed back with warnings: {warnings:?}"
    );
    (map, imported.xml, imported.version, imported.warnings)
}

#[test]
fn the_vendor_mark_is_what_identifies_a_file() {
    let bytes = std::fs::read(fixture("SampleMap.ocd")).unwrap();
    assert!(is_ocd_file(&bytes));
    assert!(!is_ocd_file(b"<?xml version="));
    assert!(!is_ocd_file(b""));
}

#[test]
fn anything_else_is_refused_with_a_reason() {
    let not_ocd = b"<?xml not ocd data, but long enough to pass the size check.......";
    let err = ocd_to_omap_xml(not_ocd).unwrap_err();
    assert!(err.contains("vendor mark"), "{err}");

    // The mark alone is not enough: a version has to be one this reads.
    let mut bogus = vec![0u8; 64];
    bogus[0] = 0xad;
    bogus[1] = 0x0c;
    bogus[4] = 99;
    let err = ocd_to_omap_xml(&bogus).unwrap_err();
    assert!(err.contains("version 99"), "{err}");
}

#[test]
fn a_version_9_file_comes_through_whole() {
    let (map, xml, version, _) = import("SampleMap.ocd");
    assert_eq!(version, 9);
    assert_eq!(map.colors.len(), 37);
    assert_eq!(map.symbols.len(), 178);
    assert_eq!(map.objects.len(), 952);

    // Its ScalePar string: 1:15000, UTM zone 10, offset 520000/5221000,
    // 17 degrees between magnetic and grid north.
    assert_eq!(map.scale_denominator, 15000);
    assert!(xml.contains("<parameter>32610</parameter>"), "EPSG code");
    assert!(
        xml.contains("<ref_point x=\"520000\" y=\"5221000\"/>"),
        "reference point"
    );
    assert!(xml.contains("grivation=\"17\""), "grivation");

    // The symbol numbers become ISOM-style codes, and a contour is a brown
    // line with a width.
    let contour = map
        .symbols
        .iter()
        .find(|s| s.code() == "101")
        .expect("no contour symbol");
    let maur_o::map::Symbol::Line(line) = contour else {
        panic!("the contour symbol is not a line symbol");
    };
    assert!(line.line_width > 0.0);
    assert!(line.color >= 0, "the contour has no colour");

    // Every object was drawn with a symbol the parser resolved.
    for (i, object) in map.objects.iter().enumerate() {
        assert!(object.symbol_index.is_some(), "object {i} has no symbol");
    }
}

#[test]
fn a_version_8_file_comes_through_whole() {
    let (map, _, version, _) = import("testit.ocd");
    assert_eq!(version, 8);
    assert_eq!(map.colors.len(), 33);
    assert_eq!(map.symbols.len(), 166);
    assert_eq!(map.objects.len(), 3);
    assert_eq!(map.scale_denominator, 15000);
}

#[test]
fn a_version_6_file_comes_through_whole_and_draws() {
    let (map, _, version, _) = import("Lake Sammamish.ocd");
    assert_eq!(version, 6);
    assert_eq!(map.colors.len(), 32);
    assert_eq!(map.symbols.len(), 236);
    assert_eq!(map.objects.len(), 1097);

    // The text of that era is in the single-byte encoding, and has to survive
    // the way out of it.
    let texts: Vec<&str> = map
        .objects
        .iter()
        .filter_map(|o| match &o.kind {
            maur_o::map::ObjectKind::Text(t) if !t.text.is_empty() => Some(t.text.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        texts.len() > 10,
        "expected text objects, got {}",
        texts.len()
    );
    assert!(
        texts.iter().any(|t| t.contains("Lake Samm")),
        "the map's title is missing"
    );

    // And the whole thing is a map a renderer can be pointed at.
    let extent = Renderer::new(&map).extent();
    assert!(!extent.is_null());
    assert!(
        extent.width() > 100.0 && extent.height() > 100.0,
        "{extent:?}"
    );
}

#[test]
fn paths_keep_their_curves_and_their_closings() {
    let (map, _, _, _) = import("SampleMap.ocd");
    let mut curves = 0;
    let mut closes = 0;
    for object in &map.objects {
        for coord in &object.coords {
            if coord.is_curve_start() {
                curves += 1;
            }
            if coord.is_close_point() {
                closes += 1;
            }
        }
    }
    assert!(curves > 100, "expected many bezier curves, got {curves}");
    assert!(closes > 10, "expected closed areas, got {closes}");
}

#[test]
fn what_the_import_could_not_carry_is_reported() {
    let (_, _, _, warnings) = import("SampleMap.ocd");
    assert!(
        warnings.iter().any(|w| w.contains("marked hidden")),
        "a symbol switched off in OCAD should be reported: {warnings:?}"
    );
    assert!(
        warnings.iter().any(|w| w.contains("cell grid")),
        "a rectangle symbol's grid should be reported: {warnings:?}"
    );
}
